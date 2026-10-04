# syntax=docker/dockerfile:1
FROM rust:1-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install --no-install-recommends -y build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/zendesk-mcp-server /zendesk-mcp-server

FROM debian:bookworm-slim AS runtime

# reqwest uses rustls with the platform verifier, so only CA certificates are needed.
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --shell /usr/sbin/nologin appuser \
    && mkdir /tokens \
    && chown appuser:appuser /tokens

COPY --from=builder /zendesk-mcp-server /usr/local/bin/zendesk-mcp-server

ENV ZENDESK_TOKEN_FILE=/tokens/tokens.json \
    ZENDESK_MOBILE_TOKEN_FILE=/tokens/mobile_token.json

USER appuser
EXPOSE 8080

# Serves over stdio by default. Set MCP_HTTP_ADDR and MCP_BEARER_TOKEN for HTTP,
# or pass `auth --manual` to authorize.
ENTRYPOINT ["zendesk-mcp-server"]
