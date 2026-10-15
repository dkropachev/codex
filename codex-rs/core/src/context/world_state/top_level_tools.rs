//! Diffs the one-level Responses Lite catalog: namespaces of callable tools and built-ins.
//! Missing state starts a fresh catalog; this section does not migrate legacy history.
//! Incremental hints decorate emitted declarations only, leaving catalog hashes unchanged.
//! Whole namespace removals subsume their members in the removal notice.

use super::PreviousSectionState;
use super::SectionTransition;
use super::WorldStateHash;
use super::WorldStateSection;
use super::WorldStateUpdate;
use super::tool_instructions::MAX_ITEM_BYTES;
use super::tool_instructions::ToolInstructions;
use crate::context::ContextualUserFragment;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use codex_utils_string::take_bytes_at_char_boundary;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const NAMESPACE_UPDATE_HINT: &str = "This is an incremental namespace update. Previously declared tools remain available for direct calls unless explicitly marked unavailable. If a tool is redefined here, its latest definition replaces the earlier one.";
const REMOVED_TOOLS_HEADER: &str = "The following tools are no longer available. Do not call them:";
const REMOVED_NAMESPACES_HEADER: &str = "The following namespaces are no longer available. Do not call tools in them unless those tools are declared in a later update:";
const REMOVAL_NOTICE_BYTES: usize = 768;
const REMOVED_NAME_PART_BYTES: usize = 512;

/// The exact serialized Responses Lite declarations visible in one captured step.
pub(crate) struct TopLevelToolsState {
    definitions: Vec<Value>,
    hashes: BTreeMap<String, WorldStateHash>,
}

impl TopLevelToolsState {
    #[expect(
        clippy::expect_used,
        reason = "the Responses Lite serializer emits objects with array-valued namespace members"
    )]
    pub(crate) fn new(definitions: Vec<Value>) -> Result<Self> {
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
            insert_hash(name.to_string(), &header)?;
        }
        Ok(Self {
            definitions,
            hashes,
        })
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

impl WorldStateSection for TopLevelToolsState {
    const ID: &'static str = "top_level_tools";
    type Snapshot = BTreeMap<String, WorldStateHash>;

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> SectionTransition<Self::Snapshot> {
        let previous = match previous {
            PreviousSectionState::Known(previous) => Some(previous),
            PreviousSectionState::Absent | PreviousSectionState::Unknown => None,
        };
        let changed =
            |name: &str| previous.and_then(|previous| previous.get(name)) != self.hashes.get(name);
        let mut namespace_updates = Vec::new();
        let mut decorated_namespaces = Vec::new();
        let mut tools = Vec::new();
        for definition in &self.definitions {
            let name = definition_name(definition);
            let Some(members) = definition["tools"].as_array() else {
                if changed(name) {
                    tools.push(definition.clone());
                }
                continue;
            };
            let members = members
                .iter()
                .filter(|tool| changed(&format!("{name}.{}", definition_name(tool))))
                .cloned()
                .collect::<Vec<_>>();
            if !members.is_empty() {
                let mut namespace = definition.clone();
                namespace["tools"] = Value::Array(members);
                if previous.is_some_and(|previous| previous.contains_key(name)) {
                    let description = definition["description"].as_str().unwrap_or_default();
                    decorated_namespaces.push((tools.len(), name, description));
                    namespace["description"] = Value::String(if description.is_empty() {
                        NAMESPACE_UPDATE_HINT.to_string()
                    } else {
                        format!("{description}\n{NAMESPACE_UPDATE_HINT}")
                    });
                }
                tools.push(namespace);
            } else if changed(name) {
                // Namespace declarations require a member. Update metadata as text without
                // repeating unchanged tool definitions.
                let instructions = definition["description"].as_str().unwrap_or_default();
                let text = if instructions.is_empty() {
                    format!("The {name} namespace no longer has additional instructions.")
                } else {
                    format!("Updated instructions for the {name} namespace:\n{instructions}")
                };
                namespace_updates
                    .push(WorldStateUpdate::fragment(ToolInstructions::new(text)).standalone());
            }
        }
        let mut updates = Vec::new();
        if !tools.is_empty() {
            // AdditionalTools receives only an at_UUID ID at the history boundary, with no
            // message metadata. Size that complete envelope, preserving the small wire path.
            let prospective = ResponseItem::AdditionalTools {
                id: Some(ResponseItemId::with_suffix("at", uuid::Uuid::nil())),
                role: "developer".to_string(),
                tools: tools.clone(),
            };
            let mut hints = Vec::new();
            if serde_json::to_vec(&prospective).is_ok_and(|bytes| bytes.len() > MAX_ITEM_BYTES) {
                for (index, name, description) in decorated_namespaces {
                    tools[index]["description"] = Value::String(description.to_string());
                    hints.push(WorldStateUpdate::fragment(ToolInstructions::new(format!(
                        "For the incremental declaration of the {name} namespace:\n{NAMESPACE_UPDATE_HINT}"
                    ))).standalone());
                }
            }
            updates.push(WorldStateUpdate::prefix_item(
                ResponseItem::AdditionalTools {
                    id: None,
                    role: "developer".to_string(),
                    tools,
                },
            ));
            updates.extend(hints);
        }
        updates.extend(namespace_updates);
        if let Some(previous) = previous {
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
                .filter(|key| {
                    !namespaces.contains(key.as_str())
                        && !key
                            .split_once('.')
                            .is_some_and(|(namespace, _)| namespaces.contains(namespace))
                })
                .cloned()
                .collect::<Vec<_>>();
            if !namespaces.is_empty() || !tools.is_empty() {
                updates.extend(
                    RemovedTools {
                        namespaces: namespaces.into_iter().map(str::to_string).collect(),
                        tools,
                    }
                    .into_updates(),
                );
            }
        }
        // Persist the empty map too: it is a known empty catalog, not missing state.
        (Some(self.hashes.clone()), updates)
    }
}

#[derive(Default)]
struct RemovedTools {
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
        let mut notice = Self::default();
        let mut notice_bytes = 0;
        for (header, names, kind) in [
            (
                REMOVED_NAMESPACES_HEADER,
                self.namespaces,
                RemovedIdentityKind::Namespace,
            ),
            (REMOVED_TOOLS_HEADER, self.tools, RemovedIdentityKind::Tool),
        ] {
            for name in names {
                if name.len() > REMOVAL_NOTICE_BYTES - header.len() - 3 {
                    if notice_bytes != 0 {
                        updates.push(
                            WorldStateUpdate::fragment(std::mem::take(&mut notice)).standalone(),
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
                    updates
                        .push(WorldStateUpdate::fragment(std::mem::take(&mut notice)).standalone());
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

    fn body(&self) -> String {
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
        format!(
            "Removed {kind} name, part {part}/{parts}. Join these {parts} consecutive payloads without separators to obtain its exact name. {instruction}\nPayload ({bytes} UTF-8 bytes):\n{payload}"
        )
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
    fn body(&self) -> String {
        let sections = [
            (REMOVED_NAMESPACES_HEADER, &self.namespaces),
            (REMOVED_TOOLS_HEADER, &self.tools),
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
        body
    }
}

#[cfg(test)]
#[path = "top_level_tools_tests.rs"]
mod tests;
