import hashlib
import json
import math
import os
import re
import time
from contextlib import contextmanager
from datetime import datetime

CI_CONFIG_PREFIXES = (".github/workflows/", ".github/actions/", ".github/scripts/", "scripts/")
CI_CONFIG_FILES = {".bazelrc", ".bazelversion", "justfile", "MODULE.bazel", "MODULE.bazel.lock", "codex-rs/rust-toolchain.toml"}
PENDING_STATES = {"QUEUED", "IN_PROGRESS", "PENDING", "WAITING", "REQUESTED"}
MAX_CI_REVISIONS, MAX_HISTORY_FILES, MAX_SAMPLES = 20, 20, 500
MIN_TIMEOUT, DEFAULT_TIMEOUT, MAX_TIMEOUT = 10 * 60, 60 * 60, 4 * 60 * 60
REGISTRATION_GRACE_SECONDS = 60
RERUN_REGISTRATION_TIMEOUT_SECONDS = 10 * 60
MAX_OUTPUT_CHECK_DETAILS = 10
MAX_OUTPUT_CHECK_DETAILS_JSON_CHARS = 750
MAX_OUTPUT_COLLECTION_ITEMS = 5
MAX_OUTPUT_COLLECTION_JSON_CHARS = 450
MAX_OUTPUT_CHECK_DETAIL_VALUE_CHARS = 120
MAX_OUTPUT_PAYLOAD_JSON_CHARS = 3_600
OUTPUT_CHECK_DETAIL_FIELDS = (
    "id",
    "workflow",
    "name",
    "status",
    "state",
    "started_at",
    "completed_at",
    "url",
    "run_id",
    "job_id",
    "active_since",
)
OUTPUT_FAILED_RUN_FIELDS = (
    "run_id",
    "run_attempt",
    "workflow_name",
    "status",
    "conclusion",
    "html_url",
)
OUTPUT_FAILED_JOB_FIELDS = (
    "run_id",
    "workflow_name",
    "run_status",
    "run_conclusion",
    "job_id",
    "job_name",
    "status",
    "conclusion",
    "html_url",
    "logs_endpoint",
)
OUTPUT_REVIEW_ITEM_FIELDS = (
    "kind",
    "id",
    "author",
    "author_association",
    "created_at",
    "body",
    "path",
    "line",
    "url",
)
OUTPUT_TIMEOUT_FIELDS = (
    "workflow",
    "name",
    "active_seconds",
    "limit_seconds",
    "sample_count",
    "history_source",
    "url",
)

def effective_ci_revision(base_revision, head_revision):
    return hashlib.sha256(f"{base_revision}\0{head_revision}".encode()).hexdigest()

@contextmanager
def state_lock(path):
    lock_path = path.with_name(f"{path.name}.lock")
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+b") as lock_file:
        if os.name == "nt":
            import msvcrt
            if lock_file.tell() == 0: lock_file.write(b"\0"); lock_file.flush()
            lock_file.seek(0); msvcrt.locking(lock_file.fileno(), msvcrt.LK_LOCK, 1)
        else:
            import fcntl
            fcntl.flock(lock_file, fcntl.LOCK_EX)
        try:
            yield
        finally:
            if os.name == "nt": lock_file.seek(0); msvcrt.locking(lock_file.fileno(), msvcrt.LK_UNLCK, 1)
            else: fcntl.flock(lock_file, fcntl.LOCK_UN)

def get_ci_config_revision(gh_json, state, repo, sha):
    if not sha: return "unknown"
    revisions = state.get("ci_revisions_by_sha")
    revisions = revisions if isinstance(revisions, dict) else {}
    if sha in revisions: return str(revisions[sha])
    tree = gh_json(["api", f"repos/{repo}/git/trees/{sha}", "-X", "GET", "-f", "recursive=1"], repo=repo)
    if not isinstance(tree, dict) or not isinstance(tree.get("tree"), list):
        raise RuntimeError("Git tree payload did not contain a tree")
    if tree.get("truncated"): raise RuntimeError("GitHub truncated the CI fingerprint tree")
    entries = []
    for item in tree["tree"]:
        if not isinstance(item, dict) or item.get("type") != "blob": continue
        path = str(item.get("path") or "")
        if path in CI_CONFIG_FILES or path.startswith(CI_CONFIG_PREFIXES):
            entries.append((path, str(item.get("mode") or ""), str(item.get("sha") or "")))
    revision = hashlib.sha256(json.dumps(sorted(entries)).encode()).hexdigest()
    revisions[sha] = revision
    state["ci_revisions_by_sha"] = dict(list(revisions.items())[-MAX_CI_REVISIONS:])
    return revision

def check_identity(check): return "\x1f".join((str(check.get("workflow") or ""), str(check.get("name") or "")))
def check_status(check):
    bucket, state = str(check.get("bucket") or "").lower(), str(check.get("state") or "").upper()
    if bucket == "fail": return "failed"
    if bucket == "pass": return "passed"
    if bucket == "pending" or state in PENDING_STATES: return "running" if state == "IN_PROGRESS" else "queued"
    return "terminal"

def normalize_check(check):
    match = re.search(r"/actions/runs/(\d+)(?:/job/(\d+))?", str(check.get("link") or ""))
    return {
        "id": check_identity(check), "workflow": str(check.get("workflow") or ""),
        "name": str(check.get("name") or ""), "status": check_status(check),
        "state": str(check.get("state") or ""), "started_at": str(check.get("startedAt") or ""),
        "completed_at": str(check.get("completedAt") or ""), "url": str(check.get("link") or ""),
        "run_id": int(match.group(1)) if match else None,
        "job_id": int(match.group(2)) if match and match.group(2) else None,
    }

def bounded_records(records, fields):
    all_records = list(records or [])
    details = []
    values_truncated = False
    for record in all_records:
        if len(details) >= MAX_OUTPUT_COLLECTION_ITEMS:
            break
        if not isinstance(record, dict):
            continue
        detail = {}
        detail_values_truncated = False
        for field in fields:
            if field not in record:
                continue
            value = record[field]
            if isinstance(value, str) and len(value) > MAX_OUTPUT_CHECK_DETAIL_VALUE_CHARS:
                value = value[: MAX_OUTPUT_CHECK_DETAIL_VALUE_CHARS - 1] + "…"
                detail_values_truncated = True
            detail[field] = value
        candidate = details + [detail]
        if len(json.dumps(candidate, sort_keys=True)) > MAX_OUTPUT_COLLECTION_JSON_CHARS:
            if not details:
                for string_limit in (80, 40, 20, 8):
                    compact = {
                        key: (
                            value[: string_limit - 1] + "…"
                            if isinstance(value, str) and len(value) > string_limit
                            else value
                        )
                        for key, value in detail.items()
                    }
                    if len(json.dumps([compact], sort_keys=True)) <= MAX_OUTPUT_COLLECTION_JSON_CHARS:
                        details.append(compact)
                        values_truncated = True
                        break
            break
        details.append(detail)
        values_truncated = values_truncated or detail_values_truncated
    total_count = len(all_records)
    emitted_count = len(details)
    return details, {
        "total_count": total_count,
        "emitted_count": emitted_count,
        "omitted_count": total_count - emitted_count,
        "truncated": values_truncated or emitted_count < total_count,
    }

def bounded_output_payload(payload, collection_names):
    output = json.loads(json.dumps(payload))
    minimum_items = {"new_review_items": 1, "failures": 1, "failed_runs": 1}

    def collection_parent(path):
        parts = path.split(".")
        parent = output
        for part in parts[:-1]:
            parent = parent.get(part) if isinstance(parent, dict) else None
            if not isinstance(parent, dict):
                return None, parts[-1]
        return parent, parts[-1]

    def update_summary(path):
        parent, name = collection_parent(path)
        if parent is None:
            return
        summary = parent.get(f"{name}_summary")
        if not isinstance(summary, dict):
            return
        emitted_count = len(parent.get(name) or [])
        total_count = int(summary.get("total_count") or 0)
        summary.update(
            emitted_count=emitted_count,
            omitted_count=max(0, total_count - emitted_count),
            truncated=bool(summary.get("truncated")) or emitted_count < total_count,
        )

    while len(json.dumps(output, sort_keys=True)) > MAX_OUTPUT_PAYLOAD_JSON_CHARS:
        removed = False
        for path in collection_names:
            parent, name = collection_parent(path)
            items = parent.get(name) if parent is not None else None
            minimum = minimum_items.get(name, 0)
            if isinstance(items, list) and len(items) > minimum:
                items.pop()
                update_summary(path)
                removed = True
                break
        if not removed:
            break

    def truncate_strings(value, limit):
        if isinstance(value, str):
            return value[: limit - 1] + "…" if len(value) > limit else value
        if isinstance(value, list):
            return [truncate_strings(item, limit) for item in value]
        if isinstance(value, dict):
            return {key: truncate_strings(item, limit) for key, item in value.items()}
        return value

    for limit in (80, 40):
        if len(json.dumps(output, sort_keys=True)) <= MAX_OUTPUT_PAYLOAD_JSON_CHARS:
            break
        output = truncate_strings(output, limit)
    if len(json.dumps(output, sort_keys=True)) > MAX_OUTPUT_PAYLOAD_JSON_CHARS:
        raise RuntimeError("bounded watcher output exceeded its hard JSON size limit")
    return output

def bounded_check_details(checks):
    all_checks = list(checks or [])
    priorities = {"failed": 0, "running": 1, "queued": 2, "terminal": 3, "passed": 4}
    ordered = sorted(
        ((index, check) for index, check in enumerate(all_checks) if isinstance(check, dict)),
        key=lambda item: (priorities.get(str(item[1].get("status") or ""), 3), item[0]),
    )
    details = []
    values_truncated = False
    for _, check in ordered:
        if len(details) >= MAX_OUTPUT_CHECK_DETAILS:
            break
        detail = {}
        detail_values_truncated = False
        for field in OUTPUT_CHECK_DETAIL_FIELDS:
            if field not in check:
                continue
            value = check[field]
            if isinstance(value, str) and len(value) > MAX_OUTPUT_CHECK_DETAIL_VALUE_CHARS:
                value = value[: MAX_OUTPUT_CHECK_DETAIL_VALUE_CHARS - 1] + "…"
                detail_values_truncated = True
            detail[field] = value
        candidate = details + [detail]
        if len(json.dumps(candidate, sort_keys=True)) > MAX_OUTPUT_CHECK_DETAILS_JSON_CHARS:
            break
        details.append(detail)
        values_truncated = values_truncated or detail_values_truncated
    total_count = len(all_checks)
    emitted_count = len(details)
    return details, {
        "total_count": total_count,
        "emitted_count": emitted_count,
        "omitted_count": total_count - emitted_count,
        "truncated": values_truncated or emitted_count < total_count,
    }

def check_run_key(check):
    fallback = check.get("url") if check.get("run_id") is None and check.get("job_id") is None else ""
    return "\x1f".join(str(check.get(field) or "") for field in ("id", "run_id", "job_id")) + f"\x1f{fallback or ''}"
def check_observation_id(check): return f"{check_run_key(check)}\x1f{check.get('started_at') or ''}"
def check_signature(checks): return hashlib.sha256(json.dumps(sorted(check_run_key(check) for check in checks)).encode()).hexdigest()
def check_observation_signature(checks): return hashlib.sha256(json.dumps(sorted(check_observation_id(check) for check in checks)).encode()).hexdigest()

def update_head_refresh(
    state,
    generation_changed,
    signature,
    observation_signature,
    has_checks,
    now,
    observations_verified_fresh=False,
    check_observations=None,
    force_refresh_baseline=False,
):
    current_observations = {str(value) for value in check_observations or []}
    legacy_refresh_signature = state.pop("head_refresh_signature", None)
    if legacy_refresh_signature is not None:
        generation_changed = True
        observations_verified_fresh = False
        force_refresh_baseline = True
    timeout = state.get("check_refresh_timeout")
    if generation_changed:
        state.pop("check_refresh_timeout", None)
        timeout = None
    if isinstance(timeout, dict) and not generation_changed:
        baseline = {str(value) for value in timeout.get("observations") or []}
        if baseline:
            if current_observations and current_observations.isdisjoint(baseline):
                state.pop("check_refresh_timeout", None)
                return None
            return "timed_out"
        if timeout.get("signature") == signature and timeout.get(
            "observation_signature"
        ) == observation_signature:
            return "timed_out"
        state.pop("check_refresh_timeout", None)
    if generation_changed:
        registration = state.get("check_registration")
        previous = registration.get("signature") if isinstance(registration, dict) else None
        previous_observation = (
            registration.get("observation_signature")
            if isinstance(registration, dict)
            else None
        )
        previous_observations = (
            {str(value) for value in registration.get("observations") or []}
            if isinstance(registration, dict)
            else set()
        )
        stale_observations = (
            previous_observations
            if previous_observations
            and not current_observations.isdisjoint(previous_observations)
            else set()
        )
        needs_refresh = not has_checks or (
            not observations_verified_fresh
            and (
                force_refresh_baseline
                or previous is None
                or (
                    previous == signature
                    and previous_observation in {None, observation_signature}
                )
                or bool(stale_observations)
            )
        )
        if needs_refresh:
            state["check_refresh"] = {
                "signature": signature,
                "observation_signature": observation_signature,
                "observations": sorted(
                    stale_observations or current_observations
                ),
                "started_at": int(now),
            }
        else:
            state.pop("check_refresh", None)
            state.pop("check_refresh_timeout", None)
    refresh = state.get("check_refresh")
    if not isinstance(refresh, dict):
        return None
    baseline = {str(value) for value in refresh.get("observations") or []}
    if baseline:
        if current_observations and current_observations.isdisjoint(baseline):
            state.pop("check_refresh", None)
            state.pop("check_refresh_timeout", None)
            return None
    elif (
        refresh.get("signature") != signature
        or refresh.get("observation_signature") != observation_signature
    ):
        state.pop("check_refresh", None)
        state.pop("check_refresh_timeout", None)
        return None
    started_at = refresh.get("started_at")
    if not isinstance(started_at, (int, float)) or now - started_at >= REGISTRATION_GRACE_SECONDS:
        state.pop("check_refresh", None)
        state["check_refresh_timeout"] = {
            "signature": signature,
            "observation_signature": observation_signature,
            "observations": sorted(baseline),
            "started_at": started_at,
        }
        return "timed_out"
    return "pending"

def parse_github_time(value):
    if not value: return None
    try: return datetime.fromisoformat(str(value).replace("Z", "+00:00")).timestamp()
    except (TypeError, ValueError): return None

def update_active_checks(state, checks, now, ci_revision=None, generation=None):
    previous = state.get("active_checks"); previous = previous if isinstance(previous, dict) else {}
    statuses_before = state.get("check_statuses"); statuses_before = statuses_before if isinstance(statuses_before, dict) else {}
    active, statuses = {}, {}
    for check in checks:
        key, status = check_run_key(check), check.get("status")
        observation_id = check_observation_id(check)
        if status in {"queued", "running"}: statuses[key] = {"status": status, "ci_revision": ci_revision, "generation": generation, "observation_id": observation_id}
        if status == "running":
            prior = statuses_before.get(key)
            queued_before = (
                isinstance(prior, dict)
                and prior.get("status") == "queued"
                and prior.get("generation") == generation
            )
            started_at = parse_github_time(check.get("started_at"))
            old_entry = previous.get(key)
            entry = (
                old_entry
                if isinstance(old_entry, dict)
                and old_entry.get("generation") == generation
                and old_entry.get("observation_id") == observation_id
                else {
                    "since": int(started_at if started_at is not None else now),
                    "trainable": queued_before,
                    "ci_revision": prior.get("ci_revision") if queued_before else ci_revision,
                    "generation": generation,
                    "observation_id": observation_id,
                }
            )
            check["active_since"], active[key] = entry["since"], entry
        elif key in previous:
            entry = previous[key]
            if entry.get("generation") == generation and entry.get("observation_id") == observation_id:
                completed_at = parse_github_time(check.get("completed_at")) or now
                check.update(observed_active_seconds=max(0, int(completed_at - entry["since"])), active_sample_trainable=entry["trainable"], active_ci_revision=entry.get("ci_revision"))
    state["active_checks"], state["check_statuses"] = active, statuses

def update_check_registration(state, generation, checks, require_change=False):
    signature = check_signature(checks)
    observation_signature = check_observation_signature(checks)
    registration = state.get("check_registration"); registration = registration if isinstance(registration, dict) else {}
    same = registration.get("signature") == signature
    same_observation = registration.get("observation_signature") == observation_signature
    required_change_missing = require_change and same and same_observation
    if required_change_missing and registration.get("generation") == generation:
        registration["current"] = False
    elif registration.get("generation") != generation or not same or not same_observation:
        registration = {
            "generation": generation,
            "signature": signature,
            "observation_signature": observation_signature,
            "observations": sorted(check_observation_id(check) for check in checks),
            "current": not required_change_missing,
        }
    elif not require_change and not registration.get("current", True):
        registration["current"] = True
    state["check_registration"] = registration
    return bool(registration.get("current", True))

def rerun_registration_status(state, generation, runs, checks, now):
    pending = state.get("pending_rerun")
    timeout = state.get("rerun_timeout")
    if not isinstance(pending, dict):
        if isinstance(timeout, dict) and timeout.get("generation") == generation:
            return "timed_out"
        state.pop("rerun_timeout", None)
        return None
    if pending.get("generation") != generation:
        state.pop("pending_rerun", None)
        state.pop("rerun_timeout", None)
        return None
    started_at = pending.get("started_at")
    if not isinstance(started_at, (int, float)):
        started_at = int(now)
        pending["started_at"] = started_at
    runs_by_id = {str(run.get("id")): run for run in runs if isinstance(run, dict)}
    attempts = {
        run_id: int(run.get("run_attempt") or 1) for run_id, run in runs_by_id.items()
    }
    pending_attempts = pending.get("attempts") or {}
    advanced = all(
        attempts.get(run_id, 0) > attempt for run_id, attempt in pending_attempts.items()
    )
    observations = pending.get("check_observations")
    if isinstance(observations, dict):
        linked_run_ids = {str(run_id) for run_id in observations}
        checks_refreshed = True
        for run_id in linked_run_ids:
            current = [
                check for check in checks if str(check.get("run_id") or "") == run_id
            ]
            previous_observations = observations[run_id]
            if isinstance(previous_observations, list):
                current_observations = {check_observation_id(check) for check in current}
                if not current_observations or not current_observations.isdisjoint(
                    {str(value) for value in previous_observations}
                ):
                    checks_refreshed = False
                    break
            elif not current or check_observation_signature(current) == previous_observations:
                checks_refreshed = False
                break
    else:
        linked_value = pending.get("check_run_ids")
        linked_run_ids = (
            {str(run_id) for run_id in linked_value}
            if isinstance(linked_value, list)
            else set(pending_attempts)
        )
        checks_refreshed = pending.get("check_signature") != check_signature(checks)
    unlinked_run_ids = set(pending_attempts) - linked_run_ids
    unlinked_finished = all(
        str((runs_by_id.get(run_id) or {}).get("status") or "").lower() == "completed"
        for run_id in unlinked_run_ids
    )
    registration_waiting = not advanced or (bool(linked_run_ids) and not checks_refreshed)
    if not registration_waiting and unlinked_finished:
        state.pop("pending_rerun", None)
        state.pop("rerun_timeout", None)
        return None
    if registration_waiting:
        if now - started_at >= RERUN_REGISTRATION_TIMEOUT_SECONDS:
            state.pop("pending_rerun", None)
            state["rerun_timeout"] = {"generation": generation, "started_at": started_at}
            return "timed_out"
        return "pending"
    unlinked_running = any(
        str((runs_by_id.get(run_id) or {}).get("status") or "").lower()
        == "in_progress"
        for run_id in unlinked_run_ids
    )
    if unlinked_running:
        execution_started_at = pending.setdefault("execution_started_at", int(now))
        if now - execution_started_at >= MAX_TIMEOUT:
            state.pop("pending_rerun", None)
            state["rerun_timeout"] = {
                "generation": generation,
                "started_at": execution_started_at,
            }
            return "timed_out"
    else:
        pending.pop("execution_started_at", None)
    return "pending"

def rerun_is_pending(state, generation, runs, checks):
    return rerun_registration_status(state, generation, runs, checks, time.time()) == "pending"

def record_timing_samples(state, pr, checks, ci_revision):
    samples = [item for item in state.get("timing_samples") or [] if isinstance(item, dict)]
    keys = {(item.get("check_id"), item.get("completed_at")) for item in samples}
    for check in checks:
        duration, revision = check.get("observed_active_seconds"), check.get("active_ci_revision")
        if check.get("status") != "passed" or not check.get("active_sample_trainable") or not isinstance(duration, (int, float)) or duration <= 0 or not revision: continue
        sample = {"repo": pr["repo"], "base_branch": pr["base_branch"], "ci_revision": revision, "check_id": check_identity(check), "completed_at": str(check.get("completed_at") or ""), "duration_seconds": int(math.ceil(duration))}
        key = (sample["check_id"], sample["completed_at"])
        if key not in keys: samples.append(sample); keys.add(key)
    samples.sort(key=lambda item: str(item.get("completed_at") or ""), reverse=True)
    counts, bounded = {}, []
    for sample in samples:
        cohort = (sample.get("ci_revision"), sample.get("check_id"))
        if counts.get(cohort, 0) >= 20: continue
        bounded.append(sample); counts[cohort] = counts.get(cohort, 0) + 1
        if len(bounded) >= MAX_SAMPLES: break
    state["timing_samples"] = bounded

def snapshot_generation(snapshot):
    pr = snapshot["pr"]
    return (pr.get("head_sha"), pr.get("base_sha"), (snapshot.get("ci") or {}).get("revision"))

def _timing_history(state_path, state):
    history = [item for item in state.get("timing_samples") or [] if isinstance(item, dict)]
    try: siblings = sorted(state_path.parent.glob("pr-*.json"), key=lambda path: path.stat().st_mtime, reverse=True)
    except OSError: siblings = []
    for sibling in siblings[:MAX_HISTORY_FILES]:
        if sibling == state_path: continue
        try: payload = json.loads(sibling.read_text())
        except (OSError, json.JSONDecodeError): continue
        if isinstance(payload, dict): history.extend(item for item in payload.get("timing_samples") or [] if isinstance(item, dict))
    return history

def execution_timeouts(snapshot, now, state_path, state):
    running = [check for check in snapshot.get("check_details") or [] if check.get("status") == "running"]
    if not running: return []
    history, pr, revision = _timing_history(state_path, state), snapshot["pr"], snapshot["ci"]["revision"]
    reported = set(state.get("reported_timeouts") or []); generation = "\x1f".join(str(item or "") for item in snapshot_generation(snapshot)); timeouts = []
    for check in running:
        active_since = check.get("active_since")
        if not isinstance(active_since, (int, float)): continue
        durations = sorted(int(sample["duration_seconds"]) for sample in history if sample.get("repo") == pr["repo"] and sample.get("base_branch") == pr["base_branch"] and sample.get("ci_revision") == revision and sample.get("check_id") == check_identity(check))
        if durations:
            limit = durations[max(0, math.ceil(len(durations) * 0.95) - 1)] * 2 + 5 * 60
            limit, source = min(MAX_TIMEOUT, max(MIN_TIMEOUT, limit)), "history"
        else: limit, source = DEFAULT_TIMEOUT, "fallback"
        active_seconds, timeout_id = max(0, int(now - active_since)), f"{generation}\x1f{check_observation_id(check)}"
        if active_seconds <= limit or timeout_id in reported: continue
        reported.add(timeout_id)
        timeouts.append({"workflow": check.get("workflow"), "name": check.get("name"), "active_seconds": active_seconds, "limit_seconds": limit, "sample_count": len(durations), "history_source": source, "url": check.get("url")})
    state["reported_timeouts"] = list(reported)[-100:]
    return timeouts

def wait_reason(target, snapshot, state_path, state, initial_generation, now, allow_finished=True):
    pr, ci = snapshot["pr"], snapshot.get("ci") or {}
    if pr.get("closed") or pr.get("merged"): return "pr_closed", []
    if snapshot.get("new_review_items"): return "review_feedback", []
    if snapshot_generation(snapshot) != initial_generation: return "generation_changed", []
    if ci.get("rerun_timed_out"): return "rerun_registration_timeout", []
    if ci.get("check_refresh_timed_out"): return "check_registration_timeout", []
    if ci.get("rerun_pending") or ci.get("head_refresh_pending"): return None, []
    if not ci.get("check_set_current", True): return "ci_config_changed", []
    checks = snapshot["checks"]
    if allow_finished and int(checks.get("total_count") or 0) == 0: return "no_checks", []
    if target == "first-failure" and (snapshot.get("failed_runs") or snapshot.get("failed_jobs") or int(snapshot["checks"].get("failed_count") or 0) > 0): return "first_failure", []
    timeouts = execution_timeouts(snapshot, now, state_path, state)
    if timeouts: return "execution_timeout", timeouts
    if allow_finished and checks.get("all_terminal"): return "finished", []
    return None, []

def run_wait(
    args,
    collect_snapshot,
    load_state,
    save_state,
    print_json,
    acknowledge_snapshot=None,
):
    initial_generation = None
    while True:
        snapshot, state_path = collect_snapshot(args); state, _ = load_state(state_path)
        generation = snapshot_generation(snapshot); initial_generation = initial_generation or generation; now = time.time(); checks = snapshot["checks"]
        terminal_since = (snapshot.get("ci") or {}).get("terminal_since")
        terminal_since = terminal_since if isinstance(terminal_since, (int, float)) else now
        allow_finished = bool(checks.get("all_terminal") and now - terminal_since >= REGISTRATION_GRACE_SECONDS)
        reason, timeouts = wait_reason(args.wait_for, snapshot, state_path, state, initial_generation, now, allow_finished)
        if reason is not None:
            failures, failures_summary = bounded_check_details(
                [
                    check
                    for check in snapshot.get("check_details") or []
                    if check.get("status") == "failed"
                ]
            )
            failed_runs, failed_runs_summary = bounded_records(
                snapshot.get("failed_runs") or [], OUTPUT_FAILED_RUN_FIELDS
            )
            failed_jobs, failed_jobs_summary = bounded_records(
                snapshot.get("failed_jobs") or [], OUTPUT_FAILED_JOB_FIELDS
            )
            review_items, review_items_summary = bounded_records(
                snapshot.get("new_review_items") or [], OUTPUT_REVIEW_ITEM_FIELDS
            )
            timeouts, timeouts_summary = bounded_records(timeouts, OUTPUT_TIMEOUT_FIELDS)
            result = {"event": "wait_complete", "target": args.wait_for, "reason": reason, "generation": dict(zip(("head_sha", "base_sha", "ci_revision"), generation)), "ci": snapshot.get("ci"), "checks": checks, "failures": failures, "failures_summary": failures_summary, "failed_runs": failed_runs, "failed_runs_summary": failed_runs_summary, "failed_jobs": failed_jobs, "failed_jobs_summary": failed_jobs_summary, "new_review_items": review_items, "new_review_items_summary": review_items_summary, "review_backlog_count": int(snapshot.get("review_backlog_count") or 0), "timeouts": timeouts, "timeouts_summary": timeouts_summary, "actions": snapshot.get("actions") or [], "pr": snapshot.get("pr"), "state_file": str(state_path)[:MAX_OUTPUT_CHECK_DETAIL_VALUE_CHARS]}
            if generation != initial_generation and reason != "generation_changed": result["also_reasons"] = ["generation_changed"]
            result = bounded_output_payload(
                result, ("failed_jobs", "timeouts", "failures", "failed_runs")
            )
            print_json(result)
            if acknowledge_snapshot is not None:
                acknowledge_snapshot(snapshot, state_path)
            if reason == "execution_timeout": save_state(state_path, state)
            return 0
        time.sleep(args.poll_seconds)
