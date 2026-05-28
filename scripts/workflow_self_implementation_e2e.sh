#!/usr/bin/env bash
set -euo pipefail

if [[ "${CODEX_WORKFLOW_SELF_E2E:-}" != "1" ]]; then
  echo "Set CODEX_WORKFLOW_SELF_E2E=1 to run the real-AI workflow self-implementation e2e." >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
codex_rs="$repo_root/codex-rs"
codex_bin="$codex_rs/target/debug/codex"
real_codex_home="${CODEX_HOME:-$HOME/.codex}"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/codex-workflow-self-e2e.XXXXXX")"
isolated_home="$tmp_root/codex-home"
fixture_repo="$tmp_root/fixture-repo"

cleanup() {
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

mkdir -p "$isolated_home" "$fixture_repo"
if [[ -f "$real_codex_home/auth.json" ]]; then
  cp "$real_codex_home/auth.json" "$isolated_home/auth.json"
fi

cat > "$isolated_home/config.toml" <<EOF
model = "${CODEX_WORKFLOW_SELF_E2E_MODEL:-gpt-5.2}"
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

before_global="$(snapshot_global "$real_codex_home")"

cargo build -p codex-cli --bin codex --manifest-path "$codex_rs/Cargo.toml"

CODEX_HOME="$isolated_home" "$codex_bin" -C "$fixture_repo" workflow develop \
  --location project \
  --id todo-sweep \
  --command todo-sweep \
  "Find TODO, FIXME, and XXX markers in the current repository and return total, byTag, items, and summaryMarkdown."

workflow_dir="$(CODEX_HOME="$isolated_home" "$codex_bin" -C "$fixture_repo" workflow where todo-sweep)"
expected_workflow_dir="$fixture_repo/.codex/workflows/todo-sweep"
if [[ "$workflow_dir" != "$expected_workflow_dir" ]]; then
  echo "workflow where returned '$workflow_dir', expected '$expected_workflow_dir'" >&2
  exit 1
fi

CODEX_HOME="$isolated_home" "$codex_bin" exec \
  -C "$workflow_dir" \
  --sandbox workspace-write \
  --skip-git-repo-check \
  "Implement the todo-sweep workflow in this workflow directory only. It must scan ctx.cwd for TODO, FIXME, and XXX comments, honor input maxItems, return JSON with total, byTag, items, and summaryMarkdown, and make codex workflow validate todo-sweep print exactly valid. Do not edit files outside this workflow directory."

validate_output="$(CODEX_HOME="$isolated_home" "$codex_bin" -C "$fixture_repo" workflow validate todo-sweep)"
if [[ "$validate_output" != "valid" ]]; then
  echo "validation output was not exactly valid:" >&2
  printf '%s\n' "$validate_output" >&2
  exit 1
fi

run_output="$(CODEX_HOME="$isolated_home" "$codex_bin" -C "$fixture_repo" workflow run todo-sweep --input '{"maxItems":10}')"
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

echo "workflow self-implementation e2e passed"
