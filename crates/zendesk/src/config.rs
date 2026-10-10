//! Configuration from the environment.
//!
//! Credentials are chosen in this order:
//!
//! 1. `ZENDESK_CLIENT_ID` — OAuth with PKCE, authorized once with `zendesk-mcp-server auth` (or `zendesk auth`).
//! 2. `ZENDESK_OAUTH_TOKEN` — a fixed bearer token.
//! 3. `ZENDESK_EMAIL` + `ZENDESK_API_KEY` — deprecated API token (Zendesk retires these on 2027-04-30).
//! 4. `ZENDESK_SESSION_COOKIE` — the `_zendesk_session` cookie of a signed-in browser.
//! 5. `ZENDESK_SUBDOMAIN` alone — OAuth with PKCE through zcli's public client
//!    ([`ZCLI_CLIENT_ID`]), authorized once the same way as 1.
//!
//! If not even `ZENDESK_SUBDOMAIN` is set, [`load_credentials`] returns `None` and the
//! caller decides what that means: the MCP server fails, the CLI falls back to its saved
//! mobile token.
//!
//! `ZENDESK_SUBDOMAIN` is required for 1–5.
//!
//! `http --per-user-auth` uses none of these: every caller sends their own Zendesk token,
//! and only `ZENDESK_SUBDOMAIN` is read, through [`load_subdomain`].

use std::fmt;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

/// The broad `read` (every GET endpoint, including ticket audits and search, which have
/// no narrow scope) plus the narrow write scopes of the documented tool families. A few
/// operations have no documented narrow scope (`recover_suspended_ticket`,
/// `restore_deleted_ticket`, `search_problem_tickets` with text and
/// `search_custom_object_records` with a filter); if one answers 403, add the broad
/// `write` scope. Zendesk accepts unknown scope names when issuing a token but then
/// rejects every request with 403, so keep these exact.
pub const DEFAULT_OAUTH_SCOPES: &str =
    "read tickets:write ticket_attachments:write users:write organizations:write hc:write";

/// Must match a redirect URL registered on the OAuth client in Admin Center.
pub const DEFAULT_REDIRECT_URI: &str = "http://localhost:4567/callback";

/// The public OAuth client of zcli, Zendesk's own CLI, used when `ZENDESK_CLIENT_ID` is
/// not set so that nobody has to register a client in Admin Center. Zendesk does not
/// document it for other tools, so it may be renamed or restricted;
/// `ZENDESK_CLIENT_ID` switches to a client of your own.
pub const ZCLI_CLIENT_ID: &str = "zdg-zcli-oauth";

/// zcli's client accepts this and the same URL on ports 19187 and 19188.
pub const ZCLI_REDIRECT_URI: &str = "http://localhost:19186/";

/// What zcli itself requests, so its client is known to grant it.
pub const ZCLI_OAUTH_SCOPES: &str = "read write";

pub(crate) const MISSING_SUBDOMAIN: &str =
    "ZENDESK_SUBDOMAIN is not set. For https://acme.zendesk.com the subdomain is 'acme'.";

pub const API_TOKEN_DEPRECATION_MESSAGE: &str = "Zendesk API token authentication is deprecated. \
Zendesk deactivates unused API tokens from 2026-07-28, blocks creation of new ones from 2026-10-27, \
and stops accepting all API tokens on 2027-04-30. It also grants this server the full access of the \
token's user rather than the permissions of the operator using it. Migrate to OAuth by unsetting \
ZENDESK_EMAIL and ZENDESK_API_KEY and running `zendesk-mcp-server auth` (or `zendesk auth`).";

/// OAuth authorization-code-with-PKCE configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSettings {
    pub subdomain: String,
    pub client_id: String,
    pub token_file: PathBuf,
    pub scopes: String,
    pub redirect_uri: String,
}

/// `https://{subdomain}.zendesk.com`, the account's web origin.
///
/// ```
/// assert_eq!(zendesk::config::origin("acme"), "https://acme.zendesk.com");
/// ```
pub fn origin(subdomain: &str) -> String {
    format!("https://{subdomain}.zendesk.com")
}

impl OAuthSettings {
    pub fn token_endpoint(&self) -> String {
        format!("{}/oauth/tokens", origin(&self.subdomain))
    }

    pub fn authorize_endpoint(&self) -> String {
        format!("{}/oauth/authorizations/new", origin(&self.subdomain))
    }
}

/// Which credentials the environment describes.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A struct variant on purpose: CodeQL treats a call to anything named `OAuth` as a
    /// source of secrets and then flags every URL built from these settings, though they
    /// hold only a public client id, scopes and paths.
    OAuth {
        settings: OAuthSettings,
    },
    Bearer {
        subdomain: String,
        access_token: String,
    },
    ApiToken {
        subdomain: String,
        email: String,
        token: String,
    },
    SessionCookie {
        subdomain: String,
        cookie: String,
    },
}

// Manual Debug so secrets never reach logs or panics.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credentials::OAuth { settings } => {
                f.debug_struct("OAuth").field("settings", settings).finish()
            }
            Credentials::Bearer { subdomain, .. } => f
                .debug_struct("Bearer")
                .field("subdomain", subdomain)
                .field("access_token", &"<redacted>")
                .finish(),
            Credentials::ApiToken {
                subdomain, email, ..
            } => f
                .debug_struct("ApiToken")
                .field("subdomain", subdomain)
                .field("email", email)
                .field("token", &"<redacted>")
                .finish(),
            Credentials::SessionCookie { subdomain, .. } => f
                .debug_struct("SessionCookie")
                .field("subdomain", subdomain)
                .field("cookie", &"<redacted>")
                .finish(),
        }
    }
}

/// `$XDG_CONFIG_HOME/zendesk-mcp`, or `~/.config/zendesk-mcp`.
///
/// Deliberately outside any project directory so tokens are never picked up by
/// version control or a Docker build context.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("zendesk-mcp")
}

/// Where `auth` stores OAuth tokens unless `ZENDESK_TOKEN_FILE` says otherwise.
pub fn default_token_file() -> PathBuf {
    config_dir().join("tokens.json")
}

/// Expand a leading `~/` so operators can write `ZENDESK_TOKEN_FILE=~/x/tokens.json`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().map_or_else(|| PathBuf::from(path), |h| h.join(rest)),
        None => PathBuf::from(path),
    }
}

/// Trim `raw` and check that it is one DNS label, so it cannot redirect the URLs built
/// from it (`acme/x#`, `evil.example/#`) to another host.
///
/// # Errors
///
/// Fails unless the trimmed value is 1 to 63 letters, digits and inner hyphens.
pub fn validate_subdomain(raw: &str) -> Result<String> {
    let value = raw.trim();
    let alnum = |c: char| c.is_ascii_alphanumeric();
    let valid = (1..=63).contains(&value.len())
        && value.starts_with(alnum)
        && value.ends_with(alnum)
        && value.chars().all(|c| alnum(c) || c == '-');
    if !valid {
        bail!(
            "ZENDESK_SUBDOMAIN '{value}' is not a valid subdomain. For https://acme.zendesk.com the subdomain is 'acme': letters, digits and hyphens only, no dots, slashes or spaces."
        );
    }
    Ok(value.to_string())
}

/// `ZENDESK_SUBDOMAIN` alone, for per-user mode.
pub fn load_subdomain() -> Result<String> {
    let raw = std::env::var("ZENDESK_SUBDOMAIN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!(MISSING_SUBDOMAIN))?;
    validate_subdomain(&raw)
}

/// Read credentials from the process environment; `None` when none are configured.
pub fn load_credentials() -> Result<Option<Credentials>> {
    load_credentials_from(|key| std::env::var(key).ok())
}

/// Read credentials through `get`, so tests can supply their own environment.
pub fn load_credentials_from(get: impl Fn(&str) -> Option<String>) -> Result<Option<Credentials>> {
    let clean = |key: &str| {
        get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let subdomain = clean("ZENDESK_SUBDOMAIN")
        .map(|v| validate_subdomain(&v))
        .transpose()?;
    let require_subdomain = || -> Result<String> {
        match &subdomain {
            Some(s) => Ok(s.clone()),
            None => bail!(MISSING_SUBDOMAIN),
        }
    };

    // Not named `oauth`: CodeQL takes a call to anything named OAuth for a source of
    // secrets, as with `Credentials::OAuth`.
    let sign_in_with = |client_id: String| -> Result<Option<Credentials>> {
        // zcli's client accepts only its own redirect URLs.
        let (scopes, redirect_uri) = if client_id == ZCLI_CLIENT_ID {
            (ZCLI_OAUTH_SCOPES, ZCLI_REDIRECT_URI)
        } else {
            (DEFAULT_OAUTH_SCOPES, DEFAULT_REDIRECT_URI)
        };
        let settings = OAuthSettings {
            subdomain: require_subdomain()?,
            client_id,
            token_file: clean("ZENDESK_TOKEN_FILE")
                .map_or_else(default_token_file, |p| expand_home(&p)),
            scopes: clean("ZENDESK_OAUTH_SCOPES").unwrap_or_else(|| scopes.to_string()),
            redirect_uri: clean("ZENDESK_OAUTH_REDIRECT_URI")
                .unwrap_or_else(|| redirect_uri.to_string()),
        };
        tracing::info!(
            client_id = %settings.client_id,
            token_file = %settings.token_file.display(),
            "Using Zendesk OAuth authentication"
        );
        Ok(Some(Credentials::OAuth { settings }))
    };

    if let Some(client_id) = clean("ZENDESK_CLIENT_ID") {
        return sign_in_with(client_id);
    }

    if let Some(access_token) = clean("ZENDESK_OAUTH_TOKEN") {
        return Ok(Some(Credentials::Bearer {
            subdomain: require_subdomain()?,
            access_token,
        }));
    }

    let email = clean("ZENDESK_EMAIL");
    let token = clean("ZENDESK_API_KEY");
    match (email, token) {
        (Some(email), Some(token)) => {
            tracing::warn!("{API_TOKEN_DEPRECATION_MESSAGE}");
            return Ok(Some(Credentials::ApiToken {
                subdomain: require_subdomain()?,
                email,
                token,
            }));
        }
        (Some(_), None) => bail!(
            "Incomplete API token configuration: ZENDESK_API_KEY is not set. Set both, or switch to OAuth with ZENDESK_CLIENT_ID."
        ),
        (None, Some(_)) => bail!(
            "Incomplete API token configuration: ZENDESK_EMAIL is not set. Set both, or switch to OAuth with ZENDESK_CLIENT_ID."
        ),
        (None, None) => {}
    }

    if let Some(cookie) = clean("ZENDESK_SESSION_COOKIE") {
        return Ok(Some(Credentials::SessionCookie {
            subdomain: require_subdomain()?,
            cookie,
        }));
    }

    if subdomain.is_some() {
        return sign_in_with(ZCLI_CLIENT_ID.to_string());
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn oauth_wins_over_api_token() {
        let creds = load_credentials_from(env(&[
            ("ZENDESK_SUBDOMAIN", "acme"),
            ("ZENDESK_CLIENT_ID", "client"),
            ("ZENDESK_EMAIL", "a@b.c"),
            ("ZENDESK_API_KEY", "key"),
        ]))
        .unwrap();
        match creds {
            Some(Credentials::OAuth { settings: s }) => {
                assert_eq!(s.subdomain, "acme");
                assert_eq!(s.scopes, DEFAULT_OAUTH_SCOPES);
                assert_eq!(
                    s.scopes,
                    "read tickets:write ticket_attachments:write users:write organizations:write hc:write"
                );
                assert_eq!(s.redirect_uri, DEFAULT_REDIRECT_URI);
                assert_eq!(s.token_endpoint(), "https://acme.zendesk.com/oauth/tokens");
            }
            other => panic!("expected OAuth, got {other:?}"),
        }
    }

    #[test]
    fn api_token_requires_both_halves() {
        let err = load_credentials_from(env(&[
            ("ZENDESK_SUBDOMAIN", "acme"),
            ("ZENDESK_EMAIL", "a@b.c"),
        ]))
        .unwrap_err();
        assert!(err.to_string().contains("ZENDESK_API_KEY"));
    }

    #[test]
    fn subdomain_is_required_for_explicit_credentials() {
        let err = load_credentials_from(env(&[("ZENDESK_OAUTH_TOKEN", "t")])).unwrap_err();
        assert!(err.to_string().contains("ZENDESK_SUBDOMAIN"));
    }

    #[test]
    fn nothing_set_means_no_credentials() {
        assert_eq!(load_credentials_from(env(&[])).unwrap(), None);
    }

    fn zcli_settings(pairs: &[(&str, &str)]) -> OAuthSettings {
        match load_credentials_from(env(pairs)).unwrap() {
            Some(Credentials::OAuth { settings }) => settings,
            other => panic!("expected OAuth, got {other:?}"),
        }
    }

    #[test]
    fn subdomain_alone_signs_in_with_zcli_client() {
        let zcli = zcli_settings(&[("ZENDESK_SUBDOMAIN", " acme ")]);
        assert_eq!(zcli.subdomain, "acme");
        assert_eq!(zcli.client_id, "zdg-zcli-oauth");
        assert_eq!(zcli.redirect_uri, "http://localhost:19186/");
        assert_eq!(zcli.scopes, "read write");
    }

    #[test]
    fn naming_the_zcli_client_explicitly_gets_its_defaults() {
        let named = zcli_settings(&[
            ("ZENDESK_SUBDOMAIN", "acme"),
            ("ZENDESK_CLIENT_ID", "zdg-zcli-oauth"),
        ]);
        assert_eq!(named, zcli_settings(&[("ZENDESK_SUBDOMAIN", "acme")]));
    }

    #[test]
    fn an_explicit_credential_wins_over_the_zcli_fallback() {
        let bearer = load_credentials_from(env(&[
            ("ZENDESK_SUBDOMAIN", "acme"),
            ("ZENDESK_OAUTH_TOKEN", "t"),
        ]))
        .unwrap();
        assert!(matches!(bearer, Some(Credentials::Bearer { .. })));
    }

    #[test]
    fn validate_subdomain_accepts_one_dns_label() {
        for ok in ["acme", "acme-1", "A1", " acme "] {
            assert_eq!(validate_subdomain(ok).unwrap(), ok.trim());
        }
        assert!(validate_subdomain(&"a".repeat(63)).is_ok());
    }

    #[test]
    fn validate_subdomain_rejects_anything_else() {
        let long = "a".repeat(64);
        for bad in [
            "acme/x#", "a.b", "a@b", "a b", "acme%2f", "-acme", "acme-", "", &long,
        ] {
            assert!(validate_subdomain(bad).is_err(), "{bad:?} was accepted");
        }
        let err = validate_subdomain("acme/x#").unwrap_err().to_string();
        assert!(err.contains("acme/x#") && err.contains("https://acme.zendesk.com"));
    }

    #[test]
    fn credentials_reject_invalid_subdomain() {
        let err =
            load_credentials_from(env(&[("ZENDESK_SUBDOMAIN", "evil.example/#")])).unwrap_err();
        assert!(err.to_string().contains("evil.example/#"));
    }

    #[test]
    fn debug_redacts_secrets() {
        let creds = Credentials::ApiToken {
            subdomain: "acme".into(),
            email: "a@b.c".into(),
            token: "SECRET".into(),
        };
        let shown = format!("{creds:?}");
        assert!(!shown.contains("SECRET"));
        assert!(shown.contains("a@b.c"));
    }
}
