# /workflow Self-Implementation E2E Issues

Date: 2026-05-28

Tested feature: interactive TUI `/workflow` self-implementation.

Disposable workspace: `/tmp/codex-workflow-self-e2e.NoVKp6/workspace`

Built binary: `/extra/dkropachev/codex-3/codex-rs/target/debug/codex`

## Workflow ideas considered

1. `pr-triage`: analyze a PR diff, summarize changed files, flag risks, and suggest tests.
2. `ci-failure-digest`: read failing CI logs and produce a short root-cause/action report.
3. `dependency-bump-review`: inspect dependency bumps for release notes, breaking changes, and missing lockfile updates.
4. `release-note-drafter`: turn merged commits into grouped release-note bullets with risk notes.
5. `todo-sweep`: scan TODO/FIXME comments and generate prioritized cleanup tickets.
6. `flaky-test-reporter`: aggregate repeated local/CI failures and identify likely flaky tests.
7. `issue-branch-starter`: create a branch, summarize the issue, and draft an implementation checklist.
8. `repo-health-snapshot`: report formatting, lint, dependency, test, and stale-docs health in one pass.

Selected e2e: `pr-triage`.

## Expected behavior

From one `/workflow` prompt, Codex should create a local workflow with id and command `pr-triage`, validate it with `codex workflow validate pr-triage`, run it against `fixtures/example.diff`, and leave a working `/pr-triage` or `codex workflow run pr-triage` command.

## Actual result

Root cause: the implementation placed the workflow files in the wrong directory.

It wrote a standalone TypeScript package at the workspace root:

```text
workflow.yaml
src/
package.json
```

Codex workflow discovery does not treat the workspace root as a workflow root. With `workflows.default_location=project`, the workflow needed to live under:

```text
.codex/workflows/pr-triage/workflow.yaml
.codex/workflows/pr-triage/src/
.codex/workflows/pr-triage/package.json
```

Because the package was in the wrong place, Codex never registered `pr-triage`. The generated TypeScript package worked locally, but it could not be validated or run through the workflow system.

Passing package-level checks:

```text
bun test
10 pass, 0 fail

bun run typecheck
tsc --noEmit

bun run smoke:contract
ok=true, changedFiles=["src/login.ts", "src/cache.ts"], 7 risks
```

Failing Codex workflow checks, using the built repo binary:

```text
$ codex workflow list
code-review    /code-review    global

$ codex workflow validate pr-triage
Error: workflow id 'pr-triage' was not found

$ codex workflow run pr-triage --input '{"diffFile":"fixtures/example.diff"}'
Error: workflow id 'pr-triage' was not found
```

E2E verdict: failed. The implementation produced a plausible standalone package, not a runnable Codex workflow.

## Issues found

1. The generated workflow is not discoverable by Codex.
   - Files were placed at the disposable workspace root (`workflow.yaml`, `src/`, `package.json`) instead of a registered project workflow location.
   - `codex workflow list` did not show `pr-triage`.
   - `/pr-triage` and `codex workflow run pr-triage` were therefore unavailable.

2. The self-implementation accepted a failed required validation step.
   - The coder observed that `codex workflow validate pr-triage` could not find the workflow.
   - It switched to local package checks instead of fixing registration/discovery.

3. Workflow validation UX did not make the registration problem easy to repair.
   - `workflow validate pr-triage` only reported `workflow id 'pr-triage' was not found`.
   - The output did not point to the expected project workflow directory, registration command, or config key.

4. Large pasted prompts are hard to submit in the TUI.
   - The prompt rendered as `[Pasted Content 1024 chars]`.
   - Pressing Enter did not submit in this run; Tab was needed.
   - This is surprising during `/workflow` setup because the feature naturally invites long, structured prompts.

5. Project trust override still led to an interactive trust prompt.
   - The TUI was launched with `-c projects."/tmp/.../workspace".trust_level="trusted"`.
   - It still prompted for trust and required Enter before the workflow test could continue.

6. Workflow mode showed one model/effort but trace logs showed another.
   - The UI displayed `gpt-5.3-codex-spark medium`.
   - The trace showed model requests using `gpt-5.5` with `xhigh`.
   - This makes cost, latency, and behavior hard to reason about during workflow creation.

7. Isolated `CODEX_HOME` testing hit auth refresh failure.
   - A first attempt with a temporary `CODEX_HOME` and linked auth/config failed because the access token could not be refreshed.
   - This makes clean, hermetic TUI e2e runs brittle.

8. `/workflow` startup was noisy due unrelated MCP/app setup.
   - The run surfaced Slack MCP token errors, hook review warnings, and limit warnings.
   - These were unrelated to workflow authoring and increased the noise floor for the e2e.

9. The coder ran an unbounded root filesystem search.
   - It executed `find / -path '*workflow.yaml' -type f 2>/dev/null | head -100`.
   - This became long-running enough to require interruption/cleanup attempts.
   - A workflow authoring agent should search scoped locations first.

10. The coder over-relied on stale temp examples.
    - It read old workflows from `/tmp/...` before implementing the requested workflow.
    - This risks copying stale or environment-specific patterns instead of using the current product contract.

11. The staged workflow loop was very slow for a small workflow.
    - The initial architecture pass produced only `DESIGN.md`.
    - A 5-minute wait for the coder timed out before implementation files existed.
    - The actual implementation arrived much later, after extensive exploration.

12. Generated dependency metadata is not reproducible.
    - `package.json` used `"@types/node": "latest"` and `"typescript": "latest"`.
    - The coder ran `npm install --no-package-lock --ignore-scripts`, producing `node_modules` but no committed lockfile.
    - This is fragile for a workflow that is supposed to validate reliably.

13. Generated workflow metadata contained duplicated coverage keys.
    - `workflow.yaml` included both camelCase and kebab-case forms such as `finalResult` and `final-result`.
    - This suggests the agent was guessing at schema shape rather than using one canonical contract.

14. Generated README/usage claimed commands that do not work.
    - The metadata said to run `/pr-triage` or `codex pr-triage`.
    - The workflow was not registered, so those entrypoints were unavailable.
