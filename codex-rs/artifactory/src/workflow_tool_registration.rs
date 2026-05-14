use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowToolRegistrationRecord {
    pub workflow_id: String,
    pub workflow: JsonValue,
    pub tool_name: String,
    pub source_hook: String,
    pub source_digest: String,
    pub published_at_unix_sec: i64,
    pub updated_at_unix_sec: i64,
    pub refresh_after_unix_sec: Option<i64>,
    pub expires_at_unix_sec: Option<i64>,
    pub tool_spec: JsonValue,
}

impl WorkflowToolRegistrationRecord {
    pub fn is_expired(&self, now_unix_sec: i64) -> bool {
        self.expires_at_unix_sec
            .is_some_and(|expires_at| now_unix_sec >= expires_at)
    }

    pub fn is_refresh_due(&self, now_unix_sec: i64) -> bool {
        self.refresh_after_unix_sec
            .is_some_and(|refresh_after| now_unix_sec >= refresh_after)
    }

    pub fn is_stale(&self, now_unix_sec: i64, current_source_digest: &str) -> bool {
        self.is_expired(now_unix_sec)
            || self.is_refresh_due(now_unix_sec)
            || self.source_digest != current_source_digest
    }
}
