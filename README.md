# reqsink

A lightweight HTTP request sink inspired by the original [requestbin](https://github.com/Runscope/requestbin), implemented in Rust as a compact, single-file binary for easy deployment.

## Getting started

It's as simple as:

```bash
$ docker run -p 8000:8000 atomic77/reqsink:latest
Total 1 templates loaded:
"admin.html"
Binding to interface "0.0.0.0:8000"
```

Then send a request:

```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{"hello": "world", "foo": "bar"}' \
  localhost:8000/post/some/json
```

The `/admin` route provides a simple GUI showing the current tracked requests. 
Syntax highlighting and pretty-printing is available for formats such as JSON, using the embedded
highlight.js:

![Admin page](static/admin.png)

The admin page also lets you:

* **Filter by path** — the search box does a case-insensitive substring match against the request
  path across the *whole* cache, not just the visible page. It is driven by the `q` query parameter
  (`/admin?q=/api/widgets`), so a filtered view can be bookmarked or shared, and paging through the
  results keeps the filter applied.
* **Clear everything** — the *Clear all requests* button empties the in-memory cache and, when
  `--sqlite` is in use, also deletes the rows from the archive table. It posts to `/admin/clear`;
  only `POST` is accepted, so a crawler or link prefetch cannot wipe the cache by accident.

Timestamps are captured in UTC but rendered in **your browser's timezone**. The server emits both a
UTC string and an epoch-milliseconds attribute, and a small script rewrites the displayed value on
load — so a request logged at `Mon, 27 Jul 2026 07:02:56 +0000` shows as `2026/07/27 15:02:56
(Asia/Shanghai)` for a UTC+8 viewer. With JavaScript disabled the original UTC string remains, and
the UTC value is always available as the element's tooltip.

## Ignoring requests

Browsers send a fair amount of noise (`/favicon.ico`, a bare `GET /` when you open the sink in a
tab) that would otherwise bury the requests you actually care about. These are ignored by default:

| Method | Path |
| --- | --- |
| `GET` | `/favicon.ico` |
| `GET` | `/` |

Pass `--no-default-ignore` to turn the built-ins off, and `--ignore-rules <FILE>` to add your own.
Rules from the file are *appended* to whatever defaults are in effect:

```bash
reqsink --ignore-rules ./examples/ignore-rules.json
reqsink --ignore-rules ./examples/ignore-rules.json --no-default-ignore   # only my rules
```

```json
[
    { "method": "GET", "path": "/robots.txt" },
    { "method": "POST", "path": "/api/heartbeat" },
    { "path": "/health*" },
    { "path": "*.css" }
]
```

* `method` is matched case-insensitively. Omit it (or use `"*"`) to match any method.
* `path` is case-sensitive and supports `*` as a wildcard for any run of characters, including `/`.
* A malformed or missing rules file is a startup error rather than a warning — silently dropping
  rules would leave you thinking they were in effect.

**Ignoring only suppresses recording.** The response is produced exactly as it would be otherwise,
including any user-defined template registered for that route. So you can keep a custom `GET /`
response while keeping `GET /` out of the cache.

If you don't want to use docker, a static binary is available for linux_amd64. Other platforms should work fine, so far I've tested armv7. 

## User-defined templates

The default response to any request to is a terse "OK". If you want to customize the response for a given 
route, `reqsink` supports the use of [Tera](https://github.com/Keats/tera) (jinja-style) templates. 

User templates are rendered with access to a `StoredRequest`. See `main.rs` for the struct definition and the fields available in the template. 
eg. for the custom `robots.txt` response in the examples directory, the template is defined as:

```jinja
# Hello, IP {{ request.ip_addr }}. We've been expecting you.

# Group 1
User-agent: Googlebot
Disallow: /nogooglebot/

```

See the `examples` directory for a configuration and template for a user-defined route. 
Any .html file in `templates-dir` or its subfolders will be treated as a template. 

To run with user-defined templates, you can use a command like the following:
```bash
reqsink --user-templates-dir examples --extra-routes ./examples/example-routes.json
```

## Command line options

```
OPTIONS:
    -e, --extra-routes <EXTRA_ROUTES>
            A JSON file mapping the desired route -> template

    -h, --help
            Print help information

    -i, --ip-address <IP_ADDRESS>
            IP-address to bind to [default: 0.0.0.0]

        --ignore-rules <IGNORE_RULES>
            A JSON file with extra rules describing requests that should not be recorded. Rules are
            appended to the built-in defaults unless --no-default-ignore is given

        --no-default-ignore
            Do not apply the built-in ignore rules (GET /favicon.ico, GET /)

    -p, --port <PORT>
            Port to bind to [default: 8000]

    -r, --req-limit <REQ_LIMIT>
            Maximum number of requests to keep in memory [default: 1000]

    -s, --sqlite <SQLITE>
            Filename of sqlite database to use for persistence (EXPERIMENTAL)

    -u, --user-templates-dir <USER_TEMPLATES_DIR>
            User-defined templates directory. If you want to provide a custom response to a
            particular endpoint, you will need to also provide a JSON file mapping the template to
            the route

    -V, --version
            Print version information
```

## Limitations / TODO items

* Make request store accessible from admin UI
* Ability to export requests   
* The `--sqlite` archive is experimental and its on-disk format is **not** stable across versions:
  rows are bincode-serialized `StoredRequest`s, so adding a field invalidates previously written
  archives. There is currently no read path, so in practice this only matters if you decode the
  BLOBs yourself.
* User-defined templates cannot be used with the same route for more than one method (eg. `/robots.txt` can't have a different `GET` and `POST` response)
