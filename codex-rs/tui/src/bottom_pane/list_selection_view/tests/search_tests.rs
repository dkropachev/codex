//! Search input, source selection, and rendered emphasis in the shared list picker.

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn searchable_list_honors_configured_printable_accept_alternative() {
    let (tx, mut rx) = unbounded_channel();
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.accept = vec![
        crate::key_hint::plain(KeyCode::Char('z')),
        crate::key_hint::plain(KeyCode::Enter),
    ];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            is_searchable: true,
            items: vec![SelectionItem {
                name: "Selection".into(),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::UpdateModel("accepted".into()))
                })],
                ..Default::default()
            }],
            ..Default::default()
        },
        AppEventSender::new(tx),
        keymap,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Char('z')));

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateModel(model)) if model == "accepted"
    ));
    assert_eq!(view.search_query(), Some(""));
}

#[test]
fn renders_search_query_line_when_enabled() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![SelectionItem {
        name: "Read Only".to_string(),
        description: Some("Codex can read files".to_string()),
        is_current: false,
        dismiss_on_select: true,
        ..Default::default()
    }];
    let mut view = new_view(
        SelectionViewParams {
            title: Some("Select Approval Mode".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        },
        tx,
    );
    view.set_search_query("filters".to_string());

    let lines = render_lines(&view);
    assert!(
        lines.contains("filters"),
        "expected search query line to include rendered query, got {lines:?}"
    );
}

#[test]
fn paste_appends_to_search_query_and_filters_items() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = new_view(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "main -> feature/other".to_string(),
                    search_value: Some("feature/other".to_string()),
                    ..Default::default()
                },
                SelectionItem {
                    name: "main -> feature/paste-support".to_string(),
                    search_value: Some("feature/paste-support".to_string()),
                    ..Default::default()
                },
            ],
            is_searchable: true,
            ..Default::default()
        },
        tx,
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));

    assert!(view.handle_paste("eature/paste-support\n".to_string()));

    assert_eq!(view.search_query, "feature/paste-support");
    assert_eq!(view.filtered_indices, vec![1]);
    assert_eq!(view.selected_actual_idx(), Some(1));
}

#[test]
fn whitespace_only_paste_is_ignored() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = new_view(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "main".to_string(),
                search_value: Some("main".to_string()),
                ..Default::default()
            }],
            is_searchable: true,
            ..Default::default()
        },
        tx,
    );

    assert!(!view.handle_paste(" \n\t ".to_string()));

    assert_eq!(view.search_query, "");
    assert_eq!(view.filtered_indices, vec![0]);
}

#[test]
fn switching_tabs_changes_visible_items_and_clears_search() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            tabs: vec![
                SelectionTab {
                    id: "alpha".to_string(),
                    label: "Alpha".to_string(),
                    header: Box::new(()),
                    items: vec![SelectionItem {
                        name: "Alpha Item".to_string(),
                        dismiss_on_select: true,
                        ..Default::default()
                    }],
                },
                SelectionTab {
                    id: "beta".to_string(),
                    label: "Beta".to_string(),
                    header: Box::new(()),
                    items: vec![SelectionItem {
                        name: "Beta Item".to_string(),
                        dismiss_on_select: true,
                        ..Default::default()
                    }],
                },
            ],
            initial_tab_id: Some("beta".to_string()),
            is_searchable: true,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.set_search_query("beta".to_string());

    view.handle_key_event(KeyEvent::from(KeyCode::Left));

    assert_eq!(view.active_tab_id(), Some("alpha"));
    assert_eq!(view.search_query, "");
    view.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(view.active_tab_id(), Some("beta"));
    view.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(view.active_tab_id(), Some("alpha"));
    let rendered = render_lines(&view);
    assert!(
        rendered.contains("Alpha Item") && !rendered.contains("Beta Item"),
        "expected switched tab to render the alpha items, got:\n{rendered}"
    );
}

#[test]
fn space_appends_to_active_search_instead_of_toggling_selected_item() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Plugin".to_string(),
                toggle: Some(SelectionToggle {
                    is_on: false,
                    action: Box::new(|_enabled, tx: &_| {
                        tx.send(AppEvent::OpenApprovalsPopup);
                    }),
                }),
                ..Default::default()
            }],
            is_searchable: true,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.set_search_query("plugin".to_string());

    view.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    assert_eq!(view.search_query, "plugin ");
    assert!(
        !view.active_items()[0]
            .toggle
            .as_ref()
            .is_some_and(|toggle| toggle.is_on),
        "expected Space to leave the toggle state unchanged while search is active"
    );
    assert!(
        rx.try_recv().is_err(),
        "expected Space with an active search query to avoid firing the toggle action"
    );
}

#[test]
fn configured_list_binding_takes_precedence_over_search_toggle_key() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_down = vec![crate::key_hint::plain(KeyCode::Tab)];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "Plugin".to_string(),
                    toggle: Some(SelectionToggle {
                        is_on: false,
                        action: Box::new(|_enabled, tx: &_| {
                            tx.send(AppEvent::OpenApprovalsPopup);
                        }),
                    }),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Next".to_string(),
                    ..Default::default()
                },
            ],
            is_searchable: true,
            search_toggle_key: Some(crate::key_hint::plain(KeyCode::Tab)),
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Tab));

    assert_eq!(view.selected_actual_idx(), Some(1));
    assert!(
        !view.active_items()[0]
            .toggle
            .as_ref()
            .is_some_and(|toggle| toggle.is_on),
        "expected the configured move-down binding to leave the toggle unchanged"
    );
    assert!(
        rx.try_recv().is_err(),
        "expected the configured move-down binding not to fire the toggle action"
    );
}

#[test]
fn active_search_preserves_query_and_readable_styles_on_dark_and_light_surfaces() {
    let mut snapshots = Vec::new();
    for (name, colors) in [
        (
            "dark",
            crate::terminal_probe::DefaultColors {
                fg: (230, 230, 230),
                bg: (18, 20, 30),
            },
        ),
        (
            "light",
            crate::terminal_probe::DefaultColors {
                fg: (30, 30, 30),
                bg: (255, 255, 255),
            },
        ),
    ] {
        crate::terminal_palette::with_test_default_colors(colors, || {
            let (tx, _rx) = unbounded_channel();
            let mut view = new_view(
                SelectionViewParams {
                    is_searchable: true,
                    search_mode: SearchMode::OnDemand(crate::key_hint::plain(KeyCode::Char('/'))),
                    items: vec![SelectionItem {
                        name: "Theme".into(),
                        search_value: Some("theme".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                AppEventSender::new(tx),
            );
            view.handle_key_event(KeyEvent::from(KeyCode::Char('/')));
            assert!(view.handle_paste("theme".into()));
            let width = 48;
            let area = Rect::new(
                /*x*/ 0,
                /*y*/ 0,
                width,
                view.desired_height(width),
            );
            let mut buffer = Buffer::empty(area);
            view.render(area, &mut buffer);
            let cursor = view.cursor_pos(area).expect("active search cursor");
            let row = (0..width)
                .map(|x| buffer[(x, cursor.1)].symbol())
                .collect::<String>();
            assert_eq!(row.trim(), "Search › theme");
            assert_eq!(
                usize::from(cursor.0),
                row.find("Search").expect("search label") + "Search › theme".width(),
            );
            let mut style_runs: Vec<(String, Style)> = Vec::new();
            for x in 0..width {
                let cell = &buffer[(x, cursor.1)];
                if let Some((text, style)) = style_runs.last_mut()
                    && *style == cell.style()
                {
                    text.push_str(cell.symbol());
                } else {
                    style_runs.push((cell.symbol().to_owned(), cell.style()));
                }
            }
            snapshots.push(format!(
                "{name}\nrow: {row:?}\ncursor: {cursor:?}\nstyles: {style_runs:#?}"
            ));
        });
    }
    insta::assert_snapshot!(
        "active_search_dark_and_light_styles",
        snapshots.join("\n\n")
    );
}
