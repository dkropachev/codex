//! Bounded-cardinality telemetry dimensions for handoff lifecycle events.

use super::HandoffDisposition;
use super::HandoffTrigger;
use codex_otel::SessionTelemetry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffTelemetryReason {
    ActiveDescendants,
    EmptyPlan,
    ExecutionSubmission,
    InvalidPlan,
    Misalignment,
    ModeChange,
    ModeUnavailable,
    OversizedPlan,
    PlanningGate,
    PlanningSubmission,
    PolicyError,
    QueuedInput,
    SafetyError,
    ServerOverloaded,
    SourceChanged,
    Submission,
    ThreadNavigation,
    ThreadStart,
    TransitionPreempted,
    TurnError,
    TurnFailed,
    TurnInterrupted,
    UnresolvedDisposition,
    WrapUpSubmission,
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

impl HandoffTelemetryReason {
    #[cfg(test)]
    pub(super) const ALL: [Self; 24] = [
        Self::ActiveDescendants,
        Self::EmptyPlan,
        Self::ExecutionSubmission,
        Self::InvalidPlan,
        Self::Misalignment,
        Self::ModeChange,
        Self::ModeUnavailable,
        Self::OversizedPlan,
        Self::PlanningGate,
        Self::PlanningSubmission,
        Self::PolicyError,
        Self::QueuedInput,
        Self::SafetyError,
        Self::ServerOverloaded,
        Self::SourceChanged,
        Self::Submission,
        Self::ThreadNavigation,
        Self::ThreadStart,
        Self::TransitionPreempted,
        Self::TurnError,
        Self::TurnFailed,
        Self::TurnInterrupted,
        Self::UnresolvedDisposition,
        Self::WrapUpSubmission,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::ActiveDescendants => "active_descendants",
            Self::EmptyPlan => "empty_plan",
            Self::ExecutionSubmission => "execution_submission",
            Self::InvalidPlan => "invalid_plan",
            Self::Misalignment => "misalignment",
            Self::ModeChange => "mode_change",
            Self::ModeUnavailable => "mode_unavailable",
            Self::OversizedPlan => "oversized_plan",
            Self::PlanningGate => "planning_gate",
            Self::PlanningSubmission => "planning_submission",
            Self::PolicyError => "policy_error",
            Self::QueuedInput => "queued_input",
            Self::SafetyError => "safety_error",
            Self::ServerOverloaded => "server_overloaded",
            Self::SourceChanged => "source_changed",
            Self::Submission => "submission",
            Self::ThreadNavigation => "thread_navigation",
            Self::ThreadStart => "thread_start",
            Self::TransitionPreempted => "transition_preempted",
            Self::TurnError => "turn_error",
            Self::TurnFailed => "turn_failed",
            Self::TurnInterrupted => "turn_interrupted",
            Self::UnresolvedDisposition => "unresolved_disposition",
            Self::WrapUpSubmission => "wrap_up_submission",
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
    Completion {
        trigger: HandoffTrigger,
        disposition: HandoffDisposition,
    },
    Cancellation {
        trigger: HandoffTrigger,
        reason: HandoffTelemetryReason,
    },
    Failure {
        trigger: HandoffTrigger,
        reason: HandoffTelemetryReason,
    },
}

impl HandoffTelemetryEvent {
    pub(crate) fn record(self, telemetry: &SessionTelemetry) {
        let (name, tags) = self.dimensions();
        telemetry.counter(name, /*inc*/ 1, &tags);
    }

    pub(super) fn dimensions(self) -> (&'static str, Vec<(&'static str, &'static str)>) {
        match self {
            Self::Trigger(trigger) => (
                "codex.tui.handoff.trigger",
                vec![("source", trigger.label())],
            ),
            Self::Disposition {
                trigger,
                disposition,
            } => (
                "codex.tui.handoff.disposition",
                vec![
                    ("source", trigger.label()),
                    ("disposition", disposition.label()),
                ],
            ),
            Self::Completion {
                trigger,
                disposition,
            } => (
                "codex.tui.handoff.completion",
                vec![
                    ("source", trigger.label()),
                    ("disposition", disposition.label()),
                ],
            ),
            Self::Cancellation { trigger, reason } => (
                "codex.tui.handoff.cancellation",
                vec![("source", trigger.label()), ("reason", reason.label())],
            ),
            Self::Failure { trigger, reason } => (
                "codex.tui.handoff.failure",
                vec![("source", trigger.label()), ("reason", reason.label())],
            ),
        }
    }
}

impl HandoffTrigger {
    const fn label(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

impl HandoffDisposition {
    const fn label(self) -> &'static str {
        match self {
            Self::Proceed => "proceed",
            Self::Ask => "ask",
            Self::Defer => "defer",
        }
    }
}
