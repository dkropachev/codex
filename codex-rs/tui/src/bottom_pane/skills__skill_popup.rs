use super::*;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn mention_item(index: usize) -> MentionItem {
    MentionItem {
        display_name: format!("Mention {index:02}"),
        description: Some(format!("Description {index:02}")),
        insert_text: format!("$mention-{index:02}"),
        search_terms: vec![format!("mention-{index:02}")],
        path: Some(format!("skill://mention-{index:02}")),
        category_tag: Some("[Skill]".to_string()),
        sort_rank: 1,
    }
}

fn ranked_mention_item(
    display_name: &str,
    search_terms: &[&str],
    category_tag: &str,
    sort_rank: u8,
) -> MentionItem {
    MentionItem {
        display_name: display_name.to_string(),
        description: None,
        insert_text: format!("${display_name}"),
        search_terms: search_terms
            .iter()
            .map(|term| (*term).to_string())
            .collect(),
        path: None,
        category_tag: Some(category_tag.to_string()),
        sort_rank,
    }
}

fn named_mention_item(display_name: &str, search_terms: &[&str]) -> MentionItem {
    ranked_mention_item(display_name, search_terms, "[Skill]", /*sort_rank*/ 1)
}

fn plugin_mention_item(display_name: &str, search_terms: &[&str]) -> MentionItem {
    ranked_mention_item(display_name, search_terms, "[Plugin]", /*sort_rank*/ 0)
}

#[test]
fn filtered_mentions_preserve_results_beyond_popup_height() {
    let popup = SkillPopup::new((0..(MAX_POPUP_ROWS + 2)).map(mention_item).collect());

    let filtered_names: Vec<String> = popup
        .filtered_items()
        .into_iter()
        .map(|idx| popup.mentions[idx].display_name.clone())
        .collect();

    assert_eq!(
        filtered_names,
        (0..(MAX_POPUP_ROWS + 2))
            .map(|idx| format!("Mention {idx:02}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        popup.calculate_required_height(72),
        (MAX_POPUP_ROWS as u16) + 2
    );
}

fn render_popup(popup: &SkillPopup, width: u16) -> String {
    let area = Rect::new(0, 0, width, popup.calculate_required_height(width));
    let mut buf = Buffer::empty(area);
    popup.render_ref(area, &mut buf);
    format!("{buf:?}")
}

#[test]
fn scrolling_mentions_shifts_rendered_window_snapshot() {
    let mut popup = SkillPopup::new((0..(MAX_POPUP_ROWS + 2)).map(mention_item).collect());

    for _ in 0..=MAX_POPUP_ROWS {
        popup.move_down();
    }

    insta::assert_snapshot!("skill_popup_scrolled", render_popup(&popup, /*width*/ 72));
}

#[test]
fn display_name_match_sorting_beats_worse_secondary_search_term_matches() {
    let mut popup = SkillPopup::new(vec![
        named_mention_item("pr-review-triage", &["pr-review-triage"]),
        named_mention_item("prd", &["prd"]),
        named_mention_item("PR Babysitter", &["babysit-pr", "PR Babysitter"]),
        named_mention_item("Plugin Creator", &["plugin-creator", "Plugin Creator"]),
        named_mention_item(
            "Logging Best Practices",
            &["logging-best-practices", "Logging Best Practices"],
        ),
    ]);
    popup.set_query("pr");

    let filtered_names: Vec<String> = popup
        .filtered_items()
        .into_iter()
        .map(|idx| popup.mentions[idx].display_name.clone())
        .collect();

    assert_eq!(
        filtered_names,
        vec![
            "PR Babysitter".to_string(),
            "pr-review-triage".to_string(),
            "prd".to_string(),
            "Plugin Creator".to_string(),
            "Logging Best Practices".to_string(),
        ]
    );
}

#[test]
fn query_match_score_sorts_before_plugin_rank_bias() {
    let mut popup = SkillPopup::new(vec![
        plugin_mention_item("GitHub", &["github", "pull requests", "pr"]),
        named_mention_item("pr-review-triage", &["pr-review-triage"]),
        named_mention_item("prd", &["prd"]),
        named_mention_item("Plugin Creator", &["plugin-creator", "Plugin Creator"]),
        named_mention_item(
            "Logging Best Practices",
            &["logging-best-practices", "Logging Best Practices"],
        ),
        named_mention_item("PR Babysitter", &["babysit-pr", "PR Babysitter"]),
    ]);
    popup.set_query("pr");

    let filtered_items: Vec<(String, Option<String>)> = popup
        .filtered_items()
        .into_iter()
        .map(|idx| {
            (
                popup.mentions[idx].display_name.clone(),
                popup.mentions[idx].category_tag.clone(),
            )
        })
        .collect();

    assert_eq!(
        filtered_items,
        vec![
            ("PR Babysitter".to_string(), Some("[Skill]".to_string())),
            ("pr-review-triage".to_string(), Some("[Skill]".to_string())),
            ("prd".to_string(), Some("[Skill]".to_string())),
            ("Plugin Creator".to_string(), Some("[Skill]".to_string())),
            (
                "Logging Best Practices".to_string(),
                Some("[Skill]".to_string())
            ),
            ("GitHub".to_string(), Some("[Plugin]".to_string())),
        ]
    );
}
