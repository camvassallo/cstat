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

# Build release binaries, then stage them along with any onnxruntime shared
# libs `ort` downloaded into target/. The libs are needed at runtime for
# dynamic linking; `find` finds nothing if ort static-links, which is fine.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin cstat-api --bin cstat-ingest \
 && mkdir -p /artifacts/bin /artifacts/lib \
 && cp target/release/cstat-api target/release/cstat-ingest /artifacts/bin/ \
 && find target -name 'libonnxruntime.so*' -print -exec cp -P {} /artifacts/lib/ \; \
 && touch /artifacts/lib/.keep \
 && ls -la /artifacts/bin/ /artifacts/lib/

# ── Stage 3: Slim runtime image ──────────────────────────────────────
FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /app cstat \
 && mkdir -p /app/web /app/training/models \
 && chown -R cstat:cstat /app

COPY --from=rust-build /artifacts/bin/cstat-api  /usr/local/bin/cstat-api
COPY --from=rust-build /artifacts/bin/cstat-ingest /usr/local/bin/cstat-ingest
COPY --from=rust-build /artifacts/lib/ /usr/local/lib/onnxruntime/
RUN echo /usr/local/lib/onnxruntime > /etc/ld.so.conf.d/onnxruntime.conf && ldconfig

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
