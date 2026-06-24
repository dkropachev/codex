use super::*;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use codex_core_skills::model::SkillInterface;
use codex_plugin::AppConnectorId;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn set_plugin_mentions_refreshes_open_mention_popup() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("$".to_string(), Vec::new(), Vec::new());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
        config_name: "sample@test".to_string(),
        display_name: "Sample Plugin".to_string(),
        description: None,
        has_skills: true,
        mcp_server_names: vec!["sample".to_string()],
        app_connector_ids: Vec::new(),
    }]));

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected mention popup to open after plugin update");
    };
    let mention = popup
        .selected_mention()
        .expect("expected plugin mention to be selected");
    assert_eq!(mention.insert_text, "$sample".to_string());
    assert_eq!(mention.path, Some("plugin://sample@test".to_string()));
}

#[test]
fn mention_items_show_plugin_owned_skill_and_app_duplicates() {
    let skill_path = test_path_buf("/tmp/repo/google-calendar/SKILL.md").abs();
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_connectors_enabled(/*enabled*/ true);
    composer.set_text_content("$goog".to_string(), Vec::new(), Vec::new());
    composer.set_skill_mentions(Some(vec![SkillMetadata {
        name: "google-calendar:availability".to_string(),
        description: "Find availability and plan event changes".to_string(),
        short_description: None,
        interface: Some(SkillInterface {
            display_name: Some("Google Calendar".to_string()),
            short_description: None,
            icon_small: None,
            icon_large: None,
            brand_color: None,
            default_prompt: None,
        }),
        dependencies: None,
        policy: None,
        path_to_skills_md: skill_path.clone(),
        scope: crate::test_support::skill_scope_repo(),
        plugin_id: None,
    }]));
    composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
        config_name: "google-calendar@debug".to_string(),
        display_name: "Google Calendar".to_string(),
        description: Some(
            "Connect Google Calendar for scheduling, availability, and event management."
                .to_string(),
        ),
        has_skills: true,
        mcp_server_names: vec!["google-calendar".to_string()],
        app_connector_ids: vec![AppConnectorId("google_calendar".to_string())],
    }]));
    composer.set_connector_mentions(Some(ConnectorsSnapshot {
        connectors: vec![AppInfo {
            id: "google_calendar".to_string(),
            name: "Google Calendar".to_string(),
            description: Some("Look up events and availability".to_string()),
            logo_url: None,
            logo_url_dark: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://example.test/google-calendar".to_string()),
            is_accessible: true,
            is_enabled: true,
            plugin_display_names: vec!["Google Calendar".to_string()],
        }],
    }));

    let mentions = composer.mention_items();
    assert_eq!(mentions.len(), 3);
    assert_eq!(mentions[0].category_tag, Some("[Skill]".to_string()));
    assert_eq!(mentions[0].path, Some(skill_path.display().to_string()));
    assert_eq!(mentions[0].display_name, "Google Calendar".to_string());
    assert_eq!(mentions[1].category_tag, Some("[Plugin]".to_string()));
    assert_eq!(
        mentions[1].path,
        Some("plugin://google-calendar@debug".to_string())
    );
    assert_eq!(mentions[2].category_tag, Some("[App]".to_string()));
    assert_eq!(mentions[2].path, Some("app://google_calendar".to_string()));
}

#[test]
fn restored_bound_at_mentions_do_not_open_mention_popup() {
    for (text, move_cursor_to_end) in [
        ("@sample".to_string(), false),
        ("Please ask @sample.".to_string(), true),
    ] {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            sender,
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        );
        composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
            config_name: "sample@test".to_string(),
            display_name: "sample".to_string(),
            description: None,
            has_skills: true,
            mcp_server_names: vec!["sample".to_string()],
            app_connector_ids: Vec::new(),
        }]));

        composer.set_text_content_with_mention_bindings(
            text.clone(),
            Vec::new(),
            Vec::new(),
            vec![MentionBinding {
                sigil: '@',
                mention: "sample".to_string(),
                path: "plugin://sample@test".to_string(),
            }],
        );
        if move_cursor_to_end {
            composer.move_cursor_to_end();
        }

        assert!(matches!(composer.popups.active, ActivePopup::None));

        let (result, consumed) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(consumed);
        match result {
            InputResult::Submitted {
                text: submitted, ..
            } => assert_eq!(submitted, text),
            _ => panic!("expected restored bound mention to submit"),
        }
    }
}
