# Compatibility debt

Target Ops keeps a small number of compatibility paths for older clients, host integrations, data layouts, and command-line interfaces. These paths are intentional technical debt, not permanent architecture.

Search the repository with:

```sh
rg 'COMPAT\(COMPAT-'
```

When adding a compatibility path, add a new entry here and mark the implementation with `COMPAT(COMPAT-xxx)`. Each entry must state why it exists and the condition that makes it safe to remove.

| ID | Compatibility path | Main locations | Why it remains | Removal condition |
| --- | --- | --- | --- | --- |
| `COMPAT-001` | HTTP callers without `Mcp-Session-Id` fall back to a bearer-token fingerprint (or anonymous HTTP bucket) for per-caller state. | `src/protocol/http/auth.rs`, `src/protocol/http/mod.rs` | Older or non-session-aware MCP HTTP clients may not echo a server-issued session ID. | Remove after every supported HTTP MCP client reliably returns `Mcp-Session-Id` on subsequent requests and no supported deployment depends on sessionless HTTP calls. |
| `COMPAT-003` | MCP Apps accept legacy `window.openai` bootstrap values and multiple historical tool-input/tool-result envelope shapes in addition to the current MCP Apps notification path. Historical bootstrap results are rendered summary-only before handshake and matching replay notifications are suppressed so saved cards do not rearm fresh-result expansion. | `assets/exec-terminal.html`, `assets/file-change.html`, `assets/file-read.html`, `assets/inventory-card.html` | Older ChatGPT host plumbing may expose initial input/output through `window.openai`, may replay that same saved result after initialization, or place data in `content`/plain params rather than the newer `structuredContent` envelope. | Remove after the minimum supported ChatGPT/MCP Apps host guarantees initial hydration and subsequent input/result notifications through one stable standard envelope with an explicit way to distinguish restored results from fresh tool completion. |
| `COMPAT-004` | Newer MCP Apps metadata is emitted together with older `openai/*` aliases and OpenAI file rewrite aliases. | `src/protocol/apps.rs`, `src/tooling/tools/catalog.rs` | Supported ChatGPT host versions do not all consume the same metadata names. | Remove individual aliases only after production host capability checks show the corresponding standard `ui.*` / file rewrite metadata works without that alias. Do not remove product-specific metadata that has no standard replacement under this entry. |
| `COMPAT-006` | `file_read` accepts the original top-level `path` / `start_line` / `end_line` request shape as well as the newer `files[]` batch shape. | `src/tooling/fs.rs`, `src/tooling/fs/types.rs`, `src/tooling/tools/catalog.rs` | Existing clients and prompts may still send the original single-file request schema. | Remove after supported clients use `files[]` exclusively and a deliberate breaking schema change is acceptable. |
| `COMPAT-008` | Streamable HTTP accepts MCP POST requests at both `/mcp` and the older root `/`. | `src/protocol/http/mod.rs` | Older client configuration may point directly at the public origin without `/mcp`. | Remove after all supported clients and saved connection configurations use `/mcp`, and reconnect/migration guidance has been provided where needed. |
| `COMPAT-009` | `file_export` defaults to opaque download-link delivery but retains explicit `delivery=attachment`, MCP embedded-resource output, and ChatGPT file-result rewrite metadata. | `src/tooling/file_bridge.rs`, `src/protocol/mcp.rs`, `src/tooling/tools/catalog.rs` | Existing clients and workflows may still rely on host-side attachment materialization even though link delivery avoids the extra approval boundary. | Remove after all supported clients consume link delivery and no production workflow requires ChatGPT attachment materialization from `file_export`. |
| `COMPAT-010` | Cross-target file transfers fall back from rsync to scp, including when rsync is missing or does not support the requested options. | `src/tooling/file_transfer.rs` (`transfer_local_ssh`, `transfer_ssh_to_ssh_direct`, `rsync_is_unsupported`) | The supported target requirements make rsync optional. Local-to-SSH transfers detect missing/unsupported rsync; SSH-to-SSH transfers try scp after both rsync directions fail. | Remove only after supported transfer hosts require and provide rsync with `--protect-args`, and supported direct routes work with rsync. Remove scp transfer branches, unsupported-rsync detection, transfer-specific helpers/tests, and update both README requirements and transfer descriptions together. Push/pull direction selection is a separate connectivity feature. |

## Review: 2026-09-25

Scope: commits after the initial registry (`8a8d9ce`) through `2ae231c`, plus
read-only checks of the local Oracle deployment. These observations do not
establish the state of every supported client or deployment.

- `264a8ea` recorded historical-card hydration and replay suppression under
  `COMPAT-003`; `c9247c6` extended that marked path with
  `window.openai.toolResponseMetadata.mcp_tool_result` and `call_tool_result`.
  `70330ea` fixes file-body/envelope handling and historical restoration within
  the same entry. Preserve string-valued file `content` handling even when old
  envelope support is eventually removed: it is part of the current payload.
- `c8e54e8` recorded attachment export under `COMPAT-009` with implementation
  markers. `78f718f` documented rsync/scp behavior in the README but omitted a
  debt entry and implementation markers; this review adds `COMPAT-010`.
- Following this review, the maintainer authorized retirement of `COMPAT-002`,
  `COMPAT-005`, and `COMPAT-007`. The flat-cache reader and synthetic legacy
  bucket were removed; only session-scoped cache files are indexed. The unused
  `exec_stream` helper, foreground attachment registry, request-ID plumbing,
  output schema and `max_retained_foreground_execs` setting were removed.
  Both `--http-addr` spellings were removed; use `--http` instead.
- Upgrade boundary: historical Apps that call `exec_stream` must be refreshed
  to the current App, which polls `job_output`. Flat results from versions before
  `5028a67` are no longer supported. Pre-layout rollback artifacts are outside
  the supported rollback set for this upgrade; this source cleanup does not
  delete installed binaries or change the running service. Existing TOML files
  may retain `max_retained_foreground_execs` as an ignored unknown field; it can
  be deleted from operator configuration.
- `COMPAT-001`, `003`, `004`, `006`, `008`, and `009` remain open: this review
  has no client-capability or usage evidence satisfying their removal gates.
  The recent historical-card repairs specifically exercise `COMPAT-003`.
  `COMPAT-010` remains required by the documented optional-rsync contract.

The remaining registry IDs are stable and are not renumbered after removals.

## Cleanup rule

Compatibility code should become smaller over time. When a removal condition is satisfied, remove the compatibility branch, its tests, documentation, migration artifacts, and this registry entry in the same change. Do not silently convert a `COMPAT` path into an undocumented permanent branch.
