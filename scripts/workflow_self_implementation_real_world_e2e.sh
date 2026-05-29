#!/usr/bin/env bash
set -euo pipefail

if [[ "${CODEX_WORKFLOW_SELF_REAL_WORLD_E2E:-}" != "1" ]]; then
  echo "Set CODEX_WORKFLOW_SELF_REAL_WORLD_E2E=1 to run the real-world workflow self-implementation e2e." >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$repo_root/scripts/workflow_self_implementation_e2e_common.sh" real-world
