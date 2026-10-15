//! Catalog diffs preserve unchanged definitions, including tools inside namespaces.

use super::*;
use crate::context::world_state::test_support::render_section_cases;
use crate::context_manager::updates::merge_world_state_updates;
use pretty_assertions::assert_eq;
use serde_json::json;

fn declaration(name: &str) -> Value {
    json!({"type": "function", "name": name, "description": "Look up a value.",
        "parameters": {"type": "object", "properties": {}}})
}

fn item(tools: Vec<Value>) -> ResponseItem {
    ResponseItem::AdditionalTools {
        id: None,
        role: "developer".to_string(),
        tools,
    }
}

fn namespace(name: &str, tools: Vec<Value>) -> Value {
    json!({"type": "namespace", "name": name, "description": "Tools.", "tools": tools})
}

#[test]
fn snapshots_catalog_transitions() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;

    let search = json!({"type": "tool_search", "execution": "client",
        "description": "Search tools.",
        "parameters": {"type": "object", "properties": {}}});
    let web = json!({"type": "web_search", "external_web_access": true});
    let original = TopLevelToolsState::new(vec![search.clone()]).unwrap();
    let mut changed = search;
    changed["parameters"]["properties"] = json!({"query": {"type": "string"}});
    let updated = TopLevelToolsState::new(vec![changed, web.clone()]).unwrap();
    let only_web = TopLevelToolsState::new(vec![web]).unwrap();
    let empty = TopLevelToolsState::new(Vec::new()).unwrap();

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Known(&original)),
        (Known(&original), Known(&original)),
        (Known(&original), Known(&updated)),
        (Known(&updated), Known(&only_web)),
        (Known(&only_web), Known(&empty)),
        (Known(&empty), Known(&original)),
    ]));
}

#[test]
fn snapshots_namespace_transitions() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;

    let original = TopLevelToolsState::new(vec![
        namespace(
            "functions",
            vec![
                declaration("unchanged"),
                declaration("removed"),
                declaration("changed"),
            ],
        ),
        namespace("other", vec![declaration("lookup")]),
    ])
    .unwrap();
    let mut changed = declaration("changed");
    changed["parameters"]["properties"] = json!({"query": {"type": "string"}});
    let updated = TopLevelToolsState::new(vec![
        namespace(
            "functions",
            vec![declaration("unchanged"), changed, declaration("added")],
        ),
        namespace("other", vec![declaration("lookup")]),
    ])
    .unwrap();
    let only_other =
        TopLevelToolsState::new(vec![namespace("other", vec![declaration("lookup")])]).unwrap();
    let only_functions =
        TopLevelToolsState::new(vec![namespace("functions", vec![declaration("unchanged")])])
            .unwrap();
    let empty = TopLevelToolsState::new(Vec::new()).unwrap();
    let restored = TopLevelToolsState::new(vec![namespace(
        "functions",
        vec![declaration("unchanged"), declaration("restored")],
    )])
    .unwrap();

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Known(&original)),
        (Known(&original), Known(&original)),
        (Known(&original), Known(&updated)),
        (Known(&updated), Known(&only_other)),
        (Known(&only_other), Known(&original)),
        // A whole namespace and individual members can disappear in the same update.
        (Known(&updated), Known(&only_functions)),
        (Known(&updated), Known(&empty)),
        // An empty catalog is known state; do not repeat its removal notice.
        (Known(&empty), Known(&empty)),
        // Restoration declares both an old member and one that has never been available.
        (Known(&empty), Known(&restored)),
    ]));
}

#[test]
fn initial_catalog_preserves_order_and_unchanged_catalog_emits_nothing() {
    let definitions = vec![
        namespace("zebra", vec![declaration("lookup")]),
        namespace("alpha", vec![declaration("lookup")]),
    ];
    let state = TopLevelToolsState::new(definitions.clone()).unwrap();
    let (snapshot, updates) = state.render_diff(PreviousSectionState::Absent);
    assert_eq!(
        merge_world_state_updates(updates),
        vec![item(definitions.clone())]
    );
    let (_, updates) = state.render_diff(PreviousSectionState::Unknown);
    assert_eq!(merge_world_state_updates(updates), vec![item(definitions)]);
    let snapshot = snapshot.unwrap();
    assert_eq!(
        snapshot.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["alpha", "alpha.lookup", "zebra", "zebra.lookup"]
    );
    let (unchanged_snapshot, updates) = state.render_diff(PreviousSectionState::Known(&snapshot));
    assert_eq!(unchanged_snapshot, Some(snapshot));
    assert!(updates.is_empty());
}

#[test]
fn empty_catalog_is_persisted_and_can_add_tools_again() {
    let definitions = vec![namespace("functions", vec![declaration("lookup")])];
    let original = TopLevelToolsState::new(definitions.clone()).unwrap();
    let previous = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let empty = TopLevelToolsState::new(Vec::new()).unwrap();
    let (snapshot, updates) = empty.render_diff(PreviousSectionState::Known(&previous));
    assert_eq!(snapshot, Some(BTreeMap::new()));
    assert_eq!(
        merge_world_state_updates(updates),
        vec![ContextualUserFragment::into(RemovedTools {
            namespaces: vec!["functions".to_string()],
            ..Default::default()
        })]
    );
    let (_, updates) = original.render_diff(PreviousSectionState::Known(&snapshot.unwrap()));
    assert_eq!(merge_world_state_updates(updates), vec![item(definitions)]);
}

#[test]
fn namespace_diff_contains_only_changed_and_added_tools() {
    let original = TopLevelToolsState::new(vec![
        namespace(
            "functions",
            vec![
                declaration("changed"),
                declaration("unchanged"),
                declaration("removed"),
            ],
        ),
        namespace("other", vec![declaration("changed")]),
    ])
    .unwrap();
    let snapshot = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let mut changed = declaration("changed");
    changed["parameters"]["properties"] = json!({"query": {"type": "string"}});
    let added = declaration("added");
    let current = TopLevelToolsState::new(vec![
        namespace(
            "functions",
            vec![changed.clone(), declaration("unchanged"), added.clone()],
        ),
        namespace("other", vec![declaration("changed")]),
    ])
    .unwrap();
    let (snapshot, updates) = current.render_diff(PreviousSectionState::Known(&snapshot));
    let mut expected = namespace("functions", vec![changed, added]);
    expected["description"] = json!(format!("Tools.\n{NAMESPACE_UPDATE_HINT}"));
    assert_eq!(
        merge_world_state_updates(updates),
        vec![
            item(vec![expected]),
            ContextualUserFragment::into(RemovedTools {
                tools: vec!["functions.removed".to_string()],
                ..Default::default()
            }),
        ]
    );
    assert!(
        current
            .render_diff(PreviousSectionState::Known(&snapshot.unwrap()))
            .1
            .is_empty()
    );
}

#[test]
fn namespace_description_change_does_not_repeat_unchanged_tools() {
    let original =
        TopLevelToolsState::new(vec![namespace("functions", vec![declaration("lookup")])]).unwrap();
    let snapshot = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let mut updated = namespace("functions", vec![declaration("lookup")]);
    updated["description"] = json!("Updated namespace guidance.");
    let current = TopLevelToolsState::new(vec![updated]).unwrap();
    let (snapshot, updates) = current.render_diff(PreviousSectionState::Known(&snapshot));
    assert_eq!(
        merge_world_state_updates(updates),
        vec![ContextualUserFragment::into(ToolInstructions::new(
            "Updated instructions for the functions namespace:\nUpdated namespace guidance."
                .to_string()
        ))]
    );
    assert!(
        current
            .render_diff(PreviousSectionState::Known(&snapshot.unwrap()))
            .1
            .is_empty()
    );
}

#[test]
fn tool_type_change_keeps_the_callable_name_available() {
    let original = TopLevelToolsState::new(vec![namespace(
        "functions",
        vec![declaration("lookup"), declaration("removed")],
    )])
    .unwrap();
    let snapshot = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let custom = json!({"type": "custom", "name": "lookup", "description": "Look up a value.",
        "format": {"type": "text"}});
    let mut updated = namespace("functions", vec![custom]);
    let current = TopLevelToolsState::new(vec![updated.clone()]).unwrap();
    let (_, updates) = current.render_diff(PreviousSectionState::Known(&snapshot));
    updated["description"] = json!(format!("Tools.\n{NAMESPACE_UPDATE_HINT}"));
    assert_eq!(
        merge_world_state_updates(updates),
        vec![
            item(vec![updated]),
            ContextualUserFragment::into(RemovedTools {
                tools: vec!["functions.removed".to_string()],
                ..Default::default()
            }),
        ]
    );
}

#[test]
fn namespace_removals_emit_one_notice() {
    let original = TopLevelToolsState::new(vec![
        namespace(
            "functions",
            vec![
                declaration("lookup"),
                declaration("removed"),
                declaration("also_removed"),
            ],
        ),
        namespace("function", vec![declaration("lookup")]),
        namespace("other", vec![declaration("lookup")]),
        json!({"type": "web_search"}),
    ])
    .unwrap();
    let snapshot = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let current =
        TopLevelToolsState::new(vec![namespace("functions", vec![declaration("lookup")])]).unwrap();
    let (_, updates) = current.render_diff(PreviousSectionState::Known(&snapshot));
    assert_eq!(
        merge_world_state_updates(updates),
        vec![ContextualUserFragment::into(RemovedTools {
            namespaces: vec!["function".to_string(), "other".to_string()],
            tools: vec![
                "functions.also_removed".to_string(),
                "functions.removed".to_string(),
                "web_search".to_string(),
            ],
        })]
    );
}

#[test]
fn namespace_replaced_by_builtin_keeps_the_replacement_available() {
    let original =
        TopLevelToolsState::new(vec![namespace("web_search", vec![declaration("lookup")])])
            .unwrap();
    let snapshot = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let replacement = json!({"type": "web_search"});
    let current = TopLevelToolsState::new(vec![replacement.clone()]).unwrap();
    let (_, updates) = current.render_diff(PreviousSectionState::Known(&snapshot));
    assert_eq!(
        merge_world_state_updates(updates),
        vec![
            item(vec![replacement]),
            ContextualUserFragment::into(RemovedTools {
                tools: vec!["web_search.lookup".to_string()],
                ..Default::default()
            }),
        ],
    );
}

#[test]
fn large_member_removal_preserves_each_name_in_separate_notices() {
    let removed_names = (0..300)
        .map(|index| format!("removed_{index:03}_é"))
        .collect::<Vec<_>>();
    let original = TopLevelToolsState::new(vec![namespace(
        "functions",
        std::iter::once(declaration("retained"))
            .chain(removed_names.iter().map(|name| declaration(name)))
            .collect(),
    )])
    .unwrap();
    let snapshot = original
        .render_diff(PreviousSectionState::Absent)
        .0
        .unwrap();
    let current =
        TopLevelToolsState::new(vec![namespace("functions", vec![declaration("retained")])])
            .unwrap();
    let (_, updates) = current.render_diff(PreviousSectionState::Known(&snapshot));
    let messages = merge_world_state_updates(updates);
    assert!(messages.len() > 1);
    let mut actual_names = Vec::new();
    for message in messages {
        let ResponseItem::Message { role, content, .. } = message else {
            panic!("removal notice must be a message");
        };
        assert_eq!(role, "developer");
        let [codex_protocol::models::ContentItem::InputText { text }] = content.as_slice() else {
            panic!("removal notices must remain standalone text fragments");
        };
        assert!(text.len() <= REMOVAL_NOTICE_BYTES);
        assert!(text.starts_with(REMOVED_TOOLS_HEADER));
        actual_names.extend(
            text.lines()
                .filter_map(|line| line.strip_prefix("- "))
                .map(str::to_string),
        );
    }
    assert_eq!(
        actual_names,
        removed_names
            .into_iter()
            .map(|name| format!("functions.{name}"))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn oversized_removal_names_preserve_exact_payloads() {
    let name = format!(" \nPayload (0 UTF-8 bytes):\n{}\n ", "🦀é".repeat(200));
    for kind in [RemovedIdentityKind::Namespace, RemovedIdentityKind::Tool] {
        let removals = match kind {
            RemovedIdentityKind::Namespace => RemovedTools {
                namespaces: vec!["before".to_string(), name.clone(), "after".to_string()],
                ..Default::default()
            },
            RemovedIdentityKind::Tool => RemovedTools {
                tools: vec!["before".to_string(), name.clone(), "after".to_string()],
                ..Default::default()
            },
        };
        let messages = merge_world_state_updates(removals.into_updates());
        let mut reconstructed = String::new();
        let mut next_part = 1;
        let mut total_parts = None;
        for (index, message) in messages.iter().enumerate() {
            let ResponseItem::Message { role, content, .. } = message else {
                panic!("removal must be a standalone message");
            };
            assert_eq!(role, "developer");
            let [codex_protocol::models::ContentItem::InputText { text }] = content.as_slice()
            else {
                panic!("removal must be one text fragment");
            };
            assert!(text.len() <= REMOVAL_NOTICE_BYTES);
            if index == 0 || index == messages.len() - 1 {
                assert!(text.ends_with(if index == 0 { "- before" } else { "- after" }));
                continue;
            }
            let (header, rest) = text.split_once("\nPayload (").unwrap();
            let (bytes, payload) = rest.split_once(" UTF-8 bytes):\n").unwrap();
            assert_eq!(bytes.parse::<usize>().unwrap(), payload.len());
            let (part, parts) = header
                .split_once("name, part ")
                .unwrap()
                .1
                .split_once(". Join these ")
                .unwrap()
                .0
                .split_once('/')
                .unwrap();
            assert_eq!(part.parse::<usize>().unwrap(), next_part);
            let parts = parts.parse::<usize>().unwrap();
            assert_eq!(*total_parts.get_or_insert(parts), parts);
            assert!(header.starts_with(match kind {
                RemovedIdentityKind::Namespace => "Removed namespace name,",
                RemovedIdentityKind::Tool => "Removed tool name,",
            }));
            reconstructed.push_str(payload);
            next_part += 1;
        }
        assert_eq!(reconstructed, name);
        assert_eq!(Some(next_part - 1), total_parts);
    }
}

#[test]
fn continuation_headers_fit_with_maximum_indices() {
    for kind in [RemovedIdentityKind::Namespace, RemovedIdentityKind::Tool] {
        let text = RemovedIdentityPart {
            kind,
            part: usize::MAX,
            parts: usize::MAX,
            payload: "x".repeat(REMOVED_NAME_PART_BYTES),
        }
        .body();
        assert!(text.len() <= REMOVAL_NOTICE_BYTES);
    }
}

#[test]
fn snapshots_oversized_removal_notice() {
    let messages = merge_world_state_updates(
        RemovedTools {
            namespaces: vec!["é".repeat(350)],
            tools: vec!["functions.short".to_string()],
        }
        .into_updates(),
    );
    let texts = messages
        .into_iter()
        .map(|message| {
            let ResponseItem::Message { content, .. } = message else {
                panic!("expected removal message");
            };
            let [codex_protocol::models::ContentItem::InputText { text }] = content.as_slice()
            else {
                panic!("expected standalone text");
            };
            text.clone()
        })
        .collect::<Vec<_>>();
    insta::assert_snapshot!(texts.join("\n\n---\n\n"));
}
