pub mod error;
pub mod extract;
pub mod imap;
pub mod mcp;
pub(crate) mod sanitize;
pub mod session;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use http::Request;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use secrecy::{ExposeSecret, SecretString};
use session::{SessionStore, StaticAccount};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// Shared application state.
pub struct AppState {
    pub sessions: SessionStore,
    /// The single operator-configured IMAP account.
    pub account: Arc<StaticAccount>,
    pub base_url: String,
    /// Static bearer token required on `/mcp`. Never logged.
    pub mcp_api_token: SecretString,
    /// Hosts the rmcp Streamable-HTTP transport will accept in the `Host`
    /// header. Without our public hostname here, every MCP request is dropped
    /// as a DNS-rebinding attempt (rmcp >= 1.6 defaults to localhost only).
    pub mcp_allowed_hosts: Vec<String>,
}

impl AppState {
    pub fn new(
        sessions: SessionStore,
        account: StaticAccount,
        base_url: String,
        mcp_api_token: SecretString,
    ) -> anyhow::Result<Self> {
        let mcp_allowed_hosts = derive_mcp_allowed_hosts(&base_url)?;
        Ok(Self {
            sessions,
            account: Arc::new(account),
            base_url,
            mcp_api_token,
            mcp_allowed_hosts,
        })
    }
}

/// Build the rmcp allowed-hosts list from `base_url`. We always keep the
/// loopback defaults so local dev keeps working when `BASE_URL` is set to a
/// public hostname.
fn derive_mcp_allowed_hosts(base_url: &str) -> anyhow::Result<Vec<String>> {
    let parsed = url::Url::parse(base_url)
        .map_err(|e| anyhow::anyhow!("BASE_URL is not a valid URL ({base_url}): {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("BASE_URL has no host component: {base_url}"))?;
    Ok(vec![
        host.to_string(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ])
}

/// Build the axum Router from shared state. Used by main and integration tests.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/mcp", axum::routing::any(mcp_handler))
        .route("/mcp/{path}", axum::routing::any(mcp_handler))
        .route("/download/{token}", get(download_handler))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    http::Method::GET,
                    http::Method::POST,
                    http::Method::DELETE,
                    http::Method::OPTIONS,
                ])
                .allow_headers([
                    http::header::AUTHORIZATION,
                    http::header::CONTENT_TYPE,
                    http::header::ACCEPT,
                ]),
        )
        .with_state(state)
}

async fn healthz() -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"status":"ok"}"#,
    )
        .into_response()
}

/// Per-request handle used by MCP tools to reach the configured account and
/// the shared Redis store.
#[derive(Clone)]
pub struct AccountResolver {
    pub store: SessionStore,
    pub account: Arc<StaticAccount>,
    pub base_url: String,
}

impl AccountResolver {
    pub fn new(store: SessionStore, account: Arc<StaticAccount>, base_url: String) -> Self {
        Self {
            store,
            account,
            base_url,
        }
    }
}

/// MCP endpoint handler with static bearer-token authentication.
async fn mcp_handler(
    headers: HeaderMap,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request<axum::body::Body>,
) -> Response {
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", "Bearer")],
                "Bearer token required",
            )
                .into_response();
        }
    };

    if !token_matches(&token, state.mcp_api_token.expose_secret()) {
        return (
            StatusCode::UNAUTHORIZED,
            [("WWW-Authenticate", "Bearer")],
            "Invalid token",
        )
            .into_response();
    }

    let resolver = AccountResolver::new(
        state.sessions.clone(),
        state.account.clone(),
        state.base_url.clone(),
    );

    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(state.mcp_allowed_hosts.iter().cloned());

    let service = StreamableHttpService::new(
        move || Ok(mcp::ImapMcpServer::new(resolver.clone())),
        LocalSessionManager::default().into(),
        config,
    );

    let resp: http::Response<_> = service.handle(req).await;
    resp.map(axum::body::Body::new)
}

pub fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    Some(token.to_string())
}

/// Constant-time bearer-token comparison.
///
/// Both sides are hashed with SHA-256 first so the comparison runs over two
/// fixed 32-byte digests — that keeps the check constant-time in the token's
/// *content* and avoids leaking its *length* through early exit (which a raw
/// byte-by-byte compare would). The token itself is never logged.
fn token_matches(provided: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;
    let provided = Sha256::digest(provided.as_bytes());
    let expected = Sha256::digest(expected.as_bytes());
    provided.ct_eq(&expected).into()
}

/// One-shot download endpoint backing `download_attachment`. The token is the
/// authorization: the random 256-bit identifier the MCP tool returned, looked
/// up in Redis (with `GETDEL` so the URL works exactly once). The Bearer
/// token used for the rest of the API is **not** required here — the user
/// opens the URL in a browser tab where they don't have one.
async fn download_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Response {
    // Reject tokens that aren't URL-safe base64. Don't even hit Redis for these.
    if token.is_empty()
        || token.len() > 64
        || !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (StatusCode::NOT_FOUND, "Unknown or expired download link").into_response();
    }

    let result = match state.sessions.consume_download_ticket(&token).await {
        Ok(v) => v,
        Err(e) => {
            // The token IS the bearer credential for this endpoint, so it
            // must never appear in logs: a connection-level Redis failure
            // can fire before GETDEL reaches the server, leaving the token
            // live and redeemable for the full TTL.
            tracing::error!(error = %e, "download lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load download").into_response();
        }
    };
    let Some((ticket, data)) = result else {
        return (StatusCode::NOT_FOUND, "Unknown or expired download link").into_response();
    };

    tracing::info!(
        account_id = %ticket.account_id,
        filename = %ticket.filename,
        size = ticket.size,
        "served attachment download"
    );

    let disposition = build_content_disposition(&ticket.filename);
    // Defensive: only echo back ASCII MIME types. IMAP can carry anything.
    let mime = if ticket.mime_type.is_ascii()
        && !ticket
            .mime_type
            .contains(|c: char| c.is_control() || c == '"' || c == ',')
    {
        ticket.mime_type
    } else {
        "application/octet-stream".to_string()
    };

    (
        StatusCode::OK,
        [
            (http::header::CONTENT_TYPE, mime),
            (http::header::CONTENT_DISPOSITION, disposition),
            (
                http::header::CACHE_CONTROL,
                "no-store, max-age=0".to_string(),
            ),
            (http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        data,
    )
        .into_response()
}

/// Build an RFC 5987-encoded `Content-Disposition` header value that safely
/// carries Unicode filenames, with an ASCII-only fallback for old clients.
pub(crate) fn build_content_disposition(filename: &str) -> String {
    // Strip directory separators — `download_attachment` never accepts paths,
    // but emails can supply filenames like `../etc/passwd`. We're emitting
    // headers, not writing files, but it costs nothing to keep these out of
    // the disposition.
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("attachment")
        .trim();
    let base = if base.is_empty() { "attachment" } else { base };

    // ASCII-only fallback in the `filename=` parameter.
    let ascii_fallback: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // RFC 5987 percent-encoded UTF-8 in `filename*=`. Encode everything that
    // isn't an unreserved character or token-safe punctuation.
    let mut encoded = String::new();
    for b in base.as_bytes() {
        let c = *b;
        let safe = c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'-' | b'_' | b'.' | b'~' | b'!' | b'#' | b'$' | b'&' | b'+' | b'^'
            );
        if safe {
            encoded.push(c as char);
        } else {
            encoded.push_str(&format!("%{c:02X}"));
        }
    }

    format!(r#"attachment; filename="{ascii_fallback}"; filename*=UTF-8''{encoded}"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_matches_accepts_exact() {
        assert!(token_matches("s3cret-token", "s3cret-token"));
    }

    #[test]
    fn token_matches_rejects_different_content() {
        assert!(!token_matches("s3cret-token", "s3cret-tokeN"));
        assert!(!token_matches("", "s3cret-token"));
    }

    #[test]
    fn token_matches_rejects_prefix_of_expected() {
        assert!(!token_matches("s3cret", "s3cret-token"));
    }

    #[test]
    fn bearer_token_extraction() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer abc123".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers).as_deref(), Some("abc123"));

        headers.insert("authorization", "Basic abc123".parse().unwrap());
        assert!(extract_bearer_token(&headers).is_none());
    }

    #[test]
    fn content_disposition_ascii_filename_quoted() {
        let header = build_content_disposition("report.pdf");
        assert!(header.contains(r#"filename="report.pdf""#));
        assert!(header.contains(r"filename*=UTF-8''report.pdf"));
    }

    #[test]
    fn content_disposition_unicode_filename_is_percent_encoded() {
        // Cyrillic + emoji. RFC 5987 requires UTF-8 percent-encoding for
        // anything outside the safe ASCII subset; the ASCII fallback for old
        // clients replaces non-graphic characters with underscores.
        let header = build_content_disposition("Отчёт 📊.pdf");
        assert!(header.contains("filename=\""));
        assert!(header.contains("filename*=UTF-8''"));
        assert!(header.contains("%D0%9E")); // 'О' in UTF-8
        assert!(header.contains(".pdf"));
        // No raw non-ASCII bytes — the entire header must be 7-bit safe.
        assert!(header.is_ascii(), "header must be ASCII-safe: {header}");
    }

    #[test]
    fn content_disposition_strips_path_traversal_segments() {
        // Even though we're not writing files, keep "../" out of the header.
        let header = build_content_disposition("../etc/passwd");
        assert!(!header.contains("../"));
        assert!(header.contains("passwd"));
    }

    #[test]
    fn content_disposition_empty_filename_falls_back() {
        let header = build_content_disposition("");
        assert!(header.contains(r#"filename="attachment""#));
    }
}
