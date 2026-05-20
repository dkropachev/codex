use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_utils_absolute_path::AbsolutePathBuf;

const WORKFLOW_ARCHITECT_ROLE: &str = "workflow-architect";
const WORKFLOW_CODER_ROLE: &str = "workflow-coder";
const WORKFLOW_CODE_REVIEWER_ROLE: &str = "workflow-code-reviewer";
const WORKFLOW_ARCH_REVIEWER_ROLE: &str = "workflow-arch-reviewer";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkflowDesignPolicyRole {
    #[default]
    Architect,
    Coder,
    Reviewer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowDesignPolicy {
    pub(crate) role: WorkflowDesignPolicyRole,
    pub(crate) design_md_path: AbsolutePathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TurnToolPolicy {
    workflow_design: Option<WorkflowDesignPolicy>,
}

impl TurnToolPolicy {
    pub(crate) fn for_turn(session_source: &SessionSource, cwd: &AbsolutePathBuf) -> Self {
        let workflow_design = workflow_design_policy(session_source, cwd);
        Self { workflow_design }
    }

    pub(crate) fn workflow_design(&self) -> Option<&WorkflowDesignPolicy> {
        self.workflow_design.as_ref()
    }

    pub(crate) fn protected_read_only_paths(&self) -> Vec<AbsolutePathBuf> {
        self.workflow_design
            .as_ref()
            .filter(|workflow_design| workflow_design.role != WorkflowDesignPolicyRole::Architect)
            .map(|workflow_design| vec![workflow_design.design_md_path.clone()])
            .unwrap_or_default()
    }

    pub(crate) fn apply_file_system_overlay(
        &self,
        file_system_sandbox_policy: FileSystemSandboxPolicy,
        cwd: &AbsolutePathBuf,
    ) -> FileSystemSandboxPolicy {
        let Some(workflow_design) = &self.workflow_design else {
            return file_system_sandbox_policy;
        };
        if workflow_design.role == WorkflowDesignPolicyRole::Architect {
            return file_system_sandbox_policy;
        }
        file_system_sandbox_policy.with_additional_read_only_paths(
            cwd.as_path(),
            std::slice::from_ref(&workflow_design.design_md_path),
        )
    }
}

fn workflow_design_policy(
    session_source: &SessionSource,
    cwd: &AbsolutePathBuf,
) -> Option<WorkflowDesignPolicy> {
    let role = match session_source.get_agent_role().as_deref() {
        Some(WORKFLOW_ARCHITECT_ROLE) => WorkflowDesignPolicyRole::Architect,
        Some(WORKFLOW_CODER_ROLE) => WorkflowDesignPolicyRole::Coder,
        Some(WORKFLOW_CODE_REVIEWER_ROLE | WORKFLOW_ARCH_REVIEWER_ROLE) => {
            WorkflowDesignPolicyRole::Reviewer
        }
        _ => return None,
    };
    let mut cursor = Some(cwd.as_path());
    while let Some(path) = cursor {
        if path.join("workflow.yaml").is_file() {
            let design_md_path = AbsolutePathBuf::try_from(path.join("DESIGN.md")).ok()?;
            return Some(WorkflowDesignPolicy {
                role,
                design_md_path,
            });
        }
        cursor = path.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use codex_protocol::ThreadId;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemPath;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSandboxPolicy;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::SubAgentSource;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::TurnToolPolicy;
    use super::WorkflowDesignPolicyRole;
    use codex_utils_absolute_path::AbsolutePathBuf;

    fn workflow_session_source(role: &str) -> SessionSource {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: ThreadId::default(),
            depth: 1,
            agent_path: None,
            agent_nickname: None,
            agent_role: Some(role.to_string()),
        })
    }

    fn writable_root_policy(cwd: &AbsolutePathBuf) -> FileSystemSandboxPolicy {
        FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: codex_protocol::permissions::FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: cwd.clone() },
                access: FileSystemAccessMode::Write,
            },
        ])
    }

    #[test]
    fn workflow_coder_overlay_makes_design_md_read_only() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("workflow.yaml"), "id: example\n").unwrap();
        let cwd = AbsolutePathBuf::try_from(tmp.path().to_path_buf()).unwrap();
        let design_md = AbsolutePathBuf::try_from(tmp.path().join("DESIGN.md")).unwrap();

        let policy = TurnToolPolicy::for_turn(&workflow_session_source("workflow-coder"), &cwd);
        let overlaid = policy.apply_file_system_overlay(writable_root_policy(&cwd), &cwd);

        assert_eq!(
            policy.workflow_design().map(|policy| policy.role),
            Some(WorkflowDesignPolicyRole::Coder)
        );
        assert!(!overlaid.can_write_path_with_cwd(design_md.as_path(), cwd.as_path()));
    }

    #[test]
    fn workflow_architect_overlay_keeps_design_md_writable() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("workflow.yaml"), "id: example\n").unwrap();
        let cwd = AbsolutePathBuf::try_from(tmp.path().to_path_buf()).unwrap();
        let design_md = AbsolutePathBuf::try_from(tmp.path().join("DESIGN.md")).unwrap();

        let policy = TurnToolPolicy::for_turn(&workflow_session_source("workflow-architect"), &cwd);
        let overlaid = policy.apply_file_system_overlay(writable_root_policy(&cwd), &cwd);

        assert_eq!(
            policy.workflow_design().map(|policy| policy.role),
            Some(WorkflowDesignPolicyRole::Architect)
        );
        assert!(overlaid.can_write_path_with_cwd(design_md.as_path(), cwd.as_path()));
    }
}
