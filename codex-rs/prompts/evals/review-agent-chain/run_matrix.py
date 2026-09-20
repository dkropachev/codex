import argparse
import collections
import hashlib
import json
import os
import pathlib
import queue
import random
import subprocess
import threading
import time


MODEL = "gpt-5.6-sol"
EFFORT = "medium"


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_output(root, *args):
    return subprocess.run(
        ["git", *args],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout


def binary_source_inputs(root):
    root = pathlib.Path(root).resolve()
    tracked = git_output(
        root, "ls-files", "--cached", "--others", "--exclude-standard", "-z"
    ).decode().split("\0")
    return [
        root / relative
        for relative in tracked
        if pathlib.Path(relative).suffix
        in {".rs", ".toml", ".lock", ".bazel", ".bzl", ".lark"}
        or relative.startswith("codex-rs/prompts/templates/")
    ]


def source_identity(root):
    root = pathlib.Path(root).resolve()
    head = git_output(root, "rev-parse", "HEAD").decode().strip()
    digest = hashlib.sha256()
    for path in sorted(binary_source_inputs(root)):
        if not path.exists():
            continue
        digest.update(str(path.relative_to(root)).encode())
        digest.update(path.read_bytes())
    return f"{head}+source-{digest.hexdigest()[:12]}"


def verify_binary_is_current(binary, root):
    source_inputs = binary_source_inputs(root)
    newest_input = max(path.stat().st_mtime for path in source_inputs if path.exists())
    if pathlib.Path(binary).stat().st_mtime < newest_input:
        raise RuntimeError(f"benchmark binary is older than source inputs: {binary}")


def tree_sha256(root):
    digest = hashlib.sha256()
    for path in sorted(path for path in pathlib.Path(root).rglob("*") if path.is_file()):
        if ".git" in path.parts:
            continue
        digest.update(str(path.relative_to(root)).encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def send(process, message):
    process.stdin.write(json.dumps(message) + "\n")
    process.stdin.flush()


def read_message(process, messages, deadline):
    timeout = deadline - time.monotonic()
    if timeout <= 0:
        raise TimeoutError("app-server response timed out")
    try:
        message = messages.get(timeout=timeout)
    except queue.Empty as error:
        raise TimeoutError("app-server response timed out") from error
    if message is None:
        raise RuntimeError(f"app-server exited with {process.poll()}")
    return message


def wait_for_response(process, messages, request_id, deadline, observed):
    while True:
        message = read_message(process, messages, deadline)
        observed.append(message)
        if message.get("id") == request_id:
            if "error" in message:
                raise RuntimeError(message["error"])
            return message["result"]


def run_review(binary, codex_home, case_root, verification, legacy):
    command = [
        binary,
        "app-server",
        "--stdio",
        "-c",
        f'model="{MODEL}"',
        "-c",
        f'review_model="{MODEL}"',
        "-c",
        f'model_reasoning_effort="{EFFORT}"',
        "-c",
        "mcp_servers={}",
        "-c",
        "memories.use_memories=false",
        "-c",
        "memories.dedicated_tools=false",
    ]
    environment = os.environ.copy()
    environment["CODEX_HOME"] = codex_home
    started = time.monotonic()
    observed = []
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=environment,
    )
    messages = queue.Queue()
    stderr_tail = collections.deque(maxlen=200)

    def read_stdout():
        for line in process.stdout:
            messages.put(json.loads(line))
        messages.put(None)

    def read_stderr():
        for line in process.stderr:
            stderr_tail.append(line.rstrip())

    threading.Thread(target=read_stdout, daemon=True).start()
    threading.Thread(target=read_stderr, daemon=True).start()
    deadline = time.monotonic() + 600
    try:
        send(
            process,
            {
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "review-benchmark",
                        "title": None,
                        "version": "1",
                    },
                    "capabilities": {"experimentalApi": True},
                },
            },
        )
        wait_for_response(process, messages, 1, deadline, observed)
        send(process, {"method": "initialized"})
        send(
            process,
            {
                "id": 2,
                "method": "thread/start",
                "params": {
                    "cwd": str(case_root),
                    "model": MODEL,
                    "approvalPolicy": "never",
                    "sandbox": "read-only",
                    "ephemeral": True,
                },
            },
        )
        thread = wait_for_response(process, messages, 2, deadline, observed)["thread"]
        review_params = {
            "threadId": thread["id"],
            "target": {"type": "uncommittedChanges"},
        }
        if not legacy:
            review_params.update({"verification": verification, "action": "report"})
        send(
            process,
            {"id": 3, "method": "review/start", "params": review_params},
        )
        review = wait_for_response(process, messages, 3, deadline, observed)
        turn_id = review["turn"]["id"]
        report = None
        finding_count = None
        usage = None
        while True:
            message = read_message(process, messages, deadline)
            observed.append(message)
            method = message.get("method")
            params = message.get("params") or {}
            if method == "item/completed" and params.get("turnId") == turn_id:
                item = params.get("item") or {}
                if item.get("type") == "exitedReviewMode":
                    report = item.get("review")
                    finding_count = item.get("findingCount")
            elif method == "thread/tokenUsage/updated" and params.get("turnId") == turn_id:
                usage = params.get("tokenUsage")
            elif method == "turn/completed" and (params.get("turn") or {}).get("id") == turn_id:
                break
        return {
            "verification": verification,
            "report": report,
            "findingCount": finding_count,
            "usage": usage,
            "elapsedSeconds": round(time.monotonic() - started, 3),
            "messageCount": len(observed),
        }
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def write_artifact(path, artifact):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--main-binary", required=True)
    parser.add_argument("--branch-binary", required=True)
    parser.add_argument("--codex-home", required=True)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--main-source-root", required=True)
    parser.add_argument("--branch-source-root", required=True)
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--seed", type=int, default=144)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    verify_binary_is_current(args.main_binary, args.main_source_root)
    verify_binary_is_current(args.branch_binary, args.branch_source_root)
    root = pathlib.Path(args.root).resolve()
    cases = sorted(path.name for path in (root / "cases").iterdir() if path.is_dir())
    arms = ["main", "singlePass", "doubleCheck"]
    work = [
        (repetition, case, arm)
        for repetition in range(1, args.repetitions + 1)
        for case in cases
        for arm in arms
    ]
    random.Random(args.seed).shuffle(work)
    output = pathlib.Path(args.output)
    manifest = {
        "model": MODEL,
        "reasoningEffort": EFFORT,
        "repetitions": args.repetitions,
        "randomSeed": args.seed,
        "mainRevision": source_identity(args.main_source_root),
        "branchRevision": source_identity(args.branch_source_root),
        "mainBinarySha256": sha256(args.main_binary),
        "branchBinarySha256": sha256(args.branch_binary),
        "corpusSha256": sha256(args.corpus),
        "preparedCasesSha256": tree_sha256(root / "cases"),
        "codexHomeConfigSha256": (
            sha256(pathlib.Path(args.codex_home) / "config.toml")
            if (pathlib.Path(args.codex_home) / "config.toml").is_file()
            else None
        ),
        "configOverrides": {
            "mcp_servers": {},
            "memories.use_memories": False,
            "memories.dedicated_tools": False,
        },
    }
    results = []
    if output.exists():
        prior = json.loads(output.read_text(encoding="utf-8"))
        if prior.get("manifest") != manifest:
            raise RuntimeError("existing result manifest does not match this run")
        results = prior.get("results", [])
    completed = {(row["run"], row["case"], row["arm"]) for row in results}
    for repetition, case, arm in work:
        if (repetition, case, arm) in completed:
            print(f"run={repetition} case={case} arm={arm} cached", flush=True)
            continue
        binary = args.main_binary if arm == "main" else args.branch_binary
        for attempt in range(1, 4):
            try:
                result = run_review(
                    binary,
                    args.codex_home,
                    root / "cases" / case,
                    "singlePass" if arm == "main" else arm,
                    arm == "main",
                )
                break
            except (RuntimeError, TimeoutError) as error:
                if attempt == 3:
                    raise
                print(
                    f"run={repetition} case={case} arm={arm} "
                    f"retry={attempt} error={error}",
                    flush=True,
                )
        result.update({"run": repetition, "case": case, "arm": arm})
        results.append(result)
        write_artifact(output, {"manifest": manifest, "results": results})
        print(f"run={repetition} case={case} arm={arm} done", flush=True)
    results.sort(key=lambda row: (row["run"], row["case"], row["arm"]))
    write_artifact(output, {"manifest": manifest, "results": results})


if __name__ == "__main__":
    main()
