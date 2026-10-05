//! Configuration from the environment.
//!
//! Credentials are chosen in this order:
//!
//! 1. `ZENDESK_CLIENT_ID` — OAuth with PKCE, authorized once with `zendesk-mcp-server auth`.
//! 2. `ZENDESK_OAUTH_TOKEN` — a fixed bearer token.
//! 3. `ZENDESK_EMAIL` + `ZENDESK_API_KEY` — deprecated API token (Zendesk retires these on 2027-04-30).
//! 4. `ZENDESK_SESSION_COOKIE` — the `_zendesk_session` cookie of a signed-in browser.
//! 5. Nothing — the token saved by `zendesk-mcp-server mobile-auth`, or a browser sign-in at startup.
//!
//! `ZENDESK_SUBDOMAIN` is required for 1–4; for 5 it may also come from the saved mobile token.
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

const MISSING_SUBDOMAIN: &str =
    "ZENDESK_SUBDOMAIN is not set. For https://acme.zendesk.com the subdomain is 'acme'.";

pub const API_TOKEN_DEPRECATION_MESSAGE: &str = "Zendesk API token authentication is deprecated. \
Zendesk deactivates unused API tokens from 2026-07-28, blocks creation of new ones from 2026-10-27, \
and stops accepting all API tokens on 2027-04-30. It also grants this server the full access of the \
token's user rather than the permissions of the operator using it. Migrate to OAuth by setting \
ZENDESK_CLIENT_ID and running `zendesk-mcp-server auth`.";

/// OAuth authorization-code-with-PKCE configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSettings {
    pub subdomain: String,
    pub client_id: String,
    pub token_file: PathBuf,
    pub scopes: String,
    pub redirect_uri: String,
}

impl OAuthSettings {
    pub fn token_endpoint(&self) -> String {
        format!("https://{}.zendesk.com/oauth/tokens", self.subdomain)
    }

    pub fn authorize_endpoint(&self) -> String {
        format!(
            "https://{}.zendesk.com/oauth/authorizations/new",
            self.subdomain
        )
    }
}

/// Which credentials the environment describes.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    OAuth(OAuthSettings),
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
    /// No explicit credentials: use the saved mobile-app token, or sign in through a
    /// browser at startup. `subdomain` is `None` when `ZENDESK_SUBDOMAIN` is unset and must
    /// then come from the saved token.
    Mobile {
        subdomain: Option<String>,
    },
}

// Manual Debug so secrets never reach logs or panics.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credentials::OAuth(s) => f.debug_tuple("OAuth").field(s).finish(),
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
            Credentials::Mobile { subdomain } => f
                .debug_struct("Mobile")
                .field("subdomain", subdomain)
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
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("zendesk-mcp")
}

/// Where `auth` stores OAuth tokens unless `ZENDESK_TOKEN_FILE` says otherwise.
pub fn default_token_file() -> PathBuf {
    config_dir().join("tokens.json")
}

/// Where `mobile-auth` stores its token unless `ZENDESK_MOBILE_TOKEN_FILE` says otherwise.
pub fn default_mobile_token_file() -> PathBuf {
    config_dir().join("mobile_token.json")
}

/// Expand a leading `~/` so operators can write `ZENDESK_TOKEN_FILE=~/x/tokens.json`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// `ZENDESK_SUBDOMAIN` alone, for per-user mode.
pub fn load_subdomain() -> Result<String> {
    std::env::var("ZENDESK_SUBDOMAIN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!(MISSING_SUBDOMAIN))
}

/// Read credentials from the process environment.
pub fn load_credentials() -> Result<Credentials> {
    load_credentials_from(|key| std::env::var(key).ok())
}

/// Read credentials through `get`, so tests can supply their own environment.
pub fn load_credentials_from(get: impl Fn(&str) -> Option<String>) -> Result<Credentials> {
    let clean = |key: &str| {
        get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    let subdomain = clean("ZENDESK_SUBDOMAIN");
    let require_subdomain = || -> Result<String> {
        match &subdomain {
            Some(s) => Ok(s.clone()),
            None => bail!(MISSING_SUBDOMAIN),
        }
    };

    if let Some(client_id) = clean("ZENDESK_CLIENT_ID") {
        let settings = OAuthSettings {
            subdomain: require_subdomain()?,
            client_id,
            token_file: clean("ZENDESK_TOKEN_FILE")
                .map(|p| expand_home(&p))
                .unwrap_or_else(default_token_file),
            scopes: clean("ZENDESK_OAUTH_SCOPES")
                .unwrap_or_else(|| DEFAULT_OAUTH_SCOPES.to_string()),
            redirect_uri: clean("ZENDESK_OAUTH_REDIRECT_URI")
                .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string()),
        };
        tracing::info!(
            client_id = %settings.client_id,
            token_file = %settings.token_file.display(),
            "Using Zendesk OAuth authentication"
        );
        return Ok(Credentials::OAuth(settings));
    }

    if let Some(access_token) = clean("ZENDESK_OAUTH_TOKEN") {
        return Ok(Credentials::Bearer {
            subdomain: require_subdomain()?,
            access_token,
        });
    }

    let email = clean("ZENDESK_EMAIL");
    let token = clean("ZENDESK_API_KEY");
    match (email, token) {
        (Some(email), Some(token)) => {
            tracing::warn!("{API_TOKEN_DEPRECATION_MESSAGE}");
            return Ok(Credentials::ApiToken {
                subdomain: require_subdomain()?,
                email,
                token,
            });
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
        return Ok(Credentials::SessionCookie {
            subdomain: require_subdomain()?,
            cookie,
        });
    }

    Ok(Credentials::Mobile { subdomain })
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
            Credentials::OAuth(s) => {
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
    fn nothing_set_means_mobile_flow() {
        assert_eq!(
            load_credentials_from(env(&[])).unwrap(),
            Credentials::Mobile { subdomain: None }
        );
        assert_eq!(
            load_credentials_from(env(&[("ZENDESK_SUBDOMAIN", " acme ")])).unwrap(),
            Credentials::Mobile {
                subdomain: Some("acme".into())
            }
        );
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
