# -*- mode: dockerfile -*-
# syntax=docker/dockerfile:1
#
# Multi-stage build for reqsink.
#
# Built natively per architecture by .github/workflows/docker.yml (linux/amd64 on
# ubuntu-latest, linux/arm64 on ubuntu-24.04-arm), so there is no cross-compilation
# or QEMU emulation here -- which matters because rusqlite's `bundled` feature
# compiles SQLite from C source and is painfully slow under emulation.

# Keep in sync with the toolchain pinned in .github/workflows/ci.yml.
ARG RUST_VERSION=1.90
ARG DEBIAN_RELEASE=bookworm

# ---------------------------------------------------------------------------
# Build stage
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS builder

# gcc + libc6-dev are required by rusqlite's `bundled` feature, which compiles
# sqlite3.c via cc-rs. binutils (for `strip`) comes in as a gcc dependency.
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc \
        libc6-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/reqsink

# --- dependency pre-build layer -------------------------------------------
# Compile just the dependency graph against a stub binary. This layer is keyed
# on Cargo.toml/Cargo.lock alone, so editing Rust source does not force a
# recompile of SQLite and friends.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src \
    # Drop every artifact belonging to the stub crate. Without this, Cargo may
    # consider the real sources "fresh" (COPY preserves mtimes from the build
    # context) and happily ship the stub binary.
    && rm -rf \
        target/release/reqsink \
        target/release/reqsink.d \
        target/release/deps/reqsink-* \
        target/release/.fingerprint/reqsink-*

# --- real build ------------------------------------------------------------
COPY . .
RUN cargo build --release --locked \
    && strip target/release/reqsink

# ---------------------------------------------------------------------------
# Runtime stage
# ---------------------------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim

# No apt-get here on purpose. reqsink is a plain-HTTP server -- tiny_http is used
# without its `ssl` feature and nothing in the dependency tree makes outbound TLS
# calls -- so ca-certificates would be dead weight. Keeping this stage free of
# package installs also makes the image reproducible without network access.
# `useradd` ships in debian-slim already.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin reqsink \
    # /data is the suggested mount point for `--sqlite`; pre-create it with the
    # right owner so a bind mount does not blow up with EACCES under the
    # non-root user.
    && mkdir /data \
    && chown 10001:10001 /data

COPY --from=builder /usr/src/reqsink/target/release/reqsink /usr/local/bin/reqsink

USER 10001:10001
WORKDIR /data

# reqsink binds 0.0.0.0:8000 by default; 8000 > 1024 so no privileges needed.
EXPOSE 8000

# exec form: lets `docker run <image> --port 9000` pass flags straight through,
# and lets the process receive SIGTERM directly instead of it being swallowed
# by a PID 1 shell.
ENTRYPOINT ["/usr/local/bin/reqsink"]
