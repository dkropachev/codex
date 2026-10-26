//! Synchronizes PTY input with the TUI's actual task state, independent of footer layout.

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::ThreadId;
use tokio::sync::broadcast;

#[derive(Default)]
pub(super) struct ProgramStatus {
    ready: bool,
}

impl vt100::Callbacks for ProgramStatus {
    fn unhandled_osc(&mut self, _screen: &mut vt100::Screen, params: &[&[u8]]) {
        if let [b"7501", state] = params {
            self.ready = *state == b"state=idle:app=codex"
                || *state == b"state=done:app=codex"
                || *state == b"state=error:app=codex";
        }
    }
}

pub(super) type LiveScreen = vt100::Parser<ProgramStatus>;

pub(super) async fn wait_for_ready(
    output_rx: &mut broadcast::Receiver<Vec<u8>>,
    screen: &mut LiveScreen,
) -> Result<()> {
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        while !screen.callbacks().ready
            || !screen
                .screen()
                .contents()
                .split_whitespace()
                .any(|candidate| ThreadId::from_string(candidate).is_ok())
        {
            let chunk = output_rx.recv().await.context("waiting for ready TUI")?;
            screen.process(&chunk);
            raw.extend_from_slice(&chunk);
            // The parser preserves split escape sequences; bound failure diagnostics.
            if raw.len() > 65_536 {
                raw.drain(..raw.len() - 65_536);
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .with_context(|| {
        format!(
            "timed out waiting for ready TUI; screen:\n{}\nraw:\n{}",
            screen.screen().contents(),
            String::from_utf8_lossy(&raw)
        )
    })?
}
