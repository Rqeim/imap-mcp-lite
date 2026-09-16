use clap::Parser;
use imap_mcp_lite::{
    build_router,
    session::{SessionStore, StaticAccount},
    AppState,
};
use std::sync::Arc;

/// Server configuration. Static mode only: a single IMAP account and a single
/// bearer token, both supplied via environment variables.
#[derive(Parser)]
#[command(version, about = "IMAP MCP server (lite)", long_about = None)]
struct Cli {
    /// Authentication mode. Only `static` is supported.
    #[arg(long, env = "AUTH_MODE", default_value = "static")]
    auth_mode: String,

    /// Bearer token required on `/mcp`. Never logged.
    #[arg(long, env = "MCP_API_TOKEN")]
    mcp_api_token: String,

    /// IMAP host for the static account.
    #[arg(long, env = "IMAP_HOST", default_value = "mail.privateemail.com")]
    imap_host: String,

    /// IMAP port for the static account.
    #[arg(long, env = "IMAP_PORT", default_value_t = 993)]
    imap_port: u16,

    /// IMAP login username (usually the full email address).
    #[arg(long, env = "IMAP_USERNAME")]
    imap_username: String,

    /// IMAP app password. Never logged.
    #[arg(long, env = "IMAP_APP_PASSWORD")]
    imap_app_password: String,

    /// Public URL of this service, no trailing slash.
    #[arg(long, env = "BASE_URL", default_value = "http://localhost:8080")]
    base_url: String,

    /// Central Redis connection URL (used for one-shot download tickets).
    #[arg(long, env = "REDIS_URL")]
    redis_url: String,

    /// Prefix for every Redis key this app writes.
    #[arg(long, env = "REDIS_KEY_PREFIX", default_value = "imap-mcp-lite:")]
    redis_key_prefix: String,

    #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
    bind_addr: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    if cli.auth_mode != "static" {
        anyhow::bail!(
            "unsupported AUTH_MODE '{}': only 'static' is supported",
            cli.auth_mode
        );
    }
    // Trust-boundary validation: refuse to start with an empty bearer token
    // (which would accept an empty credential) or missing IMAP credentials.
    if cli.mcp_api_token.len() < 32 {
        anyhow::bail!("MCP_API_TOKEN must be at least 32 characters");
    }
    if cli.imap_username.trim().is_empty() {
        anyhow::bail!("IMAP_USERNAME must not be empty");
    }
    if cli.imap_app_password.is_empty() {
        anyhow::bail!("IMAP_APP_PASSWORD must not be empty");
    }
    if cli.redis_key_prefix.trim().is_empty() {
        anyhow::bail!("REDIS_KEY_PREFIX must not be empty");
    }

    let sessions = SessionStore::new(&cli.redis_url, &cli.redis_key_prefix)?;

    let account = StaticAccount {
        account_id: "static".to_string(),
        label: cli.imap_username.clone(),
        imap_email: cli.imap_username.clone(),
        imap_host: cli.imap_host.clone(),
        imap_port: cli.imap_port,
        password: cli.imap_app_password.as_str().into(),
    };

    tracing::info!(
        imap_host = %cli.imap_host,
        imap_port = cli.imap_port,
        redis_key_prefix = %cli.redis_key_prefix,
        "static IMAP account configured"
    );

    let state = Arc::new(AppState::new(
        sessions,
        account,
        cli.base_url.clone(),
        cli.mcp_api_token.as_str().into(),
    )?);

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cli.bind_addr).await?;
    tracing::info!("Server listening on {}", cli.bind_addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl+c");
    tracing::info!("Shutdown signal received");
}
