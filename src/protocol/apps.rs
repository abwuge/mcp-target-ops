use crate::tooling::tools::FILE_CHANGE_UI_URI;
use serde_json::{json, Value};

pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";

pub fn list_resources() -> Value {
    json!({
        "resources": [{
            "uri": FILE_CHANGE_UI_URI,
            "name": "target-ops-file-change",
            "title": "Target Ops file change",
            "description": "Read-only summary and diff preview for Target Ops file mutations.",
            "mimeType": MCP_APP_MIME_TYPE,
            "_meta": {
                "ui": {
                    "prefersBorder": true,
                    "csp": {
                        "connectDomains": [],
                        "resourceDomains": []
                    }
                },
                "openai/widgetDescription": "Shows the affected target, file paths, write status, hashes, and unified diff returned by a Target Ops file mutation.",
                "openai/widgetPrefersBorder": true
            }
        }]
    })
}

pub fn read_resource(uri: &str) -> Option<Value> {
    (uri == FILE_CHANGE_UI_URI).then(|| {
        json!({
            "contents": [{
                "uri": FILE_CHANGE_UI_URI,
                "mimeType": MCP_APP_MIME_TYPE,
                "text": FILE_CHANGE_UI_HTML,
                "_meta": {
                    "ui": {
                        "prefersBorder": true,
                        "csp": {
                            "connectDomains": [],
                            "resourceDomains": []
                        }
                    },
                    "openai/widgetDescription": "Shows the affected target, file paths, write status, hashes, and unified diff returned by a Target Ops file mutation.",
                    "openai/widgetPrefersBorder": true
                }
            }]
        })
    })
}

const FILE_CHANGE_UI_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root { color-scheme: light dark; font: 13px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
body { margin: 0; padding: 12px; background: transparent; color: inherit; }
.card { border: 1px solid color-mix(in srgb, currentColor 18%, transparent); border-radius: 10px; overflow: hidden; }
.head { display: flex; justify-content: space-between; gap: 12px; padding: 10px 12px; align-items: center; }
.title { font-weight: 650; }
.badge { font-size: 12px; opacity: .72; }
.meta { padding: 0 12px 10px; display: grid; gap: 4px; }
.row { display: flex; gap: 8px; min-width: 0; }
.label { opacity: .62; width: 64px; flex: 0 0 auto; }
.value { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow-wrap: anywhere; }
details { border-top: 1px solid color-mix(in srgb, currentColor 14%, transparent); }
summary { cursor: pointer; padding: 9px 12px; user-select: none; }
pre { margin: 0; padding: 10px 12px 12px; max-height: 360px; overflow: auto; white-space: pre; font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.empty { opacity: .62; }
</style>
</head>
<body>
<div class="card">
  <div class="head"><div class="title" id="title">File change</div><div class="badge" id="badge">waiting for result</div></div>
  <div class="meta" id="meta"></div>
  <details id="diffBox" hidden><summary>Unified diff</summary><pre id="diff"></pre></details>
</div>
<script>
(() => {
  const title = document.getElementById('title');
  const badge = document.getElementById('badge');
  const meta = document.getElementById('meta');
  const diffBox = document.getElementById('diffBox');
  const diff = document.getElementById('diff');
  const esc = value => String(value ?? '').replace(/[&<>"']/g, ch => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
  const row = (label, value) => value === undefined || value === null || value === '' ? '' : `<div class="row"><div class="label">${esc(label)}</div><div class="value">${esc(value)}</div></div>`;
  function render(raw) {
    const data = raw && typeof raw === 'object' ? raw : {};
    const target = data.resolved_target?.target || data.target;
    const paths = Array.isArray(data.files) && data.files.length ? data.files.map(x => x.path).filter(Boolean) : [data.path || data.destination].filter(Boolean);
    const changed = data.changed ?? data.moved ?? data.written ?? data.created;
    title.textContent = paths.length > 1 ? `${paths.length} files changed` : (paths[0] ? `File change: ${paths[0].split('/').pop()}` : 'File change');
    badge.textContent = data.written === false ? 'preview only' : changed === false ? 'no change' : 'applied';
    meta.innerHTML = row('target', target) + row('path', paths.join(', ')) + row('old sha', data.old_sha256) + row('new sha', data.new_sha256) + row('bytes', data.bytes);
    const d = typeof data.diff === 'string' ? data.diff : '';
    diffBox.hidden = !d;
    diff.textContent = d;
  }
  let rpcId = 0;
  const pendingRequests = new Map();
  const rpcRequest = (method, params) => new Promise((resolve, reject) => {
    const id = ++rpcId;
    pendingRequests.set(id, { resolve, reject });
    window.parent.postMessage({ jsonrpc: '2.0', id, method, params }, '*');
  });
  window.addEventListener('message', event => {
    const msg = event.data;
    if (msg?.jsonrpc === '2.0' && msg.id !== undefined && pendingRequests.has(msg.id)) {
      const pending = pendingRequests.get(msg.id);
      pendingRequests.delete(msg.id);
      if (msg.error) pending.reject(msg.error);
      else pending.resolve(msg.result);
      return;
    }
    if (msg?.method === 'ui/notifications/tool-result') render(msg.params?.structuredContent || msg.params?.content || msg.params);
  });
  async function initializeBridge() {
    await rpcRequest('ui/initialize', {
      appInfo: { name: 'target-ops-file-change', version: '1.0.0' },
      appCapabilities: {},
      protocolVersion: '2026-01-26',
    });
    window.parent.postMessage({ jsonrpc: '2.0', method: 'ui/notifications/initialized', params: {} }, '*');
  }
  if (window.openai?.toolOutput) render(window.openai.toolOutput);
  else if (window.openai?.toolResponseMetadata?.structuredContent) render(window.openai.toolResponseMetadata.structuredContent);
  initializeBridge().catch(error => console.error('Failed to initialize MCP Apps bridge:', error));
})();
</script>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_reads_file_change_resource() {
        let listed = list_resources();
        assert_eq!(listed["resources"][0]["uri"], FILE_CHANGE_UI_URI);
        assert_eq!(listed["resources"][0]["mimeType"], MCP_APP_MIME_TYPE);

        let read = read_resource(FILE_CHANGE_UI_URI).expect("resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("ui/notifications/tool-result"));
        assert!(html.contains("rpcRequest('ui/initialize'"));
        assert!(html.contains("ui/notifications/initialized"));
        assert!(html.contains("appInfo: { name: 'target-ops-file-change'"));
    }
}
