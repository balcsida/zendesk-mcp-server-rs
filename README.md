# zendesk-rs

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

A Zendesk MCP server and a Zendesk CLI, sharing one Rust client library.

> **Not an official Zendesk product.** This is an independent, community-maintained project. It is not affiliated with, endorsed by or supported by Zendesk, Inc. Zendesk is a trademark of Zendesk, Inc., used here only to describe the service the software talks to.

This is a Rust rewrite of [reminia/zendesk-mcp-server](https://github.com/reminia/zendesk-mcp-server). It is licensed under Apache-2.0.

## What is in the repository

- `zendesk-mcp-server`: the MCP server. Tools for tickets, comments and attachments; search and counts; users, organizations, groups and brands; views, macros and triggers; custom objects; Help Center reading and writing; SLA and satisfaction data. Four catalog tools that search, describe and call any other Zendesk API operation, so the server reaches the full API. Prompts for ticket analysis and response drafting. The Help Center articles as a knowledge base resource. Speaks stdio or streamable HTTP.
- `zendesk`: the CLI for scripted use: a command for every Zendesk API operation (`zendesk <group> <operation>`), plus `api`, `token`, `auth`, `mobile-auth`, `login` and `skills`.
- `crates/zendesk`: the shared client library.
- `skills/`: the agent skills, embedded in both binaries.

Both binaries are single files with no runtime dependencies.

## Install

Install the binary you need, or both. The MCP server is also published as a [Docker image](#docker).

### Prebuilt binaries

Download the archives for your platform from the [releases](https://github.com/balcsida/zendesk-rs/releases) page. Each has a `.sha256` file. Unpack them and put the binaries on your `PATH`.

Each archive carries a build provenance attestation. Verify a download with the [GitHub CLI](https://cli.github.com/):

```bash
gh attestation verify <archive> --repo balcsida/zendesk-rs
```

The image is attested too:

```bash
gh attestation verify oci://ghcr.io/balcsida/zendesk-mcp-server:<version> --repo balcsida/zendesk-rs
```

| Binary | Archives |
| --- | --- |
| `zendesk-mcp-server` | `zendesk-mcp-server-x86_64-unknown-linux-gnu.tar.gz`, `zendesk-mcp-server-aarch64-unknown-linux-gnu.tar.gz`, `zendesk-mcp-server-aarch64-apple-darwin.tar.gz`, `zendesk-mcp-server-x86_64-apple-darwin.tar.gz`, `zendesk-mcp-server-x86_64-pc-windows-msvc.zip` |
| `zendesk` | `zendesk-x86_64-unknown-linux-gnu.tar.gz`, `zendesk-aarch64-unknown-linux-gnu.tar.gz`, `zendesk-aarch64-apple-darwin.tar.gz`, `zendesk-x86_64-apple-darwin.tar.gz`, `zendesk-x86_64-pc-windows-msvc.zip` |

### With Cargo

The crates are not on crates.io, so Cargo builds them from this repository. This needs Rust 1.89 or later.

```bash
cargo install --locked --git https://github.com/balcsida/zendesk-rs zendesk-mcp-server  # the MCP server
cargo install --locked --git https://github.com/balcsida/zendesk-rs zendesk-cli         # the zendesk CLI
```

Cargo installs them to `~/.cargo/bin`. This builds the latest `main`. To build a release instead, add its tag, for example `--tag v0.6.0`.

From a clone, run `cargo install --locked --path crates/zendesk-mcp-server` (or `crates/zendesk-cli`), or `cargo build --release`, which puts both binaries in `target/release/`.

On x86-64 Windows, the TLS library (aws-lc) needs [NASM](https://www.nasm.us/) to build. Without NASM, set `AWS_LC_SYS_PREBUILT_NASM=1` to use prebuilt objects.

## MCP server

- Sign in once: see [Authentication](#authentication).
- Configure Claude Desktop (or any MCP client that runs a command over stdio):

```json
{
  "mcpServers": {
    "zendesk": {
      "command": "/path/to/zendesk-mcp-server",
      "env": {
        "ZENDESK_SUBDOMAIN": "acme"
      }
    }
  }
}
```

- Or add it to Claude Code:

```bash
claude mcp add zendesk -e ZENDESK_SUBDOMAIN=acme -- /path/to/zendesk-mcp-server
```

The server also reads a `.env` file from its working directory.

Without a subcommand the binary serves over stdio; `zendesk-mcp-server stdio` is
the same thing. `zendesk-mcp-server http` serves streamable HTTP instead, see
[Remote hosting](#remote-hosting).

## Authentication

The server and the CLI authenticate the same way, with OAuth. Each operator authorizes with their own
Zendesk login, so API calls carry their identity and Zendesk applies exactly the
permissions it applies in the UI: their role, their group restrictions, their
ticket access. Comments they post are authored by them.

API token authentication still works but is deprecated. See
[Migrating from an API token](#migrating-from-an-api-token).

### 1. Set the subdomain

Copy `.env.example` to `.env` and set:

```bash
# for https://acme.zendesk.com
ZENDESK_SUBDOMAIN=acme
```

Keep `.env` out of version control.

No OAuth client has to be registered. Unless `ZENDESK_CLIENT_ID` is set, the
server and the CLI sign in through the public OAuth client of
[zcli](https://github.com/zendesk/zcli), Zendesk's own command-line tool: client
`zdg-zcli-oauth`, redirect URL `http://localhost:19186/`, scopes `read write`.

Zendesk does not document that client for other tools. It may rename or restrict
it, and an account's admins may block it. If sign-in fails for that reason,
[use your own OAuth client](#using-your-own-oauth-client).

### 2. Authorize this machine, once

```bash
zendesk-mcp-server auth
```

The CLI's `zendesk auth` does the same and writes the same token file.

This opens a browser, asks the operator to approve access, and stores the
resulting tokens locally. From then on the server and the CLI renew access on their own. The
operator never repeats this unless the tokens are revoked or left unused past the
refresh token's lifetime (both binaries request 90 days).

If the browser cannot reach this machine (a remote shell, or an OAuth client
registered with `https://localhost`), use the paste-based flow instead:

```bash
zendesk-mcp-server auth --manual
```

(`zendesk auth --manual` works the same way.)

If port 19186 is in use, set `ZENDESK_OAUTH_REDIRECT_URI` to
`http://localhost:19187/` or `http://localhost:19188/`. zcli's client accepts
those three.

Tokens are written to `$XDG_CONFIG_HOME/zendesk-mcp/tokens.json`
(`~/.config/zendesk-mcp/tokens.json` by default), created `0600` inside a `0700`
directory on Unix. On Windows the file inherits the folder's permissions. Override the
location with `ZENDESK_TOKEN_FILE`. The file holds live credentials. Treat it like a password and never commit it.

### Using your own OAuth client

Admins, and agents with the Manage APIs permission, can register an OAuth client
instead. It does not depend on zcli's client, and its allowed scopes cap what any
token can request.

In Admin Center, go to **Apps and integrations > APIs > OAuth clients** and
create a client:

| Field | Value |
| --- | --- |
| Client kind | **Public**. The server and the CLI run on each operator's machine, so there is no secret they could keep. PKCE is used instead. |
| Redirect URLs | `http://localhost:4567/callback` |
| Allowed scopes | `read tickets:write ticket_attachments:write users:write organizations:write hc:write` |

Setting **Allowed scopes** is optional but recommended. It caps what any token
from this client can ever request, even if the code changes.

Set the client's **Identifier** as `ZENDESK_CLIENT_ID` and run `auth` as above.

`ZENDESK_OAUTH_REDIRECT_URI` then defaults to `http://localhost:4567/callback`.
Change it when port 4567 is in use or the client has a different redirect URL. It
must match a redirect URL on the client exactly. If Zendesk rejects
`http://localhost:4567/callback`, register `https://localhost` instead and use
`zendesk-mcp-server auth --manual` (or `zendesk auth --manual`).

### How token renewal works

Zendesk access tokens are short-lived (30 minutes by default, 48 hours at most),
so they are refreshed for you:

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
telling the operator to re-run `zendesk-mcp-server auth` (or `zendesk auth`).

### Choosing scopes

zcli's client gets `read write`, the broad scopes, which cover every tool and the
whole API catalog. `ZENDESK_OAUTH_SCOPES=read` makes it read-only.

With your own client the default scopes are narrower. They cover the documented
tool families this server exposes:

| Scope | Needed for |
| --- | --- |
| `read` | every read tool, including search, views, macros, groups, organizations, ticket audits, SLA data and the knowledge base |
| `tickets:write` | `create_ticket`, `update_ticket`, `create_ticket_comment`, `delete_ticket`, `merge_tickets` and other ticket changes |
| `ticket_attachments:write` | uploading files to attach to tickets |
| `users:write` | creating or updating users |
| `organizations:write` | updating organizations |
| `hc:write` | creating or editing Help Center articles |

A few operations have no documented narrow scope: `recover_suspended_ticket`,
`restore_deleted_ticket`, `search_problem_tickets` with `text` and
`search_custom_object_records` with a `filter`. If one of them answers 403, add the
broad `write` scope to `ZENDESK_OAUTH_SCOPES` (reads stay covered by `read`).

The API catalog (the CLI's generated commands, and `call_api_read` and
`call_api_write` on the server) also reaches families this table does not list,
such as macros, triggers, webhooks and Talk. Writing to them needs the broad
`write` scope, or the family's own scope where Zendesk documents one. The server's
catalog tools hide and refuse the operations that return or change credentials, and
the account-administration writes (see [API catalog](#api-catalog)); the CLI runs them.
`MCP_READ_ONLY=true` makes the server list and run only its read-only tools.

`read` alone is the read-only configuration. Zendesk gives search, job statuses
and ticket audits no narrow read scope (`tickets:read` is not enough for audits),
so the broad `read` is requested and covers all of these. Narrow the write
scopes with `ZENDESK_OAUTH_SCOPES` if you do not need every tool, and update the
OAuth client's allowed scopes to match.

Scopes are a ceiling, not a grant. A token can never do more than the
authorizing operator is allowed to do. Zendesk accepts unrecognised scope names
when issuing a token but then rejects every request with `403`, so
`auth` prints the scope Zendesk actually granted for comparison.

### Migrating from an API token

Zendesk is retiring API tokens on this schedule:

| Date | Change |
| --- | --- |
| 2026-07-28 | Tokens unused for 30 days are deactivated automatically. New accounts cannot create tokens. |
| 2026-10-27 | No account can create new API tokens. |
| 2027-04-30 | All API tokens stop working permanently. |

Until then `ZENDESK_EMAIL` + `ZENDESK_API_KEY` continue to work, with a deprecation
warning from the server and the CLI. To migrate, remove them and run
`zendesk-mcp-server auth`. Setting `ZENDESK_CLIENT_ID` (`zdg-zcli-oauth` for
zcli's client) also puts OAuth first, without removing them.

There is a reason to move sooner: a Zendesk API token is account-level and
unscoped. Whoever holds it gets the full access of the user it is paired with,
which for most installations is an admin. Per-operator OAuth fixes that.

> **Why not the client credentials flow?** It is simpler, with no browser step
> and no refresh tokens. But its tokens are attributed to the Zendesk user who
> created the OAuth client. Every operator would act as that one user, usually an
> admin, and audit logs and comment authorship would all point at them. Since the
> goal is for operators to have exactly their own Zendesk permissions, the
> authorization code flow is the only one that fits.

### Session cookie

Set `ZENDESK_SESSION_COOKIE` to the `_zendesk_session` cookie of a browser that is
signed in to Zendesk, together with `ZENDESK_SUBDOMAIN`. The cookie expires when
the browser session does.

### Credential precedence

The first match wins:

| # | Set | Credentials used |
| --- | --- | --- |
| 1 | `ZENDESK_CLIENT_ID` | OAuth with PKCE through that client (needs `ZENDESK_SUBDOMAIN`) |
| 2 | `ZENDESK_OAUTH_TOKEN` | Fixed bearer token (needs `ZENDESK_SUBDOMAIN`) |
| 3 | `ZENDESK_EMAIL` + `ZENDESK_API_KEY` | API token, deprecated (needs `ZENDESK_SUBDOMAIN`) |
| 4 | `ZENDESK_SESSION_COOKIE` | Session cookie (needs `ZENDESK_SUBDOMAIN`) |
| 5 | `ZENDESK_SUBDOMAIN` alone | OAuth with PKCE through zcli's client |
| 6 | nothing | Server: fails with an error. CLI: the saved mobile token, or a browser sign-in |

## CLI

`zendesk` calls the Zendesk API from scripts and the shell. `api` takes a path relative to `/api/v2/` (a leading `/` or `api/v2/` is tolerated), or an absolute URL on the account, so a `next_page` link can be passed straight back. It handles JSON endpoints only. It prints pretty-printed JSON on stdout, nothing for an empty body, and sends errors to stderr with exit code 1.

`-X` sets the method (GET by default, POST when `-d` is given), `-d` the body (inline JSON, `@file` or `@-` for stdin) and `-q key=value` a query parameter, repeatable.

```bash
zendesk api users/me.json
zendesk api tickets.json -q sort_by=updated_at -q sort_order=desc | jq '.tickets[].subject'
zendesk api search.json -q 'query=type:ticket status:open'
zendesk api -X POST tickets.json -d '{"ticket":{"subject":"Printer on fire","comment":{"body":"Help"}}}'
zendesk api -X PUT tickets/123.json -d @update.json
echo '{"ticket":{"status":"solved"}}' | zendesk api -X PUT tickets/123.json -d @-
zendesk api -X DELETE tickets/123.json
zendesk api "$(zendesk api tickets.json | jq -r .next_page)"     # follow pagination
ZENDESK_OAUTH_TOKEN=$(zendesk token) zendesk-mcp-server          # hand the CLI's token to the server
```

`zendesk token` prints the bearer access token the CLI would use, refreshing OAuth first. It fails for API-token and cookie credentials. `zendesk token --mobile` prints the saved mobile token even when other credentials are configured.

`token` and `api` read the same environment variables as the server, in the same [precedence](#credential-precedence), also from a `.env` file in the working directory. When none is set, not even `ZENDESK_SUBDOMAIN`, they use the saved mobile token (checked with a `users/me` call). A browser sign-in opens if it is missing or rejected.

The CLI logs only warnings unless `RUST_LOG` is set.

### API commands

Every operation in the API catalog is a command, `zendesk <group> <operation>`. Both names are the catalog's kebab-cased: the group `Ticket Comments` is `ticket-comments`, the operation `ShowTicket` is `show-ticket`, `ListSLAPolicies` is `list-sla-policies`. `zendesk --help` lists the groups and `zendesk <group> --help` the operations in one. `zendesk <group> <operation> --help` shows the method, path, description and an example body.

Path parameters are positional arguments. Query parameters are long options named after the parameter, with `[`, `]`, `_` and `.` turned into `-`: `page[size]` is `--page-size`. Every query option is repeatable; repeats become a list, sent comma-separated or as repeated parameters as the API expects. For an object parameter such as `page`, pass `KEY=VALUE`, which is sent as `page[KEY]=VALUE`. Where the API lists values for a parameter, help shows them as a hint; any value is accepted. `-p KEY=VALUE` (`--param`) adds a query parameter the catalog does not list, and is the way to set one whose option name would clash with `--data`, `--param` or another option. `-d` (`--data`) gives the JSON body of operations that write, inline, as `@file`, or as `@-` for stdin; it is required where the API requires a body.

```bash
zendesk tickets show-ticket 123
zendesk tickets list-tickets --sort-by updated_at --sort-order desc
zendesk search list-search-results --query 'type:ticket status:open'
zendesk tickets create-ticket -d @ticket.json
zendesk ticket-comments list-ticket-comments 123 --page size=10
zendesk tickets list-tickets -p external_id=abc-1
```

Output and errors work as for `api`. The commands are built from the catalog on every run.

### Mobile sign-in

```bash
zendesk mobile-auth
```

This signs in through the Zendesk mobile app's OAuth flow. Like `auth`, it needs
no OAuth client of your own. Use it for [per-user mode](#per-user-mode), or when
zcli's client is blocked on your account.
Other `zendesk` commands use the mobile token only while `ZENDESK_SUBDOMAIN` is
unset; with it set they sign in through zcli's client, and `zendesk token --mobile`
still prints the mobile token.

- Accounts with email and password sign in directly, without a browser.
- Accounts with SSO (SAML, Google, Office 365) open the system browser. The final
  redirect goes to `zendesk-support://authenticate?...`, which is captured by a
  temporary URL-scheme handler, or by pasting the URL.

If `ZENDESK_SUBDOMAIN` is already set, `mobile-auth` uses it directly instead of
prompting (the subdomain is printed, since it is not a secret).

SSO sign-in (from `mobile-auth`, or when a command finds no saved token) opens a
private/incognito window in Chrome, Firefox or Edge where one of those browsers is
installed, so the mobile OAuth cookies stay separate from the operator's normal
Zendesk session. It falls back to the system default browser otherwise.

The access token has no refresh token, so sign in again when it expires.

The token is saved to `~/.config/zendesk-mcp/mobile_token.json` (under
`$XDG_CONFIG_HOME` if set). Override the location with `ZENDESK_MOBILE_TOKEN_FILE`.
The file has the same JSON format as the `.zendesk_token` file of the Python
version, so an old file can be copied over.

The MCP server does not read this file. Pass the token to it as a fixed bearer token:

```bash
ZENDESK_OAUTH_TOKEN=$(zendesk token --mobile) zendesk-mcp-server
```

## Agent skills

The repository ships [agent skills](skills/README.md): SKILL.md bundles in the open [Agent Skills](https://agentskills.io/specification) format. They teach AI coding agents to use Zendesk through the `zendesk` CLI or the MCP tools: when to use each, how to find the right command, and which writes to confirm first. Both binaries embed them, so either one installs them:

```bash
zendesk skills install
zendesk-mcp-server skills install
zendesk skills list
```

By default the skills go under your home directory. `--project` installs under the current directory instead, `--agent claude` (or `kiro`) also copies them into that agent's own directory even if it is not detected, and `--dir <DIR>` installs into one directory only.

| Path | Content |
| --- | --- |
| `.agents/skills/zendesk/SKILL.md` | Root operating rules, setup guard and command discovery |
| `.agents/skills/zendesk-tickets/` | Finding, reading and changing tickets |
| `.agents/skills/zendesk-help-center/` | Categories, sections and articles |
| `.agents/skills/zendesk-admin/` | Users, organizations, views, macros, triggers, custom objects, SLAs |

`.agents/skills` is read by Codex, Cursor, Gemini CLI, GitHub Copilot, OpenCode, Amp and Pi. Claude Code does not read it, so it gets a copy under `.claude/skills` when `.claude` exists (Kiro likewise with `.kiro`). Restart the agent afterwards.

Without a binary, `npx skills add balcsida/zendesk-rs` or `gh skill install balcsida/zendesk-rs` installs the same files. The agent loads a skill when a task matches its description.

## Docker

The image contains the MCP server only, not the `zendesk` CLI.

1. Copy `.env.example` to `.env` and fill in your Zendesk configuration. Keep this file outside version control.
2. Pull the published image (`ghcr.io/balcsida/zendesk-mcp-server`; tags `latest`, `MAJOR.MINOR` and `MAJOR.MINOR.PATCH`, published from 0.2.0 on; linux/amd64 and linux/arm64):

   ```bash
   docker pull ghcr.io/balcsida/zendesk-mcp-server:latest
   ```

   Or build it locally:

   ```bash
   docker build -t ghcr.io/balcsida/zendesk-mcp-server .
   ```

The image is built on distroless (`gcr.io/distroless/cc-debian12`), so it has no shell, and
runs as the non-root user `nonroot` (uid 65532). It stores tokens in
`/tokens` (`ZENDESK_TOKEN_FILE=/tokens/tokens.json`). Mount a named volume there
so tokens survive restarts. The server rewrites the file each time it rotates the
refresh token, so the mount must be writable. A bind-mounted directory must be
writable by uid 65532 (for example `chown 65532:65532 ./tokens`).

### Authorize on a headless server

Run the paste-based flow once, with the token volume mounted:

```bash
docker run -it --rm --env-file .env -v zendesk-tokens:/tokens ghcr.io/balcsida/zendesk-mcp-server auth --manual
```

### stdio

Add `-i` when wiring the container to an MCP client over stdin/stdout:

```bash
docker run --rm -i --env-file .env -v zendesk-tokens:/tokens ghcr.io/balcsida/zendesk-mcp-server
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
        "ghcr.io/balcsida/zendesk-mcp-server"
      ]
    }
  }
}
```

### HTTP

```bash
docker run -d --name zendesk-mcp \
  --env-file .env \
  -e MCP_BEARER_TOKEN="$(openssl rand -hex 32)" \
  -v zendesk-tokens:/tokens \
  -p 8080:8080 \
  ghcr.io/balcsida/zendesk-mcp-server http
```

Inside the image `http` listens on `0.0.0.0:8080` by default.

### Compose

`compose.yaml` runs the HTTP transport with a persistent `zendesk-tokens` volume.
Put `MCP_BEARER_TOKEN` in `.env` (generate it with `openssl rand -hex 32`). Compose publishes the port on `127.0.0.1` only, so put a TLS reverse proxy on the host in front of it. Then:

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
- The `http` transport requires a bearer token (`--bearer-token` or `MCP_BEARER_TOKEN`), unless it runs in [per-user mode](#per-user-mode). Clients send it as `Authorization: Bearer <token>`.
- `GET /healthz` is unauthenticated, for health checks.
- `GET /` is a setup page for people: the MCP endpoint, how to authenticate and the Zendesk subdomain, with the commands to add the server to an MCP client. Like `/healthz`, it needs no token. Behind a proxy it shows `https` addresses when the proxy sends `X-Forwarded-Proto: https`.
- The server speaks plain HTTP. Put TLS in front with a reverse proxy such as Caddy or nginx. The proxy must terminate TLS and should enforce request timeouts and rate limits, particularly on `/authorize` and `/cli/*`: the server has no connection limits of its own.

Add it to Claude Code:

```bash
claude mcp add --transport http zendesk https://host/mcp --header "Authorization: Bearer ..."
```

All MCP clients share the one Zendesk identity the server was authorized with.
Everyone who holds the bearer token acts as that Zendesk user.

### Per-user mode

In per-user mode the server holds no Zendesk login of its own. Each client sends
its own Zendesk token as the bearer token, the server passes it on, and Zendesk
applies that user's permissions and records them as the author.

```bash
ZENDESK_SUBDOMAIN=acme zendesk-mcp-server http --per-user-auth --bind 0.0.0.0:8080
```

`--per-user-auth` (or `MCP_PER_USER_AUTH=true`) replaces `MCP_BEARER_TOKEN`, and
setting both is an error. The server reads only `ZENDESK_SUBDOMAIN`.

Each user signs in once on their own machine with the CLI, then adds the server
with the token it prints:

```bash
zendesk mobile-auth
claude mcp add --transport http zendesk https://host/mcp \
  --header "Authorization: Bearer $(zendesk token --mobile)"
```

`--mobile` makes sure the 30-minute OAuth token from `auth` is not picked up instead.

- A `mobile-auth` token suits a fixed header because it has no refresh token to
  rotate. When Zendesk stops accepting it, run `zendesk mobile-auth` again and add the
  server again. Tokens from `auth` expire after 30 minutes and a header cannot
  renew them, so they do not fit this mode.
- Only `Bearer` tokens are accepted, not API tokens.
- The server lets any bearer token through and leaves it to Zendesk to reject
  invalid ones, so anyone who can reach the port can make it send requests to
  Zendesk. Keep the port on a private network or behind a proxy that limits
  request rates. The subdomain is fixed on the server, so the server cannot be
  used to reach anything else.
- Requests don't share sessions: each one stands alone with its own token. No
  caller can join another's session, and a load balancer needs no sticky
  sessions.
- The knowledge-base resource is fetched on every read rather than cached,
  because Help Center articles can be restricted to some users.
- The tokens are personal credentials. Serve them over TLS only, and keep
  request headers out of the proxy's logs.
- The MCP specification calls this token passthrough. It works in clients that
  let you set headers, such as Claude Code, Cursor and VS Code. For clients that
  only support OAuth sign-in, see [Sign-in through the server](#sign-in-through-the-server).

### Sign-in through the server

The server can sign users in itself, so they need no Zendesk token of their own. Turn
it on in per-user mode with the server's public address:

```bash
ZENDESK_SUBDOMAIN=acme zendesk-mcp-server http --per-user-auth --public-url https://zendesk-mcp.example.com
```

`--public-url` (or `MCP_PUBLIC_URL`) must be an `https` origin with nothing after the
host (`http` only on localhost, for testing): the server has to sit at the root of its
host. The server uses the Zendesk
variables from [Authentication](#authentication), with zcli's OAuth client by default.
If any other credential variable is set, it does not start.

MCP clients sign in on their own. Add the server without a header, and the client opens
a browser:

```bash
claude mcp add --transport http zendesk https://zendesk-mcp.example.com/mcp
```

Pi, in `~/.pi/agent/mcp.json`:

```json
{"mcpServers": {"zendesk": {"url": "https://zendesk-mcp.example.com/mcp"}}}
```

OpenCode, in `opencode.json`:

```json
{"mcp": {"zendesk": {"type": "remote", "url": "https://zendesk-mcp.example.com/mcp"}}}
```

The page the server shows sends the user to Zendesk. Zendesk then redirects to a
`localhost` page that fails to load. The user copies that page's address and pastes it
into the form within 2 minutes.

To skip the paste, run `zendesk login` on your own machine. It catches the redirect
itself and prints a server token on stdout:

```bash
export ZENDESK_MCP_TOKEN=$(zendesk login https://zendesk-mcp.example.com/mcp)
```

Send the token as `Authorization: Bearer <token>`.

```bash
claude mcp add --transport http zendesk https://zendesk-mcp.example.com/mcp \
  --header "Authorization: Bearer $ZENDESK_MCP_TOKEN"
```

Pi:

```json
{"mcpServers": {"zendesk": {"url": "https://zendesk-mcp.example.com/mcp", "headers": {"Authorization": "Bearer ${ZENDESK_MCP_TOKEN}"}}}}
```

OpenCode also needs `"oauth": false`:

```json
{"mcp": {"zendesk": {"type": "remote", "url": "https://zendesk-mcp.example.com/mcp", "oauth": false, "headers": {"Authorization": "Bearer {env:ZENDESK_MCP_TOKEN}"}}}}
```

Some OpenCode versions send `{env:...}` headers empty. If yours does, put the token in
the file directly.

A sign-in lasts until it goes 90 days without use, until the login is revoked in
Zendesk, or until its directory is deleted. The client then signs in again. A login
revoked in Zendesk keeps failing tool calls for up to 30 minutes, until its access
token expires.

- Logins are kept in `grants/`, next to `ZENDESK_TOKEN_FILE`: `/tokens/grants/` in
  Docker.
- A pasted sign-in ends after three failed attempts. Logins unused for 90 days are
  deleted.
- Each login has its own directory. Its `user.json` names the user. Deleting the
  directory signs that user out.
- The volume holds every signed-in user's Zendesk refresh token. Protect it like a
  password store.
- Run one server process only. Logins are renewed and removed on local disk, and
  sign-ins in progress live in memory.
- Raw Zendesk tokens are still passed through, as in per-user mode above.

## Environment variables

Both binaries read these from the environment or from a `.env` file in the working directory. The `MCP_*` variables apply to the server only.

| Variable | Default | Purpose |
| --- | --- | --- |
| `ZENDESK_SUBDOMAIN` | none | Zendesk subdomain (`acme` for `acme.zendesk.com`). Required for credentials 1 to 5 and for per-user mode. Alone, it signs in through zcli's OAuth client. |
| `ZENDESK_CLIENT_ID` | `zdg-zcli-oauth` (zcli's client) | Identifier of a public OAuth client. Setting it puts OAuth ahead of the other credentials. |
| `ZENDESK_OAUTH_SCOPES` | `read write` for zcli's client, else `read tickets:write ticket_attachments:write users:write organizations:write hc:write` | Scopes requested at sign-in. |
| `ZENDESK_OAUTH_REDIRECT_URI` | `http://localhost:19186/` for zcli's client, else `http://localhost:4567/callback` | Redirect URL registered on the OAuth client. |
| `ZENDESK_TOKEN_FILE` | `~/.config/zendesk-mcp/tokens.json` | OAuth token store. |
| `ZENDESK_OAUTH_TOKEN` | none | Fixed bearer token. |
| `ZENDESK_MOBILE_TOKEN_FILE` | `~/.config/zendesk-mcp/mobile_token.json` | CLI only: token store for `zendesk mobile-auth` and the CLI's fallback when nothing is configured, not even `ZENDESK_SUBDOMAIN`. |
| `ZENDESK_EMAIL` | none | Email for API token auth (deprecated). |
| `ZENDESK_API_KEY` | none | API token (deprecated). |
| `ZENDESK_SESSION_COOKIE` | none | `_zendesk_session` cookie of a signed-in browser. |
| `MCP_HTTP_ADDR` | `127.0.0.1:8080` (`0.0.0.0:8080` in Docker) | Listen address for the `http` subcommand. Same as `--bind`. |
| `MCP_BEARER_TOKEN` | none | Bearer token clients must present to the `http` transport, unless `MCP_PER_USER_AUTH` is set. At least 16 characters; prefer the variable over `--bearer-token`, whose value shows in `ps`. |
| `MCP_READ_ONLY` | `false` | `true` (or `1`) lists and runs only the read-only tools. |
| `MCP_PER_USER_AUTH` | `false` | `true` has every `http` client act with its own Zendesk token, see [Per-user mode](#per-user-mode). Same as `--per-user-auth`. |
| `MCP_PUBLIC_URL` | none | Public origin of the server, like `https://zendesk-mcp.example.com`. With `MCP_PER_USER_AUTH`, MCP clients sign in through the server; see [Sign-in through the server](#sign-in-through-the-server). |
| `RUST_LOG` | `info` (server), `warn` (CLI) | Log filter. Logs go to stderr. |

Both binaries honour `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY`.

## Development

The repository is a Cargo workspace with three crates:

- `crates/zendesk`: the client library (API client, credentials, OAuth).
- `crates/zendesk-mcp-server`: the `zendesk-mcp-server` binary.
- `crates/zendesk-cli`: the `zendesk` binary.
- `skills/`: the agent skills (SKILL.md bundles) embedded in both binaries.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo run -p zendesk-cli -- api users/me.json
cargo run -p zendesk-mcp-server
```

The tests use mocked HTTP and never contact Zendesk.

`crates/zendesk/src/catalog.json`, the list of API operations the CLI and the MCP server are built on, is generated. Regenerate it with `uv run scripts/gen_catalog.py`: it reads Zendesk's OpenAPI specs and the collections of its public Postman workspace. Add `--specs DIR` to read local copies instead of downloading.

### Releasing

Bump `version` in `Cargo.toml`, commit it together with the updated `Cargo.lock` (the release builds with `--locked`), and merge it to `main`. Once CI passes on `main`, it tags the commit `vX.Y.Z` and starts the release workflow. A tag pushed by hand starts it too, but the tagged commit must be on `main`; the release workflow fails otherwise. The release
workflow builds both binaries (`zendesk-mcp-server` and `zendesk`) for five targets and the
two-arch `ghcr.io/balcsida/zendesk-mcp-server` image, then publishes the draft
release once everything has succeeded. The binaries and the image carry build provenance attestations. The Homebrew formulae in [balcsida/homebrew-tap](https://github.com/balcsida/homebrew-tap) pick up a new release within the hour.

## Troubleshooting

### Safari Authentication Issues (macOS)

If you're using Safari and seeing errors like "Safari cannot open the page because the address is invalid" with URLs containing `SAMLRequest` or `SAMLResponse`, this is a **known issue with Safari's handling of SAML redirects**.

**Solution:** The sign-in tries Chrome first. If you don't have Chrome installed:

1. Install Chrome: `brew install --cask google-chrome`
2. Re-run `zendesk mobile-auth`.

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

1. Check that the URL scheme handler registered successfully (run `RUST_LOG=info zendesk mobile-auth` and look for `Registered macOS URL scheme handler`, or the Linux or Windows equivalent, in the output; set `RUST_LOG=debug` for more detail).
2. Try the manual fallback by opening the sign-in URL the command prints in your browser.
3. Complete the sign-in and copy/paste the `zendesk-support://` URL from the address bar.

## MCP resources

- `zendesk://knowledge-base`: all Help Center articles in the sections you can view, keyed by section ID.

## MCP prompts

### analyze-ticket

Analyze a Zendesk ticket and provide a detailed analysis of the ticket.

- `ticket_id` (required)

### draft-ticket-response

Draft a response to a Zendesk ticket.

- `ticket_id` (required)

## MCP tools

Tools carry MCP annotations (read-only, destructive) so clients can ask for confirmation before changes. The tools that overwrite data, such as the update tools and `execute_macro`, are marked destructive, so clients ask before running them. With `MCP_READ_ONLY=true` the server lists and runs only the read-only tools. The categories below cover tickets, ticket operations, search, users and organizations, views, macros and triggers, account, custom objects, the Help Center, metrics and SLAs, and the API catalog.

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

Includes `requester_name` and `assignee_name` when Zendesk returns the users, and `group_name` for the ticket's group. Also returns `type`, `tags`, `group_id`, `due_at`, `ticket_form_id`, `brand_id`, `custom_status_id`, `problem_id`, `has_incidents`, `is_public`, `external_id`, `followup_ids`, `email_cc_ids`, `follower_ids`, `comment_count`, `channel` (how the ticket was created, e.g. `email`) and `satisfaction_rating` (as Zendesk returns it, or null).

#### get_tickets_bulk

Fetch multiple tickets by IDs, requested from Zendesk in batches of 100.

- `ticket_ids` (array[integer])

#### get_ticket_comments

Retrieve all comments for a ticket. Each comment has `author_id` and, when Zendesk returns the user, `author_name`.

- `ticket_id` (integer)
- `sort_order` (string, optional): `asc` or `desc` (defaults to `asc`)

#### create_ticket_comment

Create a new comment on an existing ticket. Public by default, which emails the requester; set `public` to false for an internal note. Returns `{"message": "Comment created", "comment": {"id", "public", "status"}}`; `id` is the new comment's ID (null if Zendesk's audit did not list it) and `status` appears only when you set one.

- `ticket_id` (integer)
- `comment` (string): Markdown, plain text and HTML are accepted
- `public` (boolean, optional): Whether the comment is public (defaults to true)
- `status` (string, optional): Also set the ticket status in the same update: `new`, `open`, `pending`, `hold`, `solved`, `closed`
- `upload_tokens` (array of strings, optional): Tokens from `upload_attachment` to attach to the comment

#### get_ticket_attachment

Fetch a ticket attachment by its `content_url` and return it as an image. Images only (JPEG, PNG, GIF, WebP, up to 10 MB); other types are rejected.

- `content_url` (string): The `content_url` from `get_ticket_comments`

#### upload_attachment

Upload a file to attach to a ticket. Returns a `token` (valid for 60 minutes) and the `attachment` (`id`, `file_name`, `content_type`, `size`, `content_url`); pass the token to `create_ticket_comment` or `create_ticket` as `upload_tokens`. Needs the `ticket_attachments:write` scope.

- `filename` (string): The name the file gets on the comment; keep the extension matching the content type
- `content_type` (string): MIME type, e.g. `image/png` or `application/pdf`
- `data_base64` (string): The file content, base64-encoded (max 10 MB decoded)

#### create_ticket

Create a new ticket.

- `subject` (string)
- `description` (string)
- `requester_id` (integer, optional)
- `requester` (object, optional): `{name, email}`; creates the end user if needed. Not allowed together with `requester_id`
- `assignee_id` (integer, optional)
- `priority` (string, optional): `low`, `normal`, `high`, `urgent`
- `type` (string, optional): `problem`, `incident`, `question`, `task`
- `tags` (array[string], optional)
- `custom_fields` (array[object], optional): `[{"id": 1, "value": "x"}]`
- `group_id` (integer, optional)
- `ticket_form_id` (integer, optional)
- `brand_id` (integer, optional)
- `problem_id` (integer, optional): Links this incident to a problem ticket
- `via_followup_source_id` (integer, optional): The closed ticket this ticket follows up
- `custom_status_id` (integer, optional)
- `due_at` (string, optional): ISO 8601 datetime
- `external_id` (string, optional)
- `public` (boolean, optional): Whether the description is a public comment; `false` makes it an internal note (defaults to true)
- `email_ccs` (array[string], optional): Email addresses to add as CCs
- `upload_tokens` (array[string], optional): Tokens from `upload_attachment` to attach to the description

The created ticket also reports `group_id`, `ticket_form_id`, `brand_id`, `custom_status_id`, `problem_id`, `due_at` and `external_id`.

#### update_ticket

Update fields on an existing ticket (for example status, priority, assignee).

- `ticket_id` (integer)
- `subject` (string, optional)
- `status` (string, optional): `new`, `open`, `pending`, `hold`, `solved`, `closed`
- `priority` (string, optional): `low`, `normal`, `high`, `urgent`
- `type` (string, optional): `problem`, `incident`, `question`, `task`
- `assignee_id` (integer, optional)
- `requester_id` (integer, optional)
- `tags` (array[string], optional): Replaces ALL tags on the ticket; use `update_ticket_tags` to add or remove individual tags
- `custom_fields` (array[object], optional): `[{"id": 1, "value": "x"}]`
- `due_at` (string, optional): ISO8601 datetime
- `group_id` (integer, optional)
- `custom_status_id` (integer, optional)
- `problem_id` (integer, optional)
- `external_id` (string, optional)
- `email_ccs` (array[object], optional): CCs to add or remove, each `{user_id or user_email, action}` with `action` `put` or `delete`
- `followers` (array[object], optional): Followers to add or remove, each `{user_id, action}` with `action` `put` or `delete`
- `safe_update` (boolean, optional): Avoid overwriting concurrent changes: Zendesk answers 409 if the ticket changed since `updated_stamp`
- `updated_stamp` (string, optional): The ticket's current `updated_at` (from `get_ticket`); required when `safe_update` is true

#### delete_ticket

Soft-delete a ticket. It is recoverable for 30 days with `restore_deleted_ticket` (see `list_deleted_tickets`). Needs permission to delete tickets.

- `ticket_id` (integer)

#### merge_tickets

Merge source tickets into a target ticket. Zendesk merges in a background job; the tool waits up to 20 seconds and reports whether it completed or is still running (use `get_job_status` to check later).

- `target_id` (integer): The ticket to merge into
- `source_ids` (array[integer]): Tickets to merge from; at least one, no duplicates, not the target
- `target_comment` (string, optional): Private unless `target_comment_is_public` is true (defaults to `Merged from related tickets.`)
- `source_comment` (string, optional): Private unless `source_comment_is_public` is true (defaults to `This ticket has been merged.`)
- `target_comment_is_public` (boolean, optional): Whether the comment on the target ticket is public (Zendesk defaults to private)
- `source_comment_is_public` (boolean, optional): Whether the comments on the source tickets are public (Zendesk defaults to private)

#### get_job_status

Get the status of a Zendesk background job, such as a merge or `update_tickets_bulk` that was still running. Returns `id`, `status`, `progress`, `total`, `message`, `url`, `pending` (true while queued or working), `failed_count` (items whose `success` is false) and per-item `results`. When `merge_tickets` or `update_tickets_bulk` cannot poll the job (for example a rate limit), they return the last known status with `pending: true` and a `poll_error`; call `get_job_status` later.

- `job_id` (string): The job status ID returned by `merge_tickets`, `update_tickets_bulk` or another bulk operation

#### get_user_tickets

Get tickets for a user by role. Returns `count`, `tickets` (id, subject, status, priority, created_at, updated_at) and `has_more`.

- `user_id` (integer)
- `role` (string, optional): `requested`, `assigned`, `ccd` or `followed` (defaults to `requested`)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)

#### count_tickets

Count all tickets, or those matching a ZQL query: a cheap way to size a result set before searching. Returns `count` and `query`.

- `query` (string, optional): ZQL query; include `type:ticket`, since a search also counts users and organizations. Defaults to `type:ticket`, which counts every ticket, archived ones included

#### get_ticket_collaborators

List the followers and email CCs of a ticket as `{id, name, email, role}`. Requires the CCs and followers feature; `update_ticket` changes them. If the email CCs request fails (the feature is off), the CCs come from Zendesk's collaborators endpoint instead and `source` is `collaborators` (otherwise `email_ccs`).

- `ticket_id` (integer)

#### search_problem_tickets

Find problem tickets to link incidents to via `update_ticket`'s `problem_id`. Returns `count` and `tickets` (id, subject, status, priority, requester_id, assignee_id, group_id and timestamps).

- `text` (string, optional): Text the subject contains; without it, one page of the 100 most recently updated problems

#### get_organization_tickets

List the tickets of an organization one page at a time. Returns `count`, `tickets` (id, subject, status, priority, description, created_at, updated_at, requester_id, assignee_id, custom_fields, plus `requester_name` and `assignee_name` when Zendesk returns the users), `page`, `per_page` and `has_more`.

- `organization_id` (integer)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)

#### update_ticket_tags

Add and/or remove specific tags on a ticket and return its current tags. Unlike `update_ticket`'s `tags`, which replaces the whole list, this changes only the tags given.

- `ticket_id` (integer)
- `add` (array of strings, optional): Tags to add
- `remove` (array of strings, optional): Tags to remove (no commas). At least one of `add` and `remove` is required.

### Ticket operations

#### list_deleted_tickets

List soft-deleted tickets from the last 30 days: `id`, `subject`, `deleted_at`, `actor` (`id`, `name`) and `previous_state`. Needs permission to view deleted tickets: admins have it, and agents only if their role grants it; otherwise 403. Zendesk limits this to 10 requests per minute. `restore_deleted_ticket` undoes a deletion.

- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)

#### restore_deleted_ticket

Restore a soft-deleted ticket.

- `ticket_id` (integer)

#### list_suspended_tickets

List suspended tickets one page at a time: `id`, `subject`, `cause`, `cause_id`, `author`, `recipient`, `created_at`, `ticket_id`, `channel` and optionally `content`. Returns `has_more` and `after_cursor`. The content is untrusted (mostly spam). Needs an admin or unrestricted agent.

- `page_size` (integer, optional): Max 100 (defaults to 25)
- `after_cursor` (string, optional): The `after_cursor` of the previous page
- `include_content` (boolean, optional): Include the flagged content (defaults to false)

#### recover_suspended_ticket

Recover a suspended ticket. The new ticket's requester is the authenticated user, not the original sender. If Zendesk cannot recover it, the error says why.

- `suspended_ticket_id` (integer): The ID from `list_suspended_tickets`

#### make_comment_private

Make a public comment private. One-way: a private comment cannot be made public again.

- `ticket_id` (integer)
- `comment_id` (integer)

#### redact_comment_text

Permanently replace a string in a comment with block characters, for PII such as card numbers. Irreversible; does not work on closed tickets. Returns the comment.

- `ticket_id` (integer)
- `comment_id` (integer)
- `text` (string): The exact string to redact

#### mark_ticket_as_spam

Mark a ticket as spam and suspend its requester. No tool here lifts that suspension.

- `ticket_id` (integer)

#### update_tickets_bulk

Apply the same change to up to 100 tickets. Waits up to 30 seconds for Zendesk's background job and returns its status under `job`; if it is still `pending`, call `get_job_status` with the returned `id`. At most 30 jobs may be queued at once. The message says when some tickets failed.

- `ticket_ids` (array of integers): 1 to 100 ticket IDs
- `status` (string, optional): `new`, `open`, `pending`, `hold`, `solved`, `closed`
- `priority` (string, optional): `low`, `normal`, `high`, `urgent`
- `type` (string, optional): `problem`, `incident`, `question`, `task`
- `assignee_id` (integer, optional)
- `group_id` (integer, optional)
- `custom_status_id` (integer, optional)
- `tags` (array of strings, optional): Replaces all tags on every ticket
- `additional_tags` (array of strings, optional): Tags to add, keeping the existing ones
- `remove_tags` (array of strings, optional): Tags to remove
- `custom_fields` (array of objects, optional): `[{"id": 1, "value": "x"}]`

At least one field is required.

### Search

#### search

Search with Zendesk Query Language (ZQL) across tickets, users, organizations and groups, one page at a time. Zendesk returns at most 1,000 results per query. Examples: `type:ticket status:open priority:urgent`, `type:ticket assignee:me`, `type:user email:john@example.com`.

- `query` (string): ZQL query
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)
- `sort_by` (string, optional): `updated_at`, `created_at`, `priority`, `status`, `ticket_type` (omitted by default, so Zendesk sorts by relevance)
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

Also returns `user_fields` (custom user fields, or null), `notes`, `details`, `external_id`, `locale`, `last_login_at`, `ticket_restriction`, `verified`, `default_group_id` and `alias`.

#### get_current_user

Get the currently authenticated user. No inputs. Also returns `custom_role_id`, `ticket_restriction`, `restricted_agent`, `shared_agent`, `locale`, `active` and `verified`.

#### search_users

Search users by name, email or other properties, or by exact external_id. Give at least one of the two. Returns `count`, `users` (including `external_id` and `suspended`) and `has_more`: the first page of up to 100 matches, so narrow the query for more.

- `query` (string, optional): Name, email, notes, phone or another user property
- `external_id` (string, optional): Exact external_id (not a search expression)

#### get_organization

Get an organization by ID. Also returns `organization_fields` (custom organization fields, or null), `external_id`, `shared_tickets` and `shared_comments`.

- `organization_id` (integer)

#### search_organizations

Search organizations whose name starts with the query. Returns `count`, `organizations` (id, name, domain_names) and `has_more`: the first page only, so narrow the query for more.

- `query` (string)

#### get_users_bulk

Get many users by ID in one call, to resolve many requester or assignee IDs at once. Returns `id`, `name`, `email`, `role`, `organization_id`, `external_id`, `active`, `suspended`, `time_zone` and `locale`.

- `user_ids` (array of integers): Fetched in requests of 100

#### get_user_identities

List the emails, phone numbers and other identities of a user with their verification and deliverability state. Returns `count` and `identities` (`id`, `type`, `value`, `primary`, `verified`, `verification_method`, `deliverable_state`, `undeliverable_count`).

- `user_id` (integer)

#### get_user_organizations

List the organizations a user belongs to as `{id, name, default, view_tickets}`. A user can belong to several; `default` marks the primary one.

- `user_id` (integer)

#### create_or_update_user

Create a user, or update the existing one that matches the email or external_id. A new user is an end user; this tool never changes the role. Returns the user as `get_user` does.

- `name` (string)
- `email` (string)
- `external_id` (string, optional)
- `phone` (string, optional)
- `organization_id` (integer, optional)
- `tags` (array of strings, optional)
- `notes` (string, optional)
- `details` (string, optional)
- `user_fields` (object, optional): Custom user field values as `{"field_key": value}`
- `locale` (string, optional): For example `en-US`
- `time_zone` (string, optional): For example `Europe/Budapest`

#### update_user

Update a user's profile. Role, suspension and password changes are deliberately not supported. A new email is added as a secondary identity (Zendesk behaviour), not made primary. At least one field is required.

- `user_id` (integer)
- `name`, `email`, `phone`, `external_id`, `notes`, `details`, `alias`, `locale`, `time_zone` (string, optional)
- `organization_id` (integer, optional)
- `tags` (array of strings, optional): Replaces the whole tag list
- `user_fields` (object, optional): Custom user field values as `{"field_key": value}`

#### list_organization_users

List the users of an organization as `{id, name, email, role, active, suspended, external_id, phone}`.

- `organization_id` (integer)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Defaults to 25, max 100

#### update_organization

Update an organization. Agents without extra permission can usually change only `notes`. At least one field is required. Returns the organization as `get_organization` does.

- `organization_id` (integer)
- `name`, `details`, `notes`, `external_id` (string, optional)
- `domain_names` (array of strings, optional): Replaces the whole list
- `group_id` (integer, optional)
- `tags` (array of strings, optional): Replaces the whole tag list
- `organization_fields` (object, optional): Custom organization field values as `{"field_key": value}`
- `shared_tickets` (boolean, optional)
- `shared_comments` (boolean, optional)

### Views, fields, forms, groups, macros

#### list_views

List all available views (saved ticket queues). No inputs.

#### execute_view

Execute a view and return its tickets.

- `view_id` (integer)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)
- `sort_by` (string, optional): Column to sort by, e.g. `created_at` or `updated_at`; subject and submitter columns are not supported
- `sort_order` (string, optional): `asc` or `desc`

#### list_ticket_fields

List all ticket fields (system and custom) with their types and valid options. No inputs.

#### list_ticket_forms

List all ticket forms and their associated field IDs. No inputs.

#### list_custom_statuses

List custom ticket statuses with `id`, `status_category`, `agent_label`, `end_user_label`, `description`, `active` and `default`. Maps the `custom_status_id` on tickets to labels; a ticket's `status` is only the category.

- `active_only` (boolean, optional): Only active statuses (defaults to true)

#### list_groups

List assignable groups for ticket routing. No inputs.

#### get_group_members

List the members of a group as `{id, name, email, role, active, suspended}`. Group IDs come from `list_groups`.

- `group_id` (integer)

#### get_view_counts

Get ticket counts for up to 20 views at once, as `{view_id, value, pretty, fresh}`. `value` is null while Zendesk is still computing it, so retry later. Limited to 6 calls per minute.

- `view_ids` (array of integers): 1 to 20 view IDs, from `list_views`

#### get_macro

Get one macro with its `actions` (`{field, value}`). Shows exactly what a macro changes before `apply_macro` (preview) or `execute_macro` (save).

- `macro_id` (integer)

#### search_macros

Find macros by title, with their actions. Returns one page of up to 100; `list_macros` returns all of them.

- `query` (string): Text to match against macro titles

#### list_macros

List available macros (canned responses and actions). Returns every page.

- `active_only` (boolean, optional): Defaults to true

#### apply_macro

PREVIEW ONLY, saves nothing. Use `execute_macro` to apply the macro. Returns `ticket_changes` (the previewed ticket) and the macro's `comment`.

- `ticket_id` (integer)
- `macro_id` (integer)

#### execute_macro

Apply a macro to a ticket for real. Zendesk's preview returns the whole ticket, so only the fields the macro changes (compared with the current ticket) are saved, plus the macro's comment (private unless the macro makes it public; a public one emails the requester). The update fails with a 409 if the ticket changed in the meantime, and the macro is recorded in the ticket audit. `apply_macro` only previews. Returns the updated ticket.

- `ticket_id` (integer)
- `macro_id` (integer)

### Triggers

#### list_triggers

List ticket triggers, the business rules that change tickets automatically, as `{id, title, active, category_id, position, description, updated_at}`. Use with `get_trigger` to explain changes seen in `get_ticket_audits`. Returns every page.

- `active_only` (boolean, optional): Only active triggers (defaults to true)
- `category_id` (string, optional): Only triggers in this trigger category

#### get_trigger

Get one ticket trigger with its `conditions` (`{all, any}`) and `actions`.

- `trigger_id` (integer)

### Account

#### list_brands

List the brands of the account as `{id, name, subdomain, brand_url, default, active, has_help_center, help_center_state, ticket_form_ids}`; it maps `brand_id` on tickets to names. Agents may see only their own brands. No inputs.

#### get_account_settings

Get feature flags and defaults: `active_features` (such as `on_hold_status`, `business_hours`, `allow_ccs`), `brands`, `tickets`, `agents`, `localization`, `limits`, `routing` and `users`, each as Zendesk returns it (null when absent). They tell you which features are on. For custom objects, try `list_custom_objects` and treat a 403 or 404 as "not available". No inputs.

### Custom objects

#### list_custom_objects

List the custom objects of the account as `{key, title, title_pluralized, description, created_at, updated_at}`. Custom objects are account-defined record types (products, orders, assets) linked to tickets through lookup fields. If the account has none this fails with 403 or 404; treat that as "not available". No inputs.

#### get_custom_object

Get a custom object and its fields. Returns `object` and `fields` (`key`, `title`, `type`, `required`, `description`, `custom_field_options` as `[{name, value}]` or null, `relationship_target_type`).

- `key` (string): The custom object key from `list_custom_objects`

#### search_custom_object_records

List or search the records of a custom object, one cursor page at a time. With neither `query` nor `filter` it lists the records. Returns `records` (`id`, `name`, `external_id`, `custom_object_fields`, `created_at`, `updated_at`), `count` (when Zendesk sends it), `has_more` and `after_cursor`.

Non-admin agents may get 403 when listing or text-searching objects with cascading permissions; use `filter` instead.

- `key` (string): The custom object key
- `query` (string, optional): Text search; it covers text fields only
- `filter` (object, optional): Zendesk filter for other field types, such as `{"custom_object_fields.status": {"$eq": "open"}}` or `{"$and": [...]}`
- `sort` (string, optional): `id`, `updated_at` (list) or `name`, `created_at`, `updated_at` (search), with a leading `-` for descending
- `page_size` (integer, optional): Defaults to 25, max 100
- `after_cursor` (string, optional): The `after_cursor` of the previous page

#### get_custom_object_record

Get one custom object record.

- `key` (string): The custom object key
- `record_id` (string)

### Help Center

#### search_articles

Search Help Center articles by text and/or filters. Give at least one of `query`, `category`, `section` or `label_names`. Zendesk returns at most 1,000 results per search.

- `query` (string, optional)
- `locale` (string, optional): For example `en-us`, `fr`, `es`
- `category` (integer, optional): Only articles in this category (`category_id` is accepted as an alias)
- `section` (integer, optional): Only articles in this section (`section_id` is accepted as an alias)
- `label_names` (array of strings, optional): Only articles with these labels
- `sort_by` (string, optional): `created_at` or `updated_at` (defaults to relevance)
- `sort_order` (string, optional): `asc` or `desc` (defaults to `desc`)
- `created_after`, `created_before`, `updated_after`, `updated_before` (string, optional): Dates as `YYYY-MM-DD`
- `per_page` (integer, optional): Max 100 (defaults to 25)
- `page` (integer, optional): Page number (defaults to 1)

Returns matching articles with id, title, body, snippet (matching text in `<em>` tags), author_id, section_id, locale, html_url, timestamps, draft and promoted status, label_names and vote_sum, plus `query`, `page`, `per_page`, `count`, `total_count` and `has_more`.

#### list_articles

List Help Center articles one page at a time, without their bodies. Use `get_article` for an article's full text.

- `section_id` (integer, optional): Only list the articles in this section
- `locale` (string, optional): For example `en-us`, `fr`, `es` (defaults to the help center's default locale)
- `per_page` (integer, optional): Max 100 (defaults to 25)
- `page` (integer, optional): Page number (defaults to 1)

Returns articles with id, title, section_id, html_url, draft status and updated_at, plus `total_count` and `has_more`.

#### get_article

Get a Help Center article by ID.

- `article_id` (integer)
- `locale` (string, optional): For example `en-us`, `fr`, `es`

Returns the article with id, title, body, author_id, section_id, locale, html_url, timestamps, draft and promoted status, position, voting statistics and label names.

#### list_categories

List Help Center categories (the top level of the knowledge base) as `{id, name, description, locale, position, html_url, updated_at}`. Returns every page.

- `locale` (string, optional): For example `en-us`, `fr`, `es` (defaults to the help center's default locale)

#### list_sections

List Help Center sections as `{id, name, description, category_id, parent_section_id, locale, position, html_url, updated_at}`. Returns every page.

- `category_id` (integer, optional): Only list the sections in this category
- `locale` (string, optional): For example `en-us`, `fr`, `es` (defaults to the help center's default locale)

#### list_article_translations

List every locale version of an article with its draft and outdated state, as `{id, locale, title, draft, outdated, html_url, updated_at}`. Bodies are not included: use `get_article` with a `locale` for the text.

- `article_id` (integer)

#### create_article

Create a Help Center article. The Markdown body is converted to HTML without sanitising; Zendesk sanitises on its side. It is a draft unless `draft` is false. Publish later with `update_article` and `draft` false. Returns the article in the `get_article` shape.

- `section_id` (integer)
- `title` (string)
- `body` (string): Markdown or HTML
- `locale` (string): For example `en-us`; must be enabled for the help center
- `permission_group_id` (integer, optional): Who can edit and publish the article (defaults to the admins group)
- `user_segment_id` (integer, optional): Who can view the article (omit to make it visible to everyone)
- `label_names` (array of strings, optional)
- `draft` (boolean, optional): Defaults to true
- `notify_subscribers` (boolean, optional): Defaults to false

#### update_article

Edit the text of one locale and/or the article's metadata. Markdown is converted to HTML without sanitising; Zendesk sanitises on its side. Set `draft` to false to publish, true to unpublish. Returns the updated article in the `get_article` shape. It may make two writes (the translation, then the metadata), so if the second fails the first stays applied.

- `article_id` (integer)
- `locale` (string, optional): Required with `title`, `body` or `draft`
- `title` (string, optional)
- `body` (string, optional): Markdown or HTML
- `draft` (boolean, optional)
- `section_id` (integer, optional): Move the article to this section
- `promoted` (boolean, optional)
- `position` (integer, optional)
- `label_names` (array of strings, optional): Replaces the article's labels
- `user_segment_id` (integer, optional)
- `permission_group_id` (integer, optional)

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

Find tickets that breached SLA within a time period. Admin-only. It reads Zendesk's incremental export, limited to 10 requests per minute, and scans every metric event in the window, so keep `days_back` small.

- `days_back` (integer, optional): Defaults to 7
- `metric` (string, optional): `reply_time`, `first_reply_time`, `agent_work_time`, `requester_wait_time` or `periodic_update_time`

#### get_sla_policies

Get all SLA policies with their metric targets per priority level. Admin-only: agents get 403. No inputs.

#### list_satisfaction_ratings

List CSAT satisfaction ratings with `score`, `comment`, `reason`, `reason_id`, ticket, requester, assignee and group IDs and timestamps. Admin-only: agents get 403.

- `score` (string, optional): `offered`, `unoffered`, `received`, `received_with_comment`, `received_without_comment`, `good`, `good_with_comment`, `good_without_comment`, `bad`, `bad_with_comment` or `bad_without_comment`
- `days_back` (integer, optional): Only ratings from the last N days (defaults to 30)
- `page` (integer, optional): Defaults to 1
- `per_page` (integer, optional): Max 100 (defaults to 25)

### API catalog

These four tools reach the full Zendesk API (Support, Help Center, Talk, webhooks, chat and more) without one tool per endpoint. Search for an operation, read its description, then call it.

Operations that return or change credentials (API and OAuth tokens, OAuth client and webhook signing secrets, Help Center JWTs, passwords, SSO shared secrets, targets, ZIS connections and inbound webhooks) are hidden and refused, so a secret never lands in the model's context through a read a client may approve automatically. So are the writes that administer the account: users, custom roles, user identities and passwords, webhooks, account settings, themes, ticket imports, sessions, deletion schedules, global clients, reseller operations, and bulk or permanent deletes. Reads in those families still run. The CLI runs everything the server refuses.

#### search_api_operations

Search the full Zendesk API by keywords. Use it when no dedicated tool fits.

- `query` (string): Keywords, e.g. `list ticket comments`; an operation must match every word
- `limit` (integer, optional): Operations to return, max 100 (defaults to 20)

Returns `total` and the first `limit` operations, each with `id`, `method`, `path` and `summary`. Operations the server refuses (credentials, account administration) are not listed.

#### get_api_operation

Describe one API operation.

- `operation_id` (string): An id from `search_api_operations`, e.g. `ShowTicket` (case-insensitive)

Returns the operation with its group, path and query parameters, an example request body and a description, plus `tool`: `call_api_read` or `call_api_write`, whichever runs it. Refuses credential and account-administration operations, which the CLI runs.

#### call_api_read

Run a read-only (`GET`) operation. Refuses other operations and points to `call_api_write`, and refuses credential operations. With `MCP_READ_ONLY=true` it is the only catalog tool that runs.

- `operation_id` (string)
- `params` (object, optional): Path and query parameters by name, e.g. `{"ticket_id": 1, "include": "users"}`. An array is sent comma-separated, or as repeated pairs when the name ends in `[]` or the API wants the parameter repeated. An object is sent as `name[key]=value`, so `{"page": {"size": 10}}` becomes `page[size]=10`
- `fields` (array of strings, optional): Keep only these keys of each object in the response, to save tokens. It applies to the elements of top-level arrays and to top-level objects other than `meta` and `links`; other values such as `next_page` and `count` stay. An empty list keeps everything

Returns the response as compact JSON. Page through large listings with the operation's paging parameters, such as `per_page` or `page[size]`.

#### call_api_write

Run an operation that writes (`POST`, `PUT`, `PATCH` or `DELETE`). It may create, change or delete Zendesk data and may notify customers, so call `get_api_operation` first to see the parameters and an example body. Refuses reads and points to `call_api_read`, and refuses credential operations and the account-administration writes, which the CLI runs. Unavailable with `MCP_READ_ONLY=true`.

- `operation_id` (string)
- `params` (object, optional): Path and query parameters, as for `call_api_read`
- `body` (object, optional): The JSON request body

Returns the response as compact JSON, or `{"message": "<METHOD> <path> succeeded"}` when Zendesk answers with an empty body.
