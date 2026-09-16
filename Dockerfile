# Stage 1: Build
FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release && strip target/release/imap-mcp-lite

# Stage 2: Runtime
FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/Rqeim/imap-mcp-lite"

RUN apt-get update && apt-get install -y ca-certificates libssl3 antiword curl && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home appuser

COPY --from=builder /app/target/release/imap-mcp-lite /usr/local/bin/imap-mcp-lite

USER appuser
EXPOSE 8080
ENV RUST_LOG=info
CMD ["imap-mcp-lite"]
