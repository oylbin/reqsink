use tiny_http::{Response, Header, Request};
use tera::{Context};
use std::io::{Read, Cursor};
use std::net::{ IpAddr, Ipv4Addr };
use chrono::{Utc};
use std::collections::HashMap;
use url::{form_urlencoded, Url};
use crate::ignore::is_ignored;
use crate::{AppContext, StoredRequest};
use snap::{write};
use std::thread;
use std::sync::{Mutex, Arc};
use rusqlite::{params, Connection};
use rust_embed::RustEmbed;

use log::{info, error};
use flate2::read::GzDecoder;

/// Number of requests shown per page on the admin GUI
const PAGE_SIZE: usize = 10;

#[derive(RustEmbed)]
#[folder = "static"]
struct StaticContent;

/// Percent-decodes and `+`-unescapes the query string. Hand-splitting on `&`/`=`
/// would hand back raw `%2F`/`+` for anything a browser form submits.
fn get_param_map(url: &Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

pub fn handle_static(request: &mut Request) -> Response<Cursor<Vec<u8>>>  {
    // There's so much unwrapping here, its like Christmas!

    let base_url: Url = Url::parse("http://reqsink.local/").unwrap();
    let url = base_url.join(request.url()).unwrap();

    let req_file = url.path_segments().unwrap().last().unwrap();
    if let Some(content) = StaticContent::get(req_file) {
        let raw = std::str::from_utf8(content.as_ref()).unwrap();
        let mut resp = Response::from_data(raw);
        // TODO Send the right content-type for css
        resp.add_header(Header::from_bytes( &b"Content-Type"[..], &b"text/javascript; charset=UTF-8"[..] ).unwrap());
        resp.add_header(Header::from_bytes( &b"Cache-Control"[..], &b"public, max-age=604800, immutable"[..] ).unwrap());

        resp
    } else {
        Response::from_string("I couldn't find that.")
    }
}

pub fn handle_admin(request: &Request, app_ctx: &mut AppContext) -> Response<Cursor<Vec<u8>>>  {
    let mut context = Context::new();
    let base_url: Url = Url::parse("http://reqsink.local/").unwrap();
    let url = base_url.join(request.url()).unwrap();
    let param_map = get_param_map(&url);

    // Optional path filter. Matching is a case-insensitive substring test rather
    // than a glob: that is what a search box is expected to do.
    let q = param_map.get("q").map(|s| s.trim()).unwrap_or("");
    let needle = q.to_lowercase();
    let matched: Vec<&StoredRequest> = app_ctx.req_cache.iter()
        .filter(|r| needle.is_empty() || r.path.to_lowercase().contains(&needle))
        .collect();
    let total_matched = matched.len();

    // `start` counts backwards from the newest matching request, so that page
    // boundaries stay stable as long as no new requests come in.
    let start = param_map.get("start")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total_matched);

    let page: Vec<&StoredRequest> = matched.iter().rev()
        .skip(start).take(PAGE_SIZE).copied().collect();

    info!("Returning admin reqs {:?} to {:?} of {:?} matching {:?}",
          start, start + page.len(), total_matched, q);

    context.insert("reqs", &page);
    context.insert("req_count", &app_ctx.req_cache.len());
    context.insert("total_matched", &total_matched);
    context.insert("q", q);
    // Pre-encoded for use in the pagination hrefs; tera's autoescape then only has
    // to worry about HTML, not URL syntax.
    let q_encoded: String = form_urlencoded::byte_serialize(q.as_bytes()).collect();
    context.insert("q_encoded", &q_encoded);
    context.insert("range_from", &(if page.is_empty() { 0 } else { start + 1 }));
    context.insert("range_to", &(start + page.len()));
    context.insert("prev_page", &start.saturating_sub(PAGE_SIZE));
    context.insert("next_page", &(start + PAGE_SIZE));
    context.insert("has_prev", &(start > 0));
    context.insert("has_next", &(start + PAGE_SIZE < total_matched));

    let rend = app_ctx.tera.render("admin.html", &context).unwrap();

    let mut resp = Response::from_data(rend);
    resp.add_header(Header::from_bytes(
        &b"Content-Type"[..],
        &b"text/html; charset=UTF-8"[..]
    ).unwrap());

    resp
}

/// Drop every stored request: the in-memory cache and, when --sqlite is in use,
/// the archive table as well.
///
/// Only POST is accepted so that a crawler or a browser link-prefetch cannot wipe
/// the cache by following a URL.
pub fn handle_admin_clear(request: &Request, app_ctx: &mut AppContext) -> Response<Cursor<Vec<u8>>> {
    if !request.method().as_str().eq_ignore_ascii_case("POST") {
        return Response::from_string("Method Not Allowed - use POST to clear the cache")
            .with_status_code(405);
    }

    let cleared = app_ctx.req_cache.len();
    app_ctx.req_cache.clear();

    // Known race: prune_requests() persists overflow on a background thread, so a
    // flush that is in flight right now may land after this DELETE and survive it.
    // Locking the db for a dev/testing tool costs more than the stray rows do.
    if let Some(db_path) = &app_ctx.opts.sqlite {
        if let Err(e) = clear_persisted(db_path) {
            error!("Cleared {:?} cached requests but could not clear {:?}: {}", cleared, db_path, e);
            return Response::from_string(
                format!("Cleared {} in-memory requests, but clearing {} failed: {}", cleared, db_path, e)
            ).with_status_code(500);
        }
    }

    info!("Cleared {:?} requests from the cache", cleared);

    // 303 rather than 302: the browser must follow up with a GET, not re-POST.
    let mut resp = Response::from_data(Vec::new()).with_status_code(303);
    resp.add_header(Header::from_bytes(&b"Location"[..], &b"/admin"[..]).unwrap());
    resp
}

fn clear_persisted(sqlite: &str) -> Result<(), rusqlite::Error> {
    let conn = Connection::open(sqlite)?;
    // The table may not exist yet if nothing has overflowed the cache. VACUUM cannot
    // run inside a transaction, hence execute_batch rather than a transaction.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS stored_request (id INTEGER PRIMARY KEY, data BLOB); \
         DELETE FROM stored_request; \
         VACUUM;"
    )?;
    conn.close().map_err(|(_, e)| e)
}

fn headers_to_hashmap(raw_headers: &[Header]) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for tup in raw_headers {
        headers.insert(
            tup.field.as_str().to_string(),
            tup.value.as_str().to_string()
        );
    }
    headers
}

fn persist_requests(srs: &[StoredRequest], sqlite: &str) {
    /* TODO There is something strange about the rusqlite API that makes it painful to
     * wrap a transaction around a prepared statement. The performance penalty of re-parsing
     the INSERT INTO ... each time doesn't seem to be too bad since this function will
     execute in a thread spun off the main request handler
     */
    // let mut stmt = conn.prepare("INSERT INTO stored_request (data) VALUES (?1)").unwrap();
    info!("In a thread! Got {:?} requests to persist to {:?}", srs.len(), sqlite);
    let conn = Connection::open(sqlite).unwrap();
    conn.execute("CREATE TABLE IF NOT EXISTS stored_request (id INTEGER PRIMARY KEY, data BLOB)", params![]).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    for sr in srs {
        let mut wtr = write::FrameEncoder::new(vec![]);
        bincode::serialize_into(&mut wtr, &sr).unwrap();
        let comp_bytes: Vec<u8> = wtr.into_inner().unwrap();
        conn.execute("INSERT INTO stored_request (data) VALUES (?1)", params![comp_bytes]).unwrap();
    }
    tx.commit().unwrap();
    conn.close().unwrap();
}

/// Drain the last pct% of requests from the request cache and spin up a thread to
/// persist them to storage
fn prune_requests(app_ctx: &mut AppContext, pct: f32) {
    let prune = (app_ctx.opts.req_limit as f32  * pct) as usize;
    info!("Reqcache hit max size {:?}, removing {:?}.", app_ctx.opts.req_limit, prune);
    let drained: Vec<StoredRequest> = app_ctx.req_cache.drain(0..prune).collect();
    let sqlite = &app_ctx.opts.sqlite;
    if let Some(db_path) = sqlite.clone() {
        // This is compiling... but somehow this feels a bit too verbose to be the Right Way
        // to spin off a worker
        let adb = Arc::new(Mutex::new(db_path));
        let adrained = Arc::new(Mutex::new(drained));
        thread::spawn(move || {
            let adb = Arc::clone(&adb);
            let adrained = Arc::clone(&adrained);
            let db = &*adb.lock().unwrap();
            let srs = &*adrained.lock().unwrap();
            persist_requests(srs, db);
        });
    }
}

fn get_request_body(request: &mut Request) -> String {
    let mut body = String::new();

    // Check if the content is gzip-encoded
    let content_encoding = request.headers()
        .iter()
        .find(|h| h.field.equiv("Content-Encoding"))
        .and_then(|h| Option::from(h.value.as_str()));

    if content_encoding == Some("gzip") {
        // Use a GzDecoder to decompress the gzipped content
        let mut d = GzDecoder::new(request.as_reader());
        if let Err(e) = d.read_to_string(&mut body) {
            body = format!("Could not parse gzipped request body: {:?}", e);
        }
    } else {
        // Handle non-gzipped content
        if let Err(e) = request.as_reader().read_to_string(&mut body) {
            body = format!("Could not parse request body - is this a binary format? {:?}", e);
        }
    }

    body
}

pub fn handle_req(request: &mut Request, app_ctx: &mut AppContext) -> Response<Cursor<Vec<u8>>> {

    let base_url: Url = Url::parse("http://reqsink.local/").unwrap();
    let url = base_url.join(request.url()).unwrap();

    let body = get_request_body(request);

    let now = Utc::now();
    let sr = StoredRequest {
        time: now.to_rfc2822(),
        time_epoch_ms: now.timestamp_millis(),
        method: request.method().as_str().to_string(),
        path: url.path().to_string(),
        params: url.query().map(str::to_string),
        header_count: request.headers().len(),
        ip_addr: match request.remote_addr() { 
            Some(r) => r.ip(), 
            // tiny_http now support sockets from 0.12 - pretend this is coming from localhost if somehow we
            // get a req like this
            None => IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
        },
        headers: headers_to_hashmap(request.headers()),
        body
    };

    // An ignored request is still answered exactly as it would be otherwise -- including
    // any user-defined template for the route -- it just never enters the cache.
    if is_ignored(&app_ctx.ignore_rules, &sr.method, &sr.path) {
        info!("Not recording ignored request {:} {:}", sr.method, sr.path);
    } else {
        app_ctx.req_cache.push(sr.clone());

        if app_ctx.req_cache.len() > app_ctx.opts.req_limit {
            prune_requests(app_ctx, 0.1);
        }
    }

    let generic_response = Response::from_string("OK");

    if let Some(templates) = &app_ctx.user_templates {
        if let Some(ut) = templates.get(url.path()) {
            if request.method().as_str().to_uppercase() != ut.method.to_uppercase() {
                return generic_response;
            }
            let mut context = Context::new();
            context.insert("request", &sr);
            let rend = app_ctx.tera.render(&ut.template, &context).unwrap();
            let mut resp = Response::from_data(rend);

            let content_type = match &ut.content_type {
                Some(ct) => ct.as_bytes(),
                None => &b"text/html; charset=UTF-8"[..]
            };
            resp.add_header(
                Header::from_bytes(&b"Content-Type"[..], content_type
            ).unwrap());

            resp
        } else {
            generic_response
        }
    } else {
        generic_response
    }
}

/// Basic sanity checking 
#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use clap::Parser;
    use tera::Tera;
    use tiny_http::{TestRequest, Request, Response, Method, StatusCode};
    use crate::{Opts, AppContext, EmbeddedTemplates};

    struct TestServer {
        app_ctx: AppContext,
    }

    impl TestServer {
        fn new() -> Self {
            TestServer::with_args(&[])
        }

        /// `args` are extra command-line arguments, e.g. `&["--no-default-ignore"]`.
        ///
        /// Note we deliberately do *not* use `Opts::parse()`: that reads the test
        /// harness' own argv, so `cargo test <name>` would abort on an unknown
        /// positional argument.
        fn with_args(args: &[&str]) -> Self {
            let mut tera = Tera::default();
            let admin_templ = EmbeddedTemplates::get("admin.html").unwrap();
            let admin_rawstr = std::str::from_utf8(admin_templ.as_ref());
            tera.add_raw_template("admin.html", admin_rawstr.unwrap()).unwrap();

            let mut argv = vec!["reqsink"];
            argv.extend_from_slice(args);
            let opts = Opts::parse_from(argv);
            let ignore_rules = crate::ignore::load_rules(
                opts.ignore_rules.as_ref(), !opts.no_default_ignore
            );

            let app_ctx = super::AppContext{
                tera,
                req_cache: Vec::new(),
                user_templates: None,
                ignore_rules,
                opts
            };
            TestServer { app_ctx }
        }

        fn handle_request(&mut self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
            super::handle_req(request, &mut self.app_ctx)
        }

        fn handle_admin(&mut self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
            super::handle_admin(request, &mut self.app_ctx)
        }

        fn handle_admin_clear(&mut self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
            super::handle_admin_clear(request, &mut self.app_ctx)
        }

        /// Record one request through the normal handler path
        fn send(&mut self, method: Method, path: &str) {
            let mut request: Request = TestRequest::new()
                .with_method(method)
                .with_path(path)
                .into();
            self.handle_request(&mut request);
        }

        /// Fetch the rendered admin page for the given query string
        fn admin_body(&mut self, url: &str) -> String {
            let mut request: Request = TestRequest::new()
                .with_method(Method::Get)
                .with_path(url)
                .into();
            let resp = self.handle_admin(&mut request);
            String::from_utf8(resp.into_reader().into_inner()).unwrap()
        }

        fn cached_paths(&self) -> Vec<&str> {
            self.app_ctx.req_cache.iter().map(|r| r.path.as_str()).collect()
        }
    }

    /// Tera autoescapes `/` to `&#x2F;` in .html templates, so a rendered path never
    /// appears verbatim in the response body.
    fn esc(path: &str) -> String {
        path.replace('/', "&#x2F;")
    }

    #[test]
    fn basic_response() {
        let trequest = TestRequest::new()
            .with_method(Method::Post)
            .with_path("/api/widgets")
            .with_body("42");

        let mut request : Request = trequest.into();

        let mut server = TestServer::new();
        let response = server.handle_request(&mut request);
        assert_eq!(response.status_code(), StatusCode(200));        
        let c = String::from_utf8(response.into_reader().into_inner()).unwrap();
        println!("{:?}", c);
        assert_eq!(c, "OK");
    }

    #[test]
    fn basic_admin_response() {
        let trequest = TestRequest::new()
            .with_method(Method::Get)
            .with_path("/admin");

        let mut request : Request = trequest.into();

        let mut server = TestServer::new();
        let response = server.handle_admin(&mut request);
        assert_eq!(response.status_code(), StatusCode(200));
        let c = String::from_utf8(response.into_reader().into_inner()).unwrap();
        assert!(c.contains("Welcome to reqsink"))
    }

    // -- Ignore rules ------------------------------------------------------

    #[test]
    fn ignored_request_is_not_cached_but_still_answered() {
        let trequest = TestRequest::new()
            .with_method(Method::Get)
            .with_path("/favicon.ico");
        let mut request: Request = trequest.into();

        let mut server = TestServer::new();
        let response = server.handle_request(&mut request);

        assert_eq!(response.status_code(), StatusCode(200));
        let c = String::from_utf8(response.into_reader().into_inner()).unwrap();
        assert_eq!(c, "OK");
        assert!(server.app_ctx.req_cache.is_empty());
    }

    #[test]
    fn non_ignored_requests_are_cached() {
        let mut server = TestServer::new();
        // Default rules only cover GET / and GET /favicon.ico, so the method matters
        server.send(Method::Post, "/");
        server.send(Method::Get, "/api/widgets");
        server.send(Method::Get, "/");

        assert_eq!(server.cached_paths(), vec!["/", "/api/widgets"]);
    }

    #[test]
    fn no_default_ignore_flag_disables_the_built_in_rules() {
        let mut server = TestServer::with_args(&["--no-default-ignore"]);
        server.send(Method::Get, "/favicon.ico");
        server.send(Method::Get, "/");

        assert_eq!(server.cached_paths(), vec!["/favicon.ico", "/"]);
    }

    // -- Timestamps --------------------------------------------------------

    #[test]
    fn stored_request_carries_epoch_millis() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/widgets");

        let sr = &server.app_ctx.req_cache[0];
        let now_ms = chrono::Utc::now().timestamp_millis();
        assert!(sr.time_epoch_ms > 0);
        // Same instant as the human-readable string, give or take test overhead
        assert!((now_ms - sr.time_epoch_ms).abs() < 5_000,
                "epoch {} too far from now {}", sr.time_epoch_ms, now_ms);
    }

    #[test]
    fn admin_page_exposes_epoch_for_client_side_rendering() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/widgets");

        let body = server.admin_body("/admin");
        let ts = server.app_ctx.req_cache[0].time_epoch_ms;
        assert!(body.contains(&format!("data-ts=\"{}\"", ts)), "body was: {}", body);
    }

    // -- Search ------------------------------------------------------------

    #[test]
    fn admin_filters_by_path_query() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/alpha");
        server.send(Method::Get, "/api/beta");
        server.send(Method::Get, "/other/gamma");

        let body = server.admin_body("/admin?q=/api");
        assert!(body.contains(&esc("/api/alpha")));
        assert!(body.contains(&esc("/api/beta")));
        assert!(!body.contains(&esc("/other/gamma")));
    }

    #[test]
    fn admin_search_is_case_insensitive_substring_match() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/Widgets");
        let hit = esc("/api/Widgets");

        assert!(server.admin_body("/admin?q=WIDGET").contains(&hit));
        assert!(server.admin_body("/admin?q=widget").contains(&hit));
        assert!(!server.admin_body("/admin?q=gadget").contains(&hit));
    }

    #[test]
    fn admin_query_params_are_url_decoded() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/alpha");
        server.send(Method::Get, "/other/gamma");

        // %2F is '/' -- hand-splitting the query string would leave it literal
        let body = server.admin_body("/admin?q=%2Fapi%2F");
        assert!(body.contains(&esc("/api/alpha")));
        assert!(!body.contains(&esc("/other/gamma")));
    }

    #[test]
    fn admin_pagination_respects_the_filter() {
        let mut server = TestServer::new();
        for i in 0..15 {
            server.send(Method::Get, &format!("/api/w{}", i));
        }
        for i in 0..5 {
            server.send(Method::Get, &format!("/other/x{}", i));
        }

        // Newest first, so page one is w14..w5 and there must be a next page
        let page1 = server.admin_body("/admin?q=/api");
        assert!(page1.contains(&esc("/api/w14")));
        assert!(page1.contains(&esc("/api/w5")));
        assert!(!page1.contains(&esc("/api/w4")));
        assert!(page1.contains("Next 10"));
        assert!(!page1.contains("Prev 10"));

        // Page two holds the remaining five and terminates
        let page2 = server.admin_body("/admin?q=/api&start=10");
        assert!(page2.contains(&esc("/api/w4")));
        assert!(page2.contains(&esc("/api/w0")));
        assert!(!page2.contains(&esc("/api/w5")));
        assert!(!page2.contains("Next 10"));
        assert!(page2.contains("Prev 10"));
        // The filter must survive into the pagination links
        assert!(page2.contains("q=%2Fapi"));
    }

    #[test]
    fn admin_start_beyond_the_end_yields_an_empty_page() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/alpha");

        let body = server.admin_body("/admin?start=9999");
        assert!(!body.contains(&esc("/api/alpha")));
        assert!(!body.contains("Next 10"));
    }

    // -- Clear -------------------------------------------------------------

    #[test]
    fn clear_empties_the_cache_and_redirects() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/alpha");
        server.send(Method::Get, "/api/beta");
        assert_eq!(server.app_ctx.req_cache.len(), 2);

        let mut request: Request = TestRequest::new()
            .with_method(Method::Post)
            .with_path("/admin/clear")
            .into();
        let response = server.handle_admin_clear(&mut request);

        assert_eq!(response.status_code(), StatusCode(303));
        assert!(server.app_ctx.req_cache.is_empty());
    }

    #[test]
    fn clear_rejects_non_post_and_leaves_the_cache_alone() {
        let mut server = TestServer::new();
        server.send(Method::Get, "/api/alpha");

        let mut request: Request = TestRequest::new()
            .with_method(Method::Get)
            .with_path("/admin/clear")
            .into();
        let response = server.handle_admin_clear(&mut request);

        assert_eq!(response.status_code(), StatusCode(405));
        assert_eq!(server.app_ctx.req_cache.len(), 1);
    }
}