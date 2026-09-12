use crate::tooling::tools::{EXEC_TERMINAL_UI_URI, FILE_CHANGE_UI_URI};
use serde_json::{json, Value};

pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";

const EXEC_TERMINAL_UI_HTML: &str = include_str!("../../assets/exec-terminal.html");
const FILE_CHANGE_UI_HTML: &str = include_str!("../../assets/file-change.html");

struct AppResource {
    uri: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    widget_description: &'static str,
    html: &'static str,
    prefers_border: bool,
}

const RESOURCES: [AppResource; 2] = [
    AppResource {
        uri: EXEC_TERMINAL_UI_URI,
        name: "target-ops-exec-terminal",
        title: "Target Ops command output",
        description: "Terminal-style view of Target Ops exec results.",
        widget_description: "Shows command output in a compact terminal-style view with stdout, stderr, exit status, target, timeout, and truncation state.",
        html: EXEC_TERMINAL_UI_HTML,
        prefers_border: false,
    },
    AppResource {
        uri: FILE_CHANGE_UI_URI,
        name: "target-ops-file-change",
        title: "Target Ops file change",
        description: "Human-readable review of Target Ops file mutations.",
        widget_description: "Reviews file changes like an editor: full content for added/deleted files and color-highlighted diffs for modified files.",
        html: FILE_CHANGE_UI_HTML,
        prefers_border: true,
    },
];

fn resource_meta(resource: &AppResource, widget_domain: Option<&str>) -> Value {
    let mut meta = json!({
        "ui": {
            "prefersBorder": resource.prefers_border,
            "csp": {
                "connectDomains": [],
                "resourceDomains": []
            }
        },
        "openai/widgetDescription": resource.widget_description,
        "openai/widgetPrefersBorder": resource.prefers_border,
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
    let resources = RESOURCES
        .iter()
        .map(|resource| {
            json!({
                "uri": resource.uri,
                "name": resource.name,
                "title": resource.title,
                "description": resource.description,
                "mimeType": MCP_APP_MIME_TYPE,
                "_meta": resource_meta(resource, widget_domain)
            })
        })
        .collect::<Vec<_>>();

    json!({ "resources": resources })
}

pub fn read_resource(uri: &str, widget_domain: Option<&str>) -> Option<Value> {
    let resource = RESOURCES.iter().find(|resource| resource.uri == uri)?;
    Some(json!({
        "contents": [{
            "uri": resource.uri,
            "mimeType": MCP_APP_MIME_TYPE,
            "text": resource.html,
            "_meta": resource_meta(resource, widget_domain)
        }]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_both_app_resources_with_expected_metadata() {
        let widget_domain = "https://mcp.example.com";
        let listed = list_resources(Some(widget_domain));
        let resources = listed["resources"].as_array().expect("resources array");
        assert_eq!(resources.len(), 2);

        let terminal = resources
            .iter()
            .find(|resource| resource["uri"] == EXEC_TERMINAL_UI_URI)
            .expect("terminal resource");
        assert_eq!(terminal["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(terminal["_meta"]["ui"]["prefersBorder"], false);
        assert_eq!(terminal["_meta"]["openai/widgetPrefersBorder"], false);
        assert_eq!(terminal["_meta"]["ui"]["domain"], widget_domain);
        assert_eq!(terminal["_meta"]["ui"]["csp"]["connectDomains"], json!([]));
        assert_eq!(terminal["_meta"]["ui"]["csp"]["resourceDomains"], json!([]));

        let file_change = resources
            .iter()
            .find(|resource| resource["uri"] == FILE_CHANGE_UI_URI)
            .expect("file change resource");
        assert_eq!(file_change["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(file_change["_meta"]["ui"]["prefersBorder"], true);
        assert_eq!(file_change["_meta"]["openai/widgetPrefersBorder"], true);
        assert_eq!(file_change["_meta"]["openai/widgetDomain"], widget_domain);
    }

    #[test]
    fn reads_exec_terminal_resource() {
        let read = read_resource(EXEC_TERMINAL_UI_URI, Some("https://mcp.example.com"))
            .expect("terminal resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("ui/notifications/tool-result"));
        assert!(html.contains("rpcRequest('ui/initialize'"));
        assert!(html.contains("ui/notifications/initialized"));
        assert!(html.contains("appInfo: { name: 'target-ops-exec-terminal'"));
        assert!(html.contains("stdout_truncated"));
        assert!(html.contains("stderr_truncated"));
        assert!(html.contains("timed_out"));
    }

    #[test]
    fn reads_file_change_resource() {
        let read = read_resource(FILE_CHANGE_UI_URI, Some("https://mcp.example.com"))
            .expect("file resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("appInfo: { name: 'target-ops-file-change'"));
        assert!(html.contains("renderDiff"));
        assert!(html.contains("lineRow(kind === 'deleted'"));
        assert!(html.contains("Binary file content shown as base64"));
    }

    #[test]
    fn omits_widget_domain_without_public_base_url() {
        let listed = list_resources(None);
        for resource in listed["resources"].as_array().unwrap() {
            let meta = &resource["_meta"];
            assert!(meta["ui"].get("domain").is_none());
            assert!(meta.get("openai/widgetDomain").is_none());
        }
    }
}
