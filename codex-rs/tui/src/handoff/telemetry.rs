//! Bounded handoff lifecycle counters. User-authored text never becomes a tag.

use super::HandoffDisposition;
use codex_otel::SessionTelemetry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffTrigger {
    Manual,
    Automatic,
}

impl HandoffTrigger {
    const fn label(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffTelemetryDisposition {
    Proceed,
    Ask,
    Defer,
    Stay,
}

impl HandoffTelemetryDisposition {
    const fn label(self) -> &'static str {
        match self {
            Self::Proceed => "proceed",
            Self::Ask => "ask",
            Self::Defer => "defer",
            Self::Stay => "stay",
        }
    }
}

impl From<HandoffDisposition> for HandoffTelemetryDisposition {
    fn from(disposition: HandoffDisposition) -> Self {
        match disposition {
            HandoffDisposition::Proceed => Self::Proceed,
            HandoffDisposition::Ask => Self::Ask,
            HandoffDisposition::Defer => Self::Defer,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffTelemetryFailure {
    InvalidPlan,
    ModeUnavailable,
    SourceChanged,
    ThreadStart,
    TurnFailed,
}

impl HandoffTelemetryFailure {
    const fn label(self) -> &'static str {
        match self {
            Self::InvalidPlan => "invalid_plan",
            Self::ModeUnavailable => "mode_unavailable",
            Self::SourceChanged => "source_changed",
            Self::ThreadStart => "thread_start",
            Self::TurnFailed => "turn_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffTelemetryEvent {
    Trigger(HandoffTrigger),
    Disposition {
        trigger: HandoffTrigger,
        disposition: HandoffTelemetryDisposition,
    },
    Completion(HandoffTrigger),
    Cancellation(HandoffTrigger),
    Failure {
        trigger: HandoffTrigger,
        reason: HandoffTelemetryFailure,
    },
}

impl HandoffTelemetryEvent {
    pub(crate) fn record(self, telemetry: &SessionTelemetry) {
        match self {
            Self::Trigger(trigger) => telemetry.counter(
                "codex.tui.handoff.trigger",
                /*inc*/ 1,
                &[("source", trigger.label())],
            ),
            Self::Disposition {
                trigger,
                disposition,
            } => telemetry.counter(
                "codex.tui.handoff.disposition",
                /*inc*/ 1,
                &[
                    ("source", trigger.label()),
                    ("disposition", disposition.label()),
                ],
            ),
            Self::Completion(trigger) => telemetry.counter(
                "codex.tui.handoff.completion",
                /*inc*/ 1,
                &[("source", trigger.label())],
            ),
            Self::Cancellation(trigger) => telemetry.counter(
                "codex.tui.handoff.cancellation",
                /*inc*/ 1,
                &[("source", trigger.label())],
            ),
            Self::Failure { trigger, reason } => telemetry.counter(
                "codex.tui.handoff.failure",
                /*inc*/ 1,
                &[("source", trigger.label()), ("reason", reason.label())],
            ),
        }
    }
}
