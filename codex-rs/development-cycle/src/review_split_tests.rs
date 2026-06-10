use pretty_assertions::assert_eq;

use super::*;

#[test]
fn validate_strategy_rejects_unknown_duplicate_missing_and_oversized_items() {
    let selected = review_types(["correctness", "tests", "api"]);

    assert_eq!(
        validate_strategy(
            strategy("bad id", [vec!["correctness", "tests"], vec!["api"]]),
            &selected,
            3
        )
        .unwrap_err()
        .code,
        "invalid_strategy_id"
    );
    let mut duplicate_group_id = strategy("g:1", [vec!["correctness", "tests"], vec!["api"]]);
    duplicate_group_id.groups[1].group_id = duplicate_group_id.groups[0].group_id.clone();
    assert_eq!(
        validate_strategy(duplicate_group_id, &selected, 3)
            .unwrap_err()
            .code,
        "duplicate_group_id"
    );
    let mut negative_savings = strategy("g:1", [vec!["correctness", "tests"], vec!["api"]]);
    negative_savings.expected_reviewer_count_savings = -1.0;
    assert_eq!(
        validate_strategy(negative_savings, &selected, 3)
            .unwrap_err()
            .code,
        "invalid_expected_savings"
    );
    assert_eq!(
        validate_strategy(
            strategy("g:1", [vec!["correctness", "docs"], vec!["tests"]]),
            &selected,
            3
        )
        .unwrap_err()
        .code,
        "unknown_item"
    );
    assert_eq!(
        validate_strategy(
            strategy(
                "g:1",
                [vec!["correctness", "tests"], vec!["correctness", "api"]]
            ),
            &selected,
            3
        )
        .unwrap_err()
        .code,
        "duplicate_item"
    );
    assert_eq!(
        validate_strategy(
            strategy("g:1", [vec!["correctness", "tests"]]),
            &selected,
            3
        )
        .unwrap_err()
        .code,
        "missing_item"
    );
    assert_eq!(
        validate_strategy(
            strategy("g:1", [vec!["correctness", "tests", "api"]]),
            &selected,
            2
        )
        .unwrap_err()
        .code,
        "oversized_group"
    );
}

#[test]
fn separate_strategy_uses_one_group_per_review_item() {
    let selected = review_types(["correctness", "domain"]);

    assert_eq!(
        separate_strategy(&selected).groups,
        vec![
            ReviewSplitGroup {
                group_id: "correctness".to_string(),
                review_type_ids: vec!["correctness".to_string()],
            },
            ReviewSplitGroup {
                group_id: "domain".to_string(),
                review_type_ids: vec!["domain".to_string()],
            },
        ]
    );
}

#[test]
fn deterministic_sampling_honors_probability_edges() {
    assert!(!should_sample(0.0, &["run"]));
    assert!(should_sample(1.0, &["run"]));
    assert_eq!(
        should_sample(0.5, &["run", "model"]),
        should_sample(0.5, &["run", "model"])
    );
}

fn strategy<const N: usize>(id: &str, groups: [Vec<&str>; N]) -> ReviewSplitStrategy {
    ReviewSplitStrategy {
        strategy_id: id.to_string(),
        groups: groups
            .into_iter()
            .enumerate()
            .map(|(index, ids)| ReviewSplitGroup {
                group_id: format!("group-{}", index + 1),
                review_type_ids: ids.into_iter().map(str::to_string).collect(),
            })
            .collect(),
        rationale: String::new(),
        expected_reviewer_count_savings: 1.0,
        risk_notes: Vec::new(),
    }
}

fn review_types<const N: usize>(ids: [&str; N]) -> Vec<ReviewTypeDefinition> {
    ids.into_iter()
        .map(|id| ReviewTypeDefinition {
            id: id.to_string(),
            short_name: id.to_string(),
            description: id.to_string(),
            prompt: None,
            exclude_prompt: None,
            enabled: true,
        })
        .collect()
}
