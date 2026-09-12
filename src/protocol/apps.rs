use crate::tooling::tools::{
    EXEC_TERMINAL_UI_URI, FILE_CHANGE_UI_URI, FILE_READ_UI_URI, INVENTORY_UI_URI,
};
use serde_json::{json, Value};

pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";

const EXEC_TERMINAL_UI_HTML: &str = include_str!("../../assets/exec-terminal.html");
const FILE_CHANGE_UI_HTML: &str = include_str!("../../assets/file-change.html");
const FILE_READ_UI_HTML: &str = include_str!("../../assets/file-read.html");
const INVENTORY_UI_HTML: &str = include_str!("../../assets/inventory-card.html");

struct AppResource {
    uri: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    widget_description: &'static str,
    html: &'static str,
    prefers_border: bool,
}

const RESOURCES: [AppResource; 4] = [
    AppResource {
        uri: EXEC_TERMINAL_UI_URI,
        name: "target-ops-exec-result",
        title: "Target Ops command result",
        description: "Target Ops command output.",
        widget_description: "Shows the command, live output, target, exit status, timeout, and truncation state.",
        html: EXEC_TERMINAL_UI_HTML,
        prefers_border: true,
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
    AppResource {
        uri: FILE_READ_UI_URI,
        name: "target-ops-file-read",
        title: "Target Ops file read",
        description: "Readable view of one or more Target Ops file reads.",
        widget_description: "Shows file contents with line numbers, line ranges, metadata, truncation state, and compact batch results.",
        html: FILE_READ_UI_HTML,
        prefers_border: true,
    },
    AppResource {
        uri: INVENTORY_UI_URI,
        name: "target-ops-inventory",
        title: "Target Ops inventory",
        description: "Compact overview of configured targets or downstream MCP servers.",
        widget_description: "Shows configured local and SSH targets or downstream MCP servers as a compact status card.",
        html: INVENTORY_UI_HTML,
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
    fn lists_app_resources_with_expected_metadata() {
        let widget_domain = "https://mcp.example.com";
        let listed = list_resources(Some(widget_domain));
        let resources = listed["resources"].as_array().expect("resources array");
        assert_eq!(resources.len(), 4);

        let exec_result = resources
            .iter()
            .find(|resource| resource["uri"] == EXEC_TERMINAL_UI_URI)
            .expect("exec result resource");
        assert_eq!(exec_result["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(exec_result["_meta"]["ui"]["prefersBorder"], true);
        assert_eq!(exec_result["_meta"]["openai/widgetPrefersBorder"], true);
        assert_eq!(exec_result["_meta"]["ui"]["domain"], widget_domain);
        assert_eq!(
            exec_result["_meta"]["ui"]["csp"]["connectDomains"],
            json!([])
        );
        assert_eq!(
            exec_result["_meta"]["ui"]["csp"]["resourceDomains"],
            json!([])
        );

        let file_change = resources
            .iter()
            .find(|resource| resource["uri"] == FILE_CHANGE_UI_URI)
            .expect("file change resource");
        assert_eq!(file_change["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(file_change["_meta"]["ui"]["prefersBorder"], true);
        assert_eq!(file_change["_meta"]["openai/widgetPrefersBorder"], true);
        assert_eq!(file_change["_meta"]["openai/widgetDomain"], widget_domain);

        let file_read = resources
            .iter()
            .find(|resource| resource["uri"] == FILE_READ_UI_URI)
            .expect("file read resource");
        assert_eq!(file_read["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(file_read["_meta"]["ui"]["prefersBorder"], true);
        assert_eq!(file_read["_meta"]["openai/widgetPrefersBorder"], true);
        assert_eq!(file_read["_meta"]["openai/widgetDomain"], widget_domain);

        let inventory = resources
            .iter()
            .find(|resource| resource["uri"] == INVENTORY_UI_URI)
            .expect("inventory resource");
        assert_eq!(inventory["mimeType"], MCP_APP_MIME_TYPE);
        assert_eq!(inventory["_meta"]["ui"]["prefersBorder"], true);
        assert_eq!(inventory["_meta"]["openai/widgetPrefersBorder"], true);
        assert_eq!(inventory["_meta"]["openai/widgetDomain"], widget_domain);
    }

    #[test]
    fn reads_exec_result_resource() {
        let read = read_resource(EXEC_TERMINAL_UI_URI, Some("https://mcp.example.com"))
            .expect("exec result resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("ui/notifications/tool-result"));
        assert!(html.contains("rpcRequest('ui/initialize'"));
        assert!(html.contains("ui/notifications/initialized"));
        assert!(html.contains("appInfo: { name: 'target-ops-exec-result'"));
        assert!(html.contains("Command result"));
        assert!(html.contains("job_output"));
        assert!(html.contains("result_read"));
        assert!(html.contains("scheduleLifecycle"));
        assert!(html.contains("data.command"));
        assert!(html.contains("renderAnsi"));
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
    fn reads_file_read_resource() {
        let read = read_resource(FILE_READ_UI_URI, Some("https://mcp.example.com"))
            .expect("file read resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("appInfo: { name: 'target-ops-file-read'"));
        assert!(html.contains("renderSingle"));
        assert!(html.contains("renderBatch"));
        assert!(html.contains("lineRows"));
        assert!(html.contains("requested_count"));
    }

    #[test]
    fn reads_inventory_resource() {
        let read = read_resource(INVENTORY_UI_URI, Some("https://mcp.example.com"))
            .expect("inventory resource exists");
        assert_eq!(read["contents"][0]["mimeType"], MCP_APP_MIME_TYPE);
        let html = read["contents"][0]["text"].as_str().unwrap();
        assert!(html.contains("appInfo: { name: 'target-ops-inventory'"));
        assert!(html.contains("renderTargets"));
        assert!(html.contains("renderServers"));
        assert!(!html.contains("This view is read-only"));
        assert!(!html.contains("Endpoint URLs and secret values are not exposed"));
    }

    #[test]
    fn historical_app_cards_boot_collapsed_without_rearming_lifecycle() {
        for html in [
            EXEC_TERMINAL_UI_HTML,
            FILE_CHANGE_UI_HTML,
            FILE_READ_UI_HTML,
            INVENTORY_UI_HTML,
        ] {
            assert!(!html.contains("class=\"card\" id=\"card\" open"));
            assert!(!html.contains("class=\"card\" id=\"execution\" open"));
            assert!(html.contains("toolOutput"));
            assert!(html.contains(", false)"));
            assert!(html.contains("scheduleSleepOnly"));
        }
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
