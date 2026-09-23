use super::*;
use crate::app_event::AppEvent;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;
use tokio::sync::mpsc::unbounded_channel;

fn render_lines(view: &SkillsToggleView, width: u16) -> String {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    let lines: Vec<String> = (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(area.x + col, area.y + row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line
        })
        .collect();
    lines.join("\n")
}

fn toggle_test_items() -> Vec<SkillsToggleItem> {
    vec![
        SkillsToggleItem {
            name: "Repo Scout".to_string(),
            skill_name: "repo_scout".to_string(),
            description: "Summarize the repo layout".to_string(),
            enabled: true,
            path: test_path_buf("/tmp/skills/repo_scout.toml").abs(),
        },
        SkillsToggleItem {
            name: "Changelog Writer".to_string(),
            skill_name: "changelog_writer".to_string(),
            description: "Draft release notes".to_string(),
            enabled: false,
            path: test_path_buf("/tmp/skills/changelog_writer.toml").abs(),
        },
    ]
}

fn long_name_items() -> Vec<SkillsToggleItem> {
    vec![
        SkillsToggleItem {
            name: "superpowers-systematic-debugging (polish)".to_string(),
            skill_name: "polish:superpowers-systematic-debugging".to_string(),
            description: "Find root causes before fixing bugs".to_string(),
            enabled: true,
            path: test_path_buf("/tmp/skills/systematic-debugging/SKILL.md").abs(),
        },
        SkillsToggleItem {
            name: "superpowers-verification-before-completion (polish)".to_string(),
            skill_name: "polish:superpowers-verification-before-completion".to_string(),
            description: "Verify completion before claiming success".to_string(),
            enabled: false,
            path: test_path_buf("/tmp/skills/verification-before-completion/SKILL.md").abs(),
        },
    ]
}

#[test]
fn renders_basic_popup() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = SkillsToggleView::new(
        toggle_test_items(),
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    assert_snapshot!("skills_toggle_basic", render_lines(&view, /*width*/ 72));
}

#[test]
fn build_rows_preserves_full_skill_display_names() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = SkillsToggleView::new(
        long_name_items(),
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    let row_names = view
        .build_rows()
        .into_iter()
        .map(|row| row.name)
        .collect::<Vec<_>>();

    assert_eq!(
        row_names,
        vec![
            "› [x] superpowers-systematic-debugging (polish)",
            "  [ ] superpowers-verification-before-completion (polish)",
        ]
    );
}

#[test]
fn filtering_long_skill_names_preserves_the_matching_row() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = SkillsToggleView::new(
        long_name_items(),
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.search_query = "completion".to_string();
    view.apply_filter();

    let row_names = view
        .build_rows()
        .into_iter()
        .map(|row| row.name)
        .collect::<Vec<_>>();

    assert_eq!(
        row_names,
        vec!["› [ ] superpowers-verification-before-completion (polish)"]
    );
}

#[test]
fn renders_long_names_using_available_width() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = SkillsToggleView::new(
        long_name_items(),
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_snapshot!(
        "skills_toggle_long_names_use_available_width",
        render_lines(&view, /*width*/ 96)
    );
}

#[test]
fn renders_long_names_at_narrow_width() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = SkillsToggleView::new(
        long_name_items(),
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_snapshot!(
        "skills_toggle_long_names_at_narrow_width",
        render_lines(&view, /*width*/ 48)
    );
}

#[test]
fn footer_hint_uses_list_keymap_accept_and_cancel() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.accept = vec![key_hint::ctrl(KeyCode::Char('t'))];
    keymap.cancel = vec![key_hint::ctrl(KeyCode::Char('x'))];
    let view = SkillsToggleView::new(Vec::new(), tx, keymap);
    let rendered = render_lines(&view, /*width*/ 72);

    assert!(rendered.contains("ctrl + t"));
    assert!(rendered.contains("ctrl + x"));
    assert!(!rendered.contains("enter"));
    assert!(!rendered.contains("esc"));
}

#[test]
fn space_toggles_selected_skill_and_emits_event() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = SkillsToggleView::new(
        toggle_test_items(),
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SetSkillEnabled { path, enabled }) => {
            assert_eq!(
                path,
                test_path_buf("/tmp/skills/changelog_writer.toml").abs()
            );
            assert!(enabled);
        }
        other => panic!("expected SetSkillEnabled event, got {other:?}"),
    }
    assert!(view.items[1].enabled);
}
