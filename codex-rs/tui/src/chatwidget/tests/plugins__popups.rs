use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn plugins_popup_space_toggles_installed_plugin_from_list() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let cwd = chat.config.cwd.to_path_buf();
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SetPluginEnabled {
            cwd: event_cwd,
            plugin_id,
            enabled,
        }) => {
            assert_eq!(event_cwd, cwd);
            assert_eq!(plugin_id, "plugin-drive");
            assert!(!enabled);
        }
        other => panic!("expected SetPluginEnabled event, got {other:?}"),
    }

    chat.on_plugin_enabled_set(
        cwd,
        "plugin-drive".to_string(),
        /*enabled*/ false,
        Ok(()),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("› [ ] Drive"),
        "expected selected plugin row to stay selected after refresh, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_space_on_uninstalled_row_does_not_start_search() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    while rx.try_recv().is_ok() {}
    let before = render_bottom_popup(&chat, /*width*/ 100);
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let after = render_bottom_popup(&chat, /*width*/ 100);

    assert!(
        rx.try_recv().is_err(),
        "did not expect Space on an uninstalled plugin to emit an event"
    );
    assert_eq!(after, before);
}

#[tokio::test]
async fn plugins_popup_space_with_active_search_does_not_toggle_installed_plugin() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    type_plugins_search_query(&mut chat, "dr");
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    assert!(
        rx.try_recv().is_err(),
        "did not expect Space with an active plugin search to emit a toggle event"
    );
}
