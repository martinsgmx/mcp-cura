# syntax=docker/dockerfile:1

FROM rust:1.89-bookworm AS builder
WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/mcp-cura \
        --shell /usr/sbin/nologin mcp-cura \
    && install -d -o mcp-cura -g mcp-cura -m 0700 /var/lib/mcp-cura

COPY --from=builder /build/target/release/mcp-cura /usr/local/bin/mcp-cura

USER mcp-cura
WORKDIR /var/lib/mcp-cura

# HTTP is the only useful transport in a container. A token is still required
# when binding to a non-loopback address.
ENV MCP_CURA_TRANSPORT=http \
    MCP_CURA_HTTP_HOST=0.0.0.0 \
    MCP_CURA_HTTP_PORT=8765 \
    MCP_CURA_STATE_DIR=/var/lib/mcp-cura \
    MCP_CURA_LOG_LEVEL=info

EXPOSE 8765

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8765/health || exit 1

ENTRYPOINT ["mcp-cura"]
CMD ["serve"]
