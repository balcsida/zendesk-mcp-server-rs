# syntax=docker/dockerfile:1
FROM lukemathwalker/cargo-chef:latest-rust-1-slim-bookworm AS planner
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM lukemathwalker/cargo-chef:latest-rust-1-slim-bookworm AS builder
WORKDIR /src

# Dependencies compile in their own layer, cached until Cargo.toml/Cargo.lock change.
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked \
    && mkdir -m 700 /tokens

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

COPY --from=builder /src/target/release/zendesk-mcp-server /usr/local/bin/zendesk-mcp-server
# Distroless has no shell, so the token directory is created in the builder stage.
COPY --from=builder --chown=65532:65532 /tokens /tokens

# Inside a container the network namespace is the boundary, so `http` listens on
# every interface; publish the port with -p to expose it.
ENV ZENDESK_TOKEN_FILE=/tokens/tokens.json \
    ZENDESK_MOBILE_TOKEN_FILE=/tokens/mobile_token.json \
    MCP_HTTP_ADDR=0.0.0.0:8080

# reqwest uses rustls with the platform verifier; the base image ships the CA certificates.
LABEL org.opencontainers.image.source=https://github.com/balcsida/zendesk-mcp-server-rs
USER nonroot
EXPOSE 8080

# Serves over stdio by default. Pass `http` (with MCP_BEARER_TOKEN or MCP_PER_USER_AUTH
# set) for streamable HTTP, or `auth --manual` to authorize.
ENTRYPOINT ["zendesk-mcp-server"]
