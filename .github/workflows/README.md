# Workflow Strategy

Pull requests and `main` run Linux x86_64 and ARM64 Bazel tests plus fast
checks. Stable release tags run the broader platform checks before publication.

## Pull Requests

- Required checks run against GitHub's synthetic merge commit, not the pull
  request head alone. This includes changes already on `main` and catches
  conflicts before they reach the branch.
- `blocking-ci.yml` requires the changed-blob policy, `rust-ci.yml`, and the
  Linux x86_64 and ARM64 Bazel suites covering Core integration tests and TUI,
  protocol, and app-server-protocol unit tests.
- `rust-ci.yml` runs the fast Rust checks:
  - `cargo fmt --check`
  - `cargo shear`
  - `argument-comment-lint` on Linux
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes

### Optional pre-release CI

Add the `pre-release-ci` label to a pull request to run the same validation
that gates stable release tags before packaging. The workflow runs on the PR's
synthetic merge commit and repeats when the labeled PR receives new commits.
Adding the label also starts a run immediately. Removing the label stops future
full-suite runs; the regular required PR checks still run.

This includes full Bazel `//...` tests on Linux x86_64 and ARM64 (GNU and musl)
and macOS x86_64 and ARM64, Clippy and release-build checks, native Windows
x86_64 and ARM64 sandbox/protocol smoke tests, cargo-deny, codespell,
repository checks, and SDK tests. `Pre-release CI results` fails if any of
those checks fail. This opt-in workflow validates a PR but does not build or
publish release packages.

## Stable Release Tags

- `fork-rust-release.yml` runs full Bazel `//...` tests on Linux x86_64 and
  ARM64 (GNU and musl) and macOS x86_64 and ARM64, plus Clippy and release-build
  checks, Windows x86_64 and ARM64 sandbox/protocol tests, cargo-deny, codespell,
  repository checks, and SDK tests. These must pass before the unpublished
  release draft is created.
- The Windows hosted-runner suite omits sandbox subprocess tests that need a
  separately provisioned sandbox account and PowerShell module installation.
- Manual package dry-runs and historical backfills retain their package
  validation without running release CI against the default branch.

## Fork Rust Releases

`fork-rust-release.yml` owns releases in `dkropachev/codex`. Push a stable
`rust-vX.Y.Z` tag only after the changes have been merged to `main`; do not
create a GitHub Release first. A tag push creates an
unpublished draft, builds and verifies the four supported Unix packages, and
publishes the draft as the latest release only after every package succeeds.
The previous out-of-repository source-only release publisher must therefore be
disabled before the first managed tag is pushed.

If that publisher already created an empty source-only release, remove that
release and recreate its tag at the final merged fork commit before allowing
the fork workflow to run. Do not use `backfill` to convert it: managed releases
must pass through the unpublished-draft path so no partial release is public.

Manual dispatch has two modes:

- `dry-run` builds and smoke-tests all four targets without creating or changing
  a release. Use `rust-v0.149.1` for the initial rehearsal.
- `backfill` adds exact package archives and a checksum manifest to an existing
  published release without uploading an installer or changing its latest
  status. Run the historical tags oldest first: `rust-v0.144.1` through
  `rust-v0.144.6`, then `rust-v0.145.0`, `rust-v0.146.0`, `rust-v0.146.1`,
  `rust-v0.147.0`, `rust-v0.148.0`, `rust-v0.149.0`, and `rust-v0.149.1`.

Existing assets are reused only when GitHub's recorded digest matches the
locally built bytes. A mismatch fails by default; `replace_existing` is an
explicit recovery option. Failed normal releases remain unpublished drafts.
