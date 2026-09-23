# Review-and-fix PR skill/workflow plan

## Document status

- Status: proposed
- Analysis baseline: `review-chain-04-bounded-context` at `0157964634`
- Target: this Codex fork, not upstream/public Codex
- Behavioral reference: `dkropachev/automated-development` at
  `61c696d5bf4cbf31583128deb87e8ac479605117`
- Intended user entry point: `/review-and-fix-pr [PR number or URL]`

## Executive decision

Build the final feature as a repository-local Codex skill backed by a first-class, app-server-owned
workflow. Do not port the Claude implementation line-for-line.

The Claude workflow needs `review-and-fix-pr-driver.js` mainly because its workflow can make only
one agent call and cannot send that agent a follow-up. Codex should instead retain an agent thread
and send explicit follow-up turns from the workflow controller. Deterministic code should still own
coverage accounting, state transitions, Git verification, validation results, and commit gating.

The current checkout cannot host that final design yet. Its registered workflow path is a
compatibility launcher:

- `codex-rs/cli/src/workflow_cmd/compat.rs` starts `bun src/workflow.ts --input ...` and waits.
- `codex-rs/core/src/tasks/workflow_command.rs` gives TUI workflows a minimal context containing
  `progress` and working-directory aliases, then invokes `run` and `format` once.
- The tracked TypeScript SDK has persistent `Thread` objects, but it does not export the richer
  workflow SDK or inject `createAgent`, `resumeAgent`, or `AgentHandle` into workflow code.

Therefore delivery has two tracks:

1. Restore and finish the agent-aware workflow substrate.
2. Build the review-and-fix package after that substrate has stable tests.

An interim skill can orchestrate the existing `spawn_agent`, `followup_task`, `send_message`, and
`wait_agent` tools, but it must be described as an interim model-controlled controller, not as the
finished deterministic workflow.

### Optional compatibility prototype

If review-only behavior is needed before the runtime foundation lands, build a disposable prototype
behind an `AgentRuntime` interface:

```text
AgentRuntime
  createReviewer(options) -> handle
  createFixer(options) -> handle
  run(handle, prompt, outputSchema, signal) -> structured result
  close(handle)
```

The temporary implementation may use the tracked TypeScript SDK's persistent `Thread` and repeated
`run` calls from inside the Bun process. `workflow.ts` would need both an exported `run`/`format`
module contract for the TUI and a guarded `--input` CLI entry point, both calling one canonical
orchestrator. Keep all SDK use inside one adapter so it can later be replaced by native
`WorkflowContext` agents.

This prototype must remain report-only by default and must be clearly labeled as a compatibility
bridge. It launches nested Codex processes outside the workflow subsystem's agent tree, so current
workflow cancellation, approvals, child cleanup, observability, and binary identity do not cover it
correctly. Do not treat it as satisfying the acceptance criteria below.

## Goals

The completed feature should:

1. Resolve a pull request from a number, URL, or the current branch.
2. Refuse unsafe starting conditions before any mutation.
3. Review every classifier-approved hunk or report it explicitly as not reviewed.
4. Use parallel read-only reviewers without permitting concurrent writers.
5. Keep reviewer and fixer threads across follow-up turns where retained context is useful.
6. Fix findings in bounded serial batches.
7. Run validation itself and use process exit status, not an agent's statement, as evidence.
8. Commit only a verified batch and verify the resulting Git transition.
9. Re-chunk remaining work after every successful mutation.
10. Remember unchanged files that were conclusively reviewed clean.
11. Resume safely after interruption without silently losing or repeating work.
12. Report release blockers, fixed findings, still-present findings, deferred work, out-of-scope
    findings, false positives, follow-ups, and unreviewed chunks separately.
13. Never push, comment, merge, reset, clean, stash, rebase, amend, or automatically revert.

## Non-goals

- Proving that reviewed code is defect-free. Coverage accounting proves which reviewable hunks
  reached a completed review state, not that the model understood them perfectly.
- Automatically pushing commits or writing to the pull request.
- Fixing pre-existing defects that the pull request did not introduce, worsen, expose, or claim to
  fix.
- Running multiple writers against one working tree.
- Treating generated, binary, vendored, or classifier-excluded content as reviewed.
- Replacing the repository's existing native review chain. Reuse its structured schemas, isolated
  stage execution, Git transactions, and verification evidence wherever their contracts fit.

## User experience

### Invocation

```text
/review-and-fix-pr
/review-and-fix-pr 1234
/review-and-fix-pr https://github.com/owner/repo/pull/1234
```

The skill performs cheap checks and then starts the workflow with structured input. It does not
implement the review loop itself.

### Normal inputs

| Field               |             Default | Meaning                                             |
| ------------------- | ------------------: | --------------------------------------------------- |
| `pr`                |   current branch PR | Pull request number or URL                          |
| `action`            | ownership-dependent | `report`, `fix`, or `fixAndCommit`                  |
| `mode`              |              `auto` | `auto`, `single`, `parallel`, or `full`             |
| `verification`      |       `doubleCheck` | Candidate verification policy                       |
| `detailedReview`    |             `false` | Permit experiments in isolated disposable checkouts |
| `reviewConcurrency` |       runtime limit | Maximum simultaneous read-only reviewers            |
| `maxFixBatch`       |                `10` | Maximum findings assigned to one fixer              |
| `maxOutstanding`    |                `10` | Findings that trigger review backpressure           |
| `chunkBytes`        |  `20_000` per stage | Target packed hunk bytes, never splitting one hunk  |
| `ignoreLedger`      |             `false` | Re-review content already recorded clean            |
| `maxAgents`         |          configured | Hard agent/thread ceiling                           |
| `maxTokens`         |          configured | Hard workflow token ceiling                         |
| `repoRoot`          |          caller cwd | Repository being reviewed                           |

Fixing must always be explicit in the workflow input. The skill should recommend `report` for an
unknown or different PR author and `fixAndCommit` for the authenticated user's own PR, but it must
show both choices. An inferred ownership field must never grant write authority.

### Mode selection

- `single`: one retained agent reviews the entire PR, then fixes if authorized.
- `parallel`: size-capped chunks go to fresh read-only reviewers; findings are fixed serially.
- `full`: one whole-PR pass regardless of size.
- `auto`: use `single` for at most 20 KB of remaining reviewable diff, otherwise `parallel`.

When a parallel pass completes with zero findings, run a skeptical whole-PR confirmation pass to
look for cross-chunk interactions. Do not run that confirmation when the clean-content ledger has
already removed all remaining work unless the caller explicitly ignores the ledger.

### Final handoff

The report must lead with human-actionable state in this order:

1. Release blockers.
2. Uncommitted edits left by an incomplete batch.
3. Chunks not reviewed to completion.
4. Findings still present after a fix attempt.
5. Deferred findings.
6. Open follow-ups.
7. Fixed findings and their commits.
8. Verified out-of-scope defects and set-aside candidates.
9. Candidates rejected as not bugs.
10. Validation and baseline status.
11. Exact starting SHA, ending SHA, branch, ledger path, and safe inspection instructions.

## Proposed package layout

The end-state package should be created with the workflow scaffolder after the runtime foundation
exists:

```text
.codex/
  skills/
    review-and-fix-pr/
      SKILL.md
      reference.md
  workflows/
    review-and-fix-pr/
      workflow.yaml
      README.md
      DESIGN.md
      package.json
      .gitignore
      src/
        workflow.ts
        contracts.ts
        state.ts
        scope.ts
        classify.ts
        chunk.ts
        ledger.ts
        baseline.ts
        review.ts
        findings.ts
        fix.ts
        validate.ts
        report.ts
        git.ts
        tests/
          ...
      state/
        .gitkeep
      artifacts/
```

Runtime state and generated artifacts must be ignored. No per-repository state may be written into
the plugin or workflow source checkout.

Security-sensitive Git operations should reuse or extend `codex-rs/git-utils` instead of being
implemented twice in TypeScript. Workflow-specific orchestration belongs in the workflow package,
not `codex-core`. If a reusable concept does not fit `git-utils`, introduce a focused crate instead
of adding more surface to `codex-core`.

## Target architecture

```text
Skill adapter
  -> app-server workflowRun/start
     -> deterministic workflow controller
        -> PR scope + safety gate
        -> baseline discovery and execution
        -> classifier + hunk chunker + clean ledger
        -> read-only reviewer thread pool
        -> finding normalization and backpressure
        -> one fixer thread at a time
        -> deterministic validation + Git transaction
        -> re-chunk remaining hunks
        -> final report + durable state
```

### 1. Skill adapter

Responsibilities:

- Run fast, read-only preflight checks.
- Resolve the selected PR and authenticated user.
- Measure the remaining ledger-filtered diff before presenting mode choices.
- Ask only for choices that change behavior: mode for a large PR and mutation authority.
- Invoke the registered workflow with typed input.
- Relay the workflow's final markdown handoff.
- If a failed batch leaves edits, inspect and summarize them before proposing any later commit.

The skill must not implement chunking, finding aggregation, validation, or recovery. Keeping those
inside the workflow makes CLI, TUI, and app-server runs behave identically.

### 2. Workflow run controller

The controller is a deterministic state machine. It owns transitions; agents return data and make
requested edits but do not decide which phase comes next.

```text
created
  -> preflighted
  -> scoped
  -> baselined
  -> reviewing(stage, round, wave)
  -> fixing(stage, batch)
  -> validating(stage, batch)
  -> committing(stage, batch)
  -> rechunking(stage, round)
  -> reviewing(...)
  -> reconciling
  -> reporting
  -> completed
```

Terminal or human-intervention states:

```text
refused
stopped_agent_budget
stopped_token_budget
stopped_reviewer_failure
stopped_validation_failure_with_edits
stopped_commit_verification_failure
stopped_state_corruption
canceled
```

Every transition is persisted atomically before launching the next agent or mutating Git. Recovery
must reconcile persisted state against the real working tree and `HEAD`; it must never trust the
state file over Git.

### 3. Scope and safety gate

Resolve and record:

- Repository identity and a filesystem-safe repository slug.
- PR number, URL, title, body, author, base ref, exact merge base, and exact head SHA.
- Starting branch and starting SHA.
- Authenticated GitHub identity.
- Changed paths and statuses, including renames, deletions, binaries, and submodules.
- Explicit author deferrals quoted verbatim from the PR or linked issue.
- Acceptance criteria when stated.

Refuse before mutation if:

- GitHub authentication or PR resolution fails.
- The working tree is dirty.
- Local `HEAD` differs from the PR head.
- Any SHA, repository path, cache path, or generated manifest is malformed or escapes its root.
- Required runtime or helper capabilities are unavailable.
- The selected permission profile cannot enforce the requested review or fix mode.

Repeat the dirty-tree and expected-parent checks immediately before every write batch, not only at
startup.

### 4. Repository classifier

Produce a declarative, versioned rule mapping paths to:

- `code`
- `test`
- `cicd`
- `other`
- `excluded`, with a reason

The first implementation should ship conservative defaults and permit a repository override. A
later model-generated classifier is acceptable only if its output is validated against a strict
schema and tied to a deterministic repository-shape fingerprint.

Excluded paths must appear in the final report. A classifier failure must never become an empty
clean review.

### 5. Deterministic hunk chunker

Inputs:

- Repository root.
- Merge-base SHA and current review-head SHA.
- Classifier.
- Per-stage byte caps.
- Isolation expression.
- Clean-content ledger.
- Hash sidecars from chunks completed earlier in the same run.

Outputs:

- Frozen diff files.
- One hash sidecar per chunk.
- A manifest containing chunk ID, stage, files, whole files, lock key, bytes, and hunk count.
- Counts and reasons for ledger-filtered or non-reviewable content.

Rules:

- Never split a Git hunk.
- A hunk larger than the cap becomes an oversized chunk.
- Mark a file `wholeFiles` only when the chunk contains every remaining hunk for that file.
- Exclude already-completed work by hunk hash during a run, not merely by filename.
- Use collision-resistant IDs derived from the frozen input plus stage and round.
- Treat an empty manifest with helper errors as failure, not a clean PR.
- Recompute against the new `HEAD` after every successful commit.

### 6. Clean-content ledger

The durable ledger records only files that a completed reviewer found clean and for which its chunk
contained every remaining hunk.

A ledger key should bind at least:

- Repository identity.
- Merge-base identity.
- Relative file path.
- SHA-256 of the file's current diff against the merge base.
- Classifier version.
- Review protocol version.

The write must be locked and atomic. If a later finding conflicts with a clean mark, revoke the mark
before continuing. Any content change, merge-base change, classifier change, or review-protocol
change invalidates the corresponding entry naturally or by version.

### 7. Baseline and validation

Discover build, lint, and test commands from repository instructions and existing CI configuration,
then run the chosen baseline before edits. Store commands, exit codes, bounded output summaries, and
the origin of each command.

Validation policy:

- The workflow executes commands and reads their exit codes itself.
- Agents may interpret failures but may not assert that a command passed without workflow evidence.
- Prefer the narrowest reliable validation for a batch, followed by required project-level checks.
- Compare against a bounded baseline where comparison is meaningful.
- If the untouched repository cannot build, downgrade explicitly to the strongest meaningful check;
  never describe the result as fully validated.
- After the fixer changes files, at most one repair turn is attempted for a failed validation unless
  the configured policy says otherwise.

The native staged review work already contains useful evidence collection for commands, file
changes, exact Git snapshots, and verified fix commits. Reuse those pieces after the corresponding
review-chain commits land rather than reimplementing their security boundary in workflow code.

### 8. Reviewer orchestration

For each review wave:

1. Create fresh reviewer threads with read-only filesystem permissions.
2. Use an isolated/minimal prompt context and a strict tool allow-list.
3. Give each reviewer exactly one frozen chunk, PR intent, explicit deferrals, acceptance criteria,
   and the settled findings relevant to that chunk's files.
4. Request schema-constrained output.
5. Retain the reviewer thread for follow-up turns.
6. Ask for new candidates repeatedly until two consecutive passes produce none or the pass limit is
   reached.
7. Ask the same thread to double-check accumulated candidates against the real code.
8. Accept clean-file claims only for `wholeFiles` and only after a complete, non-vacuous result.
9. Close the reviewer thread after its result and ledger decision are durable.

Only reviewers run concurrently. If the runtime concurrency limit includes the controlling root
thread, calculate the usable reviewer count from the live limit instead of assuming five.

`detailedReview` reviewers remain read-only in the actual checkout. They may clone into a disposable
location, run experiments there, and compare base versus head. Disposable paths must be unique and
cleaned best-effort without ever cleaning the real checkout.

### 9. Finding normalization

Each finding has a stable schema including:

- Fingerprint without a line number.
- Title and concise explanation.
- Primary file, all files required by the fix, and symbol.
- Defect class, evidence, severity, confidence, and estimated fix size.
- Scope label: `in`, `deferred`, or `out`.
- Deferral reason.
- Release-blocker flag and concrete consequence.

Normalize exact fingerprints first, then conservatively merge near-duplicates only when file and
symbol match and descriptions substantially overlap. Preserve alternate fingerprints and the
highest severity/blocker status.

Keep these outcomes distinct:

- Verified finding in this PR's scope.
- Explicitly author-deferred work.
- Verified pre-existing/out-of-scope defect.
- Set aside as apparently out of scope but not investigated.
- Rejected after re-reading because it is not a bug.

Unknown scope resolves to in-scope. A PR that makes an old defect reachable or worse owns that
finding even when the defective line was not edited.

### 10. Backpressure and budgets

Review chunks in waves. After the configured outstanding-finding threshold:

1. Stop launching new waves.
2. Drain the current read-only wave.
3. Deduplicate and persist its findings.
4. Run serial fix batches.
5. Re-chunk the remaining hunks against the new `HEAD`.
6. Resume the same stage before advancing to the next stage.

Check agent, token, time, and cancellation budgets before every wave and every fix batch. If a stage
will not fit, widen chunk caps a bounded number of times. If it still will not fit, review the
largest chunks that fit and list every omitted chunk under `NOT REVIEWED`.

### 11. Fix orchestration

Fixes are serialized and use a fresh fixer thread per bounded batch. A retained fixer thread may
receive follow-up turns within that batch.

Before the fixer starts:

- Snapshot `HEAD`, the index, tracked changes, and untracked paths.
- Confirm the tree matches the expected state.
- Compute the natural file grant from the findings.
- Create a transaction record.

The fixer:

- Reads the real code before editing.
- Rejects incorrect findings rather than making cosmetic changes to justify them.
- Makes the smallest change that resolves each accepted finding.
- May touch another file only when the fix genuinely requires it; that expansion is reported.
- Handles small, clearly in-scope follow-ups in the same batch.
- Does not commit, push, reset, clean, stash, checkout, rebase, or amend.

After the fixer stops:

1. Measure changed paths from Git and recorded file-change events.
2. Reject or flag unexpected scope expansion.
3. Run deterministic validation.
4. On failure, send one repair follow-up to the same fixer and validate again.
5. If validation still fails, persist the attempted findings and leave edits in place for a human.
6. If validation succeeds, stage only explicit validated paths.
7. Create one local commit for the batch.
8. Verify that `HEAD` moved from the expected parent, the commit contains all and only accepted
   changes, and no tracked edits remain.
9. Persist the commit and only then mark findings fixed.

No result reported by an agent can substitute for these Git checks.

### 12. Recovery

Persist a versioned `RunState` similar to:

```text
run identity and schema version
repository, PR, merge base, start branch, start SHA, current expected HEAD
mode, action, permissions, stages, caps, and budgets
baseline commands and results
classifier and ledger versions
current phase, stage, round, wave, and batch
all frozen chunks and completed hunk hashes
reviewer/fixer thread IDs and last completed turns
normalized findings and their disposition
commits produced by the run
known uncommitted paths
follow-ups, violations, warnings, and stop reason
```

On resume:

1. Lock the run.
2. Validate the schema version and all paths.
3. Re-read Git state and compare it with the last durable checkpoint.
4. Reattach to a live workflow run or resume persisted agent threads when safe.
5. Never replay a committed batch.
6. Never discard uncommitted edits automatically.
7. Re-chunk from the current verified `HEAD` and completed hunk hashes.
8. Refuse and explain any ambiguous divergence.

### 13. Reporting

The report renderer consumes only durable workflow state. It does not ask another model to decide
what happened.

Every count must reconcile with itemized sections. A batch is listed as fixed only when its commit
was verified. Vacuous reviewer output is `NOT REVIEWED`, not clean. Failed or malformed outputs must
retain the findings they were attempting to address.

The report should include a concise per-stage log, total agent/turn/token use, mode, review depth,
baseline quality, ledger skips, commit SHAs, and the exact state of the working tree.

## Runtime foundation work

The first-class workflow must not depend on the current compatibility runner. Restore the richer
runtime using the historical implementation only as design archaeology; do not blindly cherry-pick
it across the current app-server and protocol revisions.

### App-server lifecycle

Add v2 workflow run APIs:

- `workflowRun/start`
- `workflowRun/read`
- `workflowRun/wait`
- `workflowRun/cancel`

Add notifications:

- `workflowRun/progress`
- `workflowRun/status`
- `workflowRun/reportToUserMarkdown`
- `workflowRun/completed`
- `workflowRun/failed`

The manager must own cancellation, terminal-state idempotence, origin-thread association, approval
delegation, bounded stored output, and cleanup. Update `app-server/README.md`, regenerate stable and
experimental schemas as appropriate, and test the public JSON-RPC behavior.

### TypeScript workflow SDK

Restore a tracked `@openai/codex-sdk/workflow` export containing:

- `defineWorkflow` and `runWorkflow`.
- Typed `WorkflowContext`.
- `createAgent`, `resumeAgent`, and agent enumeration.
- `AgentHandle.run`, `runStreamed`, `sendInput`, `wait`, `fork`, and `close`.
- Per-turn output schemas.
- Per-agent model, cwd, sandbox, approval, prompt-context, and tool policies.
- Progress, structured status, markdown handoff, and structured result APIs.
- Dynamic workflow tools where needed.

Define `sendInput` explicitly as a new turn on the same thread. If same-turn steering is required,
expose a separate `steer` method backed by existing `turn/steer`; do not overload the terms.

### CLI and TUI

- Route workflow execution through app-server workflow runs.
- Remove the minimal `WORKFLOW_TUI_RUNNER` context once the new path is live.
- Make CLI aliases, explicit runs, and TUI commands use the same normalized input and runtime.
- Render structured workflow/thread status without injecting status prose into model context.
- Persist the final markdown handoff into the origin thread exactly once.
- Preserve normal approvals, sandboxing, and cancellation.
- Fail closed when a workflow requests a capability the runtime cannot supply.

### Workflow validation

Replace the current file-existence validation with checks for:

- Layout and ignored runtime state.
- TypeScript contract extraction and loadability.
- Local dependency resolution.
- Workflow output schema correctness.
- Completion hook correctness.
- Declared validation commands.
- Required positive, negative, autocomplete, load, and recovery coverage markers.
- Contract smoke execution.
- Generated client compatibility.
- Documentation consistency.

## Delivery sequence

Keep each change independently testable and reviewable. Complex stages should stay under 500 changed
lines where practical; mechanical schema regeneration should be isolated from behavioral changes.

### Phase 0: Freeze contracts and close documentation drift

Deliverables:

- An architecture decision record for workflow ownership and transport.
- Tests proving the current runner's actual capability boundary.
- Updated workflow documentation that distinguishes current and target APIs until migration ends.
- A compatibility decision for existing Bun-script workflows.

Exit criteria:

- No tracked documentation claims that current workflow code receives APIs it does not receive.
- Existing workflows have an explicit migration or compatibility path.

### Phase 1: App-server workflow run lifecycle

Deliverables:

- Protocol types, run manager, cancellation, status storage, and notifications.
- App-server public API documentation and generated schemas.
- Unit and integration tests for start/read/wait/cancel and terminal races.

Exit criteria:

- A no-agent fixture workflow runs asynchronously and is observable and cancelable from app-server.

### Phase 2: Persistent workflow-owned agents

Deliverables:

- Tracked TypeScript workflow SDK export.
- Agent creation, resume, repeated turns, structured output, wait, and close.
- Prompt-context and tool-policy enforcement.
- Approval and sandbox propagation.

Exit criteria:

- An integration test creates one agent, receives structured output, sends a follow-up turn to the
  same thread, and verifies retained context.
- A read-only agent cannot modify the checkout.

### Phase 3: CLI/TUI convergence

Deliverables:

- CLI and TUI both start app-server-owned workflow runs.
- Live structured status and final markdown handoff.
- Compatibility adapter or migration error for legacy scripts.

Exit criteria:

- The same fixture workflow produces equivalent structured output through CLI, TUI, and direct
  app-server invocation.

### Phase 4: Deterministic review foundation

Deliverables:

- Workflow package scaffold and typed input/output contracts.
- PR scope and safety gate.
- Classifier, hunk chunker, manifest, hash sidecars, and clean-content ledger.
- Run-state persistence and recovery parser.
- Baseline discovery and execution.

Exit criteria:

- Fixture diffs produce byte-identical manifests across repeated runs.
- Ledger invalidation and all unsafe-path cases are covered without invoking a model.

### Phase 5: Review-only workflow

Deliverables:

- Single, parallel, full, and auto modes.
- Read-only reviewer pool with follow-up turns and structured findings.
- Deduplication, scope handling, ledger marking/revocation, and backpressure.
- Whole-PR confirmation after an empty parallel pass.
- Report-only final handoff.

Exit criteria:

- Every reviewable hunk is either in a completed chunk or itemized under `NOT REVIEWED`.
- No review-only test can create a repository change.

### Phase 6: Fix, validation, and local commits

Deliverables:

- Serial fix batches.
- Transaction snapshots and mutation-scope checks.
- Workflow-owned validation with one bounded repair turn.
- Exact-path staging, commit creation, and commit verification.
- Re-chunking after each commit.

Exit criteria:

- A successful batch creates exactly one verified local commit.
- A failed validation creates no commit and leaves named edits for human inspection.
- No test path can push, reset, clean, stash, rebase, or amend.

### Phase 7: Recovery, budgets, and detailed review

Deliverables:

- Resume after process loss at every durable boundary.
- Agent, token, time, and cancellation budgets.
- Cap widening, truncation, and explicit unreviewed reporting.
- Disposable-clone experiments for detailed review.
- Follow-up reconciliation.

Exit criteria:

- Fault-injection tests can stop and resume each phase without duplicate commits or lost hunks.

### Phase 8: Skill, packaging, and rollout

Deliverables:

- `/review-and-fix-pr` skill and reference documentation.
- Ownership-aware action choice.
- Workflow validation configuration and local dependencies.
- Feature flag, telemetry, and kill switch.
- Evaluation corpus for small, large, clean, findings-heavy, and adversarial PRs.

Exit criteria:

- The skill invokes only the registered workflow contract.
- Report-only is safe by default.
- Fix and commit remain explicit opt-ins.

## Suggested pull-request breakdown

1. Correct current workflow docs and add capability-boundary tests.
2. Introduce app-server workflow-run protocol types and state model.
3. Implement workflow run manager and cancellation.
4. Restore TypeScript workflow transport and package export.
5. Add persistent agent handles and structured repeated turns.
6. Add prompt/tool policy and approval propagation.
7. Route CLI workflow execution through app-server.
8. Route TUI workflow execution and status through app-server.
9. Scaffold review-and-fix contracts and deterministic state machine.
10. Add scope, classifier, chunker, and ledger.
11. Add review-only single mode.
12. Add parallel review, backpressure, and whole-PR confirmation.
13. Add finding normalization and reporting.
14. Add serial fixes and deterministic validation.
15. Add exact Git commit transactions and re-chunking.
16. Add recovery, budgets, and detailed-review isolation.
17. Add the skill adapter, packaging, telemetry, and evaluations.

Schema-only generated changes may be mechanically large, but behavioral changes should not be
hidden inside those diffs.

## Test strategy

### Deterministic unit tests

- PR identifiers, full SHAs, repository slugs, relative paths, and cache path validation.
- Classification precedence and exclusions.
- Added, modified, deleted, renamed, binary, submodule, and oversized-hunk diffs.
- Chunk packing, isolation keys, stable IDs, and hash exclusion.
- Whole-file eligibility.
- Ledger mark, check, revoke, locking, atomic writes, version invalidation, and corruption handling.
- Finding fingerprints, exact deduplication, conservative near-duplicate merging, and severity ordering.
- State-machine legal and illegal transitions.
- Budget fitting, cap widening, truncation ordering, and backpressure.
- Report count reconciliation and terminal-state wording.

### App-server integration tests

- Workflow start/read/wait/cancel.
- Progress, status, markdown handoff, completion, and failure notifications.
- Origin-thread persistence and exactly-once final output.
- Persistent agent follow-up retains thread context.
- Fresh reviewer threads do not inherit another reviewer's private context.
- Output-schema success and malformed-output recovery.
- Approval delegation and refusal.
- Cancellation during reviewer, fixer, validation, and commit finalization.
- Remote app-server/exec-server operating-system combinations using auto environment builders.

### Review workflow fixture tests

- Small PR selects single mode.
- Large PR schedules parallel chunks by stage.
- Dirty tree and wrong PR head are refused.
- No GitHub authentication or missing PR is refused.
- Empty classifier result is distinguished from chunker failure.
- Clean run populates the ledger; identical rerun schedules nothing.
- Changing a previously clean file schedules it again.
- One file split across chunks cannot be marked clean by a partial reviewer.
- Conflicting clean mark is revoked when another reviewer reports a finding.
- Reviewer returns nothing, malformed JSON, vacuous output, or aborts.
- Duplicate findings from multiple chunks merge without losing severity or blocker state.
- Outstanding findings pause later review waves.
- A successful fix validates, commits, and causes remaining hunks to be re-chunked.
- Failed validation receives one repair turn, then leaves uncommitted edits and stops.
- Agent or token cap lists every unreviewed chunk.
- Empty parallel pass triggers a whole-PR confirmation.
- Detailed review writes only to its disposable checkout.
- Out-of-scope, deferred, rejected, still-present, and fixed sections remain distinct.
- Resume after every persisted transition does not duplicate agents, fixes, or commits.

### TUI and CLI tests

- Workflow command discovery and argument completion.
- Mode/action selection and explicit write authorization.
- Multi-thread live status snapshots.
- Cancellation and restart UX.
- Final report snapshots, including release blockers and uncommitted-edit warnings.
- Equivalent normalized input and output across CLI and TUI.

### Live evaluations

Use a bounded corpus containing:

- Cross-file contract bugs.
- Caller-side panics requiring detailed review.
- Test changes that pass with the production change reverted.
- Pre-existing failures.
- Findings duplicated across chunk boundaries.
- Large newly added files emitted as one Git hunk.
- A clean PR where whole-PR confirmation should also remain clean.

Record recall, false-positive rate, hunk completion, agents, turns, tokens, wall time, validation
accuracy, and recovery success. Keep model quality metrics separate from deterministic workflow
correctness.

## Validation commands by change area

- Protocol changes: regenerate app-server schemas and run
  `just test -p codex-app-server-protocol`.
- App-server changes: run the focused app-server integration tests.
- Core agent/runtime changes: run the specific `codex-core` integration tests, then request approval
  before the complete `just test` suite as required by repository policy.
- TypeScript SDK changes: run its format, lint, build, and tests.
- Workflow package changes: run `codex workflow validate review-and-fix-pr` after validation is
  upgraded, plus the package's own tests and contract smoke command.
- TUI-visible changes: run `just test -p codex-tui`, inspect pending snapshots, and accept only the
  intended ones.
- Any source change: run `just fmt` last; for large Rust changes run scoped `just fix -p <crate>`
  before the final `just fmt`, without rerunning tests afterward.

The currently documented `just workflow-dev-check` and workflow self-e2e commands are not present
in this checkout's root `justfile`; adding verified recipes is part of the runtime-foundation work.

## Rollout and observability

1. Keep the runtime and workflow behind separate feature flags.
2. Enable report-only mode first.
3. Add fix-without-commit next.
4. Enable local commits only after Git transaction and recovery tests are green on Linux, macOS,
   Windows, and remote executor combinations.
5. Never enable push/comment/merge as an implicit extension of this workflow.
6. Emit bounded telemetry for phase duration, reviewer/fixer turns, chunks completed, ledger hits,
   findings, validation outcomes, stop reasons, and recovery attempts. Do not emit source text,
   prompts, findings, paths, PR bodies, or secrets.
7. Provide a kill switch that disables new workflow runs while leaving read/status/recovery access
   available for existing state.

## Acceptance criteria

The feature is complete only when all of these are true:

- The workflow, not a parent model, owns every phase transition.
- The workflow can retain and follow up with an agent thread.
- Reviewers are mechanically read-only and writers are mechanically serialized.
- Every classifier-approved hunk is completed or explicitly listed as not reviewed.
- No file is ledgered clean from a partial or failed review.
- Validation results come from observed command completion and exit codes.
- A batch is counted fixed only after a verified local commit, when commit mode is selected.
- Failed batches are never silently reset or omitted from the report.
- Recovery cannot duplicate a commit or forget an unreviewed hunk.
- CLI, TUI, and app-server use the same workflow contract and runtime.
- The skill is a thin adapter and does not carry a second orchestration implementation.
- The workflow never pushes, comments, merges, resets, cleans, stashes, rebases, or amends.

## Missing from the current checkout

### P0: blockers for a first-class workflow

1. An agent-capable `WorkflowContext` in the active runner. The current TUI context exposes only a
   progress stub and cwd aliases.
2. A tracked `@openai/codex-sdk/workflow` source and package export. Ignored generated `dist` files
   are stale build artifacts, not an implementation contract.
3. Workflow-owned persistent agent handles with create, resume, repeated run, structured output,
   wait, close, and failure details.
4. App-server-owned `workflowRun/start`, `read`, `wait`, and `cancel` lifecycle APIs in current HEAD.
5. Workflow progress, structured status, final markdown, completion, and failure notifications wired
   end to end.
6. CLI and TUI execution through the same app-server workflow manager. Both currently use
   compatibility Bun launchers.
7. Workflow-level prompt-context, tool-policy, sandbox, model, cwd, and approval propagation with
   strict failure when a requested policy cannot be honored.
8. Workflow cancellation and terminal-state race handling.
9. Durable workflow-run recovery and a supported way to reconnect persisted agent thread IDs.
10. Real workflow validation. Current validation mainly checks whether `src/workflow.ts` exists.
11. The `workflow-dev-check`, workflow self-e2e, and real-world workflow self-e2e recipes described
    by the Workflow-mode instructions.
12. A compatibility and migration policy for existing direct Bun workflows.
13. One consistent executable contract. The TUI imports `run` and `format`, while the CLI executes
    `src/workflow.ts` as a program and the current scaffolder does not generate one implementation
    that correctly satisfies both paths.
14. Interactive workflow input or a formal pre-launch consent handoff. A running compatibility
    workflow cannot currently ask the origin thread for a decision.
15. Injection of the exact modified Codex binary identity for any temporary SDK-backed bridge.
16. A large-result transport. Current recorded workflow output is capped at 40 KiB and error detail
    at 4 KiB, so full reports require artifact storage plus a bounded user handoff.
17. A stable feature-maturity policy. Workflows are feature-gated and the active implementation is
    not yet aligned with its documented contract.

### P1: missing review-and-fix capabilities

1. The `review-and-fix-pr` workflow package and skill adapter.
2. Typed workflow input, output, completion, and formatter contracts for this use case.
3. Exact PR-head preflight and ownership-aware mutation choice in the skill/workflow path.
4. A repository classifier with deterministic invalidation.
5. A whole-hunk, size-capped, stage-aware chunker with frozen diffs and hash sidecars.
6. A content-addressed clean-file ledger with atomic locking and revocation.
7. Durable run state and recovery reconciliation against real Git state.
8. Baseline build/lint/test discovery and bounded evidence capture.
9. Reviewer schemas, retained follow-up turns, double-check loop, vacuous-output detection, and
   whole-file clean eligibility.
10. Finding fingerprinting, conservative cross-chunk deduplication, scope classification, deferral,
    rejection, and release-blocker handling.
11. Review backpressure, agent/token/time budgets, cap widening, and explicit truncation reporting.
12. Serial fix batching with transaction snapshots and unexpected-path reporting.
13. Workflow-owned validation, bounded repair, exact-path staging, and verified local commits.
14. Re-chunking remaining hunks after each successful commit.
15. Whole-PR confirmation after an empty parallel pass.
16. Detailed-review disposable checkout management.
17. Follow-up reconciliation and the complete final report renderer.
18. Workflow-specific unit, integration, TUI snapshot, recovery, and live evaluation coverage.

### P2: available building blocks that are not yet connected

1. Multi-agent v2 already has persistent child threads plus `send_message` and `followup_task`, but
   these are model-facing collaboration tools rather than deterministic workflow JavaScript APIs.
2. The standard TypeScript SDK already supports repeated turns on one `Thread`, but using it inside a
   Bun workflow would make the workflow manually launch its own Codex processes rather than use the
   current workflow subsystem.
3. App-server already exposes `turn/steer`, but current workflow code has no typed agent `steer`
   wrapper; repeated-turn follow-up is sufficient for the initial design.
4. The protocol already defines pull-request review targets, double-check verification, and
   `report`/`fix`/`fixAndCommit` actions, but the complete staged review/fix chain exists only in later
   `review-chain-*` branches, not this analysis baseline.
5. Existing Git snapshot, validation, staged-review, and exact-commit work in the review-chain stack
   can eliminate duplicated security-sensitive code after it lands.
6. Historical branch `forked-rust-v0.129.0-rebased-origin-main` contains a richer workflow runtime
   and TypeScript SDK that can guide the port, but it must be reconciled with the current protocol,
   app-server, TUI, sandbox, and model-routing implementations rather than treated as ready code.
