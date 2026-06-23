use super::StatusAccountDisplay;
use super::StatusAccountPoolMemberDisplay;
use super::new_status_output;
use crate::history_cell::HistoryCell;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;
use chrono::TimeZone;
use codex_config::LoaderOverrides;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use insta::assert_snapshot;
use ratatui::prelude::Line;
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

#[tokio::test]
async fn status_snapshot_shows_chatgpt_pool_active_member() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.cwd = test_path_buf("/workspace/tests").abs();

    let account_display = StatusAccountDisplay::ChatGptPool {
        pool_id: "codex-pro".to_string(),
        active_member: Some(StatusAccountPoolMemberDisplay {
            id: "work-pro".to_string(),
            email: Some("work@example.com".to_string()),
            plan: Some("Pro".to_string()),
        }),
        member_count: 2,
        unavailable_count: 0,
    };
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");

    let model_slug = crate::legacy_core::test_support::get_model_offline(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        Some(&account_display),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 100));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_chatgpt_pool_unavailable_members() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.cwd = test_path_buf("/workspace/tests").abs();

    let account_display = StatusAccountDisplay::ChatGptPool {
        pool_id: "codex-pro".to_string(),
        active_member: None,
        member_count: 2,
        unavailable_count: 2,
    };
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");

    let model_slug = crate::legacy_core::test_support::get_model_offline(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        Some(&account_display),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 100));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_chatgpt_pool_without_active_member() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.cwd = test_path_buf("/workspace/tests").abs();

    let account_display = StatusAccountDisplay::ChatGptPool {
        pool_id: "codex-pro".to_string(),
        active_member: None,
        member_count: 2,
        unavailable_count: 0,
    };
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");

    let model_slug = crate::legacy_core::test_support::get_model_offline(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        Some(&account_display),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 100));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

fn app_server_workspace_write_profile(network_enabled: bool) -> PermissionProfile {
    PermissionProfile::Managed {
        network: if network_enabled {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        },
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::ProjectRoots { subpath: None },
                    },
                    access: FileSystemAccessMode::Write,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::SlashTmp,
                    },
                    access: FileSystemAccessMode::Write,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Tmpdir,
                    },
                    access: FileSystemAccessMode::Write,
                },
            ],
            glob_scan_max_depth: None,
        },
    }
}

async fn test_config(temp_home: &TempDir) -> Config {
    let mut config = ConfigBuilder::default()
        .codex_home(temp_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .expect("load config");
    config.approvals_reviewer = ApprovalsReviewer::User;
    config
        .permissions
        .set_permission_profile(app_server_workspace_write_profile(
            /*network_enabled*/ true,
        ))
        .expect("set permission profile");
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
    config
}

fn token_info_for(model_slug: &str, config: &Config, usage: &TokenUsage) -> TokenUsageInfo {
    let context_window =
        crate::legacy_core::test_support::construct_model_info_offline(model_slug, config)
            .context_window;
    TokenUsageInfo {
        total_token_usage: usage.clone(),
        last_token_usage: usage.clone(),
        model_context_window: context_window,
    }
}

fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn sanitize_directory(lines: Vec<String>) -> Vec<String> {
    let frame_width = lines
        .iter()
        .find(|line| line.starts_with('╭'))
        .map(|line| UnicodeWidthStr::width(line.as_str()));
    lines
        .into_iter()
        .map(|mut line| {
            const SNAPSHOT_CODEX_VERSION: &str = "v0.0.0";
            const TITLE_PREFIX: &str = "OpenAI Codex (";

            if let Some(title_pos) = line.find(TITLE_PREFIX)
                && let Some(version_end_offset) = line[title_pos + TITLE_PREFIX.len()..].find(')')
            {
                let version_start = title_pos + TITLE_PREFIX.len();
                let version_end = version_start + version_end_offset;
                let original_len = version_end - version_start;
                line.replace_range(version_start..version_end, SNAPSHOT_CODEX_VERSION);

                if original_len > SNAPSHOT_CODEX_VERSION.len()
                    && let Some(pipe_idx) = line.rfind('│')
                {
                    line.insert_str(
                        pipe_idx,
                        &" ".repeat(original_len - SNAPSHOT_CODEX_VERSION.len()),
                    );
                }
            }

            if let (Some(frame_width), Some(dir_pos), Some(pipe_idx)) =
                (frame_width, line.find("Directory: "), line.rfind('│'))
            {
                let prefix = &line[..dir_pos + "Directory: ".len()];
                let suffix = &line[pipe_idx..];
                let replacement = "[[workspace]]";
                let content_width = frame_width.saturating_sub(
                    UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(suffix),
                );
                let mut rebuilt = prefix.to_string();
                rebuilt.push_str(replacement);
                let replacement_width = UnicodeWidthStr::width(replacement);
                if content_width > replacement_width {
                    rebuilt.push_str(&" ".repeat(content_width - replacement_width));
                }
                rebuilt.push_str(suffix);
                rebuilt
            } else {
                line
            }
        })
        .collect()
}
