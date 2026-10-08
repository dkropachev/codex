use codex_protocol::error::Result as CodexResult;
use codex_protocol::turn_input::CompactionRequest;
use codex_protocol::turn_input::StartIfIdleSubmission;
use codex_protocol::turn_input::TurnInputMode;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;

use super::CodexThread;

impl CodexThread {
    /// Starts inferred user or automatic work only when the thread is idle.
    ///
    /// Core declines the input without recording or enqueueing it when idle
    /// work cannot start.
    pub async fn start_turn_if_idle(
        &self,
        request: TurnInputRequest,
    ) -> CodexResult<StartIfIdleSubmission> {
        self.start_turn_if_idle_with_mode(request, TurnInputMode::StartIfIdle)
            .await
    }

    /// Starts a user-owned regular turn only when the thread is idle.
    ///
    /// Unlike inferred idle starts, empty user input retains ordinary user-turn
    /// semantics, including permission to start while the thread is in Plan mode.
    pub async fn start_user_turn_if_idle(
        &self,
        request: TurnInputRequest,
    ) -> CodexResult<StartIfIdleSubmission> {
        self.start_turn_if_idle_with_mode(request, TurnInputMode::StartUserIfIdle)
            .await
    }

    async fn start_turn_if_idle_with_mode(
        &self,
        request: TurnInputRequest,
        mode: TurnInputMode,
    ) -> CodexResult<StartIfIdleSubmission> {
        match self.submit_turn_input_with_mode(request, mode).await? {
            TurnInputSubmission::Started { turn_id } => {
                Ok(StartIfIdleSubmission::Started { turn_id })
            }
            TurnInputSubmission::NotSubmitted { reason } => {
                Ok(StartIfIdleSubmission::NotSubmitted { reason })
            }
            TurnInputSubmission::Steered { .. } => {
                unreachable!("start-if-idle submission cannot steer")
            }
        }
    }

    /// Starts a standalone compaction turn only when the thread is idle.
    ///
    /// Core atomically reserves the active-turn slot before constructing the
    /// compaction turn, so a competing submission cannot interrupt work that
    /// wins the race.
    pub async fn compact_if_idle(
        &self,
        request: CompactionRequest,
    ) -> CodexResult<StartIfIdleSubmission> {
        let CompactionRequest { source, trace } = request;
        self.ensure_execution_capacity_for_turn_start(self.session.services.agent_control.as_ref())
            .await?;
        self.io.submit_compact_if_idle(source, trace).await
    }
}
