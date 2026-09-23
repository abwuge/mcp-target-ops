# Target Ops

**A policy-gated MCP control plane for local and SSH hosts.**

**English** · [简体中文](README.zh-CN.md) · [Compatibility debt](COMPATIBILITY.md)

Target Ops is a self-contained Rust server that lets MCP clients run commands,
manage files, stream background jobs, and open persistent terminals on the
local machine or configured SSH hosts. Every target has its own permissions,
filesystem roots, timeouts, and output limits.

It can run over stdio for local clients or HTTP for remote MCP clients and
ChatGPT. HTTP mode also includes OAuth, ChatGPT file import/export metadata,
compact inventory cards for targets and downstream MCP servers, a
non-interactive command-result MCP App for `exec`, and a human-readable MCP App
for reviewing file changes.

> [!CAUTION]
> Target Ops can execute commands and modify files. Keep the local target
> disabled unless it is required, use low-privilege SSH accounts, restrict
> `allowed_roots`, require explicit targets for writes, and never expose HTTP
> mode without authentication and HTTPS.

## Highlights

- One interface for local and SSH targets: 38 model-facing tools plus two App-only helpers
- Persistent OpenSSH workers for remote file operations
- Live foreground command output plus cancellable background jobs with incremental output
- Persistent PTY terminals with live resize support
- Atomic file writes, exact edits, deletion previews, SHA-256 compare-and-swap,
  validated multi-file patches with rollback, and centrally managed expiring file backups
- File-backed secret injection without placing secret values in MCP arguments
- ChatGPT and connector file import/export with bounded transfer sizes
- Self-contained MCP Apps for target/MCP-server inventory, live `exec` results,
  and file review; command output renders ANSI styling safely, while file changes
  show full added/deleted content and editor-style diffs
- Allowlisted downstream Streamable HTTP MCP gateway
- Stdio and HTTP transports, static bearer authentication, OAuth 2.0 with PKCE,
  and persistent rotating refresh tokens

## Architecture

```text
MCP / ChatGPT client
        │
        ├── stdio ─────────────────────────────┐
        └── HTTP + bearer or OAuth ────────────┤
                                               ▼
                                           MCP layer
                                               │
                              target resolution and policy checks
                                               │
                   ┌───────────────────────────┴───────────────────────────┐
                   ▼                                                       ▼
             local process                                          OpenSSH worker
         commands / files / PTY                                commands / files / PTY

Configured downstream MCP servers are reached through a separate allowlisted
Streamable HTTP gateway; callers cannot provide arbitrary endpoint URLs.
```

Target resolution follows this order:

1. An explicit `target` argument
2. The process-scoped target selected by `target_select`
3. `server.default_target`

A default target is only a fallback; it is not reported as the active target.
When `require_explicit_target_for_write = true`, active and default targets are
not sufficient for a file mutation—the caller must pass `target` explicitly.

### Architectural boundaries

The refactor deliberately keeps four top-level boundaries because they match
the system's security and dependency model:

- `core` owns configuration invariants, target identity, policy, OAuth state,
  and process-scoped registries. It does not know HTTP routes or remote shell
  syntax.
- `protocol` translates MCP, HTTP, and OAuth requests into the existing
  operation layer. Routing, authorization-page rendering, form decoding, and
  response generation are separate modules.
- `tooling` owns operation semantics and performs target/policy checks before
  reaching a backend. Request/response types and multi-file patch parsing are
  separated from the file-operation implementation.
- `transport` owns OpenSSH process behavior. Persistent worker lifecycle and
  the POSIX remote-file protocol are isolated behind the SSH facade.

The server intentionally remains a small blocking Rust service backed by the
system OpenSSH client. Its workload is bounded operator actions rather than
high-volume web traffic; introducing an async runtime, a generic backend trait
hierarchy, or remote language runtimes would add complexity without improving
the current trust boundary. A future native SSH/SFTP implementation can replace
`transport/ssh` without changing MCP tools or policy semantics.

## Requirements

- Rust stable, including `rustfmt` and `clippy`, when building from source
- The system `ssh` and `scp` executables for SSH targets; `rsync` is used
  preferentially for cross-target file transfers when available
- A POSIX-compatible remote shell and common tools such as `cat`, `mktemp`,
  `mv`, `rm`, `chmod`, `mkdir`, and `stat`
- An HTTPS reverse proxy for remotely exposed HTTP deployments

The repository pins the stable toolchain and required components in
[`rust-toolchain.toml`](rust-toolchain.toml).

## Quick start

### 1. Build

```bash
git clone git@github.com:abwuge/mcp-target-ops.git
cd mcp-target-ops
cargo build --locked --release
```

The binary is written to `target/release/mcp-target-ops`.

### 2. Configure

A separate installer or bootstrap file is not required. On first startup, the single binary creates `~/.config/mcp-target-ops/config.toml` (or the path selected by `MCP_TARGET_OPS_CONFIG` / `--config`) from its built-in template with mode `0600` on Unix. You can also start from the fully annotated example:

```bash
mkdir -p ~/.config/mcp-target-ops
cp examples/config.toml ~/.config/mcp-target-ops/config.toml
```

A minimal SSH target looks like this:

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
allow_file_write = true
allow_select_active = true
require_explicit_target_for_write = true
allowed_roots = ["/home/ubuntu/app", "/tmp"]
default_timeout_ms = 30000
max_output_bytes = 200000
```

The section `[targets.dev]` creates the external target ID `ssh:dev`.
`[targets.local]` is reserved for the single local target.

### 3. Run

Stdio is the default transport:

```bash
./target/release/mcp-target-ops \
  --config ~/.config/mcp-target-ops/config.toml
```

For HTTP:

```bash
./target/release/mcp-target-ops \
  --config ~/.config/mcp-target-ops/config.toml \
  --http 127.0.0.1:8765
```

A generic stdio MCP client configuration is:

```json
{
  "mcpServers": {
    "target-ops": {
      "command": "/absolute/path/to/mcp-target-ops",
      "args": [
        "--config",
        "/absolute/path/to/config.toml"
      ]
    }
  }
}
```

Client configuration formats vary; use absolute paths when the client starts
servers outside your interactive shell environment.

## CLI

```text
mcp-target-ops [--config PATH] [--http ADDR]
```

| Option | Meaning |
| --- | --- |
| `-c`, `--config PATH`, `--config=PATH` | TOML configuration path |
| `--http ADDR`, `--http-addr ADDR` | Listen over HTTP instead of stdio (`--http-addr` is a compatibility alias; see `COMPAT-007`) |
| `-V`, `--version` | Print the package version |
| `-h`, `--help` | Print usage information |

Without `--config`, Target Ops uses `MCP_TARGET_OPS_CONFIG` when set, otherwise `~/.config/mcp-target-ops/config.toml`. If that selected file does not exist, Target Ops creates it from the built-in deny-by-default template before starting.

## Configuration

The complete annotated example is [`examples/config.toml`](examples/config.toml).

### Configuration lifecycle

At every startup, Target Ops parses the existing TOML, merges any fields newly introduced by the current binary, validates the merged configuration, and then rewrites the document. Existing values, comments, custom targets, and unknown compatibility fields are preserved; newly introduced defaults become visible and editable immediately after an upgrade. Environment-backed secrets are deliberately not materialized into the TOML during this rewrite.

Set the following to keep the file unchanged on disk. Missing fields still receive the current built-in defaults in memory:

```toml
[config]
rewrite_on_start = false
```

Process-wide runtime behavior lives under `[runtime]`:

| Key | Default | Purpose |
| --- | ---: | --- |
| `result_cache_max_bytes` | `104857600` | Total session-aware App result cache budget; oldest inactive sessions are evicted first |
| `max_retained_jobs` | `128` | In-memory background job entries retained by the process |
| `max_retained_foreground_execs` | `64` | Foreground exec sessions retained for App attachment |
| `exec_auto_background_after_ms` | `5000` | Promote an adaptive `exec` to a background job after this foreground window |
| `stream_default_max_bytes` | `65536` | Default incremental stream/read chunk bound |
| `stream_max_bytes` | `524288` | Maximum incremental stream/read chunk bound |
| `job_wait_default_timeout_ms` | `60000` | Default server-side `job_wait` window |
| `job_wait_max_timeout_ms` | `120000` | Maximum server-side `job_wait` window |
| `file_transfer_default_max_bytes` | `26214400` | Default `file_import` / `file_export` transfer limit |
| `file_export_delivery` | `link` | Default `file_export` delivery: temporary download link or legacy attachment |
| `file_export_link_ttl_secs` | `600` | Default link lifetime; hard maximum is 86400 seconds |
| `file_export_link_single_use` | `false` | Consume a link after the first successful GET; disabled by default to tolerate prefetching |
| `file_download_timeout_ms` | `30000` | Default HTTPS connector-file download timeout |
| `file_backup_default_ttl_secs` | `86400` | Default managed rollback-snapshot lifetime (24 hours) |
| `file_backup_max_ttl_secs` | `2592000` | Maximum requested backup lifetime (30 days hard ceiling) |
| `file_backup_cleanup_interval_secs` | `60` | Background cleanup interval; normal tool traffic also performs throttled cleanup |
| `file_backup_max_file_bytes` | `104857600` | Maximum size of one managed backup |
| `file_backup_store_max_bytes` | `1073741824` | Total managed-backup disk budget; oldest snapshots are evicted first |
| `file_backup_max_entries` | `1024` | Maximum retained snapshot count; also bounds empty/tiny-file backup metadata |
| `terminal_default_rows` | `30` | Default PTY rows |
| `terminal_default_cols` | `120` | Default PTY columns |
| `app_success_collapse_ms` | `3000` | App card auto-collapse delay after success |
| `app_failure_collapse_ms` | `6000` | App card auto-collapse delay after failure/warning |
| `app_sleep_after_ms` | `30000` | Delay before a collapsed App discards heavy DOM and relies on result restoration |
| `app_job_poll_interval_ms` | `180` | App-side background job output refresh interval |

Safety/protocol ceilings such as the 100 MiB absolute file-transfer maximum remain compiled hard limits rather than ordinary configuration knobs.

### Server settings

| Key | Default | Purpose |
| --- | --- | --- |
| `name` | `mcp-target-ops` | Name advertised to clients |
| `version` | Package version | Version advertised to clients |
| `startup_prompt` | None | Optional MCP initialize instructions, analogous to a global AGENTS.md; omitted entirely unless explicitly configured |
| `default_target` | None | Fallback target ID when no explicit or active target exists |
| `terminal_ring_buffer_bytes` | `524288` | Retained output per terminal session |
| `runtime_dir` | System temp directory + `mcp-target-ops` | Runtime directory created at startup |
| `http_bearer_token` | None | Static bearer token for protected HTTP routes |
| `public_base_url` | None | Exact external HTTPS origin used in OAuth metadata, export links, and MCP App widget metadata |
| `oauth_enabled` | `false` | Enable the embedded OAuth authorization server |
| `oauth_authorization_password` | None | Optional password gate on the authorization page |
| `oauth_scopes` | `["mcp:tools"]` | Scopes advertised and validated by OAuth |
| `oauth_allow_dynamic_client_registration` | `true` | Enable `/oauth/register` |
| `oauth_authorization_code_ttl_secs` | `600` | Authorization-code lifetime |
| `oauth_access_token_ttl_secs` | `3600` | Access-token lifetime |
| `oauth_refresh_token_ttl_secs` | `2592000` | Refresh-token lifetime |
| `oauth_state_file` | `~/.config/mcp-target-ops/oauth-state.json` | Persistent OAuth clients and tokens |

Deployment-specific values can be supplied by environment variables:

| Variable | Purpose |
| --- | --- |
| `MCP_TARGET_OPS_CONFIG` | Default configuration path |
| `MCP_TARGET_OPS_HTTP_TOKEN` | Static HTTP bearer token |
| `MCP_TARGET_OPS_OAUTH` | Enable OAuth with `1`, `true`, `yes`, or `on` |
| `MCP_TARGET_OPS_PUBLIC_BASE_URL` | External HTTPS origin, without a trailing slash |
| `MCP_TARGET_OPS_OAUTH_PASSWORD` | Authorization-page password |
| `MCP_TARGET_OPS_OAUTH_SCOPES` | Space-separated OAuth scopes |
| `MCP_TARGET_OPS_OAUTH_STATE_FILE` | OAuth state-file path |

### Targets

The local target must be configured as `[targets.local]` with `kind = "local"`.
It is disabled by default. Every other target must use `kind = "ssh"`; a section
named `[targets.<name>]` is exposed as `ssh:<name>`.

Local target fields:

| Key | Purpose |
| --- | --- |
| `enabled` | Make the local target available |
| `shell` | Optional shell program used for commands and terminals |
| `policy` | Per-target permission and limit table |

SSH target fields:

| Key | Purpose |
| --- | --- |
| `enabled` | Make the SSH target available |
| `host` | SSH hostname or address |
| `port` | SSH port, default `22` |
| `user` | Optional SSH username |
| `identity_file` | Optional private-key path passed to OpenSSH |
| `control_master` | Reuse one OpenSSH transport across commands, jobs, terminals, and file workers; default `true` |
| `control_persist_secs` | Keep an idle shared ControlMaster alive for reuse; default `1800` seconds |
| `extra_args` | Additional OpenSSH arguments, such as `BatchMode=yes` |
| `shell` | Optional remote shell program |
| `policy` | Per-target permission and limit table |

OpenSSH remains responsible for host-key verification, agents, jump hosts,
Kerberos, and other client behavior. Put those settings in your normal SSH
configuration or in carefully reviewed `extra_args`.

### Target policy

All capabilities are denied unless explicitly enabled.

| Key | Default | Purpose |
| --- | --- | --- |
| `allow_exec` | `false` | Permit foreground and background commands |
| `allow_terminal` | `false` | Permit persistent PTY sessions |
| `allow_file_read` | `false` | Permit file and directory reads |
| `allow_file_write` | `false` | Permit mutations, imports, moves, chmod, and mkdir |
| `allow_select_active` | `false` | Permit `target_select` for this target |
| `require_explicit_target_for_write` | `true` | Reject mutations resolved through active/default target state |
| `allowed_roots` | Empty | Absolute path roots available to file tools |
| `default_timeout_ms` | `30000` | Default command and remote-operation timeout |
| `max_output_bytes` | `200000` | Default command output bound |

An empty `allowed_roots` list refuses all file access even when file read or
write permission is enabled.

### Downstream MCP gateway

Target Ops can initialize and call explicitly configured Streamable HTTP MCP
servers. The preferred layout keeps endpoint credentials in a dedicated
mode-`0600` file:

```toml
# ~/.config/mcp-target-ops/config.toml
[mcp_servers.memory]
enabled = true
config_file = "/home/me/.config/mcp-target-ops/mcp/memory.toml"
timeout_ms = 20000
max_response_bytes = 1048576
```

```toml
# ~/.config/mcp-target-ops/mcp/memory.toml
url = "https://memory.example.com/mcp"

[headers]
Authorization = "Bearer replace-me"
```

On Unix, credential files readable by group or others are rejected. Redirects
are disabled, so headers are never forwarded to a redirect destination.
`mcp_server_list` returns safe metadata only; URLs and secret values are not
exposed. Inline URLs and file-backed URL/header references remain available for
advanced deployments; see the example configuration.

## Tool catalog

Target Ops publishes 40 tool descriptors: 38 model-facing tools and two App-only helpers, `exec_stream` and `result_read`.

| Area | Tools |
| --- | --- |
| Server | `server_info` |
| Targets | `target_list`, `target_current`, `target_select`, `target_connect`, `target_disconnect` |
| Downstream MCP | `mcp_server_list`, `mcp_tools_list`, `mcp_tool_call` |
| Commands and jobs | `exec`, `exec_batch`, `exec_start`, `job_poll`, `job_output`, `job_wait`, `job_cancel`; App-only: `exec_stream`, `result_read` |
| Files and directories | `file_read`, `file_backup`, `file_backup_list`, `file_restore`, `file_backup_delete`, `file_list`, `file_find`, `file_edit`, `file_write`, `file_delete`, `file_import`, `file_export`, `file_transfer`, `file_patch`, `file_move`, `file_chmod`, `directory_create` |
| Terminals | `terminal_open`, `terminal_send`, `terminal_read`, `terminal_resize`, `terminal_close` |

Every tool declares an object `outputSchema`, returns successful data through
`structuredContent`, and includes standard MCP annotations. When OAuth is
enabled, tool descriptors also advertise the configured OAuth scopes.

### Target and MCP server inventory

`target_list` and `mcp_server_list` share the stable
`ui://target-ops/inventory/v1.html` MCP App. The compact card shows target kind,
active/enabled state, allowed operations and roots, or downstream server state,
configuration source, timeout, response limit, and header-count metadata.

### Commands and background jobs

`exec` runs one bounded non-interactive shell command or script. `exec_batch`
runs up to 32 logically independent commands in one tool call, sequentially by
default or in parallel when explicitly requested, and returns one structured
result per command. Sequential batches may stop on the first failure. Use
`exec` when commands need shared shell state such as `cd`, variables, pipelines,
or control flow; use `exec_batch` for unrelated inspections instead of joining
them with shell separators.

`exec`, `exec_batch`, and `exec_start` bind to the stable `ui://target-ops/exec-terminal/v1.html` MCP App. Synchronous `exec` remains the short-command path and returns its final result normally. For commands likely to run more than a few seconds, or whenever live output matters, prefer `exec_start`: it returns a job ID immediately, allowing the App to poll `job_output` at the `runtime.app_job_poll_interval_ms` interval without being blocked by the parent tool call. ANSI SGR color/style sequences are rendered safely instead of being shown as raw escape codes.

For model-side workflows that only need to continue after a background command finishes, prefer `job_wait` over repeated `job_poll` or `job_output` calls. Its server-side default and cap come from `runtime.job_wait_default_timeout_ms` and `runtime.job_wait_max_timeout_ms`, and a call may request a shorter window.

All Target Ops Apps use the same configurable result-card lifecycle. The default is to collapse successful cards after 3 seconds, failures/warnings after 6 seconds, and enter the DOM-light sleep state after 30 seconds. When an existing conversation is reopened, saved App results take a separate cold-start path: the App hydrates only the compact summary row before the bridge handshake, does not rebuild the heavy output/diff DOM until the user expands the card, and suppresses a duplicate host replay of that same historical result so it cannot rearm the fresh-result expansion timer. Apps also report intrinsic dimensions with the MCP Apps `ui/notifications/size-changed` notification using `ResizeObserver`, allowing a flexible-height host to shrink historical iframes to the collapsed row as soon as the bridge is ready. Static tool results are cached under `runtime_dir/results/` with no TTL; `runtime.result_cache_max_bytes` defaults to 100 MiB and eviction removes the least-recently-active whole session first. Reopening a sleeping card uses the App-only `result_read` helper and an opaque per-result `result_id`; new activity refreshes that session's LRU lifetime. Long-running job cards rebuild from the retained job output buffer only after expansion. The cards remain present and reopenable; Target Ops does not automatically request iframe teardown.

`exec` and `exec_start` share the same `CommandSession` implementation for
process lifecycle, stdout/stderr capture, timeout, cancellation, and incremental
output. `exec` waits for the session and uses the target policy's default timeout
when none is supplied; `exec_start` returns a job ID immediately and has no
runtime timeout unless one is requested. Current ChatGPT hosts serialize App-originated tool calls behind an in-flight synchronous `exec`, so live UI output is intentionally implemented through `exec_start` rather than foreground `exec_stream` polling. Child stdin is disconnected from the
MCP control stream. SSH command sessions still use dedicated local OpenSSH child
processes for independent lifecycle, streaming, timeout, and cancellation, but
those clients multiplex over a per-target OpenSSH ControlMaster when
`control_master = true`. Remote file workers and persistent terminals use that
same transport, so independent operations remain concurrent without repeating
TCP/SSH handshakes.

Both command tools accept `secret_env`. A value may come from a text file or a
dot-selected scalar in TOML or JSON:

```json
{
  "target": "local",
  "command": "./client",
  "secret_env": {
    "SERVICE_AUTH": {
      "path": "/home/me/service.toml",
      "format": "toml",
      "key": "client.headers.Authorization"
    }
  }
}
```

The secret source must pass normal file-read policy. Its value is resolved
inside Target Ops and does not appear in the MCP request, but the launched
command can still reveal it by printing or transmitting its environment.

### File safety and patching

- `file_read` returns SHA-256 metadata and supports 1-based UTF-8 line ranges. It accepts either legacy single `path` mode or up to 32 independent `files[]` reads in one call; repeating a path with different ranges reads multiple ranges efficiently. Batch failures are isolated per item and the batch shares one bounded output budget.
- `file_find` performs bounded literal matching with optional context.
- `file_write` is atomic. Replacing an existing file requires
  `expected_sha256` or `overwrite = true`.
- `file_edit`, single-file `file_patch`, and `file_delete` support SHA-256
  compare-and-swap guards and dry-run/preview behavior where applicable.
- Existing Unix modes are preserved unless a new mode is supplied.
- `file_delete` refuses directories and returns the pre-delete content for
  review.
- Multi-file `file_patch` treats `path` as a base directory, validates all UTF-8
  inputs before writing, rejects paths that escape the base, and rolls back
  earlier writes if a later write fails. Multi-file create, delete, and rename
  sections are intentionally rejected.
- `file_move` does not overwrite by default; `file_chmod` and
  `directory_create` use the same write policy as other mutations.
- `file_transfer` copies one regular file between two explicit, different
  targets. It prefers `rsync` and falls back to `scp`. For SSH-to-SSH copies,
  the file data travels directly between the two target machines: Target Ops
  tries source-side push and destination-side pull, and never stages the file on
  the plugin host. At least one remote side must therefore be able to
  authenticate non-interactively to the other for a direct SSH-to-SSH copy.

### Managed rollback snapshots

Use `file_backup` whenever a temporary safety copy is needed before an edit,
configuration change, service restart, migration, or other risky operation.
Models should not create adjacent `.bak`, `.old`, timestamped, or similarly
ad-hoc copies with `cp`/`mv`: the `exec` tool description explicitly points
backup workflows to the managed tool instead.

Managed snapshots are stored centrally under `server.runtime_dir/file-backups/`
rather than beside the source file. A snapshot records the target, original path,
SHA-256, byte size, Unix mode when available, creation/expiry timestamps, and an
optional short note. `file_backup` defaults to a 24-hour TTL; callers may request
a different TTL up to the configured maximum (30 days by default). On Unix the
backup directories are mode 0700 and payload/metadata files are mode 0600.

`file_backup_list` discovers still-valid snapshots after the caller has lost the
original tool result or conversation context. It returns the newest 100 matches by
default, accepts `limit` up to 200, and reports `total_matches` plus `truncated`.
`file_restore` restores either to
the original path or an explicit destination and verifies backup integrity first.
Restoring over an existing file requires either `expected_current_sha256` or an
explicit `overwrite = true`; the original mode is reapplied where supported.
`file_backup_delete` removes an individual snapshot early without touching its
source file.

Expired snapshots are made unavailable by backup operations immediately and are
physically removed at server startup, by a background cleanup worker (every 60
seconds by default), and opportunistically during normal tool traffic. Cleanup
enforces both `file_backup_store_max_bytes` and
`file_backup_max_entries`; if either budget would be exceeded, the oldest
snapshots are evicted first even when their TTL has not yet elapsed. No manual
backup-directory housekeeping is required.

### Terminals

`terminal_open` starts a persistent PTY and binds it permanently to the resolved
target. `terminal_read` is incremental, `terminal_send` writes input, and
`terminal_resize` resizes the live PTY. Sessions use bounded ring buffers and
remain process-local.

### ChatGPT file integration and review UI

`file_read` binds to `ui://target-ops/file-read/v1.html`, which presents text with line numbers, range and hash metadata, and compact expandable batch results. This makes structured file reads easier to inspect than shell-based `cat`/`sed` output.

`file_import` accepts a ChatGPT or connector file parameter after runtime
rewriting to a mounted local path or HTTPS file reference. It writes through the
normal atomic/CAS policy. HTTPS redirects are disabled.

> [!WARNING]
> `file_export` may require mandatory front-end review/approval. If the user is
> away from the client, the approval can time out and terminate an in-progress
> model run. Models should avoid `file_export` during substantive reasoning
> unless it is necessary, and should prefer using it only after the main work is
> complete—especially for expensive Pro-tier model runs.

`file_export` defaults to `delivery = "link"`. After the normal target read-policy
check, the server stages a private copy under `runtime_dir/downloads/`, assigns a
cryptographically random 256-bit opaque token, and returns a URL under
`/downloads/<token>` instead of an MCP file attachment. The default lifetime is
10 minutes; `link_ttl_secs` may override it up to 24 hours. Links are reusable by
default so browser or host prefetching does not consume them, while `single_use =
true` makes the first successful GET consume the token. On Unix the staging
directory is mode 0700 and staged files are mode 0600. Expired tokens are
rejected, and staged exports are purged whenever the service starts.

Link delivery requires `server.public_base_url`; HTTPS is required except for
localhost testing. Treat the opaque download URL as short-lived sensitive data.
`delivery = "attachment"` preserves the original MCP embedded-resource and
ChatGPT file-output path for compatibility. Only attachment mode emits the
embedded base64 resource and `file` result field, while link mode returns download
metadata only.

Both import and export use `runtime.file_transfer_default_max_bytes` (25 MiB by
default) and retain a compiled 100 MiB hard maximum.

File-changing tools bind to the stable MCP App resource:

```text
ui://target-ops/file-change/v1.html
```

The self-contained widget:

- shows the complete content of added and deleted files;
- renders modified UTF-8 files as an editor-style inline diff with line numbers;
- presents multi-file changes in one review surface;
- keeps hashes, sizes, encoding, and target details in a collapsed section;
- requests a bordered presentation and declares a closed network/resource CSP;
- uses `server.public_base_url` as its widget domain when configured.

The command result, file read, file change, and inventory MCP Apps use the same bordered card system: consistent 10 px outer radius, header spacing, monospace secondary text, editor-like content surface, expandable details, and shared success/warning/error pills. The OAuth authorization page keeps its full-page hierarchy while using the same system and monospace font families and related corner treatment.

After changing tool descriptors or App metadata, reconnect or refresh the
ChatGPT app so the client reloads `tools/list` and MCP resources.

## HTTP mode

Bind the Rust server to localhost and publish it through an HTTPS reverse proxy.
If neither static bearer authentication nor OAuth is configured, protected
routes are open; this is suitable only for a trusted local environment.

Static bearer authentication:

```bash
MCP_TARGET_OPS_HTTP_TOKEN='replace-me' \
  ./target/release/mcp-target-ops --http 127.0.0.1:8765
```

Clients send:

```text
Authorization: Bearer <token>
```

Static bearer authentication and OAuth may be enabled together; either valid
token type authorizes a protected request.

### OAuth for remote MCP and ChatGPT

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
oauth_state_file = "/home/me/.config/mcp-target-ops/oauth-state.json"
```

The embedded server supports:

- OAuth protected-resource and authorization-server metadata
- Public dynamic client registration
- Authorization code flow with mandatory PKCE S256
- HTTPS redirect URIs and HTTP loopback redirect URIs
- Optional password-gated, responsive authorization page with light/dark mode,
  registered-client details, explicit approval, and cancellation
- Opaque access tokens
- Refresh-token rotation bound to client, resource, and scope

Clients, access tokens, and refresh tokens are persisted atomically in the OAuth
state file. On Unix, the file must have mode `0600`. Authorization codes remain
short-lived and memory-only. A successful refresh returns a replacement refresh
token and invalidates the previous token. Because client and token state is
persistent, a previously authorized ChatGPT connection can normally refresh
after a service restart without asking for the authorization password again.
Deleting the state file intentionally revokes the persisted authorization
state.

### HTTP endpoints

| Endpoint | Access and purpose |
| --- | --- |
| `GET /` | Public service summary and endpoint discovery |
| `GET /health` | Public health status |
| `GET /favicon.ico` | Public application icon |
| `GET /downloads/<token>` | Temporary `file_export` download addressed by its opaque token |
| `POST /` or `POST /mcp` | MCP JSON-RPC; protected when auth is configured |
| `DELETE /mcp` | Acknowledge MCP session close; protected when auth is configured |
| `/.well-known/oauth-protected-resource` | OAuth resource metadata when OAuth is enabled |
| `/.well-known/oauth-authorization-server` | OAuth authorization metadata when enabled |
| `GET|POST /oauth/authorize` | Authorization page, approval, and denial |
| `POST /oauth/token` | Authorization-code and refresh-token exchange |
| `POST /oauth/register` | Dynamic client registration when enabled |

Bearer authentication represents one shared service identity. It is not a
multi-user authorization system and does not add per-user target permissions.

## Security model

Target Ops layers several controls rather than treating authentication as the
only boundary:

1. Targets must exist and be enabled.
2. Each operation class must be enabled by target policy.
3. File paths must remain within configured roots.
4. Writes require an explicit target by default.
5. Existing-file replacements can use SHA-256 compare-and-swap.
6. Command time, retained output, file transfer, and downstream responses are
   bounded.
7. Downstream MCP endpoints are configured server-side and redirects are
   disabled.
8. Remote HTTP deployments should use HTTPS and bearer/OAuth authentication.

Use a dedicated service account and a separate low-privilege account for each
remote environment. Review OpenSSH options and host keys outside Target Ops.
Do not place secrets directly in commands or logs, and remember that any
permitted command can act with the operating-system privileges of its account.

## Docker and CI

The included multi-stage [`Dockerfile`](Dockerfile) builds the release binary
and installs `openssh-client` in a Debian runtime image:

```bash
docker build -t mcp-target-ops .
```

Mount configuration, SSH material, and any writable OAuth state directory
explicitly for the deployment. Do not bake credentials into the image.

GitHub Actions includes:

- `CI`: formatting, tests, and Clippy
- `Linux binaries`: x86_64 and aarch64 GNU release artifacts
- `Docker images`: manually triggered multi-architecture GHCR publishing

## Development

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

Source layout:

```text
assets/
  exec-terminal.html     MCP App non-interactive exec result interface
  file-change.html       MCP App file review interface
  file-read.html         MCP App single/batch file reader
  inventory-card.html    MCP App target and MCP server inventory card
  oauth-authorize.html   OAuth authorization page

src/core/
  config.rs              configuration types, defaults, and loading
  config/validation.rs   configuration invariants and target validation
  oauth.rs               persistent OAuth state and token rotation
  policy.rs              target permission and path enforcement
  state.rs               process-scoped registries and target resolution

src/protocol/
  apps.rs                MCP App resource metadata
  html.rs                shared HTML escaping
  http/mod.rs            HTTP routing and public endpoint discovery
  http/auth.rs           bearer-token authorization
  http/form.rs           URL-encoded form/query codec
  http/oauth.rs          OAuth metadata, registration, authorization, tokens
  http/response.rs       HTTP responses, redirects, CORS, and security headers
  oauth_page.rs          authorization-page rendering
  mcp.rs                 MCP JSON-RPC over stdio and HTTP

src/tooling/
  tools/catalog.rs       tool descriptors and input schemas
  tools/dispatch.rs      tool dispatch and target connection operations
  tools/schema.rs        output schemas
  exec.rs, job.rs        foreground and background commands
  fs.rs                  policy-aware file-operation semantics
  fs/backend.rs          local/SSH byte I/O and atomic local writes
  fs/types.rs            file request and response types
  fs/patch_format.rs     multi-file unified-diff parsing and path checks
  file_bridge.rs         ChatGPT and connector transfer bridge
  file_transfer.rs       direct cross-target rsync/scp transfer
  terminal.rs            persistent PTY sessions
  mcp_client.rs          allowlisted downstream MCP client
  secret.rs              file-backed secret resolution

src/transport/
  ssh/mod.rs             SSH facade and command/PTY construction
  ssh/session.rs         persistent OpenSSH worker lifecycle
  ssh/files.rs           POSIX remote-file protocol
```

## Known constraints

- Active target, jobs, terminals, and authorization codes are process-local.
  OAuth clients and tokens are persistent when a state file is configured.
- SSH support intentionally depends on the system OpenSSH client and a
  POSIX-like remote environment.
- Multi-file patches modify existing UTF-8 files only; create/delete/rename
  sections are rejected.
- The embedded OAuth server is designed for a private service deployment, not
  as a full identity provider with user accounts and administrative revocation.

## License

[MIT](LICENSE)
