use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use codex_protocol::ThreadId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexConfigHistoryBundle {
    pub(crate) path: PathBuf,
    pub(crate) config_path: PathBuf,
}

pub(crate) fn create_bundle(
    codex_home: &Path,
    thread_id: Option<ThreadId>,
    approved_plan: &str,
    conversation: &str,
) -> io::Result<CodexConfigHistoryBundle> {
    let config_path = codex_home.join("config.toml");
    let bundle_path = codex_home
        .join("config-history")
        .join(bundle_dir_name(thread_id));

    fs::create_dir_all(bundle_path.join("before"))?;
    fs::create_dir_all(bundle_path.join("after"))?;

    let before_config_path = bundle_path.join("before/config.toml");
    match fs::read(&config_path) {
        Ok(contents) => fs::write(&before_config_path, contents)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::write(bundle_path.join("before/config.toml.missing"), "config.toml was missing\n")?;
        }
        Err(err) => return Err(err),
    }

    fs::write(bundle_path.join("approved-plan.md"), approved_plan)?;
    fs::write(bundle_path.join("conversation.md"), conversation)?;
    fs::write(
        bundle_path.join("summary.md"),
        summary_template(&config_path, &bundle_path),
    )?;
    fs::write(
        bundle_path.join("rollback.md"),
        rollback_template(&config_path, &bundle_path),
    )?;

    Ok(CodexConfigHistoryBundle {
        path: bundle_path,
        config_path,
    })
}

pub(crate) fn finalize_bundle(
    bundle: &CodexConfigHistoryBundle,
    final_assistant_result: &str,
) -> io::Result<()> {
    let after_config_path = bundle.path.join("after/config.toml");
    match fs::read(&bundle.config_path) {
        Ok(contents) => fs::write(&after_config_path, contents)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::write(bundle.path.join("after/config.toml.missing"), "config.toml is missing\n")?;
        }
        Err(err) => return Err(err),
    }

    fs::write(bundle.path.join("config.diff"), config_diff(bundle)?)?;

    if !final_assistant_result.trim().is_empty() {
        let mut conversation = fs::read_to_string(bundle.path.join("conversation.md"))?;
        conversation.push_str("\n\n## Apply Turn Result\n\n");
        conversation.push_str(final_assistant_result.trim());
        conversation.push('\n');
        fs::write(bundle.path.join("conversation.md"), conversation)?;
    }

    Ok(())
}

fn bundle_dir_name(thread_id: Option<ThreadId>) -> String {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let thread_id = thread_id
        .map(|id| sanitize_path_component(&id.to_string()))
        .unwrap_or_else(|| "no-thread".to_string());
    format!("{timestamp}-{thread_id}")
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn summary_template(config_path: &Path, bundle_path: &Path) -> String {
    format!(
        "# Codex Config Change Summary\n\nConfig path: `{}`\nHistory bundle: `{}`\nRollback source: `{}`\n\nAfter applying, replace this template with a concise summary of the config changes, validation performed, and any follow-up needed.\n",
        config_path.display(),
        bundle_path.display(),
        bundle_path.join("before").display()
    )
}

fn rollback_template(config_path: &Path, bundle_path: &Path) -> String {
    let before_config = bundle_path.join("before/config.toml");
    format!(
        "# Codex Config Rollback\n\nConfig path: `{}`\nBefore snapshot: `{}`\nMissing marker: `{}`\n\nIf `before/config.toml` exists, restore it to the config path. If `before/config.toml.missing` exists instead, remove the config path to restore the missing-file state. Keep this bundle intact for audit history.\n",
        config_path.display(),
        before_config.display(),
        bundle_path.join("before/config.toml.missing").display()
    )
}

fn config_diff(bundle: &CodexConfigHistoryBundle) -> io::Result<String> {
    let before = read_snapshot_text(
        &bundle.path.join("before/config.toml"),
        &bundle.path.join("before/config.toml.missing"),
    )?;
    let after = read_snapshot_text(
        &bundle.path.join("after/config.toml"),
        &bundle.path.join("after/config.toml.missing"),
    )?;
    Ok(diffy::create_patch(&before, &after).to_string())
}

fn read_snapshot_text(config_path: &Path, missing_marker: &Path) -> io::Result<String> {
    match fs::read_to_string(config_path) {
        Ok(contents) => Ok(contents),
        Err(err) if err.kind() == io::ErrorKind::NotFound && missing_marker.exists() => {
            Ok(String::new())
        }
        Err(err) => Err(err),
    }
}
