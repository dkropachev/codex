//! Diffs the one-level Responses Lite catalog: namespaces of callable tools and built-ins.
//! Missing state starts a fresh catalog; this section does not migrate legacy history.
//! Namespace header hashes track whether guidance needs separate text fragments.
//! Whole namespace removals subsume their members in the removal notice.
//! Metrics count rendered tool deltas and measure replacement description bytes.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateUpdate;
use super::tool_declaration_batches::declaration_batches;
use super::tool_declaration_batches::namespace_instructions_need_text;
use super::tool_instructions::ToolInstructions;
use crate::context::ContextualUserFragment;
use codex_code_mode::ExecDefinition;
use codex_code_mode::PUBLIC_TOOL_NAME;
use codex_extension_api::ExtensionMetrics;
use codex_otel::EXEC_TOOL_ENTRY_UPDATES_METRIC;
use codex_otel::EXEC_TOOL_UPDATE_BYTES_METRIC;
use codex_otel::TOOL_INCREMENTAL_UPDATES_METRIC;
use codex_otel::TOOL_SPEC_DESCRIPTION_BYTES_METRIC;
use codex_otel::TOOL_SPEC_DESCRIPTION_BYTES_METRIC_BUCKETS;
use codex_prompts::ResolvedIncrementalToolMessages;
use codex_protocol::DEFAULT_FUNCTION_NAMESPACE;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::input_text;
use codex_utils_string::take_bytes_at_char_boundary;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

const REMOVAL_NOTICE_BYTES: usize = 768;
const REMOVED_NAME_PART_BYTES: usize = 512;
const ACTION_ADDED: &str = "added";
const ACTION_REMOVED: &str = "removed";
const ACTION_SCHEMA_CHANGED: &str = "schema_changed";

/// The exact serialized Responses Lite declarations visible in one captured step.
pub(crate) struct TopLevelToolsState {
    messages: ResolvedIncrementalToolMessages,
    definitions: Vec<Value>,
    hashes: BTreeMap<String, WorldStateHash>,
    metrics: Option<Arc<dyn ExtensionMetrics>>,
    /// Whether this captured step uses granular exec catalog updates.
    incremental_exec_tools: bool,
    /// Diffable entries for the current CodeModeOnly catalog; absent in other tool modes.
    exec_definitions: Option<Arc<Vec<ExecDefinition>>>,
}

/// Ordinary catalogs keep their landed flat format; exec mode adds its own comparison state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum TopLevelToolsSnapshot {
    WithExec {
        tools: BTreeMap<String, WorldStateHash>,
        /// Stable entry keys and definition hashes, retained while exec is unavailable.
        exec: BTreeMap<String, WorldStateHash>,
    },
    ToolsOnly(BTreeMap<String, WorldStateHash>),
}

impl TopLevelToolsState {
    #[expect(
        clippy::expect_used,
        reason = "the Responses Lite serializer emits objects with array-valued namespace members"
    )]
    pub(crate) fn new(
        messages: ResolvedIncrementalToolMessages,
        definitions: Vec<Value>,
        metrics: Option<Arc<dyn ExtensionMetrics>>,
    ) -> Result<Self> {
        declaration_batches(&definitions)?;
        let mut hashes = BTreeMap::new();
        let mut insert_hash = |name: String, definition: &Value| -> Result<()> {
            let hash = WorldStateHash::from_json(definition);
            if hashes.insert(name.clone(), hash).is_some() {
                return Err(CodexErrorDetails::ToolCollision(name).into());
            }
            Ok(())
        };
        for definition in &definitions {
            let name = definition_name(definition);
            let mut header = definition.clone();
            if let Some(tools) = header
                .as_object_mut()
                .expect("serialized tool object")
                .remove("tools")
            {
                for tool in tools.as_array().expect("serialized namespace members") {
                    insert_hash(format!("{name}.{}", definition_name(tool)), tool)?;
                }
            }
            if namespace_instructions_need_text(definition) {
                // A growing member can move unchanged guidance out of the native header.
                // Distinguish that representation, including snapshots from older rollouts,
                // so the transition appends the complete guidance once.
                header = serde_json::json!({"declaration": header, "instructions_in_text": true});
            }
            insert_hash(name.to_string(), &header)?;
        }
        Ok(Self {
            messages,
            definitions,
            hashes,
            metrics,
            incremental_exec_tools: false,
            exec_definitions: None,
        })
    }

    /// Track granular exec updates as part of this tool catalog.
    pub(crate) fn with_incremental_exec_tools(
        mut self,
        exec_definitions: Option<Arc<Vec<ExecDefinition>>>,
    ) -> Self {
        self.incremental_exec_tools = true;
        self.exec_definitions = exec_definitions;
        self
    }
}

// Input comes from create_tools_json_for_responses_lite: namespaces and their members have
// names; built-ins are identified by type. Namespace members cannot contain namespaces.
#[expect(
    clippy::expect_used,
    reason = "the Responses Lite serializer gives every declaration a name or type"
)]
fn definition_name(definition: &Value) -> &str {
    definition["name"]
        .as_str()
        .unwrap_or_else(|| definition["type"].as_str().expect("serialized tool type"))
}

fn record_changed_tool_metrics(
    name: &str,
    definition: &Value,
    previous: Option<&BTreeMap<String, WorldStateHash>>,
    metrics: Option<&dyn ExtensionMetrics>,
) {
    const ACTION_TAG: &str = "action";
    const TOOL_TAG: &str = "tool";

    let (Some(previous), Some(metrics)) = (previous, metrics) else {
        return;
    };
    let action = if previous.contains_key(name) {
        ACTION_SCHEMA_CHANGED
    } else {
        ACTION_ADDED
    };
    metrics.counter(
        TOOL_INCREMENTAL_UPDATES_METRIC,
        /*inc*/ 1,
        &[(ACTION_TAG, action), (TOOL_TAG, name)],
    );
    if action == ACTION_SCHEMA_CHANGED
        && let Some(description) = definition["description"].as_str()
    {
        metrics.histogram_with_boundaries(
            TOOL_SPEC_DESCRIPTION_BYTES_METRIC,
            i64::try_from(description.len()).unwrap_or(i64::MAX),
            TOOL_SPEC_DESCRIPTION_BYTES_METRIC_BUCKETS.as_slice(),
            &[(TOOL_TAG, name)],
        );
    }
}

impl WorldStateSection for TopLevelToolsState {
    const ID: &'static str = "top_level_tools";
    type Snapshot = TopLevelToolsSnapshot;

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let (previous, previous_exec) = match previous {
            PreviousSectionState::Known(TopLevelToolsSnapshot::WithExec { tools, exec }) => {
                (Some(tools), Some(exec))
            }
            PreviousSectionState::Known(TopLevelToolsSnapshot::ToolsOnly(tools)) => {
                (Some(tools), None)
            }
            PreviousSectionState::Absent | PreviousSectionState::Unknown => (None, None),
        };
        // Only an advertised CodeModeOnly exec declaration gets granular updates.
        let exec_catalog = self.exec_definitions.as_ref().and_then(|entries| {
            self.definitions.iter().find_map(|namespace| {
                if namespace["type"] != "namespace"
                    || namespace["name"] != DEFAULT_FUNCTION_NAMESPACE
                {
                    return None;
                }
                let exec = namespace["tools"]
                    .as_array()?
                    .iter()
                    .find(|tool| tool["type"] == "custom" && tool["name"] == PUBLIC_TOOL_NAME)?;
                Some((namespace, exec, entries.as_slice()))
            })
        });
        let manages_exec = exec_catalog.is_some();
        let mut updates = self.render_declaration_updates(previous, manages_exec);
        self.append_removed_tools(previous, &mut updates);
        let snapshot = if self.incremental_exec_tools {
            let exec =
                self.append_exec_updates(previous, previous_exec, exec_catalog, &mut updates);
            TopLevelToolsSnapshot::WithExec {
                tools: self.hashes.clone(),
                exec,
            }
        } else {
            TopLevelToolsSnapshot::ToolsOnly(self.hashes.clone())
        };
        // Persist empty catalogs as known comparison baselines.
        (Some(snapshot), updates)
    }
}

impl TopLevelToolsState {
    /// Render added or changed declarations and namespace instruction updates.
    #[expect(
        clippy::expect_used,
        reason = "new() validates every atomic declaration before diffing"
    )]
    fn render_declaration_updates(
        &self,
        previous: Option<&BTreeMap<String, WorldStateHash>>,
        manages_exec: bool,
    ) -> Vec<WorldStateUpdate> {
        let changed =
            |name: &str| previous.and_then(|previous| previous.get(name)) != self.hashes.get(name);
        let mut namespace_updates = Vec::new();
        let mut tools = Vec::new();
        // Tool declarations: collect changed standalone tools and namespace members.
        for definition in &self.definitions {
            let name = definition_name(definition);
            let Some(members) = definition["tools"].as_array() else {
                if changed(name) {
                    record_changed_tool_metrics(
                        name,
                        definition,
                        previous,
                        self.metrics.as_deref(),
                    );
                    tools.push(definition.clone());
                }
                continue;
            };
            let mut changed_members = Vec::new();
            for tool in members {
                let tool_name = format!("{name}.{}", definition_name(tool));
                if manages_exec && previous.is_some() && tool_name == "functions.exec" {
                    continue;
                }
                if changed(&tool_name) {
                    record_changed_tool_metrics(
                        &tool_name,
                        tool,
                        previous,
                        self.metrics.as_deref(),
                    );
                    changed_members.push(tool.clone());
                }
            }
            let needs_text_update = changed_members.is_empty();
            let large_namespace_instructions = namespace_instructions_need_text(definition);
            if !changed_members.is_empty() {
                let mut namespace = definition.clone();
                namespace["tools"] = Value::Array(changed_members);
                if large_namespace_instructions {
                    namespace["description"] = Value::String(String::new());
                }
                tools.push(namespace);
            }
            if changed(name)
                && (needs_text_update || large_namespace_instructions)
                && (previous.is_some() || !manages_exec)
            {
                // Namespace declarations require a member. Update metadata as text without
                // repeating unchanged tool definitions.
                let instructions = definition["description"].as_str().unwrap_or_default();
                let text = if instructions.is_empty() {
                    self.messages
                        .namespace_instructions_cleared
                        .replace("{name}", name)
                } else {
                    let mut text = self
                        .messages
                        .namespace_instructions_prefix
                        .replace("{name}", name);
                    text.push_str(instructions);
                    text
                };
                if !text.is_empty() {
                    namespace_updates.push(
                        WorldStateUpdate::fragment(ToolInstructions::new(
                            text,
                            name.to_string().into(),
                        ))
                        .standalone(),
                    );
                }
            }
        }
        let mut updates = Vec::new();
        if !tools.is_empty() {
            // Incremental tool updates: introduce an appended batch of namespace declarations.
            if previous.is_some()
                && tools.iter().any(|tool| tool["type"] == "namespace")
                && !self.messages.tool_update_hint.is_empty()
            {
                updates.push(WorldStateUpdate::prefix_item(ContextualUserFragment::into(
                    IncrementalToolsUpdate(self.messages.tool_update_hint.clone()),
                )));
            }
            // Every delta is a subset of the declarations validated by new().
            let batches = declaration_batches(&tools).expect("validated tool declarations");
            updates.extend(batches.into_iter().map(|tools| {
                WorldStateUpdate::prefix_item(ResponseItem::AdditionalTools {
                    id: None,
                    role: "developer".to_string(),
                    tools: Arc::from(tools),
                })
            }));
        }
        updates.extend(namespace_updates);
        updates
    }

    /// Append a removal notice, listing removed namespaces instead of their individual members.
    fn append_removed_tools(
        &self,
        previous: Option<&BTreeMap<String, WorldStateHash>>,
        updates: &mut Vec<WorldStateUpdate>,
    ) {
        if let Some(previous) = previous {
            // Removal notices: withdraw namespaces and tools absent from the current catalog.
            // Namespace and member names cannot contain dots. A qualified key identifies
            // its old namespace without changing the persisted header/member hash format.
            let namespaces = previous
                .keys()
                .filter_map(|key| key.split_once('.').map(|(namespace, _)| namespace))
                .filter(|namespace| {
                    previous.contains_key(*namespace) && !self.hashes.contains_key(*namespace)
                })
                .collect::<BTreeSet<_>>();
            let tools = previous
                .keys()
                .filter(|key| !self.hashes.contains_key(*key))
                .filter(|key| !namespaces.contains(key.as_str()))
                .inspect(|name| {
                    // Count members even when the notice collapses their removed namespace.
                    if let Some(metrics) = self.metrics.as_deref() {
                        metrics.counter(
                            TOOL_INCREMENTAL_UPDATES_METRIC,
                            /*inc*/ 1,
                            &[("action", ACTION_REMOVED), ("tool", name.as_str())],
                        );
                    }
                })
                .filter(|key| {
                    !key.split_once('.')
                        .is_some_and(|(namespace, _)| namespaces.contains(namespace))
                })
                .cloned()
                .collect::<Vec<_>>();
            if !namespaces.is_empty() || !tools.is_empty() {
                updates.extend(
                    RemovedTools {
                        messages: self.messages.clone(),
                        namespaces: namespaces.into_iter().map(str::to_string).collect(),
                        tools,
                    }
                    .into_updates(),
                );
            }
        }
    }

    /// Append exec catalog updates and return the matching comparison snapshot.
    fn append_exec_updates(
        &self,
        previous: Option<&BTreeMap<String, WorldStateHash>>,
        previous_exec: Option<&BTreeMap<String, WorldStateHash>>,
        exec_catalog: Option<(&Value, &Value, &[ExecDefinition])>,
        updates: &mut Vec<WorldStateUpdate>,
    ) -> BTreeMap<String, WorldStateHash> {
        // Retain entry hashes while functions.exec is not advertised.
        // When it returns, send the full current catalog and withdraw removed entries.
        let mut exec = BTreeMap::new();
        if let Some((namespace, declaration, entries)) = exec_catalog {
            exec = entries
                .iter()
                .map(|entry| {
                    (
                        entry.key.clone(),
                        WorldStateHash::from_json(&Value::String(entry.description.clone())),
                    )
                })
                .collect();
            if previous_exec.is_some()
                && previous.is_some_and(|tools| tools.contains_key("functions.exec"))
            {
                // Incremental tool updates: append entries; the exec declaration stays frozen.
                let changed = entries
                    .iter()
                    .filter(|entry| {
                        previous_exec.and_then(|previous| previous.get(&entry.key))
                            != exec.get(&entry.key)
                    })
                    .collect::<Vec<_>>();
                if !changed.is_empty() {
                    let update =
                        render_exec_update(&changed, &self.messages.exec_tools_update_hint);
                    if let Some(metrics) = self.metrics.as_deref() {
                        let added = changed
                            .iter()
                            .filter(|entry| {
                                !previous_exec
                                    .is_some_and(|previous| previous.contains_key(&entry.key))
                            })
                            .count();
                        for (action, count) in [
                            (ACTION_ADDED, added),
                            (ACTION_SCHEMA_CHANGED, changed.len() - added),
                        ] {
                            if count > 0 {
                                metrics.counter(
                                    EXEC_TOOL_ENTRY_UPDATES_METRIC,
                                    i64::try_from(count).unwrap_or(i64::MAX),
                                    &[("action", action)],
                                );
                            }
                        }
                        metrics.histogram_with_boundaries(
                            EXEC_TOOL_UPDATE_BYTES_METRIC,
                            i64::try_from(update.0.len()).unwrap_or(i64::MAX),
                            TOOL_SPEC_DESCRIPTION_BYTES_METRIC_BUCKETS.as_slice(),
                            &[],
                        );
                    }
                    updates.push(WorldStateUpdate::fragment(update).standalone());
                }
            } else if previous.is_some() {
                // Install the full catalog when enabling granular updates or restoring exec.
                if previous_exec.is_some()
                    && let Some(metrics) = self.metrics.as_deref()
                {
                    metrics.counter(
                        TOOL_INCREMENTAL_UPDATES_METRIC,
                        /*inc*/ 1,
                        &[("action", ACTION_ADDED), ("tool", "functions.exec")],
                    );
                }
                let mut namespace = namespace.clone();
                namespace["tools"] = Value::Array(vec![declaration.clone()]);
                updates.push(WorldStateUpdate::prefix_item(
                    ResponseItem::AdditionalTools {
                        id: None,
                        role: "developer".to_string(),
                        tools: Arc::from(vec![namespace]),
                    },
                ));
            }
            // Removal notices: include exec entries removed while it was unavailable.
            if let Some(previous) = previous_exec {
                let removed = previous
                    .keys()
                    .filter(|key| !exec.contains_key(*key))
                    .cloned()
                    .collect::<Vec<_>>();
                if !removed.is_empty() {
                    if let Some(metrics) = self.metrics.as_deref() {
                        metrics.counter(
                            EXEC_TOOL_ENTRY_UPDATES_METRIC,
                            i64::try_from(removed.len()).unwrap_or(i64::MAX),
                            &[("action", ACTION_REMOVED)],
                        );
                    }
                    updates.push(
                        WorldStateUpdate::fragment(RemovedExecDefinitions {
                            header: self.messages.exec_tools_removed_definitions_header.clone(),
                            names: removed,
                        })
                        .standalone(),
                    );
                }
            }
        } else if let Some(previous) = previous_exec {
            exec = previous.clone();
        }
        exec
    }
}

/// An appended catalog hint or exec definition update, retained or discarded with its baseline.
pub(crate) struct IncrementalToolsUpdate(pub(crate) String);

impl IncrementalToolsUpdate {
    pub(crate) const KIND: &str = "tools.incremental_update";
}

impl ContextualUserFragment for IncrementalToolsUpdate {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(Self::KIND.to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        input_text(self.0.clone())
    }
}

#[derive(Default)]
struct RemovedTools {
    messages: ResolvedIncrementalToolMessages,
    namespaces: Vec<String>,
    tools: Vec<String>,
}

#[derive(Clone, Copy)]
enum RemovedIdentityKind {
    Namespace,
    Tool,
}

impl RemovedTools {
    fn into_updates(self) -> Vec<WorldStateUpdate> {
        let mut updates = Vec::new();
        let mut notice = Self {
            messages: self.messages.clone(),
            ..Self::default()
        };
        let mut notice_bytes = 0;
        for (header, names, kind) in [
            (
                self.messages.removed_namespaces_header.as_str(),
                self.namespaces,
                RemovedIdentityKind::Namespace,
            ),
            (
                self.messages.removed_tools_header.as_str(),
                self.tools,
                RemovedIdentityKind::Tool,
            ),
        ] {
            for name in names {
                if name.len() > REMOVAL_NOTICE_BYTES - header.len() - 3 {
                    if notice_bytes != 0 {
                        updates.push(
                            WorldStateUpdate::fragment(std::mem::replace(
                                &mut notice,
                                Self {
                                    messages: self.messages.clone(),
                                    ..Self::default()
                                },
                            ))
                            .standalone(),
                        );
                        notice_bytes = 0;
                    }
                    let mut remaining = name.as_str();
                    let mut payloads = Vec::new();
                    while !remaining.is_empty() {
                        let payload =
                            take_bytes_at_char_boundary(remaining, REMOVED_NAME_PART_BYTES);
                        payloads.push(payload);
                        remaining = &remaining[payload.len()..];
                    }
                    for (index, payload) in payloads.iter().enumerate() {
                        updates.push(
                            WorldStateUpdate::fragment(RemovedIdentityPart {
                                kind,
                                part: index + 1,
                                parts: payloads.len(),
                                payload: (*payload).to_string(),
                            })
                            .standalone(),
                        );
                    }
                    continue;
                }
                let section_is_empty = match kind {
                    RemovedIdentityKind::Namespace => notice.namespaces.is_empty(),
                    RemovedIdentityKind::Tool => notice.tools.is_empty(),
                };
                let header_bytes = if section_is_empty {
                    header.len() + usize::from(notice_bytes != 0) * 2
                } else {
                    0
                };
                let entry_bytes = 3 + name.len();
                if notice_bytes != 0
                    && notice_bytes + header_bytes + entry_bytes > REMOVAL_NOTICE_BYTES
                {
                    updates.push(
                        WorldStateUpdate::fragment(std::mem::replace(
                            &mut notice,
                            Self {
                                messages: self.messages.clone(),
                                ..Self::default()
                            },
                        ))
                        .standalone(),
                    );
                    notice_bytes = 0;
                }
                let section = match kind {
                    RemovedIdentityKind::Namespace => &mut notice.namespaces,
                    RemovedIdentityKind::Tool => &mut notice.tools,
                };
                if section.is_empty() {
                    notice_bytes += header.len() + usize::from(notice_bytes != 0) * 2;
                }
                notice_bytes += entry_bytes;
                section.push(name);
            }
        }
        if notice_bytes != 0 {
            updates.push(WorldStateUpdate::fragment(notice).standalone());
        }
        updates
    }
}

/// One consecutive, lossless part of an identity that cannot fit a normal notice.
struct RemovedIdentityPart {
    kind: RemovedIdentityKind,
    part: usize,
    parts: usize,
    payload: String,
}

impl ContextualUserFragment for RemovedIdentityPart {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("tools.removed_definition".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        let (kind, instruction) = match self.kind {
            RemovedIdentityKind::Namespace => (
                "namespace",
                "Do not call tools in it unless later redeclared.",
            ),
            RemovedIdentityKind::Tool => ("tool", "Do not call it unless later redeclared."),
        };
        let Self {
            part,
            parts,
            payload,
            ..
        } = self;
        let bytes = payload.len();
        input_text(format!(
            "Removed {kind} name, part {part}/{parts}. Join these {parts} consecutive payloads without separators to obtain its exact name. {instruction}\nPayload ({bytes} UTF-8 bytes):\n{payload}"
        ))
    }
}

impl ContextualUserFragment for RemovedTools {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("tools.removed_definition".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    /// Lists removed namespaces and individual tools in separate sections of one notice.
    fn body(&self) -> ContentItem {
        let sections = [
            (
                self.messages.removed_namespaces_header.as_str(),
                &self.namespaces,
            ),
            (self.messages.removed_tools_header.as_str(), &self.tools),
        ];
        let mut body = String::new();
        for (header, names) in sections {
            if names.is_empty() {
                continue;
            }
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(header);
            for name in names {
                body.push_str("\n- ");
                body.push_str(name);
            }
        }
        input_text(body)
    }
}

/// Render a nonempty batch of complete replacement definitions with the exec update hint.
fn render_exec_update(definitions: &[&ExecDefinition], hint: &str) -> IncrementalToolsUpdate {
    let description = definitions
        .iter()
        .map(|entry| entry.description.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut text = hint.to_owned();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str("## Namespace: functions\n");
    text.push_str(&description);
    IncrementalToolsUpdate(text)
}

/// Removal notice containing the stable keys of withdrawn tools or namespace instructions.
struct RemovedExecDefinitions {
    /// Guidance preceding the removed keys; an empty header emits only the keys.
    header: String,
    /// Stable entry keys that are no longer present in the current catalog.
    names: Vec<String>,
}

impl ContextualUserFragment for RemovedExecDefinitions {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("tools.removed_definition".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        let mut text = self.header.clone();
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("- ");
        text.push_str(&self.names.join("\n- "));
        input_text(text)
    }
}

#[cfg(test)]
#[path = "top_level_tools_tests.rs"]
mod tests;
