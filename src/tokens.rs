//! Local storage for OAuth tokens.
//!
//! Zendesk rotates the refresh token on every refresh and invalidates the previous
//! one immediately, so a lost write costs the operator a full re-authorization.
//! Two safeguards follow from that:
//!
//! * writes are atomic (write a sibling temp file, fsync, rename), so a crash
//!   mid-write cannot truncate the file, and
//! * reads and writes can be wrapped in a cross-process lock, so two MCP servers
//!   running at once cannot refresh concurrently and discard each other's token.
//!
//! The file holds live credentials and is created `0600` inside a `0700` directory.
//!
//! On-disk format (identical to the Python version, so existing files keep working):
//!
//! ```json
//! {
//!   "access_token": "...",
//!   "refresh_token": "..." | null,
//!   "expires_at": "2026-10-04T12:00:00+00:00" | null,
//!   "refresh_token_expires_at": "..." | null,
//!   "subdomain": "acme",
//!   "client_id": "...",
//!   "scope": "tickets:read ..." | null
//! }
//! ```

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Refresh slightly early so a request is never sent with a token that expires in
/// flight, and to absorb modest clock drift against Zendesk.
pub const DEFAULT_EXPIRY_SKEW: Duration = Duration::from_secs(60);

/// How long `TokenStore::lock` waits before giving up.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// An access token and the refresh token that renews it.
///
/// `expires_at` is `None` for tokens issued by OAuth clients created before
/// 2026-04-30 without an explicit `expires_in`; those never expire and have no
/// refresh token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
    pub subdomain: String,
    pub client_id: String,
    #[serde(default)]
    pub scope: Option<String>,
}

// TODO(worker): manual `Debug` that redacts access_token and refresh_token.

impl TokenSet {
    /// Build a `TokenSet` from a Zendesk `/oauth/tokens` response body.
    ///
    /// Errors when `access_token` is missing. `expires_in` / `refresh_token_expires_in`
    /// (seconds) are turned into absolute instants relative to `issued_at`.
    pub fn from_token_response(
        payload: &serde_json::Value,
        subdomain: &str,
        client_id: &str,
        issued_at: DateTime<Utc>,
    ) -> Result<Self> {
        let _ = (payload, subdomain, client_id, issued_at);
        todo!("worker: tokens")
    }

    /// True when the access token is within `DEFAULT_EXPIRY_SKEW` of `expires_at`.
    pub fn access_token_expired(&self) -> bool {
        self.access_token_expired_at(Utc::now())
    }

    pub fn access_token_expired_at(&self, now: DateTime<Utc>) -> bool {
        let _ = now;
        todo!("worker: tokens")
    }

    pub fn refresh_token_expired(&self) -> bool {
        self.refresh_token_expired_at(Utc::now())
    }

    pub fn refresh_token_expired_at(&self, now: DateTime<Utc>) -> bool {
        let _ = now;
        todo!("worker: tokens")
    }

    /// A refresh token is present and not yet expired.
    pub fn can_refresh(&self) -> bool {
        todo!("worker: tokens")
    }
}

/// Reads and writes a `TokenSet` as JSON on the local filesystem.
#[derive(Debug, Clone)]
pub struct TokenStore {
    pub path: PathBuf,
    pub lock_path: PathBuf,
}

/// Exclusive cross-process lock on the store. Released on drop.
#[derive(Debug)]
pub struct TokenLock {
    _file: File,
}

impl TokenStore {
    /// `path` is the token file; the lock file is `<path>.lock` beside it.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path: PathBuf = path.into();
        let lock_path = lock_path_for(&path);
        TokenStore { path, lock_path }
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// Hold an exclusive cross-process lock (`std::fs::File::try_lock`, polled until
    /// `LOCK_TIMEOUT`). Wrap the read-refresh-write cycle in this so a second MCP
    /// process cannot refresh at the same time and invalidate the token this one
    /// just stored. Creates the directory (0700) if needed.
    pub async fn lock(&self) -> Result<TokenLock> {
        todo!("worker: tokens")
    }

    /// Read and parse the token file. The error for a missing file tells the operator
    /// to run `zendesk-mcp-server auth`.
    pub fn load(&self) -> Result<TokenSet> {
        todo!("worker: tokens")
    }

    /// Write tokens atomically, replacing any existing file: temp file in the same
    /// directory with mode 0600, write, fsync, rename. Ensures the directory exists
    /// with mode 0700 first.
    pub fn save(&self, tokens: &TokenSet) -> Result<()> {
        let _ = tokens;
        todo!("worker: tokens")
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".lock");
    path.with_file_name(name)
}
