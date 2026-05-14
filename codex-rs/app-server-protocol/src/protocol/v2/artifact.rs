use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactSourceInfo {
    pub path: PathBuf,
    pub kind: String,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateInfo {
    pub id: i64,
    pub namespace: String,
    pub scope_key: String,
    pub source_key: String,
    pub state_dir: PathBuf,
    pub metadata: JsonValue,
    pub created_at_unix_sec: i64,
    pub updated_at_unix_sec: i64,
    pub last_hit_at_unix_sec: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactFileInfo {
    pub state_id: i64,
    pub relative_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub updated_at_unix_sec: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactFileMatchInfo {
    pub state: ArtifactStateInfo,
    pub file: ArtifactFileInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheEntryInfo {
    pub namespace: String,
    pub key: String,
    pub artifact_id: String,
    pub status: String,
    pub metadata: JsonValue,
    pub created_at_unix_sec: i64,
    pub updated_at_unix_sec: i64,
    pub last_hit_at_unix_sec: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateRegisterParams {
    pub namespace: String,
    pub scope_key: String,
    pub source_key: String,
    pub state_dir: PathBuf,
    pub sources: Vec<ArtifactSourceInfo>,
    pub metadata: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateRegisterResponse {
    pub state: ArtifactStateInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateReadParams {
    pub namespace: String,
    pub scope_key: String,
    pub source_key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateReadResponse {
    pub state: Option<ArtifactStateInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateListParams {
    pub namespace: String,
    pub scope_key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateListResponse {
    pub states: Vec<ArtifactStateInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateHitParams {
    pub namespace: String,
    pub state_dir: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStateHitResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStatePruneParams {
    pub namespace: String,
    pub retention_secs: i64,
    pub throttle_secs: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactStatePruneResponse {
    pub pruned: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactFileIndexParams {
    pub namespace: String,
    pub state_dir: PathBuf,
    pub relative_path: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactFileIndexResponse {
    pub file: ArtifactFileInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactFileFindParams {
    pub namespace: String,
    pub relative_path: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactFileFindResponse {
    pub entry: Option<ArtifactFileMatchInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheReadParams {
    pub namespace: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheReadResponse {
    pub entry: Option<ArtifactCacheEntryInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheWriteParams {
    pub namespace: String,
    pub key: String,
    pub artifact_id: String,
    pub status: String,
    pub metadata: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheWriteResponse {
    pub entry: ArtifactCacheEntryInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheDeleteParams {
    pub namespace: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ArtifactCacheDeleteResponse {}
