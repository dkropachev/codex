//! Synchronizes PTY input with the TUI's actual task state, independent of footer layout.

use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
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
    log_dir: &Path,
) -> Result<()> {
    let mut raw = Vec::new();
    // Cold startup on busy release runners can take longer than an individual UI update.
    // Keep waiting for the session and OSC state rather than sending input to the loading UI.
    tokio::time::timeout(Duration::from_secs(/*secs*/ 60), async {
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
        // Keep trace output useful in CI without dumping an unbounded log file.
        let log_path = log_dir.join("codex-tui.log");
        let log_tail = (|| -> std::io::Result<Vec<u8>> {
            let mut file = std::fs::File::open(&log_path)?;
            let start = file.metadata()?.len().saturating_sub(/*rhs*/ 65_536);
            file.seek(SeekFrom::Start(start))?;
            let mut tail = Vec::new();
            file.take(/*limit*/ 65_536).read_to_end(&mut tail)?;
            Ok(tail)
        })()
        .map(|tail| String::from_utf8_lossy(&tail).into_owned())
        .unwrap_or_else(|err| format!("failed to read {}: {err}", log_path.display()));
        format!(
            "timed out waiting for ready TUI (idle={}): screen:\n{}\nraw:\n{}\nTUI log tail:\n{log_tail}",
            screen.callbacks().ready,
            screen.screen().contents(),
            String::from_utf8_lossy(&raw)
        )
    })?
}

#[cfg(test)]
#[path = "live_readiness_tests.rs"]
mod tests;
