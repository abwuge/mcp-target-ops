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
| `COMPAT-004` | OAuth security schemes and file rewrite metadata include OpenAI aliases. | `src/tooling/tools/catalog.rs` | Supported ChatGPT host versions do not all consume the same metadata names. | Remove individual aliases only after production host capability checks show the corresponding security scheme / file rewrite metadata works without that alias. Do not remove product-specific metadata that has no standard replacement under this entry. |
| `COMPAT-006` | `file_read` accepts the original top-level `path` / `start_line` / `end_line` request shape as well as the newer `files[]` batch shape. | `src/tooling/fs.rs`, `src/tooling/fs/types.rs`, `src/tooling/tools/catalog.rs` | Existing clients and prompts may still send the original single-file request schema. | Remove after supported clients use `files[]` exclusively and a deliberate breaking schema change is acceptable. |
| `COMPAT-008` | Streamable HTTP accepts MCP POST requests at both `/mcp` and the older root `/`. | `src/protocol/http/mod.rs` | Older client configuration may point directly at the public origin without `/mcp`. | Remove after all supported clients and saved connection configurations use `/mcp`, and reconnect/migration guidance has been provided where needed. |
| `COMPAT-009` | `file_export` defaults to opaque download-link delivery but retains explicit `delivery=attachment`, MCP embedded-resource output, and ChatGPT file-result rewrite metadata. | `src/tooling/file_bridge.rs`, `src/protocol/mcp.rs`, `src/tooling/tools/catalog.rs` | Existing clients and workflows may still rely on host-side attachment materialization even though link delivery avoids the extra approval boundary. | Remove after all supported clients consume link delivery and no production workflow requires ChatGPT attachment materialization from `file_export`. |
| `COMPAT-010` | Cross-target file transfers fall back from rsync to scp, including when rsync is missing or does not support the requested options. | `src/tooling/file_transfer.rs` (`transfer_local_ssh`, `transfer_ssh_to_ssh_direct`, `rsync_is_unsupported`) | The supported target requirements make rsync optional. Local-to-SSH transfers detect missing/unsupported rsync; SSH-to-SSH transfers try scp after both rsync directions fail. | Remove only after supported transfer hosts require and provide rsync with `--protect-args`, and supported direct routes work with rsync. Remove scp transfer branches, unsupported-rsync detection, transfer-specific helpers/tests, and update both README requirements and transfer descriptions together. Push/pull direction selection is a separate connectivity feature. |

## Cleanup rule

Compatibility code should become smaller over time. When a removal condition is satisfied, remove the compatibility branch, its tests, documentation, migration artifacts, and this registry entry in the same change. Do not silently convert a `COMPAT` path into an undocumented permanent branch.
