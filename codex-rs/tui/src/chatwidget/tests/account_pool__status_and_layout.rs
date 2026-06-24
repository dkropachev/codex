use super::*;
use codex_app_server_protocol::SpendControlLimitSnapshot;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn rolling_rate_limit_snapshot_preserves_prior_individual_limit() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let mut usage_limits = snapshot(/*percent*/ 10.0);
    usage_limits.individual_limit = Some(SpendControlLimitSnapshot {
        limit: "25000".to_string(),
        used: "8000".to_string(),
        remaining_percent: 68,
        resets_at: 1_800_000_000,
    });
    chat.on_rate_limit_snapshot(Some(usage_limits));

    chat.on_rolling_rate_limit_snapshot(snapshot(/*percent*/ 20.0));

    let display = chat
        .rate_limit_snapshots_by_limit_id
        .get("codex")
        .expect("rate limits should be cached");
    let individual_limit = display
        .individual_limit
        .as_ref()
        .expect("rolling updates should preserve monthly limits");
    assert_eq!(individual_limit.used, "8,000");
    assert_eq!(individual_limit.limit, "25,000");
    assert_eq!(individual_limit.percent_remaining, 68.0);

    chat.on_rate_limit_snapshot(Some(snapshot(/*percent*/ 30.0)));
    let display = chat
        .rate_limit_snapshots_by_limit_id
        .get("codex")
        .expect("rate limits should be cached");
    assert!(display.individual_limit.is_none());
}
