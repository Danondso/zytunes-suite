# Build context is the repo root (zytunes-stream depends on the root
# `zytunes` crate via a path dependency, so the whole workspace has to be
# in the build context): docker build -f zytunes-stream/Dockerfile .
# The provided docker-compose.yml already sets this up.

# --- Build stage -----------------------------------------------------------
FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    libclang-dev \
    pkg-config \
    libasound2-dev \
    libdiscid-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Workspace members zytunes-stream's Cargo.lock resolution needs, even
# though only the zytunes-stream binary gets built below.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY zune-mtp ./zune-mtp
COPY ipod-db ./ipod-db
COPY zytunes-stream ./zytunes-stream

RUN cargo build --release -p zytunes-stream && \
    strip target/release/zytunes-serve

# --- Runtime stage -----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# libasound2 / libdiscid0: runtime libs for the alsa (rodio) and libdiscid
# dependencies the root `zytunes` crate pulls in — zytunes-serve itself
# doesn't play audio or read discs, but it links the same binary-wide deps.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libasound2 \
    libdiscid0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin zytunes \
    # Pre-create the cache mountpoint owned by zytunes: a fresh named
    # volume copies this ownership on first mount. Without it Docker
    # creates the mountpoint root-owned and every cache write fails
    # with EACCES, forcing a full library rescan on each start.
    && mkdir -p /home/zytunes/.cache/zytunes \
    && chown -R zytunes:zytunes /home/zytunes/.cache

COPY --from=builder /build/target/release/zytunes-serve /usr/local/bin/zytunes-serve

USER zytunes
ENV HOME=/home/zytunes
WORKDIR /home/zytunes

EXPOSE 9847

ENTRYPOINT ["zytunes-serve"]
