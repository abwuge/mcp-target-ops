mod catalog;
mod dispatch;
mod schema;

pub(crate) const EXEC_TERMINAL_UI_URI: &str = "ui://target-ops/exec-terminal/v1.html";
pub(crate) const FILE_CHANGE_UI_URI: &str = "ui://target-ops/file-change/v1.html";
pub(crate) const FILE_READ_UI_URI: &str = "ui://target-ops/file-read/v1.html";
pub(crate) const INVENTORY_UI_URI: &str = "ui://target-ops/inventory/v1.html";

pub use catalog::list_tools;
pub use dispatch::call_tool_with_context;
