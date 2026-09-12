mod catalog;
mod dispatch;
mod schema;

pub(crate) const FILE_CHANGE_UI_URI: &str = "ui://target-ops/file-change/v1.html";

pub use catalog::list_tools;
pub use dispatch::call_tool;
