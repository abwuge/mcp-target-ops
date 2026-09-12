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

const FILE_CHANGE_UI_HTML: &str = include_str!("../../assets/file-change.html");

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
