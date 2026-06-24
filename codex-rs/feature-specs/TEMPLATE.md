# Feature Name

## Summary

Describe the user-facing purpose and contract for this feature.

## Behavior

Describe the behavior that users, clients, or integrations can observe. Include default behavior,
state transitions, error handling, and configuration effects when they are part of the contract.

## Entry Points

List repository-relative links to the concrete files or directories that define the main
implementation, API, TUI, CLI, or protocol context.

- [codex-rs/path/to/file.rs](../path/to/file.rs)

## Subfeatures

List named subfeatures that belong inside this feature spec. A subfeature may include its own entry
points and invariants. Do not create separate subfeature files in this framework.

### Subfeature Name

#### Entry Points

- [codex-rs/path/to/subfeature.rs](../path/to/subfeature.rs)

#### Invariants

- Describe subfeature-specific invariants.

## Invariants

List behavior that must remain true across refactors.

## Test Places

For every test place from the README catalog, add a heading with the exact catalog description,
then describe what should be tested for this feature without file references.

If the test place applies to this feature, `Test cases` must list textual behavior expectations.
Each item must end with `missing`, `missing:<stable-id>`, or a repo-relative test target in the
form `path/to/test.rs:test_name[,test_name]`. The verifier checks that target files and functions
exist and that target filenames map to the feature. Use `missing` for expected behavior that still
needs test coverage, or `missing:<stable-id>` when a kebab-case backlog ID would help track the
item across edits.

If the test place should not cover this feature, include only `Description` and `Status`. The status
must be `Not covered`, and the description must explain why that test place does not apply.

### agent-e2e (agent behavior under core integration tests)

#### Description

Describe what agent behavior should be tested for this feature, without file references.

#### Test cases

- Main user-visible behavior still needs coverage: missing:main-user-visible
- Important edge behavior is covered: codex-rs/path/to/feature_name__scenario.rs:test_name

### app-server-api (app-server API behavior)

#### Description

Explain why app-server API coverage does not apply.

#### Status

Not covered

Repeat the same heading shape for the remaining README catalog entries.

## Test Generation Notes

Describe positive, negative, edge, and regression cases that generated tests should consider.
