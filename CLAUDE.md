# CLAUDE.md

Rust Streamable HTTP MCP server bridging one statically configured IMAP
mailbox to clients authenticated with a bearer token.

## Build & Test

```bash
cargo build
cargo test
cargo fmt -- --check
cargo clippy -- -D warnings
```

## Version Control

We use `jj` (Jujutsu) when available, otherwise plain `git`.

## Error Handling

Never suppress errors with `.unwrap()`, `.expect()`, or silent `let _ =`. Propagate errors using `?` and return meaningful errors as late as possible. Use `thiserror` for typed domain errors and `anyhow` for ad-hoc context.

## Project Layout

- `src/main.rs` — entrypoint, Axum server setup
- `src/lib.rs` — app config, shared state
- `src/mcp.rs` — MCP tool definitions
- `src/imap.rs` — IMAP client operations
- `src/session.rs` — static account types and Redis download tickets
- `src/error.rs` — error types
- `src/extract.rs` — text extraction for attachments (PDF, DOCX, XLSX, PPTX)
- `tests/integration.rs` — integration tests
