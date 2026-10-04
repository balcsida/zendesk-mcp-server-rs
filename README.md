# Zendesk MCP Server (Rust)

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

A Model Context Protocol server for Zendesk.

This is a Rust rewrite of [reminia/zendesk-mcp-server](https://github.com/reminia/zendesk-mcp-server). It is licensed under Apache-2.0.

It offers:

- Tools for retrieving and managing Zendesk tickets, comments, users, organizations, views, macros and SLAs
- Specialized prompts for ticket analysis and response drafting
- Full access to the Zendesk Help Center articles as a knowledge base
- A single binary with no runtime dependencies, speaking stdio or streamable HTTP

## Setup

- Build: `cargo install --path .` or `cargo build --release` (the binary is `target/release/zendesk-mcp-server`).
- Configure authentication: see [Authentication](#authentication).
- Configure Claude Desktop (or any MCP client that runs a command over stdio):

```json
{
  "mcpServers": {
    "zendesk": {
      "command": "/path/to/zendesk-mcp-server",
      "env": {
        "ZENDESK_SUBDOMAIN": "acme",
        "ZENDESK_CLIENT_ID": "your-client-identifier"
      }
    }
  }
}
```

- Or add it to Claude Code:

```bash
claude mcp add zendesk -e ZENDESK_SUBDOMAIN=acme -e ZENDESK_CLIENT_ID=your-client-identifier -- /path/to/zendesk-mcp-server
```

The server also reads a `.env` file from its working directory or any parent.

Without a subcommand the binary serves over stdio; `zendesk-mcp-server stdio` is
the same thing. `zendesk-mcp-server http` serves streamable HTTP instead, see
[Remote hosting](#remote-hosting).

## Authentication

This server authenticates with OAuth. Each operator authorizes with their own
Zendesk login, so API calls carry their identity and Zendesk applies exactly the
permissions it applies in the UI: their role, their group restrictions, their
ticket access. Comments they post are authored by them.

API token authentication still works but is deprecated. See
[Migrating from an API token](#migrating-from-an-api-token).

### 1. Register a public OAuth client

In Admin Center, go to **Apps and integrations > APIs > OAuth clients** and
create a client:

| Field | Value |
| --- | --- |
| Client kind | **Public**. This server runs on each operator's machine, so there is no secret it could keep. PKCE is used instead. |
| Redirect URLs | `http://localhost:4567/callback` |
| Allowed scopes | `tickets:read tickets:write ticket_attachments:read users:read hc:read` |

Setting **Allowed scopes** is optional but recommended. It caps what any token
from this client can ever request, even if the code changes.

Note the client's **Identifier**. That is the `ZENDESK_CLIENT_ID` below.

If Zendesk rejects `http://localhost:4567/callback`, register `https://localhost`
instead and use `zendesk-mcp-server auth --manual` in step 3.

### 2. Configure the environment

Copy `.env.example` to `.env` and set:

```bash
# for https://acme.zendesk.com
ZENDESK_SUBDOMAIN=acme
ZENDESK_CLIENT_ID=your-client-identifier
```

Keep `.env` out of version control.

Two optional settings must agree with the OAuth client:

| Variable | Default | When to change it |
| --- | --- | --- |
| `ZENDESK_OAUTH_REDIRECT_URI` | `http://localhost:4567/callback` | Port 4567 is in use, or the client is registered with a different redirect URL. Must match a redirect URL on the client exactly. |
| `ZENDESK_TOKEN_FILE` | `$XDG_CONFIG_HOME/zendesk-mcp/tokens.json` | Storing tokens elsewhere, for example a Docker volume. |

### 3. Authorize this machine, once

```bash
zendesk-mcp-server auth
```

This opens a browser, asks the operator to approve access, and stores the
resulting tokens locally. From then on the server renews access on its own. The
operator never repeats this unless the tokens are revoked or left unused past the
refresh token's lifetime (90 days as requested by this server).

If the browser cannot reach this machine (a remote shell, or an OAuth client
registered with `https://localhost`), use the paste-based flow instead:

```bash
zendesk-mcp-server auth --manual
```

Tokens are written to `$XDG_CONFIG_HOME/zendesk-mcp/tokens.json`
(`~/.config/zendesk-mcp/tokens.json` by default), created `0600` inside a `0700`
directory. Override the location with `ZENDESK_TOKEN_FILE`. The file holds live
credentials. Treat it like a password and never commit it.

### How token renewal works

Zendesk access tokens are short-lived (30 minutes by default, 48 hours at most),
so the server refreshes them for you:

- **Before expiry**, when the stored token is within 60 seconds of expiring.
- **On rejection**, when Zendesk answers `401` with `{"error": "invalid_token"}`.
  The request is retried once with a fresh token.

Only `invalid_token` triggers a retry. A `401` or `403` from insufficient scope or
from the operator's own Zendesk permissions is passed through unchanged, so
permission problems stay visible instead of looking like auth flakiness.

Each refresh rotates the refresh token and invalidates the previous one
immediately, so the new pair is written to disk before it is used. Writes are
atomic and guarded by a lock file, which matters if you run the server from more
than one MCP client at the same time.

When the refresh token itself is expired or revoked, tools fail with a message
telling the operator to re-run `zendesk-mcp-server auth`.

### Choosing scopes

The default scopes cover every tool this server exposes:

| Scope | Needed for |
| --- | --- |
| `tickets:read` | `get_ticket`, `get_tickets`, `get_ticket_comments` |
| `tickets:write` | `create_ticket`, `update_ticket`, `create_ticket_comment` |
| `ticket_attachments:read` | `get_ticket_attachment` |
| `users:read` | requester and assignee details on tickets |
| `hc:read` | the `zendesk://knowledge-base` resource |

Narrow them with `ZENDESK_OAUTH_SCOPES` if you do not need every tool. For
read-only access, use `tickets:read users:read hc:read`.

The views, macros, organizations, groups, forms, search and SLA tools were
added after this table was written and have not been checked against each
Zendesk scope. If one of them answers `403`, widen `ZENDESK_OAUTH_SCOPES` (for
example with `organizations:read`, `macros:read` or the broad `read`), update
the OAuth client's allowed scopes to match, and re-run `zendesk-mcp-server auth`.

Scopes are a ceiling, not a grant. A token can never do more than the
authorizing operator is allowed to do. Zendesk accepts unrecognised scope names
when issuing a token but then rejects every request with `403`, so
`zendesk-mcp-server auth` prints the scope Zendesk actually granted for comparison.

### Migrating from an API token

Zendesk is retiring API tokens on this schedule:

| Date | Change |
| --- | --- |
| 2026-07-28 | Tokens unused for 30 days are deactivated automatically. New accounts cannot create tokens. |
| 2026-10-27 | No account can create new API tokens. |
| 2027-04-30 | All API tokens stop working permanently. |

Until then `ZENDESK_EMAIL` + `ZENDESK_API_KEY` continue to work, and the server
logs a deprecation warning the first time it authenticates. Set
`ZENDESK_CLIENT_ID` and OAuth takes precedence, so you can migrate without
removing the old variables.

There is a reason to move sooner: a Zendesk API token is account-level and
unscoped. Whoever holds it gets the full access of the user it is paired with,
which for most installations is an admin. Per-operator OAuth fixes that.

> **Why not the client credentials flow?** It is simpler, with no browser step
> and no refresh tokens. But its tokens are attributed to the Zendesk user who
> created the OAuth client. Every operator would act as that one user, usually an
> admin, and audit logs and comment authorship would all point at them. Since the
> goal is for operators to have exactly their own Zendesk permissions, the
> authorization code flow is the only one that fits.

### Mobile sign-in

```bash
zendesk-mcp-server mobile-auth
```

This signs in through the Zendesk mobile app's OAuth flow. No OAuth client is
needed. Use it when you cannot register an OAuth client in Admin Center.

- Accounts with email and password sign in directly, without a browser.
- Accounts with SSO (SAML, Google, Office 365) open the system browser. The final
  redirect goes to `zendesk-support://authenticate?...`, which is captured by a
  temporary URL-scheme handler, or by pasting the URL.

The access token has no refresh token, so sign in again when it expires.

The token is saved to `~/.config/zendesk-mcp/mobile_token.json` (under
`$XDG_CONFIG_HOME` if set). Override the location with `ZENDESK_MOBILE_TOKEN_FILE`.
The file has the same JSON format as the `.zendesk_token` file of the Python
version, so an old file can be copied over.

With none of the other credentials set, the server uses this saved token, or
starts a browser sign-in at startup. `ZENDESK_SUBDOMAIN` is then optional, because
the saved token records it.

### Session cookie

Set `ZENDESK_SESSION_COOKIE` to the `_zendesk_session` cookie of a browser that is
signed in to Zendesk, together with `ZENDESK_SUBDOMAIN`. The cookie expires when
the browser session does.

### Credential precedence

The first match wins:

| # | Set | Credentials used |
| --- | --- | --- |
| 1 | `ZENDESK_CLIENT_ID` | OAuth with PKCE (needs `ZENDESK_SUBDOMAIN`) |
| 2 | `ZENDESK_OAUTH_TOKEN` | Fixed bearer token (needs `ZENDESK_SUBDOMAIN`) |
| 3 | `ZENDESK_EMAIL` + `ZENDESK_API_KEY` | API token, deprecated (needs `ZENDESK_SUBDOMAIN`) |
| 4 | `ZENDESK_SESSION_COOKIE` | Session cookie (needs `ZENDESK_SUBDOMAIN`) |
| 5 | nothing | Saved mobile token, or browser sign-in at startup |

## Docker

1. Copy `.env.example` to `.env` and fill in your Zendesk configuration. Keep this file outside version control.
2. Build the image:

   ```bash
   docker build -t zendesk-mcp-server-rs .
   ```

The image runs as the non-root user `appuser` (uid 10001). It stores tokens in
`/tokens` (`ZENDESK_TOKEN_FILE=/tokens/tokens.json` and
`ZENDESK_MOBILE_TOKEN_FILE=/tokens/mobile_token.json`). Mount a named volume there
so tokens survive restarts. The server rewrites the file each time it rotates the
refresh token, so the mount must be writable.

### Authorize on a headless server

Run the paste-based flow once, with the token volume mounted:

```bash
docker run -it --rm --env-file .env -v zendesk-tokens:/tokens zendesk-mcp-server-rs auth --manual
```

### stdio

Add `-i` when wiring the container to an MCP client over stdin/stdout:

```bash
docker run --rm -i --env-file .env -v zendesk-tokens:/tokens zendesk-mcp-server-rs
```

Claude Code or Claude Desktop configuration:

```json
{
  "mcpServers": {
    "zendesk": {
      "command": "docker",
      "args": [
        "run", "--rm", "-i",
        "--env-file", "/path/to/.env",
        "-v", "zendesk-tokens:/tokens",
        "zendesk-mcp-server-rs"
      ]
    }
  }
}
```

### HTTP

```bash
docker run -d --name zendesk-mcp \
  --env-file .env \
  -e MCP_BEARER_TOKEN=change-me \
  -v zendesk-tokens:/tokens \
  -p 8080:8080 \
  zendesk-mcp-server-rs http
```

Inside the image `http` listens on `0.0.0.0:8080` by default.

### Compose

`compose.yaml` runs the HTTP transport with a persistent `zendesk-tokens` volume.
Put `MCP_BEARER_TOKEN` in `.env`, then:

```bash
docker compose up -d --build
docker compose run --rm zendesk-mcp auth --manual   # first time only
```

## Remote hosting

Serve over streamable HTTP with the `http` subcommand:

```bash
zendesk-mcp-server http --bind 0.0.0.0:8080 --bearer-token "$(openssl rand -hex 32)"
```

- The MCP endpoint is `http://host:8080/mcp`.
- `--bind` (or `MCP_HTTP_ADDR`) defaults to `127.0.0.1:8080`; the Docker image sets it to `0.0.0.0:8080`.
- The `http` transport requires a bearer token (`--bearer-token` or `MCP_BEARER_TOKEN`). Clients send it as `Authorization: Bearer <token>`.
- `GET /healthz` is unauthenticated, for health checks.
- The server speaks plain HTTP. Put TLS in front with a reverse proxy such as Caddy or nginx.

Add it to Claude Code:

```bash
claude mcp add --transport http zendesk https://host/mcp --header "Authorization: Bearer ..."
```

All MCP clients share the one Zendesk identity the server was authorized with.
Everyone who holds the bearer token acts as that Zendesk user.

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `ZENDESK_SUBDOMAIN` | none | Zendesk subdomain (`acme` for `acme.zendesk.com`). Required for credentials 1 to 4. |
| `ZENDESK_CLIENT_ID` | none | Identifier of a public OAuth client. Enables OAuth. |
| `ZENDESK_OAUTH_SCOPES` | `tickets:read tickets:write ticket_attachments:read users:read hc:read` | Scopes requested at sign-in. |
| `ZENDESK_OAUTH_REDIRECT_URI` | `http://localhost:4567/callback` | Redirect URL registered on the OAuth client. |
| `ZENDESK_TOKEN_FILE` | `~/.config/zendesk-mcp/tokens.json` | OAuth token store. |
| `ZENDESK_OAUTH_TOKEN` | none | Fixed bearer token. |
| `ZENDESK_MOBILE_TOKEN_FILE` | `~/.config/zendesk-mcp/mobile_token.json` | Token store for `mobile-auth`. |
| `ZENDESK_EMAIL` | none | Email for API token auth (deprecated). |
| `ZENDESK_API_KEY` | none | API token (deprecated). |
| `ZENDESK_SESSION_COOKIE` | none | `_zendesk_session` cookie of a signed-in browser. |
| `MCP_HTTP_ADDR` | `127.0.0.1:8080` (`0.0.0.0:8080` in Docker) | Listen address for the `http` subcommand. Same as `--bind`. |
| `MCP_BEARER_TOKEN` | none | Bearer token clients must present to the `http` transport. Same as `--bearer-token`. |
| `RUST_LOG` | `info` | Log filter. Logs go to stderr. |

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests use mocked HTTP and never contact Zendesk.

## Troubleshooting

### Safari Authentication Issues (macOS)

If you're using Safari and seeing errors like "Safari cannot open the page because the address is invalid" with URLs containing `SAMLRequest` or `SAMLResponse`, this is a **known issue with Safari's handling of SAML redirects**.

**Solution:** The sign-in tries Chrome first. If you don't have Chrome installed:

1. Install Chrome: `brew install --cask google-chrome`
2. Run `zendesk-mcp-server mobile-auth` again, or restart the MCP server to repeat the sign-in.

**Why this happens:**

- Safari has strict security policies that prevent SAML POST requests from being completed in the OAuth mobile flow.
- The SAML identity provider tries to POST back to Zendesk, but Safari blocks this.
- Chrome and Firefox handle these SAML flows more gracefully.

**Alternative workaround:** If you must use Safari:

1. Safari > Settings > Privacy > uncheck "Prevent cross-site tracking".
2. Try signing in again.
3. Re-enable tracking prevention after sign-in completes.

### Authentication Timeout

If sign-in times out after 5 minutes:

1. Check that the URL scheme handler registered successfully (look for `Registered macOS URL scheme handler`, or the Linux or Windows equivalent, in the server log; set `RUST_LOG=debug` for more detail).
2. Try the manual fallback by opening the sign-in URL the command prints in your browser.
3. Complete the sign-in and copy/paste the `zendesk-support://` URL from the address bar.

## Resources

- `zendesk://knowledge-base`: all Help Center articles.

## Prompts

### analyze-ticket

Analyze a Zendesk ticket and provide a detailed analysis of the ticket.

- `ticket_id` (required)

### draft-ticket-response

Draft a response to a Zendesk ticket.

- `ticket_id` (required)

## Tools

### Tickets

#### get_tickets

Fetch the latest tickets with pagination support.

- `page` (integer, optional): Page number (defaults to 1)
- `per_page` (integer, optional): Tickets per page, max 100 (defaults to 25)
- `sort_by` (string, optional): `created_at`, `updated_at`, `priority` or `status` (defaults to `created_at`)
- `sort_order` (string, optional): `asc` or `desc` (defaults to `desc`)

Returns tickets with id, subject, status, priority, description, timestamps, requester and assignee (with `requester_name` and `assignee_name` when Zendesk returns the users), plus pagination metadata.

#### get_ticket

Retrieve a Zendesk ticket by its ID.

- `ticket_id` (integer)

Includes `requester_name` and `assignee_name` when Zendesk returns the users.

#### get_tickets_bulk

Fetch multiple tickets by IDs in a single request (max 100).

- `ticket_ids` (array[integer])

#### get_ticket_comments

Retrieve all comments for a ticket.

- `ticket_id` (integer)

#### create_ticket_comment

Create a new comment on an existing ticket.

- `ticket_id` (integer)
- `comment` (string): Markdown, plain text and HTML are accepted
- `public` (boolean, optional): Whether the comment is public (defaults to true)

#### get_ticket_attachment

Fetch a ticket attachment by its `content_url` and return the file as base64-encoded data.

- `content_url` (string): The `content_url` from `get_ticket_comments`

#### create_ticket

Create a new ticket.

- `subject` (string)
- `description` (string)
- `requester_id` (integer, optional)
- `assignee_id` (integer, optional)
- `priority` (string, optional): `low`, `normal`, `high`, `urgent`
- `type` (string, optional): `problem`, `incident`, `question`, `task`
- `tags` (array[string], optional)
- `custom_fields` (array[object], optional)

#### update_ticket

Update fields on an existing ticket (for example status, priority, assignee).

- `ticket_id` (integer)
- `subject` (string, optional)
- `status` (string, optional): `new`, `open`, `pending`, `on-hold`, `solved`, `closed`
- `priority` (string, optional): `low`, `normal`, `high`, `urgent`
- `type` (string, optional)
- `assignee_id` (integer, optional)
- `requester_id` (integer, optional)
- `tags` (array[string], optional)
- `custom_fields` (array[object], optional)
- `due_at` (string, optional): ISO8601 datetime

#### delete_ticket

Permanently delete a ticket. Use with caution.

- `ticket_id` (integer)

#### merge_tickets

Merge source tickets into a target ticket.

- `target_id` (integer): The ticket to merge into
- `source_ids` (array[integer]): Tickets to merge from
- `target_comment` (string, optional): Defaults to `Merged from related tickets.`
- `source_comment` (string, optional): Defaults to `This ticket has been merged.`

#### get_user_tickets

Get tickets for a user by role.

- `user_id` (integer)
- `role` (string, optional): `requested`, `assigned` or `ccd` (defaults to `requested`)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Defaults to 25

### Search

#### search

Search with Zendesk Query Language (ZQL) across tickets, users and organizations. Examples: `type:ticket status:open priority:urgent`, `type:ticket assignee:me`, `type:user email:john@example.com`.

- `query` (string): ZQL query
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)
- `sort_by` (string, optional): `relevance`, `updated_at`, `created_at`, `priority`, `status`, `ticket_type` (defaults to `relevance`)
- `sort_order` (string, optional): `asc` or `desc` (defaults to `desc`)

Ticket results include `requester_name` and `assignee_name` when Zendesk returns the users.

#### search_all_tickets

Search tickets with ZQL and return every match instead of one page, up to Zendesk's 1,000-result search limit. `truncated` is true when more tickets matched; narrow the query (for example with `created>2026-01-01`) to get the rest.

- `query` (string): ZQL query scoped to tickets, for example `type:ticket status:open`
- `sort_by` (string, optional): `updated_at`, `created_at`, `priority`, `status`, `ticket_type` (defaults to `created_at`)
- `sort_order` (string, optional): `asc` or `desc` (defaults to `desc`)

### Users and organizations

#### get_user

Get a user by ID. Use it to resolve `requester_id` or `assignee_id` from tickets.

- `user_id` (integer)

#### get_current_user

Get the currently authenticated user. No inputs.

#### search_users

Search users by name, email or external_id.

- `query` (string)

#### get_organization

Get an organization by ID.

- `organization_id` (integer)

#### search_organizations

Search organizations by name.

- `query` (string)

### Views, fields, forms, groups, macros

#### list_views

List all available views (saved ticket queues). No inputs.

#### execute_view

Execute a view and return its tickets.

- `view_id` (integer)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Defaults to 25

#### list_ticket_fields

List all ticket fields (system and custom) with their types and valid options. No inputs.

#### list_ticket_forms

List all ticket forms and their associated field IDs. No inputs.

#### list_groups

List assignable groups for ticket routing. No inputs.

#### list_macros

List available macros (canned responses and actions).

- `active_only` (boolean, optional): Defaults to true

#### apply_macro

Preview the result of applying a macro to a ticket. Does not save changes.

- `ticket_id` (integer)
- `macro_id` (integer)

### Help Center

#### search_articles

Search Help Center articles by query string.

- `query` (string)
- `locale` (string, optional): For example `en-us`, `fr`, `es`
- `per_page` (integer, optional): Max 100 (defaults to 25)
- `page` (integer, optional): Page number (defaults to 1)

Returns matching articles with id, title, body, author_id, section_id, locale, html_url, timestamps and draft status, plus pagination metadata.

#### get_article

Get a Help Center article by ID.

- `article_id` (integer)
- `locale` (string, optional): For example `en-us`, `fr`, `es`

Returns the article with id, title, body, author_id, section_id, locale, html_url, timestamps, draft and promoted status, position, voting statistics and label names.

### Metrics and SLAs

#### get_ticket_metrics

Get performance and SLA metrics for a ticket (reply time, resolution time, wait times and so on).

- `ticket_id` (integer)

#### get_ticket_audits

Retrieve the audit trail (all changes and events) for a ticket.

- `ticket_id` (integer)

Returns audits with id, author_id, created_at and a trimmed list of events (type, body, value changes and so on).

#### get_linked_incidents

Get the incident tickets linked to a problem ticket.

- `ticket_id` (integer)

Returns the linked incident tickets with id, subject, status, priority, requester_id, assignee_id, group_id and timestamps.

#### get_sla_breaches

Find tickets that breached SLA within a time period.

- `days_back` (integer, optional): Defaults to 7
- `metric` (string, optional): `reply_time`, `first_reply_time`, `agent_work_time`, `requester_wait_time` or `periodic_update_time`

#### get_sla_policies

Get all SLA policies with their metric targets per priority level. No inputs.
