# Changelog

## 0.1.0

- Initial target-based MCP server MVP.
- Unified tools across local and SSH targets.
- Session-scoped active target stickiness.
- Local and OpenSSH CLI backends.
- Optional HTTP transport with `/health` and `/mcp`.
- GPTs Actions REST facade with a public OpenAPI 3.1 Schema at
  `/openapi.json`, explicit-target operations, response bounds, and
  consequential-operation markers.
- Added `outputSchema` declarations for every tool and `structuredContent` in successful tool results.
- Made command results sparse by omitting echoed inputs, empty streams, and inactive status flags.
- Preserve file modes across atomic edits and add atomic writes, unified patches, line-range reads, literal find, move, chmod, and directory creation tools.
- Add file-backed secret environment references for non-interactive commands.
- Add an allowlisted downstream Streamable HTTP MCP gateway with `mcp_server_list`, `mcp_tools_list`, and `mcp_tool_call`; support dedicated mode-0600 per-server config files for endpoints and headers.
- Extend `file_patch` to support validated multi-file unified diffs rooted at a base directory, with rollback of earlier writes if a later write fails.
- Add background command jobs with polling, incremental output, cancellation, and dedicated SSH processes.
- Wire `terminal_resize` to the live PTY instead of recording a placeholder resize request.
- Isolate non-interactive child stdin from the MCP control stream.
