# syntax=docker/dockerfile:1.7

# ── Stage 1: Build the React frontend ────────────────────────────────
FROM node:22-bookworm-slim AS web-build
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# ── Stage 2: Build the Rust workspace ────────────────────────────────
FROM rust:1-bookworm AS rust-build
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY migrations/ migrations/

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin cstat-api --bin cstat-ingest \
 && cp target/release/cstat-api target/release/cstat-ingest /usr/local/bin/

# ── Stage 3: Slim runtime image ──────────────────────────────────────
FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /app cstat \
 && mkdir -p /app/web /app/training/models \
 && chown -R cstat:cstat /app

COPY --from=rust-build /usr/local/bin/cstat-api  /usr/local/bin/cstat-api
COPY --from=rust-build /usr/local/bin/cstat-ingest /usr/local/bin/cstat-ingest
COPY --from=web-build  /web/dist /app/web/dist
COPY training/models/margin_model.onnx \
     training/models/win_model.onnx \
     training/models/model_meta.json \
     /app/training/models/

USER cstat
EXPOSE 8080

# API is the default entrypoint. The cron service overrides this with:
#   cstat-ingest update --year 2026
CMD ["cstat-api"]
