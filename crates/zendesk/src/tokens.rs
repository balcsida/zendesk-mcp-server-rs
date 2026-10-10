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

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::oauth::ReauthRequired;

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
    #[serde(default, with = "timestamp")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, with = "timestamp")]
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
    pub subdomain: String,
    pub client_id: String,
    #[serde(default)]
    pub scope: Option<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .field("subdomain", &self.subdomain)
            .field("client_id", &self.client_id)
            .field("scope", &self.scope)
            .finish()
    }
}

/// RFC 3339 with a `+00:00` offset (what Python's `isoformat` writes). Reading also
/// accepts naive timestamps, which Python treats as UTC.
mod timestamp {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(moment) => {
                serializer.serialize_str(&moment.to_rfc3339_opts(SecondsFormat::AutoSi, false))
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        let Some(raw) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };
        parse(&raw).map(Some).map_err(serde::de::Error::custom)
    }

    pub(super) fn parse(raw: &str) -> Result<DateTime<Utc>, String> {
        if let Ok(moment) = DateTime::parse_from_rfc3339(raw) {
            return Ok(moment.with_timezone(&Utc));
        }
        NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
            .map(|naive| naive.and_utc())
            .map_err(|_| format!("invalid timestamp {raw:?}"))
    }
}

/// Seconds as a JSON number or numeric string.
fn seconds_field(payload: &serde_json::Value, key: &str) -> Option<i64> {
    match payload.get(key)? {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

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
        let access_token = payload
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("Zendesk token response did not include an access_token."))?;
        let expires_at = |key: &str| {
            seconds_field(payload, key)
                .and_then(chrono::Duration::try_seconds)
                .and_then(|ttl| issued_at.checked_add_signed(ttl))
        };
        let text = |key: &str| {
            payload
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        Ok(TokenSet {
            access_token: access_token.to_owned(),
            refresh_token: text("refresh_token"),
            expires_at: expires_at("expires_in"),
            refresh_token_expires_at: expires_at("refresh_token_expires_in"),
            subdomain: subdomain.to_owned(),
            client_id: client_id.to_owned(),
            scope: text("scope"),
        })
    }

    /// True when the access token is within `DEFAULT_EXPIRY_SKEW` of `expires_at`.
    pub fn access_token_expired(&self) -> bool {
        self.access_token_expired_at(Utc::now())
    }

    pub fn access_token_expired_at(&self, now: DateTime<Utc>) -> bool {
        expired(self.expires_at, now)
    }

    pub fn refresh_token_expired(&self) -> bool {
        self.refresh_token_expired_at(Utc::now())
    }

    pub fn refresh_token_expired_at(&self, now: DateTime<Utc>) -> bool {
        expired(self.refresh_token_expires_at, now)
    }

    /// A refresh token is present and not yet expired.
    pub fn can_refresh(&self) -> bool {
        self.refresh_token.as_deref().is_some_and(|t| !t.is_empty())
            && !self.refresh_token_expired()
    }
}

fn expired(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let skew = chrono::Duration::from_std(DEFAULT_EXPIRY_SKEW).unwrap_or_default();
    expires_at.is_some_and(|at| now + skew >= at)
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

    /// Hold an exclusive cross-process lock (`std::fs::File::try_lock`, polled until
    /// `LOCK_TIMEOUT`). Wrap the read-refresh-write cycle in this so a second MCP
    /// process cannot refresh at the same time and invalidate the token this one
    /// just stored. Creates the directory (0700) if needed.
    pub async fn lock(&self) -> Result<TokenLock> {
        self.lock_with_timeout(LOCK_TIMEOUT).await
    }

    async fn lock_with_timeout(&self, timeout: Duration) -> Result<TokenLock> {
        ensure_directory(&self.path)?;
        if fs::symlink_metadata(&self.lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!(
                "refusing to use {} as the lock file: it is a symlink",
                self.lock_path.display()
            );
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(&self.lock_path)
            .with_context(|| format!("Could not open {}", self.lock_path.display()))?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(TokenLock { _file: file }),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(err)) => {
                    return Err(err)
                        .with_context(|| format!("Could not lock {}", self.lock_path.display()));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "Timed out after {}s: another process is holding the token store lock at \
                     {}. Wait for it to finish.",
                    timeout.as_secs(),
                    self.lock_path.display()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Read and parse the token file. A missing or unparsable file is a
    /// [`ReauthRequired`], whose message tells the operator to run
    /// `zendesk-mcp-server auth` (or `zendesk auth`).
    pub fn load(&self) -> Result<TokenSet> {
        let raw = read_private(&self.path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow::Error::new(ReauthRequired::NoTokens {
                    path: self.path.clone(),
                })
            } else {
                anyhow!("Could not read {}: {err}", self.path.display())
            }
        })?;
        serde_json::from_str(&raw).map_err(|err| {
            anyhow::Error::new(ReauthRequired::CorruptTokens {
                path: self.path.clone(),
                detail: err.to_string(),
            })
        })
    }

    /// Write tokens atomically, replacing any existing file: see [`save_guarded`].
    pub fn save(&self, tokens: &TokenSet) -> Result<()> {
        let mut payload = serde_json::to_string_pretty(&serde_json::to_value(tokens)?)?;
        payload.push('\n');
        save_guarded(&self.path, payload.as_bytes())?;
        tracing::debug!("Stored Zendesk OAuth tokens at {}", self.path.display());
        Ok(())
    }
}

/// Read a private file, warning when other users can read it (unix).
pub fn read_private(path: &Path) -> std::io::Result<String> {
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                "{} has mode {mode:o}, so it is readable by other users; run chmod 600 on it.",
                path.display()
            );
        }
    }
    fs::read_to_string(path)
}

/// Write a token file atomically with mode 0600 ([`write_private`]), creating its
/// directory with mode 0700 first. Refuses to replace an existing file that is not a
/// JSON object with `access_token`, so a wrong `ZENDESK_TOKEN_FILE` cannot clobber
/// another file; an existing file that cannot be read is an error.
pub fn save_guarded(path: &Path, contents: &[u8]) -> Result<()> {
    ensure_directory(path)?;
    match fs::read(path) {
        Ok(existing) => {
            let is_token_file = serde_json::from_slice::<serde_json::Value>(&existing)
                .is_ok_and(|v| v.get("access_token").is_some());
            if !is_token_file {
                bail!(
                    "refusing to overwrite {}: it is not a Zendesk token file",
                    path.display()
                );
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => bail!("Could not read {}: {err}", path.display()),
    }
    write_private(path, contents)
}

/// Write `contents` to `path` atomically with mode 0600: a random-named temp file beside
/// it, fsync, rename. The temp file is removed if anything fails.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let temp_path = path.with_file_name(temp_name);

    let write = || -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temp_path, path)
    };
    write().map_err(|err| {
        let _ = fs::remove_file(&temp_path);
        anyhow!("Could not write {}: {err}", path.display())
    })
}

fn lock_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".lock");
    path.with_file_name(name)
}

/// Create the directory holding `path` with mode 0700, tightening it if it exists with
/// other bits.
fn ensure_directory(path: &Path) -> Result<()> {
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Ok(());
    };
    let created = !dir.exists();
    fs::create_dir_all(dir).with_context(|| format!("Could not create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            // Only tighten directories this code created: chmod-ing a shared directory
            // such as /tmp, or a mount the user cannot change, does more harm than good.
            if created {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("Could not set permissions on {}", dir.display()))?;
            } else {
                tracing::warn!(
                    "{} has mode {mode:o}, not 0700; the token file inside may be readable by \
                     other users.",
                    dir.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::REAUTH_HINT;
    use chrono::TimeZone;
    use serde_json::json;

    fn sample(expires_at: Option<DateTime<Utc>>) -> TokenSet {
        TokenSet {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at,
            refresh_token_expires_at: expires_at.map(|t| t + chrono::Duration::days(90)),
            subdomain: "acme".into(),
            client_id: "client".into(),
            scope: Some("tickets:read users:read".into()),
        }
    }

    #[test]
    fn save_then_load_preserves_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("nested/tokens.json"));
        let tokens = sample(Some(Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap()));
        store.save(&tokens).unwrap();
        assert_eq!(store.load().unwrap(), tokens);

        let raw = fs::read_to_string(&store.path).unwrap();
        assert!(raw.ends_with("}\n"));
        assert!(raw.contains("\"expires_at\": \"2026-10-04T12:00:00+00:00\""));
        // Keys are sorted, like Python's sort_keys=True.
        assert!(raw.find("access_token").unwrap() < raw.find("subdomain").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn file_is_0600_and_directory_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("zd/tokens.json"));
        store.save(&sample(None)).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&store.path), 0o600);
        assert_eq!(mode(store.path.parent().unwrap()), 0o700);
    }

    #[test]
    fn save_refuses_to_overwrite_a_foreign_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("tokens.json"));
        for foreign in ["not json", "[1]", r#"{"other": 1}"#, ""] {
            fs::write(&store.path, foreign).unwrap();
            let err = store.save(&sample(None)).unwrap_err().to_string();
            assert!(
                err.contains("refusing to overwrite") && err.contains("not a Zendesk token file")
            );
            assert_eq!(fs::read_to_string(&store.path).unwrap(), foreign);
        }
        fs::write(&store.path, r#"{"access_token": "old"}"#).unwrap();
        store.save(&sample(None)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_private_is_0600_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        // A missing directory fails and leaves nothing behind.
        assert!(write_private(&dir.path().join("no/such/secret"), b"x").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lock_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("tokens.json"));
        let target = dir.path().join("victim");
        fs::write(&target, "keep").unwrap();
        std::os::unix::fs::symlink(&target, &store.lock_path).unwrap();
        let err = store.lock().await.unwrap_err().to_string();
        assert!(err.contains("it is a symlink"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn load_still_reads_a_group_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("tokens.json"));
        store.save(&sample(None)).unwrap();
        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(store.load().unwrap(), sample(None));
    }

    #[test]
    fn expiry_skew_boundary() {
        let at = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
        let tokens = sample(Some(at));
        let skew = chrono::Duration::seconds(60);
        assert!(!tokens.access_token_expired_at(at - skew - chrono::Duration::seconds(1)));
        assert!(tokens.access_token_expired_at(at - skew));
        assert!(!sample(None).access_token_expired_at(at + chrono::Duration::days(999)));
        assert!(!sample(None).refresh_token_expired_at(at));
    }

    #[test]
    fn can_refresh_needs_unexpired_refresh_token() {
        let mut tokens = sample(None);
        assert!(tokens.can_refresh());
        tokens.refresh_token_expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        assert!(!tokens.can_refresh());
        tokens.refresh_token_expires_at = None;
        tokens.refresh_token = None;
        assert!(!tokens.can_refresh());
    }

    #[test]
    fn from_token_response_with_and_without_expires_in() {
        let issued = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
        let full = json!({
            "access_token": "a", "refresh_token": "r", "scope": "tickets:read",
            "expires_in": 1800, "refresh_token_expires_in": "7776000"
        });
        let tokens = TokenSet::from_token_response(&full, "acme", "client", issued).unwrap();
        assert_eq!(
            tokens.expires_at,
            Some(issued + chrono::Duration::seconds(1800))
        );
        assert_eq!(
            tokens.refresh_token_expires_at,
            Some(issued + chrono::Duration::seconds(7_776_000))
        );
        assert_eq!(tokens.refresh_token.as_deref(), Some("r"));
        assert_eq!(tokens.scope.as_deref(), Some("tickets:read"));

        let bare = json!({"access_token": "a"});
        let tokens = TokenSet::from_token_response(&bare, "acme", "client", issued).unwrap();
        assert_eq!(tokens.expires_at, None);
        assert_eq!(tokens.refresh_token, None);

        let err = TokenSet::from_token_response(&json!({}), "acme", "client", issued).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Zendesk token response did not include an access_token."
        );
    }

    #[test]
    fn parses_python_written_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        fs::write(
            &path,
            r#"{"access_token": "a", "refresh_token": null,
                "expires_at": "2026-10-04T12:34:56.123456+00:00",
                "refresh_token_expires_at": "2026-10-04T12:34:56",
                "subdomain": "acme", "client_id": "c", "scope": null}"#,
        )
        .unwrap();
        let tokens = TokenStore::new(&path).load().unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 4, 12, 34, 56).unwrap();
        assert_eq!(
            tokens.expires_at,
            Some(expected + chrono::Duration::microseconds(123_456))
        );
        assert_eq!(tokens.refresh_token_expires_at, Some(expected));
    }

    #[test]
    fn load_errors_name_the_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("tokens.json"));
        let missing = store.load().unwrap_err();
        assert!(matches!(
            missing.downcast_ref::<ReauthRequired>(),
            Some(ReauthRequired::NoTokens { .. })
        ));
        let missing = missing.to_string();
        assert!(missing.contains("No Zendesk OAuth tokens found") && missing.contains(REAUTH_HINT));

        for contents in ["{not json", r#"{"access_token": "a"}"#] {
            fs::write(&store.path, contents).unwrap();
            let err = store.load().unwrap_err();
            assert!(matches!(
                err.downcast_ref::<ReauthRequired>(),
                Some(ReauthRequired::CorruptTokens { .. })
            ));
            assert!(err.to_string().contains(REAUTH_HINT), "{err}");
        }
    }

    #[test]
    fn debug_redacts_secrets() {
        let shown = format!("{:?}", sample(None));
        assert!(!shown.contains("access\"") && !shown.contains("\"refresh\""));
        assert!(shown.contains("<redacted>"));
    }

    #[tokio::test]
    async fn lock_times_out_while_another_handle_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = TokenStore::new(dir.path().join("tokens.json"));
        let second = first.clone();
        let held = first.lock().await.unwrap();
        let err = second
            .lock_with_timeout(Duration::from_millis(250))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(&first.lock_path.display().to_string()));
        assert!(err.contains("holding the token store lock") && !err.contains("remove"));
        drop(held);
        second
            .lock_with_timeout(Duration::from_millis(250))
            .await
            .unwrap();
    }
}
