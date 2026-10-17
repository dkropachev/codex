import argparse
import hashlib
import http.server
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import threading
import time

from offline_exec_probe import Handler


def tree_digest(root):
    digest = hashlib.sha256()
    count = 0
    for path in sorted(root.rglob("*")):
        if path.is_file():
            digest.update(path.relative_to(root).as_posix().encode())
            digest.update(b"\x00")
            digest.update(path.read_bytes())
            count += 1
    return {"sha256": digest.hexdigest(), "files": count}


parser = argparse.ArgumentParser()
parser.add_argument("--baseline", type=Path, required=True)
parser.add_argument("--candidate", type=Path)
parser.add_argument("--samples", type=int, default=12)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
packages = {"baseline": args.baseline.resolve()}
if args.candidate:
    packages["candidate"] = args.candidate.resolve()
prefix = []
cpu = None
if hasattr(os, "sched_getaffinity") and shutil.which("taskset"):
    cpu = min(os.sched_getaffinity(0))
    prefix = ["taskset", "-c", str(cpu)]

results = {}
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    homes = {label: root / f"{label}-home" for label in packages}
    for home in homes.values():
        home.mkdir()
    work = root / "work"
    work.mkdir()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.response_count = 0
    threading.Thread(target=server.serve_forever, daemon=True).start()
    for label, package in packages.items():
        results[label] = {
            "codex_bytes": (package / "bin/codex").stat().st_size,
            "host_bytes": (package / "bin/codex-code-mode-host").stat().st_size,
            "commands": {},
        }
    labels = list(packages)
    for scenario in (
        "version",
        "help",
        "schema",
        "schema_experimental",
        "host_startup",
        "offline_exec",
    ):
        if scenario == "offline_exec":
            for home in homes.values():
                (home / "config.toml").write_text(
                    'model = "gpt-5.5"\nmodel_provider = "benchmark"\n'
                    '[model_providers.benchmark]\nname = "Benchmark"\n'
                    f'base_url = "http://127.0.0.1:{server.server_port}/v1"\n'
                    'wire_api = "responses"\nrequires_openai_auth = false\n'
                )
        samples = {label: [] for label in labels}
        schemas = {}
        for iteration in range(args.samples + 2):
            order = (
                labels[iteration % len(labels) :] + labels[: iteration % len(labels)]
            )
            for label in order:
                package = packages[label]
                output = root / f"{scenario}-{label}-{iteration}"
                if scenario == "host_startup":
                    command = [str(package / "bin/codex-code-mode-host")]
                else:
                    command = [str(package / "bin/codex")]
                    if scenario == "offline_exec":
                        command.extend(
                            [
                                "--no-daemon",
                                "exec",
                                "--skip-git-repo-check",
                                "--sandbox",
                                "read-only",
                                "--json",
                                "Reply done",
                            ]
                        )
                    elif scenario in ("version", "help"):
                        command.append(f"--{scenario}")
                    else:
                        command.extend(
                            ["app-server", "generate-json-schema", "--out", str(output)]
                        )
                        if scenario == "schema_experimental":
                            command.append("--experimental")
                started = time.perf_counter()
                prior_requests = server.response_count
                process = subprocess.run(
                    [*prefix, *command],
                    cwd=work,
                    env={
                        **os.environ,
                        "CODEX_HOME": str(homes[label]),
                        "NO_PROXY": "localhost,127.0.0.1",
                        "no_proxy": "localhost,127.0.0.1",
                    },
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=60,
                )
                elapsed = time.perf_counter() - started
                if process.returncode:
                    raise RuntimeError(
                        f"{label}/{scenario}: {process.stderr.decode(errors='replace')[:2000]}"
                    )
                if scenario == "offline_exec":
                    events = [json.loads(line) for line in process.stdout.splitlines()]
                    assert any(
                        event.get("type") == "turn.completed" for event in events
                    )
                    assert any(
                        event.get("item", {}).get("text") == "done" for event in events
                    )
                    assert server.response_count == prior_requests + 1
                if iteration >= 2:
                    samples[label].append(elapsed * 1000)
                if scenario.startswith("schema"):
                    observed = tree_digest(output)
                    if label in schemas and observed != schemas[label]:
                        raise RuntimeError(
                            f"Nondeterministic schema output for {label}/{scenario}"
                        )
                    schemas[label] = observed
                    shutil.rmtree(output)
        if len(schemas) == 2 and schemas["baseline"] != schemas["candidate"]:
            raise RuntimeError(f"Profile changed schema output: {scenario}")
        for label, values in samples.items():
            results[label]["commands"][scenario] = {
                "median_ms": statistics.median(values),
                "p95_ms": sorted(values)[
                    min(len(values) - 1, math.ceil(len(values) * 0.95) - 1)
                ],
                "samples_ms": values,
                **({"output": schemas[label]} if label in schemas else {}),
            }
    server.shutdown()
    server.server_close()
report = {
    "pinned_cpu": cpu,
    "samples": args.samples,
    "scope": "Warm startup, schema generation and one locally mocked agent turn; not full agent throughput",
    "results": results,
}
args.output.write_text(json.dumps(report, indent=2) + "\n")
print(
    json.dumps(
        {
            label: {
                name: round(data["median_ms"], 2)
                for name, data in value["commands"].items()
            }
            for label, value in results.items()
        }
    )
)
