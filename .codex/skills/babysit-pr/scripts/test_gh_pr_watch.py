import argparse
import importlib.util
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
    assert state["seen_review_comment_ids"] == ["20"]
    assert state["seen_review_ids"] == ["10"]


def test_run_watch_keeps_polling_open_ready_to_merge_pr(monkeypatch):
    sleeps = []
    events = []
    snapshot = {
        "pr": sample_pr(),
        "checks": sample_checks(),
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


def test_wait_for_first_failure_polls_silently(monkeypatch, tmp_path):
    pending_checks = sample_checks(pending_count=1, passed_count=0, all_terminal=False, total_count=1)
    pending = sample_snapshot(pending_checks)
    failed = sample_snapshot(
        sample_checks(pending_count=1, failed_count=1, passed_count=0, all_terminal=False, total_count=2),
        failed_jobs=[{"job_id": 44, "job_name": "tests"}],
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
    generation = ("abc123", "base123", "ci-v1")
    finished = gh_pr_watch.ci_wait.wait_reason("finished", sample_snapshot(), state_path, {}, generation, 1_000)
    assert finished == ("finished", [])
    pending = sample_snapshot(sample_checks(pending_count=1, all_terminal=False))
    assert gh_pr_watch.ci_wait.wait_reason("finished", pending, state_path, {}, generation, 1_000) == (None, [])
    empty = sample_snapshot(sample_checks(passed_count=0, total_count=0))
    assert gh_pr_watch.ci_wait.wait_reason("finished", empty, state_path, {}, generation, 1_000) == (None, [])
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
    gh_pr_watch.ci_wait.update_active_checks(late_state, [late_running], started_at, "ci-v1")
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
    assert not gh_pr_watch.ci_wait.update_check_registration(registration, "new", checks)
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
    external_a = gh_pr_watch.ci_wait.normalize_check({"workflow": "external", "name": "build", "link": "https://ci/build/1"})
    external_b = dict(external_a, url="https://ci/build/2")
    assert gh_pr_watch.ci_wait.check_signature([external_a]) != gh_pr_watch.ci_wait.check_signature([external_b])


def test_partial_rerun_is_persisted(monkeypatch, tmp_path):
    snapshot = sample_snapshot(
        checks=sample_checks(failed_count=2),
        failed_runs=[
            {"run_id": 1, "run_attempt": 1, "conclusion": "failure"},
            {"run_id": 2, "run_attempt": 1, "conclusion": "failure"},
        ],
        retry_state={"current_sha_retries_used": 0, "max_flaky_retries": 3},
    )
    snapshot["ci"].update({"rerun_pending": False, "check_set_current": True})
    state, saved = {}, []
    monkeypatch.setattr(gh_pr_watch, "collect_snapshot", lambda args: (snapshot, tmp_path / "pr-123.json"))
    monkeypatch.setattr(gh_pr_watch, "load_state", lambda path: (state, False))
    monkeypatch.setattr(gh_pr_watch, "resolve_pr", lambda *args, **kwargs: sample_pr())
    monkeypatch.setattr(gh_pr_watch, "save_state", lambda path, value: saved.append(dict(value)))

    def rerun(args, repo=None):
        if args[2] == "2":
            raise gh_pr_watch.GhCommandError("second rerun failed")

    monkeypatch.setattr(gh_pr_watch, "gh_text", rerun)
    with pytest.raises(gh_pr_watch.GhCommandError):
        gh_pr_watch.retry_failed_now(argparse.Namespace(pr="123", repo=None))
    assert saved[-1]["pending_rerun"]["attempts"] == {"1": 1}
