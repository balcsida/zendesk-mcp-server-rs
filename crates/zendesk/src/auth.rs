//! Credentials attached to Zendesk API requests.
//!
//! `Auth::value()` is consulted on every request rather than once at construction,
//! which is what lets an expiring OAuth token be refreshed transparently.

use std::sync::Arc;

use anyhow::Result;
use base64::Engine;

use crate::config::Credentials;
use crate::mobile_auth;
use crate::oauth::OAuthProvider;

// `OAuth` is the protocol's name, not a stutter of the enum's.
#[allow(clippy::enum_variant_names)]
#[derive(Clone)]
pub enum Auth {
    /// Deprecated email + API token, sent as HTTP Basic.
    ApiToken { header: String },
    /// A fixed OAuth access token, such as the one `mobile-auth` saves.
    Bearer { header: String },
    /// The `_zendesk_session` cookie of a browser signed in to Zendesk.
    SessionCookie { cookie: String },
    /// Stored OAuth tokens, refreshed before expiry and after an `invalid_token` 401.
    OAuth(Arc<OAuthProvider>),
}

/// The credential to put on one outgoing request.
#[derive(Clone, PartialEq, Eq)]
pub enum AuthValue {
    /// Value of the `Authorization` header.
    Authorization(String),
    /// Value of the `Cookie` header. Set per request rather than through a cookie jar
    /// so it is never replayed to another host on redirect.
    Cookie(String),
}

// Manual Debug so a `{:?}` can never print the credential.
impl std::fmt::Debug for AuthValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthValue::Authorization(_) => f.write_str("Authorization(<redacted>)"),
            AuthValue::Cookie(_) => f.write_str("Cookie(<redacted>)"),
        }
    }
}

impl AuthValue {
    pub fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            AuthValue::Authorization(v) => req.header(reqwest::header::AUTHORIZATION, v),
            AuthValue::Cookie(v) => req.header(reqwest::header::COOKIE, v),
        }
    }

    /// The bearer token carried by this value, if any. Used to tell "Zendesk rejected
    /// this exact token" apart from "another process already rotated it".
    pub fn bearer_token(&self) -> Option<&str> {
        match self {
            AuthValue::Authorization(v) => v.strip_prefix("Bearer "),
            AuthValue::Cookie(_) => None,
        }
    }
}

impl Auth {
    pub fn api_token(email: &str, token: &str) -> Self {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{email}/token:{token}"));
        Auth::ApiToken {
            header: format!("Basic {encoded}"),
        }
    }

    pub fn bearer(access_token: &str) -> Self {
        Auth::Bearer {
            header: format!("Bearer {access_token}"),
        }
    }

    pub fn session_cookie(cookie: &str) -> Self {
        Auth::SessionCookie {
            cookie: format!("_zendesk_session={cookie}"),
        }
    }

    /// Build the credential described by the environment. For `Credentials::Mobile` this
    /// may verify a saved token or open a browser. Returns the subdomain to talk to as well,
    /// because the mobile flow can supply it from the saved token.
    pub async fn from_credentials(
        creds: &Credentials,
        http: &reqwest::Client,
    ) -> Result<(String, Auth)> {
        Ok(match creds {
            Credentials::OAuth(settings) => (
                settings.subdomain.clone(),
                Auth::OAuth(Arc::new(OAuthProvider::new(settings.clone(), http.clone()))),
            ),
            Credentials::Bearer {
                subdomain,
                access_token,
            } => (subdomain.clone(), Auth::bearer(access_token)),
            Credentials::ApiToken {
                subdomain,
                email,
                token,
            } => (subdomain.clone(), Auth::api_token(email, token)),
            Credentials::SessionCookie { subdomain, cookie } => {
                (subdomain.clone(), Auth::session_cookie(cookie))
            }
            Credentials::Mobile { subdomain } => {
                let token = mobile_auth::ensure_auth(http, subdomain.as_deref()).await?;
                (token.subdomain.clone(), Auth::bearer(&token.access_token))
            }
        })
    }

    /// The credential to attach right now. For OAuth this refreshes a token that is
    /// about to expire before handing it out.
    pub async fn value(&self) -> Result<AuthValue> {
        Ok(match self {
            Auth::ApiToken { header } | Auth::Bearer { header } => {
                AuthValue::Authorization(header.clone())
            }
            Auth::SessionCookie { cookie } => AuthValue::Cookie(cookie.clone()),
            Auth::OAuth(provider) => {
                AuthValue::Authorization(format!("Bearer {}", provider.access_token().await?))
            }
        })
    }

    /// The OAuth provider, when credentials can be renewed after a rejection.
    pub fn oauth(&self) -> Option<&Arc<OAuthProvider>> {
        match self {
            Auth::OAuth(provider) => Some(provider),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn api_token_is_basic_auth_of_email_slash_token() {
        let auth = Auth::api_token("agent@example.com", "abc123");
        let value = auth.value().await.unwrap();
        // base64("agent@example.com/token:abc123")
        assert_eq!(
            value,
            AuthValue::Authorization("Basic YWdlbnRAZXhhbXBsZS5jb20vdG9rZW46YWJjMTIz".into())
        );
        assert_eq!(value.bearer_token(), None);
    }

    #[tokio::test]
    async fn bearer_exposes_its_token() {
        let value = Auth::bearer("tok").value().await.unwrap();
        assert_eq!(value, AuthValue::Authorization("Bearer tok".into()));
        assert_eq!(value.bearer_token(), Some("tok"));
    }

    #[tokio::test]
    async fn session_cookie_is_a_cookie_header() {
        let value = Auth::session_cookie("abc").value().await.unwrap();
        assert_eq!(value, AuthValue::Cookie("_zendesk_session=abc".into()));
    }
}
