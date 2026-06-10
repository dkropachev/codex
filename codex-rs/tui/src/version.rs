/// The current Codex CLI version as embedded at compile time.
#[cfg(not(test))]
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Keep snapshot tests stable across Cargo and Bazel, which provide different
/// package-version metadata for workspace crates.
#[cfg(test)]
pub const CODEX_CLI_VERSION: &str = "0.0.0";
