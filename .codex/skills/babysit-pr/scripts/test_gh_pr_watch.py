import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest


MODULE_PATH = Path(__file__).with_name("gh_pr_watch.py")
sys.path.insert(0, str(MODULE_PATH.parent))
MODULE_SPEC = importlib.util.spec_from_file_location("gh_pr_watch", MODULE_PATH)
gh_pr_watch = importlib.util.module_from_spec(MODULE_SPEC)
assert MODULE_SPEC.loader is not None
MODULE_SPEC.loader.exec_module(gh_pr_watch)


def sample_pr():
    return {
        "number": 123,
        "url": "https://github.com/openai/codex/pull/123",
        "repo": "openai/codex",
        "base_sha": "base123",
        "base_branch": "main",
        "head_sha": "abc123",
        "head_branch": "feature",
        "state": "OPEN",
        "merged": False,
        "closed": False,
        "mergeable": "MERGEABLE",
        "merge_state_status": "CLEAN",
        "review_decision": "",
    }


def sample_checks(**overrides):
    checks = {
        "pending_count": 0,
        "failed_count": 0,
        "passed_count": 12,
        "all_terminal": True,
        "total_count": 12,
    }
    checks.update(overrides)
    return checks


def sample_snapshot(checks=None, check_details=None, **overrides):
    snapshot = {
        "pr": sample_pr(),
        "ci": {"revision": "ci-v1"},
        "checks": checks or sample_checks(),
        "check_details": check_details or [],
        "failed_runs": [],
        "failed_jobs": [],
        "new_review_items": [],
        "actions": ["idle"],
    }
    snapshot.update(overrides)
    return snapshot


def sample_check_detail(index, status="passed", padding=""):
    state = {"running": "IN_PROGRESS", "queued": "QUEUED"}.get(status, "COMPLETED")
    return {
        "id": f"CI\x1fcheck-{index}{padding}",
        "workflow": f"CI{padding}",
        "name": f"check-{index}{padding}",
        "status": status,
        "state": state,
        "started_at": "2026-09-21T10:00:00Z",
        "completed_at": "2026-09-21T10:01:00Z" if state == "COMPLETED" else "",
        "url": f"https://github.com/openai/codex/actions/runs/99/job/{index}{padding}",
        "run_id": 99,
        "job_id": index,
    }


def test_collect_snapshot_fetches_review_items_before_ci(monkeypatch, tmp_path):
    call_order = []
    pr = sample_pr()

    monkeypatch.setattr(gh_pr_watch, "resolve_pr", lambda *args, **kwargs: pr)
    monkeypatch.setattr(gh_pr_watch, "load_state", lambda path: ({}, True))
    monkeypatch.setattr(
        gh_pr_watch,
        "get_authenticated_login",
        lambda: call_order.append("auth") or "octocat",
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "fetch_new_review_items",
        lambda *args, **kwargs: call_order.append("review") or [],
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "get_pr_checks",
        lambda *args, **kwargs: call_order.append("checks") or [],
    )
    monkeypatch.setattr(
        gh_pr_watch.ci_wait,
        "get_ci_config_revision",
        lambda *args, **kwargs: call_order.append("ci_revision") or "ci-v1",
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "summarize_checks",
        lambda checks: call_order.append("summarize") or sample_checks(),
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "get_workflow_runs_for_sha",
        lambda *args, **kwargs: call_order.append("workflow") or [],
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "failed_runs_from_workflow_runs",
        lambda *args, **kwargs: call_order.append("failed_runs") or [],
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "failed_jobs_from_workflow_runs",
        lambda *args, **kwargs: call_order.append("failed_jobs") or [],
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "recommend_actions",
        lambda *args, **kwargs: call_order.append("recommend") or ["idle"],
    )
    monkeypatch.setattr(gh_pr_watch, "save_state", lambda *args, **kwargs: None)

    args = argparse.Namespace(
        pr="123",
        repo=None,
        state_file=str(tmp_path / "watcher-state.json"),
        max_flaky_retries=3,
    )

    gh_pr_watch.collect_snapshot(args)

    assert call_order.index("review") < call_order.index("checks")
    assert call_order.index("review") < call_order.index("workflow")


def test_base_change_requires_fresh_check_observation(monkeypatch, tmp_path):
    pr = sample_pr()
    raw_check = {
        "name": "tests",
        "workflow": "CI",
        "event": "pull_request",
        "bucket": "pass",
        "state": "COMPLETED",
        "link": "https://github.com/openai/codex/actions/runs/99/job/1",
        "startedAt": "2026-09-21T10:00:00Z",
        "completedAt": "2026-09-21T10:01:00Z",
    }
    raw_check_two = dict(
        raw_check,
        name="lint",
        link="https://github.com/openai/codex/actions/runs/100/job/2",
    )
    normalized = [
        gh_pr_watch.ci_wait.normalize_check(raw_check),
        gh_pr_watch.ci_wait.normalize_check(raw_check_two),
    ]
    state = {
        "last_seen_head_sha": pr["head_sha"],
        "check_registration": {
            "generation": f"{pr['head_sha']}\x1fold-base",
            "signature": gh_pr_watch.ci_wait.check_signature(normalized),
            "observation_signature": gh_pr_watch.ci_wait.check_observation_signature(
                normalized
            ),
            "observations": sorted(
                gh_pr_watch.ci_wait.check_observation_id(check) for check in normalized
            ),
            "current": True,
        },
    }
    checks = [raw_check, raw_check_two]
    monkeypatch.setattr(gh_pr_watch, "load_state", lambda path: (state, False))
    monkeypatch.setattr(gh_pr_watch, "save_state", lambda *args: None)
    monkeypatch.setattr(gh_pr_watch, "get_authenticated_login", lambda: "octocat")
    monkeypatch.setattr(gh_pr_watch, "fetch_new_review_items", lambda *args, **kwargs: [])
    monkeypatch.setattr(gh_pr_watch.ci_wait, "get_ci_config_revision", lambda *args: "ci")
    monkeypatch.setattr(gh_pr_watch, "get_pr_checks", lambda *args, **kwargs: checks)
    monkeypatch.setattr(gh_pr_watch, "get_workflow_runs_for_sha", lambda *args: [])
    args = argparse.Namespace(
        pr="123",
        repo=None,
        state_file=str(tmp_path / "watcher-state.json"),
        max_flaky_retries=3,
        wait_for="finished",
    )

    stale, _ = gh_pr_watch._collect_snapshot(
        args, pr, tmp_path / "watcher-state.json"
    )
    assert stale["ci"]["head_refresh_pending"] is True
    assert stale["ci"]["check_set_current"] is False
    assert stale["actions"] == ["idle"]

    checks[0] = dict(
        raw_check,
        startedAt="2026-09-21T10:02:00Z",
        completedAt="2026-09-21T10:03:00Z",
    )
    partial, _ = gh_pr_watch._collect_snapshot(
        args, pr, tmp_path / "watcher-state.json"
    )
    assert partial["ci"]["head_refresh_pending"] is True
    assert partial["ci"]["check_set_current"] is False

    checks[1] = dict(
        raw_check_two,
        startedAt="2026-09-21T10:02:00Z",
        completedAt="2026-09-21T10:03:00Z",
    )
    fresh, _ = gh_pr_watch._collect_snapshot(
        args, pr, tmp_path / "watcher-state.json"
    )
    assert fresh["ci"]["head_refresh_pending"] is False
    assert fresh["ci"]["check_set_current"] is True

    state.clear()
    state.update(
        {
            "last_seen_head_sha": pr["head_sha"],
            "last_snapshot_at": gh_pr_watch.ci_wait.parse_github_time(
                "2026-09-21T11:00:00Z"
            ),
        }
    )
    checks[:] = [raw_check, raw_check_two]
    legacy, _ = gh_pr_watch._collect_snapshot(
        args, pr, tmp_path / "watcher-state.json"
    )
    assert legacy["ci"]["head_refresh_pending"] is True
    assert legacy["ci"]["check_set_current"] is False


def test_recommend_actions_prioritizes_review_comments():
    actions = gh_pr_watch.recommend_actions(
        sample_pr(),
        sample_checks(failed_count=1),
        [{"run_id": 99}],
        [],
        [{"kind": "review_comment", "id": "1"}],
        0,
        3,
    )

    assert actions == [
        "process_review_comment",
        "diagnose_ci_failure",
        "retry_failed_checks",
    ]
    cancelled = gh_pr_watch.recommend_actions(
        sample_pr(), sample_checks(), [{"conclusion": "cancelled"}], [], [], 0, 3
    )
    assert cancelled == ["diagnose_ci_failure", "retry_failed_checks"]
    assert gh_pr_watch.recommend_actions(
        sample_pr(), sample_checks(), [], [], [], 0, 3, registration_stable=False
    ) == ["idle"]
    assert gh_pr_watch.recommend_actions(
        sample_pr(), sample_checks(failed_count=1), [{"run_id": 1}], [], [], 0, 3,
        registration_stable=False,
    ) == ["diagnose_ci_failure"]


def test_pending_review_feedback_surfaces_only_after_publication(monkeypatch):
    state = {
        "seen_review_comment_ids": ["20"],
        "seen_review_ids": ["10"],
    }
    review = {
        "id": 10,
        "user": {"login": "octocat"},
        "author_association": "MEMBER",
        "state": "PENDING",
        "body": "Please rename this.",
        "created_at": "2026-06-08T10:00:00Z",
        "submitted_at": None,
        "html_url": "https://github.com/openai/codex/pull/123#pullrequestreview-10",
    }
    review_comment = {
        "id": 20,
        "pull_request_review_id": 10,
        "user": {"login": "octocat"},
        "author_association": "MEMBER",
        "body": "Please rename this.",
        "created_at": "2026-06-08T10:00:00Z",
        "path": "src/example.rs",
        "line": 7,
        "html_url": "https://github.com/openai/codex/pull/123#discussion_r20",
    }

    def fake_list(endpoint, **kwargs):
        if endpoint.endswith("/issues/123/comments"):
            return []
        if endpoint.endswith("/pulls/123/comments"):
            return [review_comment]
        if endpoint.endswith("/pulls/123/reviews"):
            return [review]
        raise AssertionError(f"unexpected endpoint: {endpoint}")

    monkeypatch.setattr(gh_pr_watch, "gh_api_list_paginated", fake_list)

    assert (
        gh_pr_watch.fetch_new_review_items(
            sample_pr(),
            state,
            fresh_state=True,
            authenticated_login="octocat",
        )
        == []
    )
    assert state["seen_review_comment_ids"] == []
    assert state["seen_review_ids"] == []

    review["state"] = "COMMENTED"
    review["submitted_at"] = "2026-06-08T10:05:00Z"

    published_items = gh_pr_watch.fetch_new_review_items(
        sample_pr(),
        state,
        fresh_state=False,
        authenticated_login="octocat",
    )

    assert {(item["kind"], item["id"]) for item in published_items} == {
        ("review", "10"),
        ("review_comment", "20"),
    }
    gh_pr_watch.mark_review_items_seen(state, published_items)
    assert state["seen_review_comment_ids"] == ["20"]
    assert state["seen_review_ids"] == ["10"]


def test_review_feedback_pages_without_losing_items(monkeypatch, tmp_path):
    comments = [
        {
            "id": index,
            "user": {"login": "octocat"},
            "author_association": "OWNER",
            "body": f"comment {index}",
            "created_at": f"2026-09-21T10:{index:02d}:00Z",
            "html_url": f"https://github.com/openai/codex/pull/123#issuecomment-{index}",
        }
        for index in range(1, 13)
    ]

    def fake_list(endpoint, **kwargs):
        return comments if endpoint.endswith("/issues/123/comments") else []

    monkeypatch.setattr(gh_pr_watch, "gh_api_list_paginated", fake_list)
    state = {}
    surfaced = []
    for _ in comments:
        pending = gh_pr_watch.fetch_new_review_items(
            sample_pr(), state, fresh_state=False, authenticated_login="octocat"
        )
        page = pending[:1]
        surfaced.extend(item["id"] for item in page)
        gh_pr_watch.mark_review_items_seen(state, page)

    assert surfaced == [str(index) for index in range(1, 13)]
    assert gh_pr_watch.fetch_new_review_items(
        sample_pr(), state, fresh_state=False, authenticated_login="octocat"
    ) == []

    state_path = tmp_path / "pr-123.json"
    gh_pr_watch.save_state(state_path, {})
    gh_pr_watch.acknowledge_review_items(
        {"new_review_items": [{"kind": "issue_comment", "id": "99"}]}, state_path
    )
    acknowledged, _ = gh_pr_watch.load_state(state_path)
    assert acknowledged["seen_issue_comment_ids"] == ["99"]


def test_run_watch_keeps_polling_open_ready_to_merge_pr(monkeypatch):
    sleeps = []
    events = []
    check_details = [sample_check_detail(index) for index in range(12)]
    snapshot = {
        "pr": sample_pr(),
        "checks": sample_checks(),
        "check_details": check_details,
        "failed_runs": [],
        "failed_jobs": [],
        "new_review_items": [],
        "actions": ["ready_to_merge"],
        "retry_state": {
            "current_sha_retries_used": 0,
            "max_flaky_retries": 3,
        },
    }

    monkeypatch.setattr(
        gh_pr_watch,
        "collect_snapshot",
        lambda args: (snapshot, Path("/tmp/codex-babysit-pr-state.json")),
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "print_event",
        lambda event, payload: events.append((event, payload)),
    )

    class StopWatch(Exception):
        pass

    def fake_sleep(seconds):
        sleeps.append(seconds)
        if len(sleeps) >= 2:
            raise StopWatch

    monkeypatch.setattr(gh_pr_watch.time, "sleep", fake_sleep)

    with pytest.raises(StopWatch):
        gh_pr_watch.run_watch(argparse.Namespace(poll_seconds=30))

    assert sleeps == [30, 30]
    assert [event for event, _ in events] == ["snapshot"]
    summary = events[0][1]["snapshot"]["check_details_summary"]
    assert summary["total_count"] == 12
    assert summary["emitted_count"] == len(
        events[0][1]["snapshot"]["check_details"]
    )
    assert summary["omitted_count"] == 12 - summary["emitted_count"]
    assert summary["truncated"] is True
    assert snapshot["check_details"] == check_details


def test_run_watch_emits_registration_grace_transition(monkeypatch):
    before = sample_snapshot(actions=["idle"])
    before["ci"]["registration_stable"] = False
    after = sample_snapshot(actions=["ready_to_merge"])
    after["ci"]["registration_stable"] = True
    snapshots = iter([before, after])
    events = []
    monkeypatch.setattr(
        gh_pr_watch,
        "collect_snapshot",
        lambda args: (next(snapshots), Path("/tmp/pr-state.json")),
    )
    monkeypatch.setattr(
        gh_pr_watch,
        "print_event",
        lambda event, payload: events.append((event, payload)),
    )

    class StopWatch(Exception):
        pass

    sleeps = []

    def stop_after_transition(seconds):
        sleeps.append(seconds)
        if len(sleeps) == 2:
            raise StopWatch

    monkeypatch.setattr(gh_pr_watch.time, "sleep", stop_after_transition)
    with pytest.raises(StopWatch):
        gh_pr_watch.run_watch(argparse.Namespace(poll_seconds=30))

    assert [event for event, _ in events] == ["snapshot", "snapshot"]


def test_check_detail_output_is_hard_capped_and_prioritizes_failures():
    at_cap = [
        {"name": f"check-{index}", "status": "passed"}
        for index in range(gh_pr_watch.ci_wait.MAX_OUTPUT_CHECK_DETAILS)
    ]
    details, summary = gh_pr_watch.ci_wait.bounded_check_details(at_cap)
    assert details == at_cap
    assert summary == {
        "total_count": 10,
        "emitted_count": 10,
        "omitted_count": 0,
        "truncated": False,
    }

    over_cap = at_cap + [{"name": "last", "status": "passed"}]
    _, summary = gh_pr_watch.ci_wait.bounded_check_details(over_cap)
    assert summary == {
        "total_count": 11,
        "emitted_count": 10,
        "omitted_count": 1,
        "truncated": True,
    }

    long_checks = [sample_check_detail(index, padding="x" * 1_000) for index in range(15)]
    long_checks[-1]["status"] = "failed"
    snapshot = sample_snapshot(check_details=long_checks)
    output = gh_pr_watch.snapshot_for_output(snapshot)

    assert output["check_details"][0]["status"] == "failed"
    assert output["check_details_summary"]["total_count"] == 15
    assert output["check_details_summary"]["truncated"] is True
    assert (
        len(json.dumps(output["check_details"], sort_keys=True))
        <= gh_pr_watch.ci_wait.MAX_OUTPUT_CHECK_DETAILS_JSON_CHARS
    )
    assert len(snapshot["check_details"]) == 15
    assert len(snapshot["check_details"][-1]["name"]) > len(output["check_details"][0]["name"])

    oversized = [
        {
            "kind": "review_comment",
            "id": str(index),
            "author": "reviewer",
            "body": "x" * 20_000,
            "path": "nested/" + "very-long-directory/" * 20 + "file.rs",
            "url": f"https://github.com/openai/codex/pull/123#discussion-{index}",
        }
        for index in range(20)
    ]
    bounded = gh_pr_watch.snapshot_for_output(
        sample_snapshot(
            check_details=long_checks,
            failed_runs=[{"run_id": index, "workflow_name": "x" * 1_000} for index in range(20)],
            failed_jobs=[{"job_id": index, "job_name": "x" * 1_000} for index in range(20)],
            new_review_items=oversized,
        )
    )
    for name in ("failed_runs", "failed_jobs", "new_review_items"):
        assert bounded[f"{name}_summary"]["truncated"] is True
        assert bounded[f"{name}_summary"]["total_count"] == 20
    assert len(bounded["new_review_items"][0]["body"]) <= 256
    assert len(bounded["new_review_items"]) == 1
    assert (
        len(json.dumps(bounded, sort_keys=True))
        <= gh_pr_watch.ci_wait.MAX_OUTPUT_PAYLOAD_JSON_CHARS
    )

    error = subprocess.CalledProcessError(
        1,
        ["gh"],
        output="🔥" * 5_000,
        stderr="🔥" * 5_000,
    )
    formatted = gh_pr_watch._format_gh_error(["gh", "api"], error)
    assert len(formatted.encode("utf-8")) <= 2_400


def test_once_and_retry_emit_bounded_snapshots(monkeypatch, tmp_path):
    check_details = [sample_check_detail(index) for index in range(12)]
    snapshot = sample_snapshot(
        check_details=check_details,
        retry_state={"current_sha_retries_used": 0, "max_flaky_retries": 3},
    )
    snapshot["ci"].update({"rerun_pending": False, "check_set_current": True})
    state_path = tmp_path / "pr-123.json"
    monkeypatch.setattr(gh_pr_watch, "collect_snapshot", lambda args: (snapshot, state_path))
    monkeypatch.setattr(gh_pr_watch, "resolve_pr", lambda *args, **kwargs: sample_pr())
    monkeypatch.setattr(
        gh_pr_watch,
        "_collect_snapshot",
        lambda args, pr, path: (snapshot, state_path),
    )

    emitted = []
    monkeypatch.setattr(
        gh_pr_watch,
        "parse_args",
        lambda: argparse.Namespace(
            retry_failed_now=False,
            watch=False,
            wait_for=None,
        ),
    )
    monkeypatch.setattr(gh_pr_watch, "print_json", emitted.append)

    assert gh_pr_watch.main() == 0
    assert emitted[0]["check_details_summary"]["truncated"] is True
    assert emitted[0]["state_file"] == str(state_path)

    result = gh_pr_watch.retry_failed_now(
        argparse.Namespace(pr="123", repo=None, state_file=str(state_path))
    )
    assert result["reason"] == "no_failed_pr_checks"
    assert result["snapshot"]["check_details_summary"]["truncated"] is True
    assert len(snapshot["check_details"]) == 12


def test_failed_jobs_include_direct_logs_endpoint(monkeypatch):
    jobs_by_run = {
        99: [
            {
                "id": 555,
                "name": "unit tests",
                "status": "completed",
                "conclusion": "failure",
                "html_url": "https://github.com/openai/codex/actions/runs/99/job/555",
            },
            {
                "id": 556,
                "name": "lint",
                "status": "completed",
                "conclusion": "success",
            },
        ]
    }

    monkeypatch.setattr(
        gh_pr_watch,
        "get_jobs_for_run",
        lambda repo, run_id: jobs_by_run[run_id],
    )

    failed_jobs = gh_pr_watch.failed_jobs_from_workflow_runs(
        "openai/codex",
        [
            {
                "id": 99,
                "name": "CI",
                "status": "in_progress",
                "conclusion": "",
                "head_sha": "abc123",
            }
        ],
        "abc123",
    )

    assert failed_jobs == [
        {
            "run_id": 99,
            "workflow_name": "CI",
            "run_status": "in_progress",
            "run_conclusion": "",
            "job_id": 555,
            "job_name": "unit tests",
            "status": "completed",
            "conclusion": "failure",
            "html_url": "https://github.com/openai/codex/actions/runs/99/job/555",
            "logs_endpoint": "repos/openai/codex/actions/jobs/555/logs",
        }
    ]
    before = sample_snapshot(
        check_details=[{"id": "a", "status": "failed"}, {"id": "b", "status": "passed"}],
        failed_runs=[{"run_id": 1, "run_attempt": 1, "conclusion": "failure"}],
    )
    after = sample_snapshot(
        check_details=[{"id": "a", "status": "passed"}, {"id": "b", "status": "failed"}],
        failed_runs=[{"run_id": 2, "run_attempt": 1, "conclusion": "failure"}],
    )
    assert gh_pr_watch.snapshot_change_key(before) != gh_pr_watch.snapshot_change_key(after)


def test_workflow_run_lookup_paginates(monkeypatch):
    calls = []

    def fake_gh_json(args, repo=None):
        page = int(args[args.index("-f", args.index("per_page=100") + 1) + 1].split("=")[1])
        calls.append(page)
        count = 100 if page == 1 else 1
        return {"workflow_runs": [{"id": page * 1_000 + index} for index in range(count)]}

    monkeypatch.setattr(gh_pr_watch, "gh_json", fake_gh_json)

    runs = gh_pr_watch.get_workflow_runs_for_sha("openai/codex", "abc123")

    assert len(runs) == 101
    assert calls == [1, 2]


def test_failed_runs_only_include_current_checks_and_latest_pr_startup_failure():
    failed_runs = gh_pr_watch.failed_runs_from_workflow_runs(
        [
            {
                "id": 99,
                "run_attempt": 2,
                "run_number": 4,
                "workflow_id": 1,
                "event": "pull_request",
                "name": "CI",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "abc123",
                "html_url": "https://github.com/openai/codex/actions/runs/99",
            },
            {
                "id": 100,
                "run_number": 3,
                "workflow_id": 2,
                "event": "pull_request",
                "pull_requests": [{"number": 123}],
                "name": "CI startup",
                "status": "completed",
                "conclusion": "startup_failure",
                "head_sha": "abc123",
                "html_url": "https://github.com/openai/codex/actions/runs/100",
            },
            {
                "id": 101,
                "run_number": 2,
                "workflow_id": 2,
                "event": "pull_request",
                "name": "older startup failure",
                "status": "completed",
                "conclusion": "startup_failure",
                "head_sha": "abc123",
            },
            {
                "id": 102,
                "workflow_id": 3,
                "event": "workflow_dispatch",
                "name": "manual failure",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "abc123",
            },
            {
                "id": 103,
                "workflow_id": 4,
                "event": "pull_request",
                "pull_requests": [{"number": 456}],
                "name": "other PR",
                "status": "completed",
                "conclusion": "startup_failure",
                "head_sha": "abc123",
            },
            {
                "id": 104,
                "event": "pull_request",
                "name": "stale same-SHA failure",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "abc123",
            },
            {
                "id": 105,
                "name": "old head",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "old123",
            },
        ],
        "abc123",
        {99, 102},
        123,
    )

    assert failed_runs == [
        {
            "run_id": 99,
            "run_attempt": 2,
            "workflow_name": "CI",
            "status": "completed",
            "conclusion": "failure",
            "html_url": "https://github.com/openai/codex/actions/runs/99",
        },
        {
            "run_id": 100,
            "run_attempt": 1,
            "workflow_name": "CI startup",
            "status": "completed",
            "conclusion": "startup_failure",
            "html_url": "https://github.com/openai/codex/actions/runs/100",
        },
        {
            "run_id": 102,
            "run_attempt": 1,
            "workflow_name": "manual failure",
            "status": "completed",
            "conclusion": "failure",
            "html_url": "",
        }
    ]


def test_wait_for_first_failure_polls_silently(monkeypatch, tmp_path):
    pending_checks = sample_checks(pending_count=1, passed_count=0, all_terminal=False, total_count=1)
    pending = sample_snapshot(pending_checks)
    failed = sample_snapshot(
        sample_checks(pending_count=1, failed_count=1, passed_count=0, all_terminal=False, total_count=2),
        check_details=[sample_check_detail(index, status="failed") for index in range(12)],
        failed_runs=[{"run_id": index, "workflow_name": "CI"} for index in range(20)],
        failed_jobs=[{"job_id": index, "job_name": "tests"} for index in range(20)],
    )
    snapshots = iter([pending, pending, failed])
    emitted, sleeps = [], []
    state_path = tmp_path / "pr-123.json"
    monkeypatch.setattr(gh_pr_watch.ci_wait.time, "time", lambda: 1_000)
    monkeypatch.setattr(gh_pr_watch.ci_wait.time, "sleep", sleeps.append)
    result = gh_pr_watch.ci_wait.run_wait(
        argparse.Namespace(wait_for="first-failure", poll_seconds=30),
        lambda args: (next(snapshots), state_path),
        lambda path: ({}, False),
        lambda path, state: None,
        emitted.append,
    )
    assert (result, sleeps, [item["reason"] for item in emitted]) == (0, [30, 30], ["first_failure"])
    failure_summary = emitted[0]["failures_summary"]
    assert failure_summary["total_count"] == 12
    assert failure_summary["emitted_count"] == len(emitted[0]["failures"])
    assert failure_summary["omitted_count"] == 12 - failure_summary["emitted_count"]
    assert failure_summary["truncated"] is True
    assert emitted[0]["failed_runs_summary"]["truncated"] is True
    assert emitted[0]["failed_jobs_summary"]["truncated"] is True
    generation = ("abc123", "base123", "ci-v1")
    finished = gh_pr_watch.ci_wait.wait_reason("finished", sample_snapshot(), state_path, {}, generation, 1_000)
    assert finished == ("finished", [])
    pending = sample_snapshot(sample_checks(pending_count=1, all_terminal=False))
    assert gh_pr_watch.ci_wait.wait_reason("finished", pending, state_path, {}, generation, 1_000) == (None, [])
    empty = sample_snapshot(sample_checks(passed_count=0, total_count=0))
    assert gh_pr_watch.ci_wait.wait_reason("finished", empty, state_path, {}, generation, 1_000) == ("no_checks", [])
    failed = sample_snapshot(sample_checks(failed_count=1, passed_count=11))
    assert gh_pr_watch.ci_wait.wait_reason("finished", failed, state_path, {}, generation, 1_000) == ("finished", [])
    run_failed = sample_snapshot(failed_runs=[{"conclusion": "startup_failure"}])
    assert gh_pr_watch.ci_wait.wait_reason("first-failure", run_failed, state_path, {}, generation, 1_000)[0] == "first_failure"
    stale = sample_snapshot()
    stale["ci"]["check_set_current"] = False
    assert gh_pr_watch.ci_wait.wait_reason("finished", stale, state_path, {}, generation, 1_000)[0] == "ci_config_changed"
    changed = sample_snapshot(new_review_items=[{"id": "review"}]); changed["pr"]["head_sha"] = "new"
    more = iter([pending, changed]); wake = []
    gh_pr_watch.ci_wait.run_wait(argparse.Namespace(wait_for="finished", poll_seconds=30), lambda args: (next(more), state_path), lambda path: ({}, False), lambda *args: None, wake.append)
    assert wake[0]["also_reasons"] == ["generation_changed"]

    reviews = sample_snapshot(
        new_review_items=[
            {"kind": "review", "id": str(index), "body": "x" * 20_000}
            for index in range(20)
        ]
    )
    review_wake = []
    gh_pr_watch.ci_wait.run_wait(
        argparse.Namespace(wait_for="finished", poll_seconds=30),
        lambda args: (reviews, state_path),
        lambda path: ({}, False),
        lambda *args: None,
        review_wake.append,
    )
    assert review_wake[0]["reason"] == "review_feedback"
    assert review_wake[0]["new_review_items_summary"]["truncated"] is True
    assert len(review_wake[0]["new_review_items"][0]["body"]) <= 256
    assert (
        len(json.dumps(review_wake[0], sort_keys=True))
        <= gh_pr_watch.ci_wait.MAX_OUTPUT_PAYLOAD_JSON_CHARS
    )


def test_finished_wait_honors_registration_grace_and_resets_for_new_checks(
    monkeypatch, tmp_path
):
    snapshots = []
    for terminal_since in (1_000, 1_000, 1_040, 1_040, 1_040):
        snapshot = sample_snapshot()
        snapshot["ci"]["terminal_since"] = terminal_since
        snapshots.append(snapshot)
    now_values = iter([1_000, 1_030, 1_040, 1_099, 1_100])
    sleeps = []
    emitted = []
    monkeypatch.setattr(gh_pr_watch.ci_wait.time, "time", lambda: next(now_values))
    monkeypatch.setattr(gh_pr_watch.ci_wait.time, "sleep", sleeps.append)

    gh_pr_watch.ci_wait.run_wait(
        argparse.Namespace(wait_for="finished", poll_seconds=30),
        lambda args: (snapshots.pop(0), tmp_path / "pr-123.json"),
        lambda path: ({}, False),
        lambda *args: None,
        emitted.append,
    )

    assert sleeps == [30, 30, 30, 30]
    assert [item["reason"] for item in emitted] == ["finished"]

    empty = sample_snapshot(sample_checks(passed_count=0, total_count=0))
    empty["ci"]["terminal_since"] = 2_000
    empty_times = iter([2_000, 2_060])
    empty_wake = []
    monkeypatch.setattr(gh_pr_watch.ci_wait.time, "time", lambda: next(empty_times))
    gh_pr_watch.ci_wait.run_wait(
        argparse.Namespace(wait_for="finished", poll_seconds=30),
        lambda args: (empty, tmp_path / "pr-123.json"),
        lambda path: ({}, False),
        lambda *args: None,
        empty_wake.append,
    )
    assert empty_wake[0]["reason"] == "no_checks"


def test_execution_timeout_uses_persisted_runtime_and_excludes_queue(tmp_path):
    state_path = tmp_path / "pr-123.json"
    state = {}
    completed_payload = {
        "workflow": "CI",
        "name": "tests",
        "bucket": "pass",
        "state": "COMPLETED",
        "startedAt": "2026-09-21T10:00:00Z",
        "completedAt": "2026-09-21T10:01:00Z",
    }
    running_sample = gh_pr_watch.ci_wait.normalize_check(dict(completed_payload, bucket="pending", state="IN_PROGRESS", completedAt=""))
    queued_sample = dict(running_sample, status="queued", state="QUEUED")
    completed = gh_pr_watch.ci_wait.normalize_check(completed_payload)
    started_at = gh_pr_watch.ci_wait.parse_github_time("2026-09-21T10:00:00Z")
    completed_at = gh_pr_watch.ci_wait.parse_github_time("2026-09-21T10:01:00Z")
    gh_pr_watch.ci_wait.update_active_checks(state, [queued_sample], started_at - 300, "ci-v1")
    gh_pr_watch.ci_wait.update_active_checks(state, [running_sample], started_at, "ci-v2")
    gh_pr_watch.ci_wait.update_active_checks(state, [completed], completed_at, "ci-v2")
    gh_pr_watch.ci_wait.record_timing_samples(state, sample_pr(), [completed], "ci-v2")
    gh_pr_watch.save_state(state_path, state)
    persisted, _ = gh_pr_watch.load_state(state_path)
    assert persisted["timing_samples"][0]["duration_seconds"] == 60
    assert persisted["timing_samples"][0]["ci_revision"] == "ci-v1"

    late_state = {}
    late_running = dict(running_sample)
    late_completed = gh_pr_watch.ci_wait.normalize_check(completed_payload)
    late_observed_at = started_at + gh_pr_watch.ci_wait.DEFAULT_TIMEOUT + 1
    gh_pr_watch.ci_wait.update_active_checks(late_state, [late_running], late_observed_at, "ci-v1")
    assert late_running["active_since"] == started_at
    late_timeout = gh_pr_watch.ci_wait.execution_timeouts(
        sample_snapshot(
            sample_checks(pending_count=1, passed_count=0, all_terminal=False),
            [late_running],
        ),
        late_observed_at,
        state_path,
        late_state,
    )[0]
    assert (late_timeout["active_seconds"], late_timeout["history_source"]) == (
        gh_pr_watch.ci_wait.DEFAULT_TIMEOUT + 1,
        "fallback",
    )
    gh_pr_watch.ci_wait.update_active_checks(late_state, [late_completed], completed_at, "ci-v1")
    gh_pr_watch.ci_wait.record_timing_samples(late_state, sample_pr(), [late_completed], "ci-v1")
    assert late_state["timing_samples"] == []

    queued = gh_pr_watch.ci_wait.normalize_check({"workflow": "CI", "name": "tests", "bucket": "pending", "state": "QUEUED"})
    running = dict(queued, status="running", state="IN_PROGRESS")
    running_observed_at = gh_pr_watch.ci_wait.parse_github_time("2026-09-21T09:05:00Z")
    now = gh_pr_watch.ci_wait.parse_github_time("2026-09-21T09:15:01Z")
    pending = sample_checks(pending_count=1, passed_count=0, all_terminal=False)
    gh_pr_watch.ci_wait.update_active_checks(persisted, [queued], running_observed_at - 300, "ci-v1")
    assert gh_pr_watch.ci_wait.execution_timeouts(sample_snapshot(pending, [queued]), now, state_path, persisted) == []
    gh_pr_watch.ci_wait.update_active_checks(persisted, [running], running_observed_at, "ci-v1")
    snapshot = sample_snapshot(pending, [running])
    timeout = gh_pr_watch.ci_wait.execution_timeouts(snapshot, now, state_path, persisted)[0]
    assert (timeout["active_seconds"], timeout["limit_seconds"], timeout["history_source"]) == (601, 600, "history")
    assert gh_pr_watch.ci_wait.execution_timeouts(snapshot, now, state_path, persisted) == []


def test_ci_revision_changes_only_when_base_ci_files_change():
    state, calls = {}, []
    workflow_sha = {"base-a": "workflow-1", "base-b": "workflow-1", "base-c": "workflow-2", "base-d": "workflow-1"}

    def fake_gh_json(args, repo=None):
        endpoint = args[1]
        calls.append(endpoint)
        base_sha = endpoint.rsplit("/", maxsplit=1)[-1]
        bazel_sha = "bazel-2" if base_sha == "base-d" else "bazel-1"
        return {"tree": [
            {"path": ".github/workflows/ci.yml", "type": "blob", "sha": workflow_sha[base_sha]},
            {"path": ".bazelversion", "type": "blob", "sha": bazel_sha},
        ]}

    revision_a = gh_pr_watch.ci_wait.get_ci_config_revision(fake_gh_json, state, "openai/codex", "base-a")
    call_count = len(calls)
    assert gh_pr_watch.ci_wait.get_ci_config_revision(fake_gh_json, state, "openai/codex", "base-a") == revision_a
    assert len(calls) == call_count
    assert gh_pr_watch.ci_wait.get_ci_config_revision(fake_gh_json, state, "openai/codex", "base-b") == revision_a
    assert gh_pr_watch.ci_wait.get_ci_config_revision(fake_gh_json, state, "openai/codex", "base-c") != revision_a
    assert gh_pr_watch.ci_wait.get_ci_config_revision(fake_gh_json, state, "openai/codex", "base-d") != revision_a


def test_rerun_attempt_waits_for_new_check_rollup():
    checks = [{"id": "tests", "run_id": 7, "job_id": 8}]
    legacy = {"retries_by_sha": {"abc123": 3}}
    gh_pr_watch.migrate_retry_count(legacy, "abc123", "generation")
    assert legacy["retries_by_sha"] == {"generation": 3}
    registration = {}
    assert gh_pr_watch.ci_wait.update_check_registration(registration, "old", checks)
    assert not gh_pr_watch.ci_wait.update_check_registration(registration, "new", checks, require_change=True)
    assert gh_pr_watch.ci_wait.update_check_registration(registration, "new", checks)
    state = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"7": 1},
            "check_signature": gh_pr_watch.ci_wait.check_signature(checks),
        }
    }
    assert gh_pr_watch.ci_wait.rerun_is_pending(state, "g", [{"id": 7, "run_attempt": 1}], checks)
    new_checks = [{"id": "tests", "run_id": 7, "job_id": 9}]
    assert gh_pr_watch.ci_wait.update_check_registration(registration, "new", new_checks)
    assert gh_pr_watch.ci_wait.rerun_is_pending(state, "g", [{"id": 7, "run_attempt": 1}], new_checks)
    assert not gh_pr_watch.ci_wait.rerun_is_pending(state, "g", [{"id": 7, "run_attempt": 2}], new_checks)
    unlinked = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"9": 1},
            "check_run_ids": [],
            "check_signature": gh_pr_watch.ci_wait.check_signature(checks),
        }
    }
    assert gh_pr_watch.ci_wait.rerun_is_pending(
        unlinked,
        "g",
        [{"id": 9, "run_attempt": 2, "status": "in_progress"}],
        checks,
    )
    assert not gh_pr_watch.ci_wait.rerun_is_pending(
        unlinked,
        "g",
        [{"id": 9, "run_attempt": 2, "status": "completed"}],
        checks,
    )
    external_a = gh_pr_watch.ci_wait.normalize_check(
        {
            "workflow": "external",
            "name": "build",
            "link": "https://ci/build/1",
            "startedAt": "2026-09-21T10:00:00Z",
        }
    )
    external_b = dict(external_a, started_at="2026-09-21T10:01:00Z")
    assert gh_pr_watch.ci_wait.check_signature([external_a]) == gh_pr_watch.ci_wait.check_signature([external_b])
    assert gh_pr_watch.ci_wait.check_observation_signature(
        [external_a]
    ) != gh_pr_watch.ci_wait.check_observation_signature([external_b])
    stable = dict(external_a, started_at="")
    stable_signature = gh_pr_watch.ci_wait.check_signature([stable])
    stable_observation = gh_pr_watch.ci_wait.check_observation_signature([stable])
    refresh_state = {"check_registration": {"signature": stable_signature}}
    assert gh_pr_watch.ci_wait.update_head_refresh(
        refresh_state, True, stable_signature, stable_observation, True, 1_000
    ) == "pending"
    assert gh_pr_watch.ci_wait.update_head_refresh(
        refresh_state,
        False,
        stable_signature,
        stable_observation,
        True,
        1_000 + gh_pr_watch.ci_wait.REGISTRATION_GRACE_SECONDS,
    ) == "timed_out"
    fresh_state = {
        "check_registration": {
            "signature": gh_pr_watch.ci_wait.check_signature([external_a]),
            "observation_signature": gh_pr_watch.ci_wait.check_observation_signature(
                [external_a]
            ),
        }
    }
    assert gh_pr_watch.ci_wait.update_head_refresh(
        fresh_state,
        True,
        gh_pr_watch.ci_wait.check_signature([external_b]),
        gh_pr_watch.ci_wait.check_observation_signature([external_b]),
        True,
        1_000,
    ) is None


def test_generation_refresh_timeout_and_active_check_reset():
    check = gh_pr_watch.ci_wait.normalize_check(
        {
            "workflow": "external",
            "name": "build",
            "bucket": "pending",
            "state": "IN_PROGRESS",
            "link": "https://ci/build/stable",
            "startedAt": "1970-01-01T00:16:40Z",
        }
    )
    signature = gh_pr_watch.ci_wait.check_signature([check])
    observation = gh_pr_watch.ci_wait.check_observation_signature([check])
    legacy_state = {}
    assert gh_pr_watch.ci_wait.update_head_refresh(
        legacy_state, True, signature, observation, True, 1_000
    ) == "pending"
    assert gh_pr_watch.ci_wait.update_head_refresh(
        legacy_state,
        False,
        signature,
        observation,
        True,
        1_000 + gh_pr_watch.ci_wait.REGISTRATION_GRACE_SECONDS,
    ) == "timed_out"

    pushed_state = {
        "head_refresh_signature": signature,
        "check_registration": {
            "signature": signature,
            "current": False,
        },
    }
    assert gh_pr_watch.ci_wait.update_head_refresh(
        pushed_state, False, signature, observation, True, 3_000
    ) == "pending"
    pushed_fresh_state = {
        "head_refresh_signature": signature,
        "check_registration": {"signature": signature, "current": False},
    }
    assert gh_pr_watch.ci_wait.update_head_refresh(
        pushed_fresh_state,
        False,
        signature,
        observation,
        True,
        3_000,
        observations_verified_fresh=True,
        check_observations=[gh_pr_watch.ci_wait.check_observation_id(check)],
    ) == "pending"
    mismatched_marker = {
        "head_refresh_signature": "older-signature",
        "check_registration": {"signature": signature, "current": False},
    }
    assert gh_pr_watch.ci_wait.update_head_refresh(
        mismatched_marker,
        False,
        signature,
        observation,
        True,
        3_000,
        check_observations=[gh_pr_watch.ci_wait.check_observation_id(check)],
    ) == "pending"
    assert gh_pr_watch.ci_wait.update_head_refresh(
        {},
        True,
        signature,
        observation,
        True,
        3_000,
        observations_verified_fresh=True,
    ) is None
    assert gh_pr_watch.ci_wait.update_head_refresh(
        legacy_state,
        False,
        signature,
        observation,
        True,
        2_000,
    ) == "timed_out"

    active_state = {}
    gh_pr_watch.ci_wait.update_active_checks(
        active_state, [check], 1_000, "ci-v1", "head-a\x1fbase-a"
    )
    refreshed = dict(check, started_at="1970-01-01T00:33:20Z")
    gh_pr_watch.ci_wait.update_active_checks(
        active_state, [refreshed], 2_000, "ci-v1", "head-b\x1fbase-a"
    )
    assert refreshed["active_since"] == 2_000


def test_rerun_registration_requires_each_run_and_times_out():
    old_checks = [
        {"id": "one", "run_id": 1, "job_id": 10, "started_at": "one"},
        {"id": "two", "run_id": 2, "job_id": 20, "started_at": "two"},
    ]
    state = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"1": 1, "2": 1},
            "check_observations": {
                "1": gh_pr_watch.ci_wait.check_observation_signature([old_checks[0]]),
                "2": gh_pr_watch.ci_wait.check_observation_signature([old_checks[1]]),
            },
            "started_at": 1_000,
        }
    }
    attempts = [
        {"id": 1, "run_attempt": 2, "status": "in_progress"},
        {"id": 2, "run_attempt": 2, "status": "in_progress"},
    ]
    one_refreshed = [dict(old_checks[0], job_id=11), old_checks[1]]
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        state, "g", attempts, one_refreshed, 1_001
    ) == "pending"
    both_refreshed = [dict(old_checks[0], job_id=11), dict(old_checks[1], job_id=21)]
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        state, "g", attempts, both_refreshed, 1_002
    ) is None

    two_jobs = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"1": 1},
            "check_observations": {
                "1": [
                    gh_pr_watch.ci_wait.check_observation_id(old_checks[0]),
                    gh_pr_watch.ci_wait.check_observation_id(
                        {"id": "one-b", "run_id": 1, "job_id": 12, "started_at": "old"}
                    ),
                ]
            },
            "started_at": 1_000,
        }
    }
    partly_refreshed = [
        dict(old_checks[0], job_id=11),
        {"id": "one-b", "run_id": 1, "job_id": 12, "started_at": "old"},
    ]
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        two_jobs, "g", attempts[:1], partly_refreshed, 1_001
    ) == "pending"
    fully_refreshed = [dict(partly_refreshed[0]), dict(partly_refreshed[1], job_id=13)]
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        two_jobs,
        "g",
        attempts[:1],
        fully_refreshed,
        1_000 + gh_pr_watch.ci_wait.RERUN_REGISTRATION_TIMEOUT_SECONDS,
    ) is None

    unlinked_running = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"9": 1},
            "check_observations": {},
            "started_at": 1_000,
        }
    }
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        unlinked_running,
        "g",
        [{"id": 9, "run_attempt": 2, "status": "in_progress"}],
        [],
        1_000 + gh_pr_watch.ci_wait.RERUN_REGISTRATION_TIMEOUT_SECONDS,
    ) == "pending"
    queued = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"9": 1},
            "check_observations": {},
            "started_at": 1_000,
        }
    }
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        queued,
        "g",
        [{"id": 9, "run_attempt": 2, "status": "queued"}],
        [],
        1_000 + gh_pr_watch.ci_wait.MAX_TIMEOUT + 1,
    ) == "pending"
    assert "execution_started_at" not in queued["pending_rerun"]

    timed_out = {
        "pending_rerun": {
            "generation": "g",
            "attempts": {"1": 1},
            "check_observations": {},
            "started_at": 1_000,
        }
    }
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        timed_out,
        "g",
        [{"id": 1, "run_attempt": 1, "status": "completed"}],
        [],
        1_000 + gh_pr_watch.ci_wait.RERUN_REGISTRATION_TIMEOUT_SECONDS,
    ) == "timed_out"
    assert gh_pr_watch.ci_wait.rerun_registration_status(
        timed_out, "g", [], [], 3_000
    ) == "timed_out"


def test_partial_rerun_is_persisted(monkeypatch, tmp_path):
    check_details = [sample_check_detail(index, status="failed") for index in range(12)]
    check_details[0]["run_id"] = 1
    check_details[1]["run_id"] = 1
    check_details[1]["status"] = "passed"
    snapshot = sample_snapshot(
        checks=sample_checks(failed_count=2),
        check_details=check_details,
        failed_runs=[
            {"run_id": 1, "run_attempt": 1, "conclusion": "failure"},
            {"run_id": 2, "run_attempt": 1, "conclusion": "failure"},
        ],
        retry_state={"current_sha_retries_used": 0, "max_flaky_retries": 3},
    )
    snapshot["ci"].update({"rerun_pending": False, "check_set_current": True})
    snapshot["ci"]["registration_stable"] = True
    state, saved = {}, []
    monkeypatch.setattr(
        gh_pr_watch,
        "_collect_snapshot",
        lambda args, pr, path: (snapshot, tmp_path / "pr-123.json"),
    )
    monkeypatch.setattr(gh_pr_watch, "load_state", lambda path: (state, False))
    monkeypatch.setattr(gh_pr_watch, "resolve_pr", lambda *args, **kwargs: sample_pr())
    monkeypatch.setattr(gh_pr_watch, "save_state", lambda path, value: saved.append(dict(value)))
    monkeypatch.setattr(
        gh_pr_watch,
        "get_pr_checks",
        lambda *args, **kwargs: check_details,
    )
    monkeypatch.setattr(gh_pr_watch.ci_wait, "normalize_check", lambda check: check)
    monkeypatch.setattr(
        gh_pr_watch,
        "get_workflow_runs_for_sha",
        lambda *args, **kwargs: [
            {"id": 1, "run_attempt": 1, "status": "completed", "conclusion": "failure"},
            {"id": 2, "run_attempt": 1, "status": "completed", "conclusion": "failure"},
        ],
    )

    def rerun(args, repo=None):
        if args[2] == "2":
            raise gh_pr_watch.GhCommandError("second rerun failed")

    monkeypatch.setattr(gh_pr_watch, "gh_text", rerun)
    with pytest.raises(gh_pr_watch.GhCommandError):
        gh_pr_watch.retry_failed_now(
            argparse.Namespace(pr="123", repo=None, state_file=str(tmp_path / "pr-123.json"))
        )
    assert saved[-1]["pending_rerun"]["attempts"] == {"1": 1}
    assert saved[-1]["pending_rerun"]["check_observations"] == {
        "1": [gh_pr_watch.ci_wait.check_observation_id(check_details[0])]
    }


def test_retry_revalidates_run_attempt_before_mutation(monkeypatch, tmp_path):
    check = sample_check_detail(1, status="failed")
    check["run_id"] = 1
    snapshot = sample_snapshot(
        checks=sample_checks(failed_count=1),
        check_details=[check],
        failed_runs=[{"run_id": 1, "run_attempt": 1, "conclusion": "failure"}],
        retry_state={"current_sha_retries_used": 0, "max_flaky_retries": 3},
    )
    snapshot["ci"].update(
        {
            "rerun_pending": False,
            "rerun_timed_out": False,
            "check_set_current": True,
            "registration_stable": True,
        }
    )
    monkeypatch.setattr(gh_pr_watch, "resolve_pr", lambda *args, **kwargs: sample_pr())
    monkeypatch.setattr(
        gh_pr_watch, "_collect_snapshot", lambda args, pr, path: (snapshot, path)
    )
    monkeypatch.setattr(gh_pr_watch, "load_state", lambda path: ({}, False))
    monkeypatch.setattr(gh_pr_watch, "get_pr_checks", lambda *args, **kwargs: [check])
    monkeypatch.setattr(gh_pr_watch.ci_wait, "normalize_check", lambda value: value)
    monkeypatch.setattr(
        gh_pr_watch,
        "get_workflow_runs_for_sha",
        lambda *args, **kwargs: [
            {"id": 1, "run_attempt": 2, "status": "completed", "conclusion": "failure"}
        ],
    )
    mutations = []
    monkeypatch.setattr(gh_pr_watch, "gh_text", lambda *args, **kwargs: mutations.append(args))

    result = gh_pr_watch.retry_failed_now(
        argparse.Namespace(pr="123", repo=None, state_file=str(tmp_path / "pr-123.json"))
    )

    assert result["reason"] == "failed_runs_changed"
    assert mutations == []
