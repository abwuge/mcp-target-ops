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
- Add background command jobs with polling, incremental output, cancellation, and dedicated SSH processes.
- Wire `terminal_resize` to the live PTY instead of recording a placeholder resize request.
- Isolate non-interactive child stdin from the MCP control stream.
