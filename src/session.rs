use redis::AsyncCommands;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// The single, operator-configured IMAP account. Credentials come from the
/// environment; the password is held in memory only (never persisted) and is
/// redacted from `Debug` output via [`SecretString`].
#[derive(Clone)]
pub struct StaticAccount {
    pub account_id: String,
    pub label: String,
    pub imap_email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub password: SecretString,
}

/// Password-free projection of a [`StaticAccount`] used by the
/// `list_accounts` MCP tool.
#[derive(Debug, Serialize)]
pub struct AccountSummary {
    pub account_id: String,
    pub label: String,
    pub imap_email: String,
    pub imap_host: String,
    pub last_used_at: Option<i64>,
    pub disabled: bool,
    pub is_default: bool,
}

impl AccountSummary {
    pub fn from_static(a: &StaticAccount) -> Self {
        Self {
            account_id: a.account_id.clone(),
            label: a.label.clone(),
            imap_email: a.imap_email.clone(),
            imap_host: a.imap_host.clone(),
            last_used_at: None,
            disabled: false,
            is_default: true,
        }
    }
}

/// Result payload for the `list_accounts` MCP tool.
#[derive(Debug, Serialize)]
pub struct ListAccountsResult {
    pub accounts: Vec<AccountSummary>,
}

/// One-shot signed-URL staging record for `download_attachment`. The bytes
/// themselves are stored separately under the same token to avoid round-trip
/// base64-encoding them through JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadTicket {
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
    /// Stable identifier of the account that staged the file. Logged on
    /// redemption so the server's access log can attribute the download.
    pub account_id: String,
}

/// Manages the one piece of shared state this server needs Redis for: the
/// one-shot download tickets behind `download_attachment`.
///
/// Every key this store writes lives under the configured `REDIS_KEY_PREFIX`
/// so a shared Redis instance stays namespaced.
#[derive(Clone)]
pub struct SessionStore {
    redis: redis::Client,
    key_prefix: String,
}

/// How long a staged attachment is fetchable from `/download/{token}` before
/// it expires. Plenty of time for a user to click the link in chat; short
/// enough to bound Redis memory consumption from large attachments.
pub const DOWNLOAD_TICKET_TTL: u64 = 15 * 60;

/// Hard cap on staged-attachment size. Pinned to the same 25 MB ceiling as
/// `imap::MAX_ATTACHMENT_SIZE`, which is enforced earlier in the pipeline
/// when `ImapConnection::get_attachment` decodes the part — so under normal
/// operation the check in `stage_download` is a defence-in-depth assertion
/// that should never fire. If the two constants ever drift apart, the
/// smaller one wins and the larger one becomes dead code; keep them in sync.
pub const DOWNLOAD_TICKET_MAX_SIZE: usize = 25 * 1024 * 1024;

impl SessionStore {
    pub fn new(redis_url: &str, key_prefix: &str) -> Result<Self, AppError> {
        let redis = redis::Client::open(redis_url).map_err(AppError::Redis)?;
        Ok(Self {
            redis,
            key_prefix: key_prefix.to_string(),
        })
    }

    async fn conn(&self) -> Result<redis::aio::MultiplexedConnection, AppError> {
        self.redis
            .get_multiplexed_async_connection()
            .await
            .map_err(AppError::Redis)
    }

    /// Namespace a Redis key under the configured prefix.
    fn key(&self, suffix: &str) -> String {
        format!("{}{}", self.key_prefix, suffix)
    }

    // --- Download tickets (one-shot signed URLs for attachment downloads) ---

    /// Stage attachment `data` under a fresh random token. The caller
    /// embeds the token in a download URL; the user fetching that URL
    /// redeems the token via `consume_download_ticket`, which returns the
    /// metadata and bytes and atomically deletes the staging records.
    pub async fn stage_download(
        &self,
        ticket: &DownloadTicket,
        data: &[u8],
    ) -> Result<String, AppError> {
        if data.len() > DOWNLOAD_TICKET_MAX_SIZE {
            return Err(AppError::Imap(format!(
                "attachment too large to stage for download ({} bytes, max {})",
                data.len(),
                DOWNLOAD_TICKET_MAX_SIZE
            )));
        }
        // 244 bits of randomness from two v4 UUIDs, URL-safe by construction.
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let meta_key = self.key(&format!("download:meta:{token}"));
        let data_key = self.key(&format!("download:data:{token}"));
        let meta = serde_json::to_string(ticket)?;
        let mut conn = self.conn().await?;
        // Stage data first, then metadata. Reverse order on consumption.
        // If staging the second key fails, the orphan first key expires on its
        // own TTL — no leakage, just briefly wasted Redis memory.
        conn.set_ex::<_, &[u8], ()>(&data_key, data, DOWNLOAD_TICKET_TTL)
            .await?;
        conn.set_ex::<_, _, ()>(&meta_key, meta, DOWNLOAD_TICKET_TTL)
            .await?;
        Ok(token)
    }

    /// Redeem a download token: returns metadata + bytes and atomically
    /// deletes the staging records via `GETDEL`. Returns `Ok(None)` if the
    /// token is unknown, expired, or already consumed.
    pub async fn consume_download_ticket(
        &self,
        token: &str,
    ) -> Result<Option<(DownloadTicket, Vec<u8>)>, AppError> {
        let meta_key = self.key(&format!("download:meta:{token}"));
        let data_key = self.key(&format!("download:data:{token}"));
        let mut conn = self.conn().await?;
        // Atomic read-and-delete on the metadata key first: if two clicks
        // race, only one gets `Some(...)` back. The loser sees `None` and
        // we never even touch the data key.
        let meta: Option<String> = conn.get_del(&meta_key).await?;
        let Some(meta) = meta else {
            // Token not found, expired, or already consumed.
            return Ok(None);
        };
        let ticket: DownloadTicket = serde_json::from_str(&meta)?;
        let data: Option<Vec<u8>> = conn.get_del(&data_key).await?;
        let Some(data) = data else {
            // Metadata existed but data didn't — should be impossible under
            // normal operation (the two keys were written together under the
            // same TTL). Treat as a missing token; the metadata key has
            // already been consumed, which is what we want.
            // Don't log the token: it's the bearer credential for the
            // download endpoint.
            tracing::warn!("download token had metadata but no data (data key missing or expired)");
            return Ok(None);
        };
        Ok(Some((ticket, data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(prefix: &str) -> SessionStore {
        SessionStore::new("redis://localhost:6379", prefix).unwrap()
    }

    #[test]
    fn keys_are_namespaced_under_prefix() {
        let store = test_store("imap-mcp-lite:");
        assert_eq!(
            store.key("download:meta:abc"),
            "imap-mcp-lite:download:meta:abc"
        );
        assert_eq!(
            store.key("download:data:abc"),
            "imap-mcp-lite:download:data:abc"
        );
    }

    #[test]
    fn download_ticket_roundtrip_omits_password() {
        let ticket = DownloadTicket {
            filename: "report.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 1234,
            account_id: "static".to_string(),
        };
        let json = serde_json::to_string(&ticket).unwrap();
        let de: DownloadTicket = serde_json::from_str(&json).unwrap();
        assert_eq!(de.filename, "report.pdf");
        assert_eq!(de.size, 1234);
        assert!(!json.contains("password"));
    }

    #[test]
    fn account_summary_never_carries_credentials() {
        let account = StaticAccount {
            account_id: "static".to_string(),
            label: "Work".to_string(),
            imap_email: "user@example.com".to_string(),
            imap_host: "mail.example.com".to_string(),
            imap_port: 993,
            password: "super-secret-app-password".into(),
        };
        let summary = AccountSummary::from_static(&account);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(summary.is_default);
        assert!(!summary.disabled);
        assert!(!json.contains("super-secret"));
        assert!(!json.contains("password"));
    }
}
