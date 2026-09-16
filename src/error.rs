use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Unified error type for the application.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("IMAP error: {0}")]
    Imap(String),

    #[error("IMAP authentication failed")]
    ImapAuth,

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Imap(_) => {
                tracing::error!("IMAP error: {self}");
                (StatusCode::BAD_GATEWAY, "IMAP error".to_string())
            }
            AppError::ImapAuth => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Redis(_) => {
                tracing::error!("Redis error: {self}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error".to_string(),
                )
            }
            AppError::Serialization(_) => {
                tracing::error!("Serialization error: {self}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error".to_string(),
                )
            }
            AppError::Internal(msg) => {
                tracing::error!("Internal error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal error".to_string(),
                )
            }
        };

        (status, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imap_error_returns_502() {
        let err = AppError::Imap("connection refused".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn imap_auth_error_returns_401() {
        let response = AppError::ImapAuth.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn internal_error_returns_500() {
        let err = AppError::Internal("something broke".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn error_display_messages() {
        assert_eq!(AppError::ImapAuth.to_string(), "IMAP authentication failed");
        assert_eq!(
            AppError::Imap("test".to_string()).to_string(),
            "IMAP error: test"
        );
    }
}
