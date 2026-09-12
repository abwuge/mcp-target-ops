use crate::tooling::tools::FILE_CHANGE_UI_URI;
use serde_json::{json, Value};

pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";

fn file_change_resource_meta(widget_domain: Option<&str>) -> Value {
    let mut meta = json!({
        "ui": {
            "prefersBorder": true,
            "csp": {
                "connectDomains": [],
                "resourceDomains": []
            }
        },
        "openai/widgetDescription": "Reviews file changes like an editor: full content for added/deleted files and color-highlighted diffs for modified files.",
        "openai/widgetPrefersBorder": true,
        "openai/widgetCSP": {
            "connect_domains": [],
            "resource_domains": []
        }
    });

    if let Some(domain) = widget_domain {
        meta["ui"]["domain"] = Value::String(domain.to_string());
        meta["openai/widgetDomain"] = Value::String(domain.to_string());
    }

    meta
}

pub fn list_resources(widget_domain: Option<&str>) -> Value {
    json!({
        "resources": [{
            "uri": FILE_CHANGE_UI_URI,
            "name": "target-ops-file-change",
            "title": "Target Ops file change",
            "description": "Human-readable review of Target Ops file mutations.",
            "mimeType": MCP_APP_MIME_TYPE,
            "_meta": file_change_resource_meta(widget_domain)
        }]
    })
}

pub fn read_resource(uri: &str, widget_domain: Option<&str>) -> Option<Value> {
    (uri == FILE_CHANGE_UI_URI).then(|| {
        json!({
            "contents": [{
                "uri": FILE_CHANGE_UI_URI,
                "mimeType": MCP_APP_MIME_TYPE,
                "text": FILE_CHANGE_UI_HTML,
                "_meta": file_change_resource_meta(widget_domain)
            }]
        })
    })
}

const FILE_CHANGE_UI_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root {
  color-scheme: light dark;
  font: 13px/1.45 ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  --border: color-mix(in srgb, currentColor 16%, transparent);
  --muted: color-mix(in srgb, currentColor 58%, transparent);
  --add-bg: color-mix(in srgb, #2ea043 18%, transparent);
  --add-strong: color-mix(in srgb, #2ea043 30%, transparent);
  --del-bg: color-mix(in srgb, #f85149 18%, transparent);
  --del-strong: color-mix(in srgb, #f85149 30%, transparent);
  --hunk-bg: color-mix(in srgb, #58a6ff 12%, transparent);
}
* { box-sizing: border-box; }
body { margin: 0; padding: 10px; background: transparent; color: inherit; }
.card { border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
.head { display: flex; gap: 10px; padding: 10px 12px; align-items: center; border-bottom: 1px solid var(--border); }
.file { min-width: 0; flex: 1; }
.title { font-weight: 650; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.path { margin-top: 2px; font: 11px/1.35 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; opacity: .58; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.kind, .status { flex: 0 0 auto; border: 1px solid var(--border); border-radius: 999px; padding: 2px 7px; font-size: 11px; }
.kind.added { background: var(--add-bg); }
.kind.deleted { background: var(--del-bg); }
.kind.modified { background: var(--hunk-bg); }
.status { opacity: .68; }
.viewer { max-height: 480px; overflow: auto; background: color-mix(in srgb, currentColor 2%, transparent); }
.file-block + .file-block { border-top: 1px solid var(--border); }
.file-label { position: sticky; top: 0; z-index: 2; padding: 6px 10px; border-bottom: 1px solid var(--border); background: Canvas; font: 11px/1.35 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; opacity: .78; }
.code-line { display: grid; grid-template-columns: 42px 42px minmax(max-content, 1fr); min-height: 20px; font: 12px/20px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.code-line.added { background: var(--add-bg); }
.code-line.removed { background: var(--del-bg); }
.code-line.added .gutter { background: var(--add-strong); }
.code-line.removed .gutter { background: var(--del-strong); }
.code-line.hunk { display: block; padding: 3px 10px; background: var(--hunk-bg); color: var(--muted); font: 11px/18px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.gutter { padding: 0 7px; text-align: right; user-select: none; color: var(--muted); border-right: 1px solid color-mix(in srgb, currentColor 8%, transparent); }
.code { padding: 0 10px; white-space: pre; }
.notice { padding: 14px 12px; color: var(--muted); }
.details { border-top: 1px solid var(--border); }
.details summary { cursor: pointer; padding: 8px 12px; font-size: 12px; user-select: none; color: var(--muted); }
.meta { padding: 0 12px 10px; display: grid; gap: 4px; }
.meta-row { display: grid; grid-template-columns: 70px minmax(0, 1fr); gap: 8px; }
.meta-label { color: var(--muted); }
.meta-value { font: 11px/1.4 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow-wrap: anywhere; }
</style>
</head>
<body>
<div class="card">
  <div class="head">
    <div class="file"><div class="title" id="title">File change</div><div class="path" id="path"></div></div>
    <div class="kind modified" id="kind">Modified</div>
    <div class="status" id="status">waiting</div>
  </div>
  <div class="viewer" id="viewer"><div class="notice">Waiting for file change…</div></div>
  <details class="details" id="details"><summary>Details</summary><div class="meta" id="meta"></div></details>
</div>
<script>
(() => {
  const title = document.getElementById('title');
  const pathEl = document.getElementById('path');
  const kindEl = document.getElementById('kind');
  const statusEl = document.getElementById('status');
  const viewer = document.getElementById('viewer');
  const meta = document.getElementById('meta');
  const details = document.getElementById('details');

  const esc = value => String(value ?? '').replace(/[&<>"']/g, ch => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
  const metaRow = (label, value) => value === undefined || value === null || value === '' ? '' : `<div class="meta-row"><div class="meta-label">${esc(label)}</div><div class="meta-value">${esc(value)}</div></div>`;
  const baseName = path => String(path || '').split('/').filter(Boolean).pop() || 'File change';
  const lineRow = (oldNo, newNo, text, cls = '') => `<div class="code-line ${cls}"><span class="gutter">${oldNo ?? ''}</span><span class="gutter">${newNo ?? ''}</span><span class="code">${esc(text)}</span></div>`;

  function renderFull(content, kind, encoding) {
    const cls = kind === 'deleted' ? 'removed' : 'added';
    const lines = String(content ?? '').split('\n');
    const binary = encoding === 'base64';
    let html = binary ? `<div class="notice">Binary file content shown as base64.</div>` : '';
    html += lines.map((line, i) => lineRow(kind === 'deleted' ? i + 1 : '', kind === 'added' ? i + 1 : '', line, cls)).join('');
    viewer.innerHTML = html;
  }

  function parseHunkHeader(line) {
    const match = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    return match ? { oldStart: Number(match[1]), newStart: Number(match[3]) } : null;
  }

  function renderDiff(diffText) {
    const lines = String(diffText || '').split('\n');
    let oldNo = 0;
    let newNo = 0;
    let html = '';
    let pendingOldPath = '';
    let currentPath = '';

    for (const line of lines) {
      if (line.startsWith('diff --git ')) continue;
      if (line.startsWith('index ')) continue;
      if (line.startsWith('--- ')) {
        pendingOldPath = line.slice(4).replace(/^a\//, '');
        continue;
      }
      if (line.startsWith('+++ ')) {
        const nextPath = line.slice(4).replace(/^b\//, '');
        if (nextPath !== 'after' && nextPath !== '/dev/null' && nextPath !== currentPath) {
          currentPath = nextPath;
          html += `<div class="file-label">${esc(currentPath)}</div>`;
        } else if (nextPath === '/dev/null' && pendingOldPath && pendingOldPath !== 'before' && pendingOldPath !== currentPath) {
          currentPath = pendingOldPath;
          html += `<div class="file-label">${esc(currentPath)}</div>`;
        }
        continue;
      }
      const hunk = parseHunkHeader(line);
      if (hunk) {
        oldNo = hunk.oldStart;
        newNo = hunk.newStart;
        html += `<div class="code-line hunk">Changed lines ${oldNo} → ${newNo}</div>`;
        continue;
      }
      if (line === '\\ No newline at end of file' || line === '') continue;
      if (line.startsWith('-')) {
        html += lineRow(oldNo++, '', line.slice(1), 'removed');
      } else if (line.startsWith('+')) {
        html += lineRow('', newNo++, line.slice(1), 'added');
      } else if (line.startsWith(' ')) {
        html += lineRow(oldNo++, newNo++, line.slice(1));
      }
    }

    viewer.innerHTML = html || '<div class="notice">No textual differences.</div>';
  }

  function inferKind(data) {
    if (Object.prototype.hasOwnProperty.call(data, 'deleted')) return 'deleted';
    if (data.created === true) return 'added';
    if (typeof data.diff === 'string' && data.diff) return 'modified';
    if (data.moved === true) return 'renamed';
    return 'modified';
  }

  function render(raw) {
    const data = raw && typeof raw === 'object' ? raw : {};
    const target = data.resolved_target?.target || data.target;
    const files = Array.isArray(data.files) ? data.files : [];
    const paths = files.length ? files.map(item => item.path).filter(Boolean) : [data.path || data.destination].filter(Boolean);
    const primaryPath = paths[0] || data.source || '';
    const kind = inferKind(data);
    const label = kind === 'added' ? 'Added' : kind === 'deleted' ? 'Deleted' : kind === 'renamed' ? 'Renamed' : 'Modified';

    title.textContent = paths.length > 1 ? `${paths.length} files changed` : baseName(primaryPath);
    pathEl.textContent = paths.length > 1 ? paths.join(' · ') : primaryPath;
    kindEl.textContent = label;
    kindEl.className = `kind ${kind === 'renamed' ? 'modified' : kind}`;
    statusEl.textContent = data.written === false ? 'Preview' : (data.changed === false ? 'No change' : 'Applied');

    if ((kind === 'added' || kind === 'deleted') && typeof data.content === 'string') {
      renderFull(data.content, kind, data.encoding);
    } else if (typeof data.diff === 'string' && data.diff) {
      renderDiff(data.diff);
    } else if (kind === 'renamed') {
      viewer.innerHTML = `<div class="notice">Renamed <strong>${esc(data.source || '')}</strong> to <strong>${esc(data.destination || '')}</strong>.</div>`;
    } else if (data.created === true && data.encoding === 'base64') {
      viewer.innerHTML = '<div class="notice">Binary file added.</div>';
    } else {
      viewer.innerHTML = '<div class="notice">No textual differences.</div>';
    }

    meta.innerHTML =
      metaRow('target', target) +
      metaRow('path', paths.join(', ')) +
      metaRow('bytes', data.bytes) +
      metaRow('encoding', data.encoding) +
      metaRow('old sha', data.old_sha256) +
      metaRow('new sha', data.new_sha256);
    details.hidden = !meta.innerHTML;
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
    if (msg?.method === 'ui/notifications/tool-result') {
      render(msg.params?.structuredContent || msg.params?.content || msg.params);
    }
  });

  async function initializeBridge() {
    await rpcRequest('ui/initialize', {
      appInfo: { name: 'target-ops-file-change', version: '1.1.0' },
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
</html>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_reads_file_change_resource() {
        let widget_domain = "https://mcp.example.com";
        let listed = list_resources(Some(widget_domain));
        assert_eq!(listed["resources"][0]["uri"], FILE_CHANGE_UI_URI);
        assert_eq!(listed["resources"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let listed_meta = &listed["resources"][0]["_meta"];
        assert_eq!(listed_meta["ui"]["prefersBorder"], true);
        assert_eq!(listed_meta["ui"]["domain"], widget_domain);
        assert_eq!(listed_meta["ui"]["csp"]["connectDomains"], json!([]));
        assert_eq!(listed_meta["ui"]["csp"]["resourceDomains"], json!([]));
        assert_eq!(listed_meta["openai/widgetPrefersBorder"], true);
        assert_eq!(listed_meta["openai/widgetDomain"], widget_domain);
        assert_eq!(
            listed_meta["openai/widgetCSP"]["connect_domains"],
            json!([])
        );
        assert_eq!(
            listed_meta["openai/widgetCSP"]["resource_domains"],
            json!([])
        );

        let read = read_resource(FILE_CHANGE_UI_URI, Some(widget_domain)).expect("resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(read["contents"][0]["_meta"], *listed_meta);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("ui/notifications/tool-result"));
        assert!(html.contains("rpcRequest('ui/initialize'"));
        assert!(html.contains("ui/notifications/initialized"));
        assert!(html.contains("appInfo: { name: 'target-ops-file-change'"));
        assert!(html.contains("renderDiff"));
        assert!(html.contains("lineRow(kind === 'deleted'"));
        assert!(html.contains("Binary file content shown as base64"));
    }
    #[test]
    fn omits_widget_domain_without_public_base_url() {
        let listed = list_resources(None);
        let meta = &listed["resources"][0]["_meta"];
        assert!(meta["ui"].get("domain").is_none());
        assert!(meta.get("openai/widgetDomain").is_none());
    }
}
