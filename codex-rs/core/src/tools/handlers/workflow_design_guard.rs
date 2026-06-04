use std::fs;
use std::path::PathBuf;

use crate::function_tool::FunctionCallError;
use crate::session::turn_context::TurnContext;
use crate::tools::tool_policy::WorkflowDesignPolicyRole as DesignGuardRole;
use codex_utils_absolute_path::AbsolutePathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DesignMdSnapshot {
    pub(crate) path: PathBuf,
    pub(crate) original: Option<Vec<u8>>,
}

pub(crate) fn workflow_design_guard_role(turn: &TurnContext) -> Option<DesignGuardRole> {
    turn.tool_policy.workflow_design().map(|policy| policy.role)
}

pub(crate) fn protected_design_md_path(turn: &TurnContext) -> Option<PathBuf> {
    turn.tool_policy
        .workflow_design()
        .and_then(|policy| policy.design_md_path.clone())
        .map(AbsolutePathBuf::into_path_buf)
}

pub(crate) fn reject_if_design_md_write_forbidden(
    turn: &TurnContext,
    candidate_paths: impl IntoIterator<Item = PathBuf>,
) -> Result<(), FunctionCallError> {
    let Some(role) = workflow_design_guard_role(turn) else {
        return Ok(());
    };
    if role == DesignGuardRole::Architect {
        return Ok(());
    }

    let Some(protected_path) = protected_design_md_path(turn) else {
        return Ok(());
    };
    if candidate_paths
        .into_iter()
        .any(|candidate| candidate == protected_path)
    {
        return Err(FunctionCallError::RespondToModel(
            forbidden_design_md_message(role),
        ));
    }
    Ok(())
}

pub(crate) fn snapshot_design_md(turn: &TurnContext) -> Option<DesignMdSnapshot> {
    let role = workflow_design_guard_role(turn)?;
    if role == DesignGuardRole::Architect {
        return None;
    }
    let path = protected_design_md_path(turn)?;
    let original = fs::read(&path).ok();
    Some(DesignMdSnapshot { path, original })
}

pub(crate) fn rollback_design_md_if_modified(
    turn: &TurnContext,
    snapshot: Option<&DesignMdSnapshot>,
) -> Result<(), FunctionCallError> {
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    let Some(role) = workflow_design_guard_role(turn) else {
        return Ok(());
    };
    if role == DesignGuardRole::Architect {
        return Ok(());
    }

    let current = fs::read(&snapshot.path).ok();
    if current == snapshot.original {
        return Ok(());
    }

    restore_snapshot(snapshot).map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "{} Rollback failed for `{}`: {err}",
            forbidden_design_md_message(role),
            snapshot.path.display()
        ))
    })?;
    Err(FunctionCallError::RespondToModel(
        forbidden_design_md_message(role),
    ))
}

pub(crate) fn forbidden_design_md_message(role: DesignGuardRole) -> String {
    match role {
        DesignGuardRole::Architect => "workflow architect may modify DESIGN.md".to_string(),
        DesignGuardRole::Coder => "You are not allowed to modify DESIGN.md. Restore the settled design and create a proper `DESIGN.md request` for the workflow architect that explains what should change and why.".to_string(),
        DesignGuardRole::Reviewer => "You do not have rights to modify DESIGN.md. Reviewers must not change design files; return findings or request an architect update instead.".to_string(),
    }
}

fn restore_snapshot(snapshot: &DesignMdSnapshot) -> std::io::Result<()> {
    match &snapshot.original {
        Some(original) => {
            if snapshot.path.is_dir() {
                fs::remove_dir_all(&snapshot.path)?;
            }
            if let Some(parent) = snapshot.path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&snapshot.path, original)
        }
        None => {
            if snapshot.path.is_dir() {
                fs::remove_dir_all(&snapshot.path)
            } else if snapshot.path.is_file() {
                fs::remove_file(&snapshot.path)
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::SubAgentSource;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::DesignGuardRole;
    use super::forbidden_design_md_message;
    use super::protected_design_md_path;
    use super::reject_if_design_md_write_forbidden;
    use super::rollback_design_md_if_modified;
    use super::snapshot_design_md;
    use super::workflow_design_guard_role;
    use crate::session::tests::make_session_and_context;
    use crate::tools::tool_policy::TurnToolPolicy;

    async fn workflow_turn(role: &str) -> (crate::session::turn_context::TurnContext, TempDir) {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("workflow.yaml"), "id: example\n").unwrap();
        std::fs::write(tmp.path().join("DESIGN.md"), "before\n").unwrap();
        let (_session, mut turn) = make_session_and_context().await;
        turn.cwd = AbsolutePathBuf::try_from(tmp.path().to_path_buf()).unwrap();
        turn.session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::default(),
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: Some(role.to_string()),
        });
        turn.tool_policy = TurnToolPolicy::for_turn(
            &turn.session_source,
            &turn.cwd,
            crate::prompt_context::ToolPolicy::default(),
        );
        turn.file_system_sandbox_policy = turn
            .tool_policy
            .apply_file_system_overlay(turn.file_system_sandbox_policy.clone(), &turn.cwd);
        (turn, tmp)
    }

    #[tokio::test]
    async fn coder_role_blocks_protected_design_md_path() {
        let (turn, _tmp) = workflow_turn("workflow-coder").await;
        let design_path = protected_design_md_path(&turn).unwrap();

        let err = reject_if_design_md_write_forbidden(&turn, vec![design_path]).unwrap_err();

        assert_eq!(
            err.to_string(),
            forbidden_design_md_message(DesignGuardRole::Coder)
        );
    }

    #[tokio::test]
    async fn architect_role_may_modify_design_md() {
        let (turn, _tmp) = workflow_turn("workflow-architect").await;
        let design_path = protected_design_md_path(&turn).unwrap();

        assert!(reject_if_design_md_write_forbidden(&turn, vec![design_path]).is_ok());
        assert_eq!(
            workflow_design_guard_role(&turn),
            Some(DesignGuardRole::Architect)
        );
    }

    #[tokio::test]
    async fn rollback_restores_design_md_for_reviewer_role() {
        let (turn, tmp) = workflow_turn("workflow-code-reviewer").await;
        let snapshot = snapshot_design_md(&turn).unwrap();
        std::fs::write(tmp.path().join("DESIGN.md"), "after\n").unwrap();

        let err = rollback_design_md_if_modified(&turn, Some(&snapshot)).unwrap_err();

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("DESIGN.md")).unwrap(),
            "before\n"
        );
        assert_eq!(
            err.to_string(),
            forbidden_design_md_message(DesignGuardRole::Reviewer)
        );
    }

    #[tokio::test]
    async fn rollback_restores_design_md_for_resilience_reviewer_role() {
        let (turn, tmp) = workflow_turn("workflow-resilience-reviewer").await;
        let snapshot = snapshot_design_md(&turn).unwrap();
        std::fs::write(tmp.path().join("DESIGN.md"), "after\n").unwrap();

        let err = rollback_design_md_if_modified(&turn, Some(&snapshot)).unwrap_err();

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("DESIGN.md")).unwrap(),
            "before\n"
        );
        assert_eq!(
            err.to_string(),
            forbidden_design_md_message(DesignGuardRole::Reviewer)
        );
    }

    #[tokio::test]
    async fn rollback_restores_design_md_for_coder_role() {
        let (turn, tmp) = workflow_turn("workflow-coder").await;
        let snapshot = snapshot_design_md(&turn).unwrap();
        std::fs::write(tmp.path().join("DESIGN.md"), "after\n").unwrap();

        let err = rollback_design_md_if_modified(&turn, Some(&snapshot)).unwrap_err();

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("DESIGN.md")).unwrap(),
            "before\n"
        );
        assert_eq!(
            err.to_string(),
            forbidden_design_md_message(DesignGuardRole::Coder)
        );
    }
}
