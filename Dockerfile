# Runtime image: single Rust binary (proxy + live). Author: kejiqing
# Multi-stage build keeps the runtime image small (idle RSS target ≤50MB).
ARG RUST_IMAGE=rust:bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
# Release tag/version for `claude-tap --version` (no .git in image context).
ARG CLAUDE_TAP_GIT_VERSION=

FROM ${RUST_IMAGE} AS builder
ARG CLAUDE_TAP_GIT_VERSION
ENV CLAUDE_TAP_GIT_VERSION=${CLAUDE_TAP_GIT_VERSION}
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY assets ./assets
RUN cargo build --release -p claude-tap

FROM ${RUNTIME_IMAGE}
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /src/target/release/claude-tap /usr/local/bin/claude-tap
EXPOSE 8080 3000
CMD ["claude-tap", "--tap-no-launch", "--tap-host", "0.0.0.0", "--tap-port", "8080", "--tap-live", "--tap-live-port", "3000", "--tap-output-dir", "/data/traces", "--tap-no-update-check", "--tap-no-auto-update"]
