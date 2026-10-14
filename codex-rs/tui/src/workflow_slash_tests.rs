use super::*;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use codex_app_server_client::AppServerClient;
use pretty_assertions::assert_eq;

#[test]
fn install_arguments_preserve_windows_and_quoted_host_paths() {
    assert_eq!(
        install_arguments(r"install C:\workflows\review").expect("Windows path"),
        (
            r"C:\workflows\review".to_string(),
            WorkflowUpdatePolicy::Prompt
        )
    );
    assert_eq!(
        install_arguments(r#"install "./my workflows/review" --policy manual"#)
            .expect("quoted path"),
        (
            "./my workflows/review".to_string(),
            WorkflowUpdatePolicy::Manual,
        )
    );
}

#[tokio::test]
async fn check_updates_command_uses_app_server_rpc() -> color_eyre::Result<()> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(home.path().to_path_buf()),
            ..ConfigOverrides::default()
        })
        .build()
        .await?;
    let app_server =
        AppServerClient::InProcess(crate::tests::start_test_embedded_app_server(config).await?);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_command(
        app_server.request_handle(),
        "check-updates".to_string(),
        home.path().to_path_buf(),
        AppEventSender::new(tx),
    );

    let event = tokio::time::timeout(Duration::from_secs(/*secs*/ 15), rx.recv())
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("workflow action channel closed"))?;
    let AppEvent::WorkflowManagementActionFinished { result, .. } = event else {
        panic!("expected workflow scan result: {event:?}");
    };
    assert!(
        result
            .expect("scan succeeds")
            .contains("Workflow update scan")
    );
    Ok(())
}
