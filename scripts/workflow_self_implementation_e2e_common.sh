#!/usr/bin/env bash
set -euo pipefail

mode="${1:?usage: $0 mock|real-world}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
codex_rs="$repo_root/codex-rs"
codex_bin="$codex_rs/target/debug/codex"
real_codex_home="${CODEX_HOME:-$HOME/.codex}"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/codex-workflow-self-e2e.XXXXXX")"
isolated_home="$tmp_root/codex-home"
fixture_repo="$tmp_root/fixture-repo"
mock_pid=""

cleanup() {
  if [[ -n "$mock_pid" ]] && kill -0 "$mock_pid" 2>/dev/null; then
    kill "$mock_pid" 2>/dev/null || true
    wait "$mock_pid" 2>/dev/null || true
  fi
  if [[ "${KEEP_WORKFLOW_SELF_E2E_TMP:-}" != "1" ]]; then
    rm -rf "$tmp_root"
  else
    echo "Kept e2e temp directory: $tmp_root" >&2
  fi
}
trap cleanup EXIT

snapshot_global() {
  local root="$1"
  if [[ -d "$root/workflows" ]]; then
    find "$root/workflows" -mindepth 1 -maxdepth 3 -print | sort
  fi
}

snapshot_fixture_outside_workflow() {
  local workflow_dir="$1"
  SNAPSHOT_ROOT="$fixture_repo" SNAPSHOT_EXCLUDE="$workflow_dir" python3 - <<'PY'
import hashlib
import os
from pathlib import Path

root = Path(os.environ["SNAPSHOT_ROOT"]).resolve()
exclude = Path(os.environ["SNAPSHOT_EXCLUDE"]).resolve()

def is_excluded(path: Path) -> bool:
    try:
        path.relative_to(exclude)
        return True
    except ValueError:
        return False

entries = []
for path in root.rglob("*"):
    resolved = path.resolve()
    if is_excluded(resolved):
        continue
    relative = path.relative_to(root).as_posix()
    if path.is_symlink():
        entries.append(f"L {relative} -> {os.readlink(path)}")
    elif path.is_dir():
        entries.append(f"D {relative}/")
    elif path.is_file():
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        entries.append(f"F {digest} {relative}")

print("\n".join(sorted(entries)))
PY
}

assert_fixture_outside_workflow_unchanged() {
  local label="$1"
  local before="$2"
  local after
  after="$(snapshot_fixture_outside_workflow "$workflow_dir")"
  if [[ "$before" != "$after" ]]; then
    echo "fixture files outside $workflow_dir changed $label" >&2
    diff -u <(printf '%s\n' "$before") <(printf '%s\n' "$after") >&2 || true
    exit 1
  fi
}

assert_real_world_auth_is_isolated() {
  if [[ "$mode" != "real-world" ]]; then
    return
  fi

  AUTH_PATH="$isolated_home/auth.json" python3 - <<'PY'
import base64
import json
import os
import sys
import time
from pathlib import Path

auth_path = Path(os.environ["AUTH_PATH"])
try:
    auth = json.loads(auth_path.read_text())
except Exception as exc:
    print(f"failed to read isolated auth file: {exc}", file=sys.stderr)
    raise SystemExit(1)

api_key = auth.get("OPENAI_API_KEY")
tokens = auth.get("tokens") or {}
access_token = tokens.get("access_token")
refresh_token = tokens.get("refresh_token")

if isinstance(refresh_token, str) and refresh_token.strip():
    print("isolated real-world auth retained a reusable refresh token", file=sys.stderr)
    raise SystemExit(1)

if isinstance(api_key, str) and api_key.strip():
    raise SystemExit(0)

if not isinstance(access_token, str) or not access_token.strip():
    print("isolated real-world auth has neither an API key nor a ChatGPT access token", file=sys.stderr)
    raise SystemExit(1)

try:
    payload_segment = access_token.split(".")[1]
    payload_segment += "=" * (-len(payload_segment) % 4)
    payload = json.loads(base64.urlsafe_b64decode(payload_segment.encode()))
    expires_at = payload.get("exp")
except Exception as exc:
    print(f"failed to inspect isolated ChatGPT access token expiry: {exc}", file=sys.stderr)
    raise SystemExit(1)

if not isinstance(expires_at, (int, float)) or expires_at <= time.time() + 300:
    print("isolated ChatGPT access token expires in less than five minutes", file=sys.stderr)
    raise SystemExit(1)
PY
}

print_sanitized_log() {
  local log_path="$1"
  if [[ "$mode" != "real-world" ]]; then
    cat "$log_path" >&2
    return
  fi

  AUTH_PATH="$isolated_home/auth.json" LOG_PATH="$log_path" python3 - <<'PY' >&2
import json
import os
from pathlib import Path

auth = json.loads(Path(os.environ["AUTH_PATH"]).read_text())
secrets = []
api_key = auth.get("OPENAI_API_KEY")
if isinstance(api_key, str) and api_key:
    secrets.append(api_key)
tokens = auth.get("tokens") or {}
for key in ("access_token", "refresh_token"):
    value = tokens.get(key)
    if isinstance(value, str) and value:
        secrets.append(value)

text = Path(os.environ["LOG_PATH"]).read_text(errors="replace")
for secret in secrets:
    text = text.replace(secret, "[REDACTED_AUTH_TOKEN]")
print(text, end="")
PY
}

assert_no_auth_token_leaked() {
  if [[ "$mode" != "real-world" ]]; then
    return
  fi

  AUTH_PATH="$isolated_home/auth.json" SEARCH_ROOT="$tmp_root" python3 - <<'PY'
import json
import os
import sys
from pathlib import Path

auth_path = Path(os.environ["AUTH_PATH"]).resolve()
root = Path(os.environ["SEARCH_ROOT"])
auth = json.loads(auth_path.read_text())
secrets = []
api_key = auth.get("OPENAI_API_KEY")
if isinstance(api_key, str) and api_key.strip():
    secrets.append(("API key", api_key.encode()))
tokens = auth.get("tokens") or {}
access_token = tokens.get("access_token")
if isinstance(access_token, str) and access_token.strip():
    secrets.append(("ChatGPT access token", access_token.encode()))

if not secrets:
    raise SystemExit(0)

for path in root.rglob("*"):
    if not path.is_file() or path.resolve() == auth_path:
        continue
    try:
        contents = path.read_bytes()
    except OSError:
        continue
    for label, secret in secrets:
        if secret in contents:
            relative = path.relative_to(root)
            print(f"{label} leaked into {relative}", file=sys.stderr)
            raise SystemExit(1)
PY
}

run_exec_implementation() {
  local log_path="$tmp_root/codex-exec.log"
  set +e
  env "${codex_env[@]}" "$codex_bin" exec \
    -C "$workflow_dir" \
    --sandbox workspace-write \
    --skip-git-repo-check \
    "$exec_prompt" \
    >"$log_path" 2>&1
  local status="$?"
  set -e

  assert_no_auth_token_leaked

  if [[ "$status" != "0" ]]; then
    echo "codex exec failed during workflow self-implementation:" >&2
    print_sanitized_log "$log_path"
    exit "$status"
  fi
}

assert_generated_tests_replaced() {
  WORKFLOW_DIR="$workflow_dir" python3 - <<'PY'
import os
import sys
from pathlib import Path

workflow_dir = Path(os.environ["WORKFLOW_DIR"])

def compact_without_comments(path: Path) -> str:
    lines = []
    for line in path.read_text().splitlines():
        if not line.lstrip().startswith("//"):
            lines.append(line)
    return "".join("".join(lines).split())

checks = [
    (
        "src/tests/workflow.load.test.ts",
        lambda compact: compact == "export{};",
        "load test still only exports an empty module",
    ),
    (
        "src/tests/workflow.autocomplete.test.ts",
        lambda compact: (
            "assert.deepEqual(suggestions,[]);" in compact
            or "assert.deepStrictEqual(suggestions,[]);" in compact
        ),
        "autocomplete test still asserts an empty suggestion list",
    ),
    (
        "src/tests/workflow.positive.test.ts",
        lambda compact: "{ok:true,input" in compact,
        "positive test still asserts scaffold echo output",
    ),
]

for relative, is_placeholder, message in checks:
    path = workflow_dir / relative
    if not path.is_file():
        print(f"missing generated test: {relative}", file=sys.stderr)
        raise SystemExit(1)
    if is_placeholder(compact_without_comments(path)):
        print(message, file=sys.stderr)
        raise SystemExit(1)
PY
}

assert_run_output() {
  local input_json="$1"
  local expected_items="$2"
  local output
  output="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow run todo-sweep --input "$input_json")"
  printf '%s\n' "$output" > "$tmp_root/workflow-run-$expected_items.log"
  RUN_OUTPUT="$output" EXPECTED_ITEMS="$expected_items" python3 - <<'PY'
import json
import os
import sys

expected_by_tag = {"TODO": 4, "FIXME": 3, "XXX": 3}
skip_segments = {".codex", ".git", "node_modules", "artifacts", "state"}
data = json.loads(os.environ["RUN_OUTPUT"])
expected_items = int(os.environ["EXPECTED_ITEMS"])

if set(data) != {"total", "byTag", "items", "summaryMarkdown"}:
    print(f"unexpected top-level output fields: {sorted(data)}", file=sys.stderr)
    raise SystemExit(1)
if data.get("ok") is True and "input" in data:
    print("workflow returned scaffold echo output", file=sys.stderr)
    raise SystemExit(1)
if data["total"] != 10:
    print(f"expected total 10 markers, got {data['total']}", file=sys.stderr)
    raise SystemExit(1)
if data["byTag"] != expected_by_tag:
    print(f"expected byTag {expected_by_tag}, got {data['byTag']}", file=sys.stderr)
    raise SystemExit(1)
if not isinstance(data["summaryMarkdown"], str) or not data["summaryMarkdown"].strip():
    print("summaryMarkdown must be a non-empty string", file=sys.stderr)
    raise SystemExit(1)
items = data["items"]
if not isinstance(items, list) or len(items) != expected_items:
    print(f"expected {expected_items} items, got {len(items) if isinstance(items, list) else type(items).__name__}", file=sys.stderr)
    raise SystemExit(1)
for index, item in enumerate(items):
    if set(item) != {"tag", "file", "line", "text"}:
        print(f"item {index} has unexpected fields: {sorted(item)}", file=sys.stderr)
        raise SystemExit(1)
    path = item["file"]
    if not isinstance(path, str):
        print(f"item {index} file must be a string", file=sys.stderr)
        raise SystemExit(1)
    parts = set(path.replace("\\", "/").split("/"))
    blocked = parts & skip_segments
    if blocked:
        print(f"item {index} included ignored path segment(s): {sorted(blocked)} in {path}", file=sys.stderr)
        raise SystemExit(1)
PY
}

assert_invalid_workflow_gate() {
  local workflow_yaml="$workflow_dir/workflow.yaml"
  local backup="$tmp_root/workflow.yaml.valid"
  cp "$workflow_yaml" "$backup"
  WORKFLOW_YAML="$workflow_yaml" python3 - <<'PY'
import os
import re
from pathlib import Path

path = Path(os.environ["WORKFLOW_YAML"])
contents = path.read_text()
corrupted, count = re.subn(r"^id:\s*todo-sweep\s*$", "id: todo-sweep-corrupted", contents, count=1, flags=re.MULTILINE)
if count != 1:
    raise SystemExit("failed to corrupt workflow.yaml id")
path.write_text(corrupted)
PY

  set +e
  invalid_validate_output="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow validate todo-sweep 2>&1)"
  invalid_validate_status="$?"
  invalid_run_output="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow run todo-sweep --input '{"maxItems":1}' 2>&1)"
  invalid_run_status="$?"
  set -e
  cp "$backup" "$workflow_yaml"

  printf '%s\n' "$invalid_validate_output" > "$tmp_root/workflow-invalid-validate.log"
  printf '%s\n' "$invalid_run_output" > "$tmp_root/workflow-invalid-run.log"

  if [[ "$invalid_validate_status" == "0" ]]; then
    echo "corrupted workflow unexpectedly validated successfully" >&2
    print_sanitized_log "$tmp_root/workflow-invalid-validate.log"
    exit 1
  fi
  if printf '%s\n' "$invalid_validate_output" | awk '$0 == "valid" { found = 1 } END { exit found ? 0 : 1 }'; then
    echo "corrupted workflow validation printed a standalone valid line" >&2
    print_sanitized_log "$tmp_root/workflow-invalid-validate.log"
    exit 1
  fi
  if [[ "$invalid_run_status" == "0" ]]; then
    echo "corrupted workflow unexpectedly ran successfully" >&2
    print_sanitized_log "$tmp_root/workflow-invalid-run.log"
    exit 1
  fi
  if [[ "$invalid_run_output" != *"invalid and cannot be run"* ]]; then
    echo "corrupted workflow did not report that invalid workflows cannot run" >&2
    print_sanitized_log "$tmp_root/workflow-invalid-run.log"
    exit 1
  fi
}

write_fixture_repo() {
  mkdir -p \
    "$fixture_repo/.codex" \
    "$fixture_repo/artifacts" \
    "$fixture_repo/docs" \
    "$fixture_repo/node_modules/ignored-package" \
    "$fixture_repo/scripts" \
    "$fixture_repo/src/nested/deeper" \
    "$fixture_repo/src/nested" \
    "$fixture_repo/state"
  cat > "$fixture_repo/README.md" <<'EOF'
# Fixture Repository

This repository intentionally contains markers for the todo-sweep workflow.
EOF
  cat > "$fixture_repo/src/main.ts" <<'EOF'
// TODO: tighten config parsing.
// FIXME: preserve user edits during rewrite.
// XXX: document retry policy.
export const main = 1;
EOF
  cat > "$fixture_repo/src/nested/worker.ts" <<'EOF'
// TODO: handle batch retries.
// TODO: normalize Windows paths.
// FIXME: avoid duplicate diagnostics.
// XXX: investigate parallel scan ordering.
export const worker = 2;
EOF
  cat > "$fixture_repo/src/nested/deeper/feature.ts" <<'EOF'
// TODO: cover release notes.
// FIXME: validate CLI options.
// XXX: remove temporary adapter.
export const feature = 3;
EOF
  cat > "$fixture_repo/docs/notes.md" <<'EOF'
# Notes

This file intentionally has no counted task markers.
EOF
  cat > "$fixture_repo/scripts/ops.sh" <<'EOF'
#!/usr/bin/env bash
echo ok
EOF
  cat > "$fixture_repo/.codex/ignored.ts" <<'EOF'
// TODO: ignored Codex metadata marker.
EOF
  cat > "$fixture_repo/node_modules/ignored-package/index.js" <<'EOF'
// FIXME: ignored dependency marker.
EOF
  cat > "$fixture_repo/artifacts/report.txt" <<'EOF'
XXX: ignored artifact marker.
EOF
  cat > "$fixture_repo/state/cache.txt" <<'EOF'
TODO: ignored state marker.
EOF
  git -C "$fixture_repo" init >/dev/null
  git -C "$fixture_repo" add .
  git -C "$fixture_repo" -c user.name=Codex -c user.email=codex@openai.com commit -m fixture >/dev/null
  cat > "$fixture_repo/.git/ignored-marker.txt" <<'EOF'
TODO: ignored git metadata marker.
EOF
}

start_mock_api() {
  local port_file="$tmp_root/mock-api.port"
  local log_file="$tmp_root/mock-api.log"
  python3 "$repo_root/scripts/workflow_self_implementation_mock_api.py" \
    --port 0 \
    --port-file "$port_file" \
    >"$log_file" 2>&1 &
  mock_pid="$!"

  for _ in {1..100}; do
    if [[ -s "$port_file" ]]; then
      cat "$port_file"
      return
    fi
    if ! kill -0 "$mock_pid" 2>/dev/null; then
      cat "$log_file" >&2 || true
      echo "mock Responses API exited before writing its port file" >&2
      exit 1
    fi
    sleep 0.1
  done

  cat "$log_file" >&2 || true
  echo "timed out waiting for mock Responses API" >&2
  exit 1
}

seed_real_world_auth() {
  if [[ ! -d "$real_codex_home" ]]; then
    echo "No Codex home found at $real_codex_home; run Codex login before real-world e2e." >&2
    exit 2
  fi

  for attempt in 1 2; do
    set +e
    SOURCE_CODEX_HOME="$real_codex_home" TARGET_AUTH="$isolated_home/auth.json" python3 - <<'PY'
import base64
import datetime as dt
import json
import os
import sys
import time
from pathlib import Path

source_home = Path(os.environ["SOURCE_CODEX_HOME"])
target = Path(os.environ["TARGET_AUTH"])

candidates = [("default", source_home / "auth.json")]
accounts_dir = source_home / "accounts"
if accounts_dir.is_dir():
    candidates.extend(
        (f"account:{path.name}", path / "auth.json")
        for path in sorted(accounts_dir.iterdir())
        if path.is_dir()
    )

def read_auth(path):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return None
    except Exception as exc:
        print(f"skipping unreadable auth file {path}: {exc}", file=sys.stderr)
        return None

def write_api_key(api_key):
    target.write_text(json.dumps({
        "auth_mode": "apikey",
        "OPENAI_API_KEY": api_key,
        "tokens": None,
        "last_refresh": None,
    }, indent=2) + "\n")
    raise SystemExit(0)

def token_expires_at(access_token):
    payload_segment = access_token.split(".")[1]
    payload_segment += "=" * (-len(payload_segment) % 4)
    payload = json.loads(base64.urlsafe_b64decode(payload_segment.encode()))
    return payload.get("exp")

def write_chatgpt_access_tokens(tokens):
    real_world_tokens = dict(tokens)
    real_world_tokens["refresh_token"] = ""
    target.write_text(json.dumps({
        "auth_mode": "chatgptAuthTokens",
        "OPENAI_API_KEY": None,
        "tokens": real_world_tokens,
        "last_refresh": dt.datetime.now(dt.timezone.utc).isoformat(),
    }, indent=2) + "\n")
    raise SystemExit(0)

stale_tokens = []
for label, auth_path in candidates:
    data = read_auth(auth_path)
    if not data:
        continue
    api_key = data.get("OPENAI_API_KEY")
    if isinstance(api_key, str) and api_key.strip():
        print(f"seeding real-world e2e API-key auth from {label}", file=sys.stderr)
        write_api_key(api_key)

for label, auth_path in candidates:
    data = read_auth(auth_path)
    if not data:
        continue
    tokens = data.get("tokens") or {}
    access_token = tokens.get("access_token")
    if not isinstance(access_token, str) or not access_token.strip():
        continue
    try:
        expires_at = token_expires_at(access_token)
    except Exception as exc:
        print(f"skipping {label} because its access token expiry could not be inspected: {exc}", file=sys.stderr)
        continue
    if isinstance(expires_at, (int, float)) and expires_at > time.time() + 300:
        print(f"seeding real-world e2e ChatGPT access token from {label}", file=sys.stderr)
        write_chatgpt_access_tokens(tokens)
    stale_tokens.append(label)

if stale_tokens:
    print("found only expired or near-expiry Codex access tokens: " + ", ".join(stale_tokens), file=sys.stderr)
    raise SystemExit(3)

print("no API key or usable ChatGPT access token found in Codex home", file=sys.stderr)
raise SystemExit(2)
PY
    status="$?"
    set -e
    if [[ "$status" == "0" ]]; then
      return
    fi
    if [[ "$status" == "3" && "$attempt" == "1" ]]; then
      echo "Refreshing current Codex account metadata before retrying real-world e2e auth seeding." >&2
      CODEX_HOME="$real_codex_home" "$codex_bin" account refresh >/dev/null
      continue
    fi
    exit "$status"
  done
}

seed_managed_bun_runtime() {
  local source_bin="$real_codex_home/workflows/.bin"
  local target_bin="$isolated_home/workflows/.bin"
  if [[ -x "$source_bin/bun" && -f "$source_bin/.bun-version" ]]; then
    mkdir -p "$target_bin"
    cp -p "$source_bin/bun" "$target_bin/bun"
    cp -p "$source_bin/.bun-version" "$target_bin/.bun-version"
  fi
}

write_config() {
  local mock_base_url="${1:-}"
  {
    cat <<EOF
model = "${CODEX_WORKFLOW_SELF_E2E_MODEL:-gpt-5.2}"
EOF
    if [[ -n "$mock_base_url" ]]; then
      cat <<'EOF'
model_provider = "workflow_self_mock"
EOF
    fi
    cat <<EOF
approval_policy = "never"
suppress_unstable_features_warning = true

[features]
workflows = true

[analytics]
enabled = false

[workflows]
default_location = "project"
commit_policy = "manual"
dependency_update_policy = "manual"
repair_mode = "full"

[projects."$fixture_repo"]
trust_level = "trusted"
EOF
  } > "$isolated_home/config.toml"

  if [[ -n "$mock_base_url" ]]; then
    cat >> "$isolated_home/config.toml" <<EOF

[model_providers.workflow_self_mock]
name = "workflow_self_mock"
base_url = "$mock_base_url/v1"
env_key = "OPENAI_API_KEY_WORKFLOW_SELF_MOCK"
request_max_retries = 0
stream_max_retries = 0
stream_idle_timeout_ms = 30000
supports_websockets = false
EOF
  fi
}

cargo build -p codex-cli --bin codex --manifest-path "$codex_rs/Cargo.toml"

case "$mode" in
  mock)
    mkdir -p "$isolated_home"
    seed_managed_bun_runtime
    mock_base_url="$(start_mock_api)"
    write_config "$mock_base_url"
    codex_env=(CODEX_HOME="$isolated_home" OPENAI_API_KEY_WORKFLOW_SELF_MOCK="mock")
    ;;
  real-world)
    mkdir -p "$isolated_home"
    seed_managed_bun_runtime
    seed_real_world_auth
    assert_real_world_auth_is_isolated
    write_config
    codex_env=(CODEX_HOME="$isolated_home")
    ;;
  *)
    echo "unknown workflow self e2e mode: $mode" >&2
    exit 2
    ;;
esac

write_fixture_repo
before_global="$(snapshot_global "$real_codex_home")"

env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow develop \
  --location project \
  --id todo-sweep \
  --command todo-sweep \
  "Find TODO, FIXME, and XXX markers in the current repository and return total, byTag, items, and summaryMarkdown."

workflow_dir="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow where todo-sweep)"
expected_workflow_dir="$fixture_repo/.codex/workflows/todo-sweep"
if [[ "$workflow_dir" != "$expected_workflow_dir" ]]; then
  echo "workflow where returned '$workflow_dir', expected '$expected_workflow_dir'" >&2
  exit 1
fi
outside_after_scaffold="$(snapshot_fixture_outside_workflow "$workflow_dir")"

exec_prompt="Implement the todo-sweep workflow in this workflow directory only. It must scan every regular text file under ctx.cwd for TODO, FIXME, and XXX comments without filtering by file extension; skip .codex, .git, node_modules, artifacts, and state directories recursively; honor input maxItems by limiting only the returned items array after computing all matches; keep total and byTag as full-repository counts regardless of maxItems; return JSON with exactly the top-level fields total, byTag, items, and summaryMarkdown; use byTag keys TODO, FIXME, and XXX; use item fields tag, file, line, and text; and make codex workflow validate todo-sweep print exactly valid. Do not edit files outside this workflow directory."
if [[ "$mode" == "real-world" ]]; then
  real_world_prompt="$(cat <<'EOF'
Real-world e2e constraints:
- The workflow validator extracts the API contract from src/workflow.ts with limited local typings. If you import node:fs/promises or node:path, add src/types.d.ts with declarations for the exact APIs you use and put the triple-slash reference path directive at the top of src/workflow.ts.
- Replace every scaffold placeholder test. The load test must do more than export {}; autocomplete must assert a non-empty useful suggestion; the positive test must assert real TODO/FIXME/XXX scan output, not { ok: true, input }.
- The repository includes ignored directories with extra markers. Do not count markers below .codex, .git, node_modules, artifacts, or state.
- Do not restrict scanning to a hard-coded extension allowlist; traverse regular text files and handle unreadable or binary files by skipping them.
- Preserve the exact output shape: total number, byTag object with TODO/FIXME/XXX counts, items array of { tag, file, line, text }, and non-empty summaryMarkdown string. Do not add scaffold fields such as ok or input.
- maxItems limits only the returned items array. total and byTag must always describe every marker found before truncation.
- Do not write generated artifacts inside this workflow directory from tests. Use temporary directories under /tmp for fixture files.
- Keep package.json dependency versions pinned; do not use latest.
- Run codex workflow validate todo-sweep from the fixture repository root until stdout is exactly valid.
EOF
)"
  exec_prompt="$exec_prompt

$real_world_prompt"
fi

run_exec_implementation
assert_fixture_outside_workflow_unchanged "during implementation" "$outside_after_scaffold"

set +e
validate_output="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow validate todo-sweep 2>&1)"
validate_status="$?"
set -e
printf '%s\n' "$validate_output" > "$tmp_root/workflow-validate.log"
if [[ "$validate_status" != "0" || "$validate_output" != "valid" ]]; then
  echo "validation output was not exactly valid:" >&2
  print_sanitized_log "$tmp_root/workflow-validate.log"
  exit 1
fi

assert_generated_tests_replaced
assert_run_output '{"maxItems":10}' 10
assert_run_output '{"maxItems":3}' 3
assert_fixture_outside_workflow_unchanged "during validation and run checks" "$outside_after_scaffold"
assert_invalid_workflow_gate
assert_no_auth_token_leaked

after_global="$(snapshot_global "$real_codex_home")"
if [[ "$before_global" != "$after_global" ]]; then
  echo "real global workflow directory changed during isolated e2e" >&2
  diff -u <(printf '%s\n' "$before_global") <(printf '%s\n' "$after_global") >&2 || true
  exit 1
fi

echo "workflow self-implementation $mode e2e passed"
