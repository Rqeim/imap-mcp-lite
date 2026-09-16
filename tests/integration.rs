use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use imap_mcp_lite::session::{SessionStore, StaticAccount};
use imap_mcp_lite::{build_router, AppState};
use std::sync::Arc;
use tower::ServiceExt;

/// Build a test `AppState` with a static account, dummy Redis URL (no
/// connection is opened at construction time) and a known bearer token.
fn test_state() -> Arc<AppState> {
    let sessions = SessionStore::new("redis://localhost:6379", "imap-mcp-lite:").unwrap();
    let account = StaticAccount {
        account_id: "static".to_string(),
        label: "Test".to_string(),
        imap_email: "user@example.com".to_string(),
        imap_host: "imap.example.com".to_string(),
        imap_port: 993,
        password: "app-password".into(),
    };
    Arc::new(
        AppState::new(
            sessions,
            account,
            "https://imap-mcp-lite.example.com".to_string(),
            "test-token".into(),
        )
        .unwrap(),
    )
}

/// Helper: send a request to the test router and get response.
async fn send_request(req: Request<Body>) -> axum::response::Response {
    let app = build_router(test_state());
    app.oneshot(req).await.unwrap()
}

/// Helper: read response body as string.
async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// --- Health endpoint ---

#[tokio::test]
async fn healthz_returns_ok() {
    let req = Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("ok"));
}

// --- MCP endpoint auth tests ---

#[tokio::test]
async fn mcp_without_bearer_returns_401() {
    let req = Request::builder().uri("/mcp").body(Body::empty()).unwrap();

    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .expect("should have WWW-Authenticate header")
        .to_str()
        .unwrap();
    assert!(www_auth.contains("Bearer"));

    let body = body_string(resp).await;
    assert_eq!(body, "Bearer token required");
}

#[tokio::test]
async fn mcp_with_invalid_bearer_returns_401() {
    let req = Request::builder()
        .uri("/mcp")
        .header("authorization", "Bearer invalid-token-xyz")
        .body(Body::empty())
        .unwrap();

    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let body = body_string(resp).await;
    assert_eq!(body, "Invalid token");
}

#[tokio::test]
async fn mcp_with_prefix_bearer_returns_401() {
    // A prefix of the real token must not authenticate.
    let req = Request::builder()
        .uri("/mcp")
        .header("authorization", "Bearer test")
        .body(Body::empty())
        .unwrap();

    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_with_wrong_auth_scheme_returns_401() {
    let req = Request::builder()
        .uri("/mcp")
        .header("authorization", "Basic dXNlcjpwYXNz")
        .body(Body::empty())
        .unwrap();

    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let body = body_string(resp).await;
    assert_eq!(body, "Bearer token required");
}

#[tokio::test]
async fn mcp_subpath_without_bearer_returns_401() {
    let req = Request::builder()
        .uri("/mcp/sse")
        .body(Body::empty())
        .unwrap();

    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_with_valid_bearer_reaches_transport() {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "imap-mcp-lite.example.com")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(payload.to_string()))
        .unwrap();

    let resp = send_request(req).await;
    assert_ne!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a valid token must not be rejected"
    );
}

// --- Removed OIDC / management routes ---

#[tokio::test]
async fn removed_oidc_and_manage_routes_are_gone() {
    for uri in [
        "/register",
        "/auth/login",
        "/auth/callback",
        "/auth/token",
        "/manage",
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-authorization-server",
    ] {
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let resp = send_request(req).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "expected {uri} to be removed"
        );
    }
}

// --- Download endpoint ---

#[tokio::test]
async fn download_with_malformed_token_returns_404() {
    let req = Request::builder()
        .uri("/download/abc!def")
        .body(Body::empty())
        .unwrap();
    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn download_with_overlong_token_returns_404() {
    let long = "a".repeat(65);
    let req = Request::builder()
        .uri(format!("/download/{long}"))
        .body(Body::empty())
        .unwrap();
    let resp = send_request(req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// --- MCP tool parameter deserialization tests ---

#[test]
fn list_emails_params_defaults() {
    let json = r#"{}"#;
    let params: imap_mcp_lite::mcp::ListEmailsParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.folder, "INBOX");
    assert_eq!(params.limit, 20);
    assert_eq!(params.offset, 0);
}

#[test]
fn list_emails_params_custom() {
    let json = r#"{"folder": "Sent", "limit": 50, "offset": 10}"#;
    let params: imap_mcp_lite::mcp::ListEmailsParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.folder, "Sent");
    assert_eq!(params.limit, 50);
    assert_eq!(params.offset, 10);
}

#[test]
fn get_email_params_with_defaults() {
    let json = r#"{"uid": 42}"#;
    let params: imap_mcp_lite::mcp::GetEmailParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.uid, 42);
    assert_eq!(params.folder, "INBOX");
}

#[test]
fn search_emails_params_defaults() {
    let json = r#"{"query": "UNSEEN"}"#;
    let params: imap_mcp_lite::mcp::SearchEmailsParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.query, "UNSEEN");
    assert_eq!(params.folder, "INBOX");
    assert_eq!(params.limit, 20);
}

#[test]
fn mark_params_with_defaults() {
    let json = r#"{"uid": 99}"#;
    let params: imap_mcp_lite::mcp::MarkParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.uid, 99);
    assert_eq!(params.folder, "INBOX");
}

#[test]
fn list_emails_params_accepts_account_for_compatibility() {
    let json = r#"{"account": "billing"}"#;
    let params: imap_mcp_lite::mcp::ListEmailsParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.account.as_deref(), Some("billing"));
    assert_eq!(params.folder, "INBOX");
}
