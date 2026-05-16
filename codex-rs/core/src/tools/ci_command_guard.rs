use crate::session::turn_context::TurnContext;

pub(crate) fn redirect_for_ci_command(_turn: &TurnContext, _command: &str) -> Option<String> {
    None
}
