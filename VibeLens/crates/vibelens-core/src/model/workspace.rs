use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFile {
    /// Path relative to the workspace root, always `/`-separated.
    pub path: String,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
}
