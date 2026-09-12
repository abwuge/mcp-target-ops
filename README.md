# mcp-target-ops

`mcp-target-ops` is a Rust MCP server for running commands, editing files, and
opening terminal sessions on local or SSH targets. The same tools work across
all targets; access is controlled per target in a TOML config.

It supports stdio for local MCP clients and HTTP for remote MCP clients. The
HTTP server also provides OAuth and a limited REST API for GPT Actions.

## Features

- Local and SSH targets with IDs such as `local` and `ssh:dev`
- Persistent OpenSSH workers and PTY terminal sessions
- Command execution with time and output limits
- Background command jobs with polling, incremental output, cancellation, and optional timeouts
- File range reads, literal search, atomic writes, exact edits, single- and multi-file unified patches, moves, chmod, directory creation, and ChatGPT file import/export
- ChatGPT Apps-compatible tool annotations plus an MCP App diff/summary UI for file mutations
- Optional SHA-256 compare-and-swap checks for file edits, single-file patches, and replacements
- Downstream Streamable HTTP MCP gateway with configured-server allowlisting and file-owned credentials
- Secret references that resolve file-backed values directly into command environments without placing the value in MCP arguments
- Per-target permissions and allowed filesystem roots
- HTTP bearer authentication
- OAuth authorization code with PKCE, dynamic client registration, and
  rotating refresh tokens
- Generated OpenAPI schema for GPT Actions

## Install

Build from source with the Rust toolchain pinned by `rust-toolchain.toml`:

```bash
cargo build --release
```

The binary is written to `target/release/mcp-target-ops`.

SSH targets use the system OpenSSH client. Remote hosts need a POSIX shell and
basic tools such as `cat`, `mktemp`, `mv`, and `stat`.

## Configure

Start from the example:

```bash
mkdir -p ~/.config/mcp-target-ops
cp examples/config.toml ~/.config/mcp-target-ops/config.toml
```

The config has one `[server]` section and one section per target. A section
named `[targets.dev]` creates the target ID `ssh:dev`.

```toml
[server]
default_target = "ssh:dev"

[targets.dev]
kind = "ssh"
host = "dev.example.com"
user = "ubuntu"
identity_file = "/home/me/.ssh/id_ed25519"
extra_args = ["-o", "BatchMode=yes"]

[targets.dev.policy]
allow_exec = true
allow_terminal = true
allow_file_read = true
allow_file_write = false
allow_select_active = true
require_explicit_target_for_write = true
allowed_roots = ["/srv/app", "/var/log"]
```

The full set of options is documented in
[`examples/config.toml`](examples/config.toml). If `--config` is omitted, the
server checks `MCP_TARGET_OPS_CONFIG` and then
`~/.config/mcp-target-ops/config.toml`.

### Downstream MCP servers

Target Ops can act as a small gateway to explicitly configured Streamable HTTP
MCP servers. Prefer a dedicated mode-`0600` file owned by Target Ops for each
server so endpoint credentials do not need to appear in the main config or in
MCP tool arguments:

```toml
# ~/.config/mcp-target-ops/config.toml
[mcp_servers.memory]
enabled = true
config_file = "/home/me/.config/mcp-target-ops/mcp/memory.toml"
timeout_ms = 20000
max_response_bytes = 1048576
```

```toml
# ~/.config/mcp-target-ops/mcp/memory.toml (chmod 0600)
url = "https://memory.example.com/mcp"

[headers]
Authorization = "Bearer replace-me"
```

The gateway disables HTTP redirects so credentials are never forwarded to a
redirect target. `mcp_server_list` exposes only safe metadata, while
`mcp_tools_list` and `mcp_tool_call` initialize and use the configured server.
The gateway does not provide an arbitrary URL/HTTP request tool.

For advanced setups, inline URLs and file-backed secret references are also
supported in `[mcp_servers.<name>]`; see `examples/config.toml`.

### Policy

Targets and operations are denied unless enabled. File access must stay within
`allowed_roots`, and file edits require an explicit target by default. The
`local` target is disabled unless the config enables it.

OpenSSH handles host-key verification and SSH configuration. Use a dedicated,
low-privilege account for remote automation.

## Run

Stdio is the default transport:

```bash
./target/release/mcp-target-ops --config ~/.config/mcp-target-ops/config.toml
```

For HTTP:

```bash
./target/release/mcp-target-ops \
  --config ~/.config/mcp-target-ops/config.toml \
  --http 127.0.0.1:8765
```

Bind to localhost and place the service behind an HTTPS reverse proxy for
remote access. Do not expose it without authentication and restrictive target
policies.

## Targets and tools

Most tools accept an optional `target`. If it is omitted, the selected target
or `server.default_target` is used. `target_select` changes the active target
for the server process.

Terminal sessions are bound to their target when opened. Later terminal calls
use the returned `terminal_id`.

| Area | Tools |
| --- | --- |
| Server | `server_info` |
| Targets | `target_list`, `target_current`, `target_select`, `target_connect`, `target_disconnect` |
| MCP gateway | `mcp_server_list`, `mcp_tools_list`, `mcp_tool_call` |
| Commands | `exec`, `exec_start`, `job_poll`, `job_output`, `job_cancel` |
| Files | `file_read`, `file_list`, `file_find`, `file_edit`, `file_write`, `file_import`, `file_export`, `file_patch`, `file_move`, `file_chmod`, `directory_create` |
| Terminals | `terminal_open`, `terminal_send`, `terminal_read`, `terminal_resize`, `terminal_close` |

Example command call:

```json
{
  "name": "exec",
  "arguments": {
    "target": "ssh:dev",
    "command": "uname -a"
  }
}
```

`file_edit` applies exact replacements and can reject a stale write when the
caller supplies the expected SHA-256. Atomic edits preserve the existing Unix
mode, so editing an executable script does not silently remove its executable
bit. `file_read` can return a 1-based line range, and `file_find` returns bounded
literal matches with line context. `file_patch` keeps its existing single-file
mode; when the patch contains multiple `---`/`+++` sections, `path` is treated
as a base directory. All affected files are validated before writing, and if a
later write fails, earlier writes are rolled back. Multi-file create/delete
patches are intentionally not supported yet.

`exec_start` is intended for builds and other long-running non-interactive
commands. It returns immediately with a job id. Use `job_poll` for status,
`job_output` with stdout/stderr sequence cursors for incremental output, and
`job_cancel` to stop a running job. SSH jobs use dedicated OpenSSH processes so
they do not monopolize a target's persistent worker.

Both `exec` and `exec_start` accept `secret_env`. Each environment variable maps
to a file-backed reference on the selected target. Text files can be injected
directly; TOML and JSON references can select a dot-separated scalar key. The
source path must pass the same file-read policy and `allowed_roots` checks as
`file_read`. The secret value is resolved inside Target Ops and is not present
in the MCP request. A command can still expose it if the command itself prints
its environment or sends the value elsewhere.

Example using a TOML value without copying it into the MCP request:

```json
{
  "name": "exec",
  "arguments": {
    "target": "local",
    "command": "./local-client",
    "secret_env": {
      "SERVICE_AUTH": {
        "path": "/home/me/service.toml",
        "format": "toml",
        "key": "client.headers.Authorization"
      }
    }
  }
}
```

`terminal_read` is incremental and uses sequence numbers to resume from the
last read position. `terminal_resize` changes the live PTY size.

### ChatGPT Apps file integration

The MCP tool descriptors include standard tool annotations and ChatGPT Apps
metadata. File-changing tools bind to the MCP App resource
`ui://target-ops/file-change/v1.html`, served as
`text/html;profile=mcp-app`. The widget renders the affected target and paths,
write status, hashes, and a unified diff when one is available. It has no
external network or asset dependencies.

`file_import` is the attachment-to-target path. Its `file` argument is exposed
as a binary file parameter and is tagged with `_meta["openai/fileParams"]`.
Connector runtimes may rewrite that argument to a mounted local path or a file
reference containing an HTTPS download URL; both forms are accepted. Imported
bytes are then written through the normal atomic `file_write` path, so target
policy, allowed roots, overwrite rules, modes, and optional SHA-256 CAS guards
still apply. HTTPS downloads do not follow redirects. Transfers default to a
25 MiB limit and may be raised per call up to 100 MiB.

`file_export` is the target-to-chat path. It reads under the normal target read
policy and returns the bytes as a standard MCP embedded resource while keeping
file name, MIME type, size, and SHA-256 in `structuredContent`. Its descriptor
also advertises `openai/fileResultPaths` and `openai/fileOutputs` compatibility
metadata. MIME type is inferred from common filename extensions unless the
caller supplies an override. Export uses the same 25 MiB default and 100 MiB
hard limit.

After changing tool descriptors or App resource metadata, refresh or reconnect
the ChatGPT app so the web client reloads `tools/list` and the resource
capabilities.

## HTTP authentication

The root document, health check, OpenAPI schema, favicon, and enabled OAuth
endpoints are public. `/mcp` and `/actions/v1/*` require a valid bearer token
when authentication is configured.

### Static bearer token

Set `server.http_bearer_token` or, preferably, provide the token through the
environment:

```bash
MCP_TARGET_OPS_HTTP_TOKEN='replace-me' \
  ./target/release/mcp-target-ops --http 127.0.0.1:8765
```

Clients send it as:

```text
Authorization: Bearer <token>
```

### OAuth for remote MCP and ChatGPT

Enable OAuth and set the externally reachable HTTPS origin:

```toml
[server]
oauth_enabled = true
public_base_url = "https://mcp.example.com"
oauth_scopes = ["mcp:tools"]
oauth_authorization_password = "replace-me"
oauth_allow_dynamic_client_registration = true
oauth_authorization_code_ttl_secs = 600
oauth_access_token_ttl_secs = 3600
oauth_refresh_token_ttl_secs = 2592000
# Optional; defaults to ~/.config/mcp-target-ops/oauth-state.json
oauth_state_file = "/home/me/.config/mcp-target-ops/oauth-state.json"
```

Secrets and deployment-specific values can be supplied with environment
variables:

| Variable | Purpose |
| --- | --- |
| `MCP_TARGET_OPS_HTTP_TOKEN` | Static bearer token |
| `MCP_TARGET_OPS_OAUTH` | Enable OAuth with `1`, `true`, `yes`, or `on` |
| `MCP_TARGET_OPS_PUBLIC_BASE_URL` | External HTTPS origin |
| `MCP_TARGET_OPS_OAUTH_PASSWORD` | Password required by the authorization page |
| `MCP_TARGET_OPS_OAUTH_SCOPES` | Space-separated scopes |

OAuth clients discover the service through:

```text
/.well-known/oauth-protected-resource
/.well-known/oauth-authorization-server
```

The authorization server supports public clients, PKCE S256, dynamic client
registration, and refresh-token rotation. Access tokens last one hour and
refresh tokens last 30 days by default. A refresh returns a replacement refresh
token; the previous one cannot be reused.

OAuth clients, access tokens, and rotating refresh tokens are persisted by
default in `~/.config/mcp-target-ops/oauth-state.json`. The state file is written
atomically with mode `0600`; on Unix, startup rejects a state file readable by
group or others. Authorization codes remain memory-only and short-lived.
Therefore an already-authorized ChatGPT client can refresh normally after a
service restart without asking for the authorization password again. Set
`server.oauth_state_file` (or `MCP_TARGET_OPS_OAUTH_STATE_FILE`) to choose a
different path; set it to an explicit deployment-controlled path when the
service account has a nonstandard home directory.

Deleting the state file intentionally revokes persisted clients and tokens and
will require authorization again. Use an external identity provider when more
advanced revocation, multi-user identity, or policy is required.

## GPT Actions

HTTP mode serves an OpenAPI 3.1 schema at `/openapi.json`. Configure a GPT
Action with bearer authentication using the same value as
`MCP_TARGET_OPS_HTTP_TOKEN`, then import:

```text
https://mcp.example.com/openapi.json
```

The Actions API exposes only these operations:

| Operation | Endpoint |
| --- | --- |
| `listTargets` | `GET /actions/v1/targets` |
| `executeCommand` | `POST /actions/v1/commands/execute` |
| `readFile` | `POST /actions/v1/files/read` |
| `listDirectory` | `POST /actions/v1/directories/list` |
| `previewFileEdits` | `POST /actions/v1/files/edits/preview` |
| `applyFileEdits` | `POST /actions/v1/files/edits/apply` |

Every Actions operation requires an explicit target. The facade applies tighter
request, response, file, and command limits than the MCP tools.
Bearer authentication represents one shared service identity; it does not
provide per-user authorization.

## HTTP endpoints

| Endpoint | Access |
| --- | --- |
| `GET /` | Public server summary |
| `GET /health` | Public health check |
| `POST /mcp` | MCP JSON-RPC |
| `DELETE /mcp` | Acknowledge an MCP session close request |
| `GET /openapi.json` | Public GPT Actions schema |
| `/actions/v1/*` | GPT Actions API |
| `GET /favicon.ico` | Application icon |
| `/.well-known/oauth-*` | OAuth metadata when OAuth is enabled |
| `/oauth/authorize` | Authorization page when OAuth is enabled |
| `POST /oauth/token` | Token and refresh exchange |
| `POST /oauth/register` | Dynamic client registration |

Quick health check:

```bash
curl -fsS http://127.0.0.1:8765/health
```

## Current limitations

- Active-target selection and OAuth state are process-wide and in memory.
- SSH operations depend on the local OpenSSH client.

## Development

```bash
cargo fmt --all -- --check
cargo test --all
cargo clippy --all-targets -- -D warnings
```

The main source areas are:

```text
src/core       configuration, state, targets, OAuth, and policy
src/protocol   MCP, HTTP, OAuth routes, and GPT Actions
src/tooling    command, file, and terminal tools
src/transport  SSH transport
```

## License

MIT
