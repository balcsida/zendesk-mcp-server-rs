//! The page at `/`: what this server is and how to connect an MCP client to it.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::Response;

use super::sign_in::{escape_html, html};

/// How MCP clients authenticate to this server.
pub(super) enum Access {
    /// One shared `MCP_BEARER_TOKEN`; every client acts as the server's own Zendesk login.
    SharedToken,
    /// Each client sends its own Zendesk token.
    PerUser,
    /// Per-user, and clients may also sign in through the server at `public`.
    SignIn { public: String },
}

/// What the page says about this server.
pub(super) struct Home {
    pub subdomain: String,
    pub read_only: bool,
    pub access: Access,
}

/// `GET /`: the setup page.
pub(super) async fn page(State(home): State<Arc<Home>>, uri: Uri, headers: HeaderMap) -> Response {
    let origin = match &home.access {
        Access::SignIn { public } => Some(public.clone()),
        _ => request_origin(&uri, &headers),
    };
    let endpoint = origin.map_or("/mcp".to_string(), |origin| format!("{origin}/mcp"));
    html(StatusCode::OK, home.render(&endpoint))
}

/// The `scheme://host` the request was sent to: the `Host` header, or the `:authority`
/// of an HTTP/2 request, with `https` when a TLS proxy says so in `X-Forwarded-Proto`.
/// `None` when the host is missing or is not a plain host name, so the page never
/// pastes shell metacharacters into its commands.
fn request_origin(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .or_else(|| uri.authority().map(|authority| authority.as_str()))?;
    let plain = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:[]".contains(&b));
    if !plain {
        return None;
    }
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|proto| proto.to_str().ok())
        .and_then(|proto| proto.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"));
    Some(format!("{}://{host}", if https { "https" } else { "http" }))
}

impl Home {
    fn render(&self, endpoint: &str) -> String {
        let endpoint = escape_html(endpoint);
        let subdomain = escape_html(&self.subdomain);
        let read_only = if self.read_only {
            " Read-only mode is on: only the tools that do not change anything are available."
        } else {
            ""
        };
        // Adding the server with the token in `ZENDESK_MCP_TOKEN`, for both token modes.
        let token_clients = format!(
            r#"<h3>Claude Code</h3>
<pre>claude mcp add --transport http zendesk {endpoint} --header "Authorization: Bearer $ZENDESK_MCP_TOKEN"</pre>
<h3>Codex</h3>
<pre>codex mcp add zendesk --url {endpoint} --bearer-token-env-var ZENDESK_MCP_TOKEN</pre>
<h3>OpenCode</h3>
<p>In <code>opencode.json</code>:</p>
<pre>{{"mcp": {{"zendesk": {{"type": "remote", "url": "{endpoint}", "oauth": false, "headers": {{"Authorization": "Bearer {{env:ZENDESK_MCP_TOKEN}}"}}}}}}}}</pre>
<h3>Pi</h3>
<pre>pi mcp add zendesk --url {endpoint} --bearer-token-env-var ZENDESK_MCP_TOKEN</pre>
<h3>Other clients</h3>
<p>In their MCP configuration:</p>
<pre>{{"mcpServers": {{"zendesk": {{"url": "{endpoint}", "headers": {{"Authorization": "Bearer &lt;token&gt;"}}}}}}}}</pre>"#
        );
        let (auth, setup) = match &self.access {
            Access::SharedToken => (
                "A shared bearer token, sent as <code>Authorization: Bearer &lt;token&gt;</code>. Ask whoever runs this server for it. Every client acts as the Zendesk user this server was authorized with.",
                format!(
                    r#"<p>Put the token in <code>ZENDESK_MCP_TOKEN</code>. Codex, OpenCode and Pi read the variable whenever they start, so set it in your shell profile:</p>
<pre>export ZENDESK_MCP_TOKEN=&lt;token&gt;</pre>
{token_clients}"#
                ),
            ),
            Access::PerUser => (
                "Your own Zendesk token, sent as <code>Authorization: Bearer &lt;token&gt;</code>. Zendesk applies your permissions, and comments you post are authored by you.",
                format!(
                    r#"<p>Sign in once with the <a href="https://github.com/balcsida/zendesk-rs#install" rel="noopener noreferrer"><code>zendesk</code> CLI</a>:</p>
<pre>zendesk mobile-auth</pre>
<p>Then put your token in <code>ZENDESK_MCP_TOKEN</code>. Codex, OpenCode and Pi read the variable whenever they start, so set it in your shell profile:</p>
<pre>export ZENDESK_MCP_TOKEN=$(zendesk token --mobile)</pre>
{token_clients}"#
                ),
            ),
            Access::SignIn { .. } => (
                "Sign in with your Zendesk login: add the server without a token and your MCP client opens a browser. Your own Zendesk token also works, sent as <code>Authorization: Bearer &lt;token&gt;</code>.",
                format!(
                    r#"<h3>Claude Code</h3>
<pre>claude mcp add --transport http zendesk {endpoint}</pre>
<h3>Codex</h3>
<pre>codex mcp add zendesk --url {endpoint}
codex mcp login zendesk</pre>
<h3>OpenCode</h3>
<p>In <code>opencode.json</code>:</p>
<pre>{{"mcp": {{"zendesk": {{"type": "remote", "url": "{endpoint}"}}}}}}</pre>
<p>OpenCode asks you to sign in on first use, or run <code>opencode mcp auth zendesk</code>.</p>
<h3>Pi</h3>
<pre>pi mcp add zendesk --url {endpoint}
pi mcp login zendesk</pre>
<h3>Other clients</h3>
<p>In their MCP configuration:</p>
<pre>{{"mcpServers": {{"zendesk": {{"url": "{endpoint}"}}}}}}</pre>
<p>For a client that cannot sign in on its own, run <code>zendesk login {endpoint}</code> on your machine and send the token it prints as <code>Authorization: Bearer &lt;token&gt;</code>.</p>"#
                ),
            ),
        };
        format!(
            r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Zendesk MCP server</title>
<style>body{{font-family:system-ui,sans-serif;max-width:48em;margin:2em auto;padding:0 1em}}</style></head>
<body>
<h1>Zendesk MCP server</h1>
<p>This server gives MCP clients access to <code>{subdomain}.zendesk.com</code>.{read_only}</p>
<dl>
<dt>MCP endpoint</dt><dd><code>{endpoint}</code> (streamable HTTP)</dd>
<dt>Authentication</dt><dd>{auth}</dd>
<dt>Zendesk account</dt><dd><code>{subdomain}.zendesk.com</code></dd>
<dt>Health check</dt><dd><code>GET /healthz</code>, no token needed</dd>
</dl>
<h2>Set up a client</h2>
{setup}
<p>More in the <a href="https://github.com/balcsida/zendesk-rs#remote-hosting" rel="noopener noreferrer">README</a>.</p>
</body></html>
"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::sign_in::SignIn;
    use super::super::tests::server;
    use super::super::{Login, ZendeskServer, http_router};
    use super::*;
    use axum::http::{HeaderName, HeaderValue};
    use tokio_util::sync::CancellationToken;
    use zendesk::config::OAuthSettings;

    /// Serve `router` and `GET /` with `headers`; returns the response and the address.
    async fn get_root(
        router: axum::Router,
        headers: &[(&str, &str)],
    ) -> (reqwest::Response, std::net::SocketAddr) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        let mut request = reqwest::Client::new().get(format!("http://{addr}/"));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        (request.send().await.unwrap(), addr)
    }

    fn shared_router() -> axum::Router {
        http_router(
            server(),
            Some("right-token"),
            None,
            CancellationToken::new(),
        )
    }

    fn per_user_server() -> ZendeskServer {
        ZendeskServer::with_login(
            Login::PerUser {
                subdomain: "acme".into(),
                base_url: "http://127.0.0.1:1/api/v2".into(),
            },
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn shared_token_page() {
        let (response, _) = get_root(
            shared_router(),
            &[("Host", "mcp.example.com"), ("X-Forwarded-Proto", "https")],
        )
        .await;
        assert_eq!(response.status(), 200);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("https://mcp.example.com/mcp"), "{body}");
        assert!(body.contains("acme.zendesk.com"), "{body}");
        assert!(body.contains("Authorization: Bearer"), "{body}");
        assert!(!body.contains("mobile-auth"), "{body}");
        for client in [
            "codex mcp add zendesk --url https://mcp.example.com/mcp --bearer-token-env-var ZENDESK_MCP_TOKEN",
            r#""url": "https://mcp.example.com/mcp", "oauth": false, "headers": {"Authorization": "Bearer {env:ZENDESK_MCP_TOKEN}"}"#,
            "pi mcp add zendesk --url https://mcp.example.com/mcp --bearer-token-env-var ZENDESK_MCP_TOKEN",
        ] {
            assert!(body.contains(client), "{client}\n{body}");
        }
    }

    #[tokio::test]
    async fn per_user_read_only_page() {
        let router = http_router(
            per_user_server().read_only(true),
            None,
            None,
            CancellationToken::new(),
        );
        let (response, addr) = get_root(router, &[]).await;
        let body = response.text().await.unwrap();
        assert!(body.contains(&format!("http://{addr}/mcp")), "{body}");
        assert!(body.contains("zendesk mobile-auth"), "{body}");
        assert!(
            body.contains("export ZENDESK_MCP_TOKEN=$(zendesk token --mobile)"),
            "{body}"
        );
        assert!(body.contains("codex mcp add zendesk --url"), "{body}");
        assert!(body.contains("Read-only mode"), "{body}");
    }

    #[tokio::test]
    async fn sign_in_page() {
        let http = reqwest::Client::new();
        let settings = OAuthSettings {
            subdomain: "acme".into(),
            client_id: "zdg-zcli-oauth".into(),
            token_file: std::env::temp_dir().join("home-test-tokens.json"),
            scopes: "read write".into(),
            redirect_uri: "http://localhost:19186/".into(),
        };
        let sign_in = SignIn::new(
            "https://mcp.example.com".into(),
            settings,
            "http://127.0.0.1:1".into(),
            http,
        );
        let router = http_router(
            per_user_server(),
            None,
            Some(sign_in),
            CancellationToken::new(),
        );
        let (response, _) = get_root(router, &[("Host", "other.example")]).await;
        let body = response.text().await.unwrap();
        assert!(body.contains("https://mcp.example.com/mcp"), "{body}");
        assert!(
            body.contains("zendesk login https://mcp.example.com/mcp"),
            "{body}"
        );
        assert!(!body.contains("other.example"), "{body}");
        for client in [
            "codex mcp login zendesk",
            "opencode mcp auth zendesk",
            r#"{"mcp": {"zendesk": {"type": "remote", "url": "https://mcp.example.com/mcp"}}}"#,
            "pi mcp login zendesk",
        ] {
            assert!(body.contains(client), "{client}\n{body}");
        }
        // Signing in needs no token, so none of the token setups apply.
        assert!(!body.contains("ZENDESK_MCP_TOKEN"), "{body}");
    }

    #[test]
    fn request_origin_cases() {
        let origin = |uri: &str, headers: &[(&str, &str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in headers {
                map.insert(
                    HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    HeaderValue::from_str(value).unwrap(),
                );
            }
            request_origin(&uri.parse().unwrap(), &map)
        };
        assert_eq!(
            origin(
                "/",
                &[
                    ("host", "mcp.example.com"),
                    ("x-forwarded-proto", "HTTPS, http")
                ]
            )
            .as_deref(),
            Some("https://mcp.example.com")
        );
        assert_eq!(
            origin(
                "/",
                &[("host", "[::1]:8080"), ("x-forwarded-proto", "http")]
            )
            .as_deref(),
            Some("http://[::1]:8080")
        );
        // HTTP/2 carries the host in `:authority`, which lands in the URI.
        assert_eq!(
            origin("http://h2.example/", &[]).as_deref(),
            Some("http://h2.example")
        );
        assert_eq!(origin("/", &[]), None);
        assert_eq!(origin("/", &[("host", "a$(id).example")]), None);
        assert_eq!(origin("/", &[("host", "")]), None);
    }

    #[tokio::test]
    async fn hostile_host_header_is_dropped() {
        let (response, _) = get_root(shared_router(), &[("Host", "<b>x</b>")]).await;
        let body = response.text().await.unwrap();
        assert!(body.contains("<code>/mcp</code>"), "{body}");
        assert!(!body.contains("<b>x</b>"), "{body}");
    }

    #[test]
    fn rendered_values_are_escaped() {
        let home = Home {
            subdomain: "acme".into(),
            read_only: false,
            access: Access::SharedToken,
        };
        let body = home.render("http://<b>x</b>/mcp");
        assert!(body.contains("http://&lt;b&gt;x&lt;/b&gt;/mcp"), "{body}");
        assert!(!body.contains("<b>x</b>"), "{body}");
    }
}
