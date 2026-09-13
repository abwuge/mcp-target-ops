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
| `COMPAT-002` | Pre-session flat result files under `runtime/results/*.json` remain readable alongside session-scoped `runtime/results/sessions/<hash>/...`. | `src/tooling/result_store.rs` | Existing conversation cards may still reference result IDs written before the session-LRU cache layout was introduced. | Remove after old flat-cache result IDs are no longer needed and every rollback version that can be deployed writes the session-scoped layout. Delete the flat-cache scan and `.legacy-activity` handling together. |
| `COMPAT-003` | MCP Apps accept legacy `window.openai` bootstrap values and multiple historical tool-input/tool-result envelope shapes in addition to the current MCP Apps notification path. | `assets/exec-terminal.html`, `assets/file-change.html`, `assets/file-read.html`, `assets/inventory-card.html` | Older ChatGPT host plumbing may expose initial input/output through `window.openai`, or place data in `content`/plain params rather than the newer `structuredContent` envelope. | Remove after the minimum supported ChatGPT/MCP Apps host guarantees initial hydration and subsequent input/result notifications through one stable standard envelope. |
| `COMPAT-004` | Newer MCP Apps metadata is emitted together with older `openai/*` aliases and OpenAI file rewrite aliases. | `src/protocol/apps.rs`, `src/tooling/tools/catalog.rs` | Supported ChatGPT host versions do not all consume the same metadata names. | Remove individual aliases only after production host capability checks show the corresponding standard `ui.*` / file rewrite metadata works without that alias. Do not remove product-specific metadata that has no standard replacement under this entry. |
| `COMPAT-005` | `exec_stream` can attach by `target + command + cwd` when the original MCP request ID is unavailable. | `src/tooling/job.rs`, `src/tooling/tools/catalog.rs` | Some host versions do not expose the original `tools/call` request ID to the App. | Remove after all supported hosts expose a stable request/session identifier that lets the App attach to the exact foreground execution. |
| `COMPAT-006` | `file_read` accepts the original top-level `path` / `start_line` / `end_line` request shape as well as the newer `files[]` batch shape. | `src/tooling/fs.rs`, `src/tooling/fs/types.rs`, `src/tooling/tools/catalog.rs` | Existing clients and prompts may still send the original single-file request schema. | Remove after supported clients use `files[]` exclusively and a deliberate breaking schema change is acceptable. |
| `COMPAT-007` | CLI accepts `--http-addr` as an alias for `--http`. | `src/main.rs`, README option tables | Existing service files or scripts may still use the older flag name. | Remove after repository/deployment searches confirm no supported service, script, documentation, or user configuration uses `--http-addr`. |
| `COMPAT-008` | Streamable HTTP accepts MCP POST requests at both `/mcp` and the older root `/`. | `src/protocol/http/mod.rs` | Older client configuration may point directly at the public origin without `/mcp`. | Remove after all supported clients and saved connection configurations use `/mcp`, and reconnect/migration guidance has been provided where needed. |
| `COMPAT-009` | `file_export` defaults to opaque download-link delivery but retains explicit `delivery=attachment`, MCP embedded-resource output, and ChatGPT file-result rewrite metadata. | `src/tooling/file_bridge.rs`, `src/protocol/mcp.rs`, `src/tooling/tools/catalog.rs` | Existing clients and workflows may still rely on host-side attachment materialization even though link delivery avoids the extra approval boundary. | Remove after all supported clients consume link delivery and no production workflow requires ChatGPT attachment materialization from `file_export`. |

## Cleanup rule

Compatibility code should become smaller over time. When a removal condition is satisfied, remove the compatibility branch, its tests, documentation, migration artifacts, and this registry entry in the same change. Do not silently convert a `COMPAT` path into an undocumented permanent branch.
