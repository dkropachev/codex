use super::LiveScreen;
use super::wait_for_ready;
use std::time::Duration;

use anyhow::Result;
use pretty_assertions::assert_eq;
use tempfile::tempdir;
use tokio::sync::broadcast;

#[tokio::test(start_paused = true)]
async fn readiness_waits_for_session_and_complete_status_sequence() -> Result<()> {
    let log_dir = tempdir()?;
    let (tx, mut rx) = broadcast::channel(/*capacity*/ 8);
    let mut screen = LiveScreen::new_with_callbacks(
        /*rows*/ 24,
        /*cols*/ 80,
        /*scrollback_len*/ 0,
        Default::default(),
    );
    let start = tokio::time::Instant::now();
    let sender = tokio::spawn(async move {
        tx.send(b"\x1b]7501;state=idle:app=codex\x1b\\".to_vec())?;
        tokio::time::sleep(Duration::from_secs(/*secs*/ 1)).await;
        tx.send(b"\x1b]7501;state=working:app=codex\x1b\\".to_vec())?;
        tx.send(b"01900000-0000-7000-8000-000000000001".to_vec())?;
        tokio::time::sleep(Duration::from_secs(/*secs*/ 1)).await;
        tx.send(b"\x1b]7501;state=idle:app=".to_vec())?;
        tokio::time::sleep(Duration::from_secs(/*secs*/ 1)).await;
        tx.send(b"codex\x1b\\".to_vec())?;
        Ok::<(), anyhow::Error>(())
    });

    wait_for_ready(&mut rx, &mut screen, log_dir.path()).await?;
    sender.await??;
    assert_eq!(start.elapsed(), Duration::from_secs(/*secs*/ 3));
    assert_eq!(
        screen.screen().contents(),
        "01900000-0000-7000-8000-000000000001"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn startup_timeout_includes_bounded_trace_tail() -> Result<()> {
    let log_dir = tempdir()?;
    let expected_tail = format!("{}startup stalled", "x".repeat(/*n*/ 65_521));
    std::fs::write(
        log_dir.path().join("codex-tui.log"),
        format!("discarded prefix{expected_tail}"),
    )?;
    let (_tx, mut rx) = broadcast::channel(/*capacity*/ 1);
    let mut screen = LiveScreen::new_with_callbacks(
        /*rows*/ 24,
        /*cols*/ 80,
        /*scrollback_len*/ 0,
        Default::default(),
    );

    let error = wait_for_ready(&mut rx, &mut screen, log_dir.path())
        .await
        .expect_err("startup must time out without session readiness")
        .to_string();
    assert_eq!(
        error.rsplit_once("\nTUI log tail:\n").map(|(_, tail)| tail),
        Some(expected_tail.as_str())
    );
    Ok(())
}
