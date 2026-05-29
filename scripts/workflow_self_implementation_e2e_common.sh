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

write_fixture_repo() {
  mkdir -p "$fixture_repo"
  cat > "$fixture_repo/README.md" <<'EOF'
# Fixture Repository

This repository intentionally contains markers for the todo-sweep workflow.
EOF
  cat > "$fixture_repo/src-a.ts" <<'EOF'
// TODO: tighten config parsing.
export const a = 1;
EOF
  cat > "$fixture_repo/src-b.ts" <<'EOF'
// FIXME: preserve user edits during rewrite.
// XXX: document retry policy.
export const b = 2;
EOF
  git -C "$fixture_repo" init >/dev/null
  git -C "$fixture_repo" add .
  git -C "$fixture_repo" -c user.name=Codex -c user.email=codex@openai.com commit -m fixture >/dev/null
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

exec_prompt="Implement the todo-sweep workflow in this workflow directory only. It must scan ctx.cwd for TODO, FIXME, and XXX comments, honor input maxItems, return JSON with total, byTag, items, and summaryMarkdown, and make codex workflow validate todo-sweep print exactly valid. Do not edit files outside this workflow directory."
if [[ "$mode" == "real-world" ]]; then
  real_world_prompt="$(cat <<'EOF'
Real-world e2e constraints:
- The workflow validator extracts the API contract from src/workflow.ts with limited local typings. If you import node:fs/promises or node:path, add src/types.d.ts with declarations for the exact APIs you use and put the triple-slash reference path directive at the top of src/workflow.ts.
- Replace every scaffold placeholder test. The load test must do more than export {}; autocomplete must assert a non-empty useful suggestion; the positive test must assert real TODO/FIXME/XXX scan output, not { ok: true, input }.
- Do not write generated artifacts inside this workflow directory from tests. Use temporary directories under /tmp for fixture files.
- Keep package.json dependency versions pinned; do not use latest.
- Run codex workflow validate todo-sweep from the fixture repository root until stdout is exactly valid.
EOF
)"
  exec_prompt="$exec_prompt

$real_world_prompt"
fi

env "${codex_env[@]}" "$codex_bin" exec \
  -C "$workflow_dir" \
  --sandbox workspace-write \
  --skip-git-repo-check \
  "$exec_prompt"

set +e
validate_output="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow validate todo-sweep 2>&1)"
validate_status="$?"
set -e
if [[ "$validate_status" != "0" || "$validate_output" != "valid" ]]; then
  echo "validation output was not exactly valid:" >&2
  printf '%s\n' "$validate_output" >&2
  exit 1
fi

run_output="$(env "${codex_env[@]}" "$codex_bin" -C "$fixture_repo" workflow run todo-sweep --input '{"maxItems":10}')"
RUN_OUTPUT="$run_output" python3 - <<'PY'
import json
import os
import sys

data = json.loads(os.environ["RUN_OUTPUT"])
for key in ("total", "byTag", "items", "summaryMarkdown"):
    if key not in data:
        print(f"missing key: {key}", file=sys.stderr)
        sys.exit(1)
if data.get("ok") is True and "input" in data:
    print("workflow returned scaffold echo output", file=sys.stderr)
    sys.exit(1)
if data["total"] < 3:
    print(f"expected at least 3 markers, got {data['total']}", file=sys.stderr)
    sys.exit(1)
if not isinstance(data["items"], list) or not data["items"]:
    print("items must be a non-empty list", file=sys.stderr)
    sys.exit(1)
PY

after_global="$(snapshot_global "$real_codex_home")"
if [[ "$before_global" != "$after_global" ]]; then
  echo "real global workflow directory changed during isolated e2e" >&2
  diff -u <(printf '%s\n' "$before_global") <(printf '%s\n' "$after_global") >&2 || true
  exit 1
fi

echo "workflow self-implementation $mode e2e passed"
