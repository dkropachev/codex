use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use codex_native_workflow::NativeWorkflowAgentHandle;
use codex_native_workflow::NativeWorkflowAgentOutput;
use codex_native_workflow::NativeWorkflowAgentRuntime;
use codex_native_workflow::NativeWorkflowAgentSpawnRequest;
use codex_native_workflow::NativeWorkflowAgentTurnRequest;
use codex_native_workflow::NativeWorkflowModelCandidate;
use codex_native_workflow::NativeWorkflowModelProviderCatalog;
use codex_native_workflow::NativeWorkflowRunContext;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::persistence::DevCycleState;
use crate::pipeline::run_dev_cycle;
use crate::split_persistence::SplitAttemptRecord;

#[tokio::test]
async fn full_pipeline_routes_verified_findings_back_to_same_writer() {
    let tempdir = tempfile::tempdir().unwrap();
    let runtime = FakeAgentRuntime::default();
    let catalog = FakeCatalog {
        candidates: vec![NativeWorkflowModelCandidate {
            provider_id: "openai".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: Some("xhigh".to_string()),
            intelligence_score: Some(1.0),
        }],
    };
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: Some(&runtime),
        model_provider_catalog: Some(&catalog),
        cancellation_token: None,
    };

    let output = run_dev_cycle(
        ctx,
        json!({
            "taskDescription": "fix bug",
            "reviewTypes": ["correctness"],
            "testMode": "provided",
            "testCommands": ["printf ok"]
        }),
    )
    .await
    .unwrap()
    .output;

    assert_eq!(output["status"], json!("succeeded"));
    assert_eq!(
        output["verifiedFindings"][0]["title"],
        json!("Broken behavior")
    );
    assert_eq!(
        output["writerCommits"],
        json!([
            {"writerAgentId": "writer-1", "commit": "writer-commit"},
            {"writerAgentId": "writer-1", "commit": "fix-commit"}
        ])
    );
    assert_eq!(
        runtime.followups.lock().unwrap().front().cloned(),
        Some(NativeWorkflowAgentTurnRequest {
            agent_id: "writer-1".to_string(),
            prompt: "Fix this verified finding in the same writer context and commit the fix.\n\ncorrectness: Broken behavior\ndetails".to_string(),
        })
    );
}

#[tokio::test]
async fn missing_agent_runtime_returns_blocked_not_preview() {
    let tempdir = tempfile::tempdir().unwrap();
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: None,
        model_provider_catalog: None,
        cancellation_token: None,
    };

    let output = run_dev_cycle(ctx, json!({ "reviewTypes": ["tests"] }))
        .await
        .unwrap()
        .output;

    assert_eq!(output["status"], json!("blocked"));
    assert_eq!(
        output["blockedReason"],
        json!("native agent runtime is unavailable")
    );
}

#[tokio::test]
async fn grouping_challenger_runs_shadow_baseline_and_promotes_when_no_loss() {
    let tempdir = tempfile::tempdir().unwrap();
    let state = DevCycleState::open(tempdir.path()).unwrap();
    state
        .record_split_attempt(SplitAttemptRecord {
            run_id: "seed",
            model_key: "openai:gpt-5.5:xhigh",
            repo_tshirt_bucket: "XS",
            item_set_key: "seed",
            strategy_id: "separate:v1",
            groups_json: "[]",
            reviewer_group_count: 2,
            baseline_strategy_id: None,
            baseline_group_count: None,
            status: "separate",
            reviewer_count_savings: 0,
            lost_evidence_count: 0,
        })
        .unwrap();
    let runtime = FakeAgentRuntime::new(FakeMode::GroupingNoLoss);
    let catalog = FakeCatalog {
        candidates: vec![NativeWorkflowModelCandidate {
            provider_id: "openai".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: Some("xhigh".to_string()),
            intelligence_score: Some(1.0),
        }],
    };
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: Some(&runtime),
        model_provider_catalog: Some(&catalog),
        cancellation_token: None,
    };

    let output = run_dev_cycle(
        ctx,
        json!({
            "taskDescription": "fix bug",
            "reviewTypes": ["correctness", "tests"],
            "experimentSampleRate": 1.0,
            "baselineResampleRate": 0.0,
            "testMode": "provided",
            "testCommands": ["printf ok"]
        }),
    )
    .await
    .unwrap()
    .output;

    assert_eq!(output["status"], json!("succeeded"));
    assert_eq!(
        output["reviewSplit"]["primarySplit"]["strategyId"],
        json!("grouping:v1")
    );
    assert_eq!(
        output["reviewSplit"]["baselineResample"]["ran"],
        json!(true)
    );
    assert_eq!(output["reviewSplit"]["lostEvidenceCount"], json!(0));
    assert_eq!(
        output["reviewSplit"]["promotionReason"],
        json!("saved 1 reviewer group(s) with no lost baseline evidence")
    );
}

#[tokio::test]
async fn verified_lost_baseline_finding_suppresses_grouping() {
    let tempdir = tempfile::tempdir().unwrap();
    let state = DevCycleState::open(tempdir.path()).unwrap();
    state
        .record_split_attempt(SplitAttemptRecord {
            run_id: "seed",
            model_key: "openai:gpt-5.5:xhigh",
            repo_tshirt_bucket: "XS",
            item_set_key: "seed",
            strategy_id: "separate:v1",
            groups_json: "[]",
            reviewer_group_count: 2,
            baseline_strategy_id: None,
            baseline_group_count: None,
            status: "separate",
            reviewer_count_savings: 0,
            lost_evidence_count: 0,
        })
        .unwrap();
    let runtime = FakeAgentRuntime::new(FakeMode::GroupingLost);
    let catalog = FakeCatalog {
        candidates: vec![NativeWorkflowModelCandidate {
            provider_id: "openai".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: Some("xhigh".to_string()),
            intelligence_score: Some(1.0),
        }],
    };
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: Some(&runtime),
        model_provider_catalog: Some(&catalog),
        cancellation_token: None,
    };

    let output = run_dev_cycle(
        ctx,
        json!({
            "taskDescription": "fix bug",
            "reviewTypes": ["correctness", "tests"],
            "experimentSampleRate": 1.0,
            "testMode": "provided",
            "testCommands": ["printf ok"]
        }),
    )
    .await
    .unwrap()
    .output;

    assert_eq!(output["reviewSplit"]["lostEvidenceCount"], json!(1));
    assert_eq!(
        output["reviewSplit"]["suppressionReason"],
        json!("1 verifier-accepted baseline finding(s) were lost")
    );
}

#[tokio::test]
async fn grouping_proposer_failure_falls_back_to_separate_review() {
    let tempdir = tempfile::tempdir().unwrap();
    seed_split_evidence(tempdir.path());
    let runtime = FakeAgentRuntime::new(FakeMode::ProposalFails);
    let catalog = review_catalog();
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: Some(&runtime),
        model_provider_catalog: Some(&catalog),
        cancellation_token: None,
    };

    let output = run_dev_cycle(
        ctx,
        json!({
            "taskDescription": "fix bug",
            "reviewTypes": ["correctness", "tests"],
            "experimentSampleRate": 1.0,
            "testMode": "provided",
            "testCommands": ["printf ok"]
        }),
    )
    .await
    .unwrap()
    .output;

    assert_eq!(output["status"], json!("succeeded"));
    assert_eq!(
        output["reviewSplit"]["primarySplit"]["strategyId"],
        json!("separate:v1")
    );
    assert_eq!(
        output["reviewSplit"]["proposalStatus"]["status"],
        json!("failed")
    );
    assert_eq!(
        output["reviewSplit"]["stopReason"],
        json!("grouping proposal unavailable")
    );
}

#[tokio::test]
async fn shadow_baseline_failure_keeps_run_grouped_but_unpromoted() {
    let tempdir = tempfile::tempdir().unwrap();
    seed_split_evidence(tempdir.path());
    let runtime = FakeAgentRuntime::new(FakeMode::BaselineFails);
    let catalog = review_catalog();
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: Some(&runtime),
        model_provider_catalog: Some(&catalog),
        cancellation_token: None,
    };

    let output = run_dev_cycle(
        ctx,
        json!({
            "taskDescription": "fix bug",
            "reviewTypes": ["correctness", "tests"],
            "experimentSampleRate": 1.0,
            "testMode": "provided",
            "testCommands": ["printf ok"]
        }),
    )
    .await
    .unwrap()
    .output;

    assert_eq!(output["status"], json!("succeeded"));
    assert_eq!(
        output["reviewSplit"]["primarySplit"]["strategyId"],
        json!("grouping:v1")
    );
    assert_eq!(
        output["reviewSplit"]["baselineResample"]["ran"],
        json!(false)
    );
    assert_eq!(
        output["reviewSplit"]["baselineResample"]["reason"],
        json!("shadow baseline review failed: baseline unavailable")
    );
    assert!(output["reviewSplit"]["promotionReason"].is_null());
}

#[tokio::test]
async fn new_model_starts_with_separate_even_when_other_model_has_grouping_evidence() {
    let tempdir = tempfile::tempdir().unwrap();
    let state = DevCycleState::open(tempdir.path()).unwrap();
    state
        .record_split_attempt(SplitAttemptRecord {
            run_id: "seed",
            model_key: "openai:gpt-5.4:xhigh",
            repo_tshirt_bucket: "XS",
            item_set_key: "seed",
            strategy_id: "grouping:v1",
            groups_json: "[]",
            reviewer_group_count: 1,
            baseline_strategy_id: Some("separate:v1"),
            baseline_group_count: Some(2),
            status: "accepted",
            reviewer_count_savings: 1,
            lost_evidence_count: 0,
        })
        .unwrap();
    let runtime = FakeAgentRuntime::default();
    let catalog = FakeCatalog {
        candidates: vec![NativeWorkflowModelCandidate {
            provider_id: "openai".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: Some("xhigh".to_string()),
            intelligence_score: Some(1.0),
        }],
    };
    let ctx = NativeWorkflowRunContext {
        codex_home: tempdir.path(),
        cwd: tempdir.path(),
        state_dir: tempdir.path(),
        output_format: None,
        event_handler: None,
        agent_runtime: Some(&runtime),
        model_provider_catalog: Some(&catalog),
        cancellation_token: None,
    };

    let output = run_dev_cycle(
        ctx,
        json!({
            "taskDescription": "fix bug",
            "reviewTypes": ["correctness", "tests"],
            "experimentSampleRate": 1.0,
            "testMode": "provided",
            "testCommands": ["printf ok"]
        }),
    )
    .await
    .unwrap()
    .output;

    assert_eq!(
        output["reviewSplit"]["primarySplit"]["strategyId"],
        json!("separate:v1")
    );
    assert_eq!(
        output["reviewSplit"]["stopReason"],
        json!("new model key starts with separate:v1 until baseline evidence exists")
    );
}

#[derive(Default)]
struct FakeAgentRuntime {
    followups: Mutex<VecDeque<NativeWorkflowAgentTurnRequest>>,
    mode: FakeMode,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum FakeMode {
    #[default]
    SingleReview,
    GroupingNoLoss,
    GroupingLost,
    ProposalFails,
    BaselineFails,
}

impl FakeAgentRuntime {
    fn new(mode: FakeMode) -> Self {
        Self {
            followups: Mutex::new(VecDeque::new()),
            mode,
        }
    }
}

impl NativeWorkflowAgentRuntime for FakeAgentRuntime {
    fn spawn_agent(
        &self,
        request: NativeWorkflowAgentSpawnRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<NativeWorkflowAgentHandle>> + Send + '_>> {
        Box::pin(async move {
            Ok(NativeWorkflowAgentHandle {
                id: request.name,
                name: request.role,
            })
        })
    }

    fn send_follow_up(
        &self,
        request: NativeWorkflowAgentTurnRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.followups.lock().unwrap().push_back(request);
            Ok(())
        })
    }

    fn wait_for_output(
        &self,
        agent_id: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<NativeWorkflowAgentOutput>> + Send + '_>> {
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            let text = match agent_id.as_str() {
                "planner" => "{\"packets\":[\"implement\"]}",
                "writer-1" if self.followups.lock().unwrap().is_empty() => {
                    "{\"commits\":[\"writer-commit\"]}"
                }
                "writer-1" => "{\"commits\":[\"fix-commit\"]}",
                "review-split-proposer" if self.mode == FakeMode::ProposalFails => {
                    return Err(anyhow::anyhow!("proposer unavailable"));
                }
                "review-split-proposer"
                    if matches!(
                        self.mode,
                        FakeMode::GroupingNoLoss | FakeMode::GroupingLost | FakeMode::BaselineFails
                    ) =>
                {
                    "{\"strategyId\":\"grouping:v1\",\"groups\":[{\"groupId\":\"quality\",\"reviewTypeIds\":[\"correctness\",\"tests\"]}],\"rationale\":\"correctness and tests overlap\",\"expectedReviewerCountSavings\":1,\"riskNotes\":[]}"
                }
                "review-split-proposer" => "{}",
                agent
                    if agent.starts_with("reviewer-grouping-v1")
                        && self.mode == FakeMode::GroupingLost =>
                {
                    "{\"findings\":[]}"
                }
                agent if agent.starts_with("reviewer-grouping-v1") => {
                    "{\"findings\":[{\"reviewTypeId\":\"correctness\",\"title\":\"Broken behavior\",\"details\":\"details\",\"filePath\":\"src/lib.rs\",\"line\":1,\"severity\":\"high\"}]}"
                }
                agent
                    if agent.starts_with("baseline-reviewer-separate-v1-correctness")
                        && self.mode == FakeMode::BaselineFails =>
                {
                    return Err(anyhow::anyhow!("baseline unavailable"));
                }
                agent
                    if agent.starts_with("baseline-reviewer-separate-v1-correctness")
                        && self.mode == FakeMode::GroupingLost =>
                {
                    "{\"findings\":[{\"title\":\"Baseline only\",\"details\":\"details\",\"filePath\":\"src/lib.rs\",\"line\":1,\"severity\":\"high\"}]}"
                }
                agent if agent.starts_with("baseline-reviewer-separate-v1-correctness") => {
                    "{\"findings\":[{\"title\":\"Broken behavior\",\"details\":\"details\",\"filePath\":\"src/lib.rs\",\"line\":1,\"severity\":\"high\"}]}"
                }
                agent if agent.starts_with("baseline-reviewer-separate-v1-tests") => {
                    "{\"findings\":[]}"
                }
                "reviewer-correctness" => {
                    "{\"findings\":[{\"title\":\"Broken behavior\",\"details\":\"details\",\"filePath\":\"src/lib.rs\",\"line\":1,\"severity\":\"high\"}]}"
                }
                "reviewer-tests" => "{\"findings\":[]}",
                agent if agent.starts_with("verifier-") => {
                    "{\"accepted\":true,\"reason\":\"confirmed\"}"
                }
                "integrator" => "{\"integrationBranch\":\"integration/dev-cycle\"}",
                _ => "{}",
            };
            Ok(NativeWorkflowAgentOutput {
                agent_id,
                text: text.to_string(),
                metadata: JsonValue::Null,
            })
        })
    }
}

struct FakeCatalog {
    candidates: Vec<NativeWorkflowModelCandidate>,
}

impl NativeWorkflowModelProviderCatalog for FakeCatalog {
    fn model_candidates(&self) -> Vec<NativeWorkflowModelCandidate> {
        self.candidates.clone()
    }
}

fn seed_split_evidence(state_dir: &std::path::Path) {
    let state = DevCycleState::open(state_dir).unwrap();
    state
        .record_split_attempt(SplitAttemptRecord {
            run_id: "seed",
            model_key: "openai:gpt-5.5:xhigh",
            repo_tshirt_bucket: "XS",
            item_set_key: "seed",
            strategy_id: "separate:v1",
            groups_json: "[]",
            reviewer_group_count: 2,
            baseline_strategy_id: None,
            baseline_group_count: None,
            status: "separate",
            reviewer_count_savings: 0,
            lost_evidence_count: 0,
        })
        .unwrap();
}

fn review_catalog() -> FakeCatalog {
    FakeCatalog {
        candidates: vec![NativeWorkflowModelCandidate {
            provider_id: "openai".to_string(),
            model: "gpt-5.5".to_string(),
            reasoning_effort: Some("xhigh".to_string()),
            intelligence_score: Some(1.0),
        }],
    }
}
