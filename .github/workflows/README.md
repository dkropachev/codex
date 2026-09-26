# Workflow Strategy

The workflows in this directory are split so that pull requests get fast, review-friendly signal while `main` still gets the full cross-platform verification pass.

## Pull Requests

- Required checks run against GitHub's synthetic merge commit, not the pull
  request head alone. This includes changes already on `main` and catches
  conflicts before they reach the branch.
- `bazel.yml` is the main pre-merge verification path for Rust code.
  It runs Bazel `test` and Bazel `clippy` on the supported Bazel targets,
  including the generated Rust test binaries needed to lint inline `#[cfg(test)]`
  code.
- `rust-ci.yml` keeps the Cargo-native PR checks intentionally small:
  - `cargo fmt --check`
  - `cargo shear`
  - `argument-comment-lint` on Linux, macOS, and Windows
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes

## Post-Merge On `main`

- `bazel.yml` also runs on pushes to `main`.
  This re-verifies the merged Bazel path and helps keep the BuildBuddy caches warm.
- `rust-ci-full.yml` is the full Cargo-native verification workflow.
  It keeps the heavier checks off the PR path while still validating them after merge:
  - the full Cargo `clippy` matrix
  - the full Cargo `nextest` matrix via per-platform archive-backed shards
  - Windows ARM64 nextest archives cross-compiled on Windows x64, then replayed on native Windows ARM64 shards
  - release-profile Cargo builds
  - cross-platform `argument-comment-lint`
  - Linux remote-env tests

## Rule Of Thumb

- If a build/test/clippy check can be expressed in Bazel, prefer putting the PR-time version in `bazel.yml`.
- Keep `rust-ci.yml` fast enough that it usually does not dominate PR latency.
- Reserve `rust-ci-full.yml` for heavyweight Cargo-native coverage that Bazel does not replace yet.

## Fork Rust Releases

`fork-rust-release.yml` owns releases in `dkropachev/codex`. The sync process
must push a stable `rust-vX.Y.Z` tag only after the fork changes have been
merged to `main`; it must not create a GitHub Release. A tag push creates an
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
