# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`reqsink` is a lightweight HTTP request sink (inspired by requestbin), built as a single self-contained Rust binary. It accepts requests on any route, stores them, and exposes them via an `/admin` GUI. Optionally it renders user-defined Tera templates as custom responses and persists overflow requests to SQLite.

## Commands

```bash
cargo build                       # debug build
cargo build --release             # release binary at target/release/reqsink
cargo test                        # run all tests
cargo test basic_response         # run a single test by name
cargo clippy -- -D warnings       # lint; CI fails on ANY warning, so keep clippy clean
cargo run -- --port 8000          # run locally
```

Run with user-defined templates + routes:
```bash
cargo run -- --user-templates-dir examples --extra-routes ./examples/example-routes.json
```

Run with custom request-ignore rules:
```bash
cargo run -- --ignore-rules ./examples/ignore-rules.json          # appended to the defaults
cargo run -- --ignore-rules ./examples/ignore-rules.json --no-default-ignore
```

CI (`.github/workflows/ci.yml`) pins **Rust 1.90** (also mirrored in `create_release.yml`, the `RUST_VERSION` arg in `Dockerfile`, and `rust-version` in `Cargo.toml` — keep all four in sync) and runs `cargo build`, `cargo clippy -- -D warnings`, and `cargo test`. Match that toolchain when reproducing CI locally.

## Architecture

Three source files, single-threaded synchronous request loop (deliberately — avoids Hyper/Tokio):

- **`src/main.rs`** — CLI parsing (`Opts` via clap), the `AppContext` struct (holds Tera engine, `req_cache: Vec<StoredRequest>`, user templates map, ignore rules, and opts), template loading, and the `main()` request loop.
- **`src/serve.rs`** — the request handlers plus SQLite persistence, gzip body decoding, and query-param parsing. Also contains the `#[cfg(test)]` module.
- **`src/ignore.rs`** — the request-ignore rule type, its `*`-only glob matcher, the built-in defaults, and JSON rule-file loading.

The `main()` loop dispatches by path, **in this order**:

| Path | Handler |
| --- | --- |
| `/admin/clear` | `handle_admin_clear` (POST only; 405 otherwise) |
| `/admin` | `handle_admin` |
| `/__static__*` | `handle_static` |
| anything else | `handle_req` |

`/admin/clear` must stay ahead of the catch-all, or clearing the cache would itself be recorded.

Key data flow:
- Every request (except the admin/static routes) is captured as a `StoredRequest` (defined in `main.rs`) and pushed to the in-memory `req_cache` — **unless** it matches an ignore rule. Ignoring only suppresses recording; the response, including any user template, is produced as normal.
- When `req_cache` exceeds `--req-limit`, `prune_requests` drains the oldest 10% and, if `--sqlite` is set, spawns a thread that bincode-serializes + snappy-compresses them into a `stored_request` BLOB table.
- `handle_req` matches the request path against the user-templates map. A route only responds with its template if the HTTP method also matches (one method per route — see TODO in README); otherwise the default `"OK"` is returned.
- `handle_admin` filters `req_cache` by the optional `q` query param (case-insensitive substring on `path`, applied to the whole cache) and then paginates the result (`PAGE_SIZE` = 10, newest first) into the embedded `admin.html` Tera template. Ordering is done in Rust, so the template iterates `reqs` directly — do **not** re-add a `| reverse` filter.
- `handle_admin_clear` empties `req_cache` and, when `--sqlite` is set, `DELETE`s + `VACUUM`s the archive table. It returns 303 to `/admin`. It deliberately does not `.unwrap()` on SQLite errors — an admin action must not take the server down.

## Embedded assets

Assets are compiled into the binary via `rust-embed`, so there are no runtime file dependencies:
- `templates/admin.html` → embedded as `EmbeddedTemplates` (main.rs)
- `static/*` (bootstrap, jquery, highlight.js) → embedded as `StaticContent`, served under `/__static__/`

If you change `templates/` or `static/`, a rebuild is required for the change to take effect.

## Conventions

- The codebase leans heavily on `.unwrap()` in request handling — this is intentional for a dev/testing tool, but keep clippy clean since CI blocks on warnings.
- `StoredRequest` fields are what user templates can access via `{{ request.* }}` (e.g. `request.ip_addr`, `request.body`, `request.headers`). Changing this struct changes the template API *and* invalidates existing `--sqlite` archives (bincode has no schema evolution).
- `StoredRequest` carries the timestamp twice: `time` (RFC2822 UTC string, the stable template API) and `time_epoch_ms` (i64). The admin page renders `time` server-side and rewrites it to the viewer's timezone in JS using `time_epoch_ms`, so it degrades gracefully without JS.
- Every short clap flag (`-e -i -p -r -s -u`) is taken. New options must be long-only or clap panics at startup on the duplicate.
- Tests in `serve.rs` use `tiny_http::TestRequest` and a `TestServer` wrapper around `AppContext`; follow that pattern for new handler tests. Build options with `TestServer::with_args(&["--flag"])`, which uses `Opts::parse_from` — never `Opts::parse()`, which would swallow the test harness' own argv and break `cargo test <name>`.
- Tera autoescapes `.html` templates, so a rendered path shows up as `&#x2F;api&#x2F;x`, not `/api/x`. Assertions against response bodies go through the `esc()` test helper.
