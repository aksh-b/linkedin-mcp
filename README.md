<p align="center"><img src="assets/logo-wordmark.svg" alt="linkedin-mcp" width="420"></p>

# linkedin-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server in Rust that lets AI assistants read your LinkedIn profile, publish posts, and comment on and like posts. It is built on the official Rust SDK [`rmcp`](https://crates.io/crates/rmcp) and served over **Streamable HTTP**.

## Tools

| Tool | What it does | LinkedIn API |
|---|---|---|
| `get_profile` | Returns name, email, picture, and person URN (read-only) | `GET /v2/userinfo` |
| `create_text_post` | Publishes a text post (`PUBLIC` or `CONNECTIONS`) | `POST /v2/ugcPosts` |
| `share_article` | Shares a link with commentary, plus an optional title and description | `POST /v2/ugcPosts` (`ARTICLE`) |
| `create_media_post` | Publishes a post with an image (up to 20 MB) or video (up to 200 MB), given as `media_url` or `media_base64` | `POST /v2/assets?action=registerUpload`, upload, then `POST /v2/ugcPosts` (`IMAGE`/`VIDEO`) |
| `comment_on_post` | Comments on a post (up to 1,250 characters) | `POST /v2/socialActions/{post}/comments` |
| `like_post` | Likes a post | `POST /v2/socialActions/{post}/likes` |
| `delete_post` | Permanently deletes one of your own posts | `DELETE /v2/ugcPosts/{post}` |

Every tool except `get_profile` acts **immediately**. They are annotated as non-read-only (and `delete_post` as destructive), so MCP clients ask you to approve each call.

Tools that take a `post` accept a post URN (`urn:li:share:…`, `urn:li:ugcPost:…` or `urn:li:activity:…`) or a linkedin.com post URL. `delete_post` needs the `share` or `ugcPost` URN, which the post tools return.

`media_url` must be a public `https` URL. The server won't follow redirects or fetch private or loopback addresses.

## 1. Get a LinkedIn access token

1. Create an app at <https://www.linkedin.com/developers/apps>.
2. On the **Products** tab, add **Sign In with LinkedIn using OpenID Connect** (scopes `openid`, `profile`, `email`) and **Share on LinkedIn** (scope `w_member_social`). Both are self-serve.
3. Generate a member token with the scopes `openid profile email w_member_social`. The quickest way is the [OAuth token generator tool](https://www.linkedin.com/developers/tools/oauth/token-generator). Tokens last about 60 days.

References: [Sign In with LinkedIn using OpenID Connect](https://learn.microsoft.com/linkedin/consumer/integrations/self-serve/sign-in-with-linkedin-v2) and [Share on LinkedIn](https://learn.microsoft.com/linkedin/consumer/integrations/self-serve/share-on-linkedin). Rate limit: 150 requests per member per day.

## 2. Run

```bash
cp .env.example .env   # then fill in LINKEDIN_ACCESS_TOKEN and MCP_API_KEY
set -a; . ./.env; set +a
cargo run --release
```

The server listens on `http://127.0.0.1:8080`:

- `GET /healthz` returns `ok` and needs no auth.
- `/mcp` is the MCP Streamable HTTP endpoint. It requires `Authorization: Bearer $MCP_API_KEY`.

> **Windows:** building with the MSVC toolchain needs Visual Studio Build Tools (the "Desktop development with C++" workload). You can also build inside WSL.

## 3. Connect a client

**Claude Code**

```bash
claude mcp add --transport http linkedin http://127.0.0.1:8080/mcp --header "Authorization: Bearer <MCP_API_KEY>"
```

**VS Code** (`.vscode/mcp.json`)

```json
{
  "servers": {
    "linkedin": {
      "type": "http",
      "url": "http://127.0.0.1:8080/mcp",
      "headers": { "Authorization": "Bearer ${input:linkedin-mcp-key}" }
    }
  },
  "inputs": [
    { "id": "linkedin-mcp-key", "type": "promptString", "description": "MCP_API_KEY", "password": true }
  ]
}
```

**MCP Inspector**

```bash
npx @modelcontextprotocol/inspector --cli http://127.0.0.1:8080/mcp --transport http --header "Authorization: Bearer <MCP_API_KEY>" --method tools/list
```

## Configuration

| Variable | Required | Default | Notes |
|---|---|---|---|
| `LINKEDIN_ACCESS_TOKEN` | yes | | Member token (see step 1) |
| `MCP_API_KEY` | yes | | At least 16 characters |
| `BIND_ADDR` | no | `127.0.0.1:8080` | Use `0.0.0.0:8080` in a container |
| `ALLOWED_HOSTS` | no | loopback only | Comma-separated `Host` values to accept when serving under a domain (DNS-rebinding protection) |
| `RUST_LOG` | no | `info` | Logs go to stderr; tokens are never logged |

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

## Roadmap

- A built-in OAuth 2.0 authorization-code flow with refresh, instead of a pasted token
- Migrating to the versioned `/rest/posts` API once it is available to self-serve apps
