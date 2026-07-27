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

CI (`.github/workflows/ci.yml`) pins **Rust 1.73** and runs `cargo build`, `cargo clippy -- -D warnings`, and `cargo test`. Match that toolchain when reproducing CI locally.

## Architecture

Two source files, single-threaded synchronous request loop (deliberately — avoids Hyper/Tokio):

- **`src/main.rs`** — CLI parsing (`Opts` via clap), the `AppContext` struct (holds Tera engine, `req_cache: Vec<StoredRequest>`, user templates map, and opts), template loading, and the `main()` request loop. The loop dispatches each incoming request by path: `/admin` → `handle_admin`, `/__static__*` → `handle_static`, everything else → `handle_req`.
- **`src/serve.rs`** — the three handlers plus SQLite persistence, gzip body decoding, and query-param parsing. Also contains the `#[cfg(test)]` module.

Key data flow:
- Every request (except `/admin` and `/__static__`) is captured as a `StoredRequest` (defined in `main.rs`) and pushed to the in-memory `req_cache`.
- When `req_cache` exceeds `--req-limit`, `prune_requests` drains the oldest 10% and, if `--sqlite` is set, spawns a thread that bincode-serializes + snappy-compresses them into a `stored_request` BLOB table.
- `handle_req` matches the request path against the user-templates map. A route only responds with its template if the HTTP method also matches (one method per route — see TODO in README); otherwise the default `"OK"` is returned.
- `handle_admin` paginates `req_cache` (10 per page, newest first) into the embedded `admin.html` Tera template.

## Embedded assets

Assets are compiled into the binary via `rust-embed`, so there are no runtime file dependencies:
- `templates/admin.html` → embedded as `EmbeddedTemplates` (main.rs)
- `static/*` (bootstrap, jquery, highlight.js) → embedded as `StaticContent`, served under `/__static__/`

If you change `templates/` or `static/`, a rebuild is required for the change to take effect.

## Conventions

- The codebase leans heavily on `.unwrap()` in request handling — this is intentional for a dev/testing tool, but keep clippy clean since CI blocks on warnings.
- `StoredRequest` fields are what user templates can access via `{{ request.* }}` (e.g. `request.ip_addr`, `request.body`, `request.headers`). Changing this struct changes the template API.
- Tests in `serve.rs` use `tiny_http::TestRequest` and a `TestServer` wrapper around `AppContext`; follow that pattern for new handler tests.
