# IMAP MCP Server (Lite)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A lightweight, self-hosted Rust service that exposes a single IMAP mailbox
over the MCP Streamable HTTP transport.

This is a trimmed fork of [factorial-io/imap-mcp](https://github.com/factorial-io/imap-mcp)
(MIT). It keeps the original mail tools and IMAP behaviour, but removes OIDC,
the login flow, the `/manage` UI, and dynamic multi-account management in
favour of:

- **Static bearer auth only** (`AUTH_MODE=static`): a single
  `Authorization: Bearer <MCP_API_TOKEN>` compared in constant time.
- **One statically configured mailbox**: `IMAP_HOST`, `IMAP_PORT`,
  `IMAP_USERNAME`, `IMAP_APP_PASSWORD`.
- **Central Redis** for the only stateful feature left (one-shot attachment
  download tickets), namespaced under `REDIS_KEY_PREFIX`.

## Prerequisites

- Docker and Docker Compose (or a Rust toolchain for local builds)
- A reachable central Redis instance
- An IMAP mail server (TLS, port 993) and an app password for the mailbox
- Traefik (or equivalent) for HTTPS, since MCP clients require it

## System dependencies

Some attachment formats are extracted by shelling out to external binaries.
Install them on the host where the server runs:

| Format | Binary | Install |
| --- | --- | --- |
| Legacy `.doc` (Word 97-2003) | `antiword` | `apt install antiword` (Debian/Ubuntu) |

If `antiword` is not on `PATH`, `.doc` attachments are returned as metadata
only and the server logs a clear "extractor not installed" error. PDF, DOCX,
XLSX, PPTX, and modern formats are handled natively. The provided Docker image
already installs `antiword`.

## Configuration

Copy `.env.example` to `.env` and fill it in:

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `AUTH_MODE` | No | `static` | Only `static` is supported. Anything else aborts at startup. |
| `MCP_API_TOKEN` | Yes | — | Bearer token required on `/mcp`. Never logged. |
| `IMAP_HOST` | No | `mail.privateemail.com` | Static account IMAP host. |
| `IMAP_PORT` | No | `993` | Static account IMAP port. |
| `IMAP_USERNAME` | Yes | — | Static account login (usually the full email address). |
| `IMAP_APP_PASSWORD` | Yes | — | Static account app password. Never logged. |
| `BASE_URL` | No | `http://localhost:8080` | Public URL, no trailing slash. Used for download links and the rmcp allowed-hosts list. |
| `REDIS_URL` | Yes | — | Central Redis connection URL. |
| `REDIS_KEY_PREFIX` | No | `imap-mcp-lite:` | Prefix for every Redis key this app writes. |
| `BIND_ADDR` | No | `0.0.0.0:8080` | Listen address. |
| `RUST_LOG` | No | `info` | Log level. |

Generate the MCP bearer token with:

```bash
openssl rand -hex 32
```

## Deployment

1. Create your `.env` from the example and fill in the values:

```bash
cp .env.example .env
```

2. Point `REDIS_URL` at your central Redis. The compose file does **not**
   start Redis.

3. Deploy:

```bash
docker compose up -d
```

The service is then available at `https://<YOUR_DOMAIN>`.

## Connecting an MCP client

Point the client at `https://<YOUR_DOMAIN>/mcp` and send:

```
Authorization: Bearer <MCP_API_TOKEN>
```

There is no OAuth dance and no browser login. For a client that supports a
custom header, configure it directly; otherwise use whatever
proxy/gateway injects the header.

## Available MCP Tools

| Tool | Description |
|------|-------------|
| `list_accounts` | List the single statically configured mailbox (no credentials returned) |
| `list_folders` | List all IMAP mailbox folders |
| `list_emails` | List emails in a folder (uid, date, from, subject, seen flag) |
| `get_email` | Fetch full email by UID (headers + plain text body, S/MIME signed supported) |
| `search_emails` | Search emails using IMAP SEARCH criteria |
| `mark_read` | Set `\Seen` flag on an email by UID |
| `mark_unread` | Unset `\Seen` flag on an email by UID |
| `get_attachment` | Fetch an attachment (text, image, or extracted text from PDF/Office docs) |
| `download_attachment` | Stage a large/binary attachment behind a one-shot signed URL |
| `create_draft` / `update_draft` | Compose or modify a draft email |

Every IMAP-touching tool accepts an optional `account` parameter for wire
compatibility. In static single-account mode it is ignored — the server always
targets its one configured mailbox. `add_account_url` and all `/manage*` and
`/auth*` routes from upstream are removed.

## Health

`GET /healthz` returns `200 {"status":"ok"}`.

## Architecture

```
MCP client → POST /mcp  (Authorization: Bearer MCP_API_TOKEN)
                        ↓
              constant-time token check
                        ↓
              connect to the static IMAP account (TLS, per request)
                        ↓
              execute MCP tool
                        ↓
              return results

Browser → GET /download/{token}  (one-shot ticket from download_attachment;
                                  stored in Redis under REDIS_KEY_PREFIX)
```

## Security

- Single static bearer token, compared with `subtle` constant-time equality
  over SHA-256 digests (content- and length-independent comparison).
- The bearer token and the IMAP app password are never logged; the password is
  held in memory in a `secrecy::SecretString`.
- HTTP request URIs are not logged, because download URLs contain one-shot
  credentials.
- IMAP passwords are never persisted to Redis; the only Redis data is
  one-shot, 15-minute download tickets.
- All Redis keys are namespaced by `REDIS_KEY_PREFIX`.
- Production Redis should use a dedicated ACL user restricted to that prefix
  and only `GET`, `GETDEL`, and `SET`.
- `list_accounts` exposes no credentials.
- IMAP input validation prevents command injection via folder names and search
  queries.
- HTML output is escaped to prevent XSS.
- CORS is restricted to the required methods and headers.
- Multi-stage container image runs as a non-root user.
- IMAP connections are opened per request (no persistent pool).

## Local Development

A `docker-compose.dev.yml` is provided for running behind an ngrok tunnel.

1. Copy `.env.example` to `.env` and fill in `MCP_API_TOKEN`, the IMAP account,
   `REDIS_URL`, and `NGROK_AUTHTOKEN`.
2. Start the services:

```bash
docker compose -f docker-compose.dev.yml up --build
```

3. Read the public URL from the ngrok logs (or http://localhost:4040), set
   `BASE_URL` in `.env` accordingly, and restart the `imap-mcp-lite` service.

## License

MIT — see [LICENSE](LICENSE). This project is a fork of
[factorial-io/imap-mcp](https://github.com/factorial-io/imap-mcp); the original
MIT license and copyright notices are preserved.

## Acknowledgements

Development time and API tokens for the upstream project were sponsored by
[Factorial.io](https://www.factorial.io/).
