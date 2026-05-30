use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
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
    pub(crate) workflow_dir: Option<AbsolutePathBuf>,
    pub(crate) design_md_path: Option<AbsolutePathBuf>,
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
            .and_then(|workflow_design| workflow_design.design_md_path.clone())
            .map(|design_md_path| vec![design_md_path])
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
        workflow_confined_file_system_policy(file_system_sandbox_policy, cwd, workflow_design)
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
    let mut workflow_dir = None;
    let mut cursor = Some(cwd.as_path());
    while let Some(path) = cursor {
        if path.join("workflow.yaml").is_file() {
            workflow_dir = AbsolutePathBuf::try_from(path.to_path_buf()).ok();
            break;
        }
        cursor = path.parent();
    }
    let design_md_path = workflow_dir
        .as_ref()
        .and_then(|path| AbsolutePathBuf::try_from(path.as_path().join("DESIGN.md")).ok());
    Some(WorkflowDesignPolicy {
        role,
        workflow_dir,
        design_md_path,
    })
}

fn workflow_confined_file_system_policy(
    file_system_sandbox_policy: FileSystemSandboxPolicy,
    cwd: &AbsolutePathBuf,
    workflow_design: &WorkflowDesignPolicy,
) -> FileSystemSandboxPolicy {
    let writable_workflow_roots = workflow_design
        .workflow_dir
        .as_ref()
        .map(|workflow_dir| {
            writable_roots_confined_to_workflow_dir(&file_system_sandbox_policy, cwd, workflow_dir)
        })
        .unwrap_or_default();
    let mut policy = file_system_sandbox_policy_without_write_access(file_system_sandbox_policy);
    if !writable_workflow_roots.is_empty() {
        policy = policy.with_additional_writable_roots(cwd.as_path(), &writable_workflow_roots);
    }
    if workflow_design.role != WorkflowDesignPolicyRole::Architect
        && let Some(design_md_path) = &workflow_design.design_md_path
    {
        policy = policy
            .with_additional_read_only_paths(cwd.as_path(), std::slice::from_ref(design_md_path));
    }
    policy
}

fn writable_roots_confined_to_workflow_dir(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &AbsolutePathBuf,
    workflow_dir: &AbsolutePathBuf,
) -> Vec<AbsolutePathBuf> {
    if file_system_sandbox_policy.has_full_disk_write_access() {
        return vec![workflow_dir.clone()];
    }

    let mut writable_roots: Vec<AbsolutePathBuf> = file_system_sandbox_policy
        .get_writable_roots_with_cwd(cwd.as_path())
        .into_iter()
        .filter_map(|writable_root| {
            if workflow_dir
                .as_path()
                .starts_with(writable_root.root.as_path())
            {
                Some(workflow_dir.clone())
            } else if writable_root
                .root
                .as_path()
                .starts_with(workflow_dir.as_path())
            {
                Some(writable_root.root)
            } else {
                None
            }
        })
        .collect();
    // macOS may normalize `/var` writable roots to `/private/var`; keep the
    // logical cwd root too so direct policy checks against cwd-relative paths
    // continue to match.
    if cwd.as_path().starts_with(workflow_dir.as_path())
        && file_system_sandbox_policy.can_write_path_with_cwd(cwd.as_path(), cwd.as_path())
        && !writable_roots.iter().any(|root| root == cwd)
    {
        writable_roots.push(cwd.clone());
    }
    writable_roots
}

fn file_system_sandbox_policy_without_write_access(
    mut file_system_sandbox_policy: FileSystemSandboxPolicy,
) -> FileSystemSandboxPolicy {
    match file_system_sandbox_policy.kind {
        FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox => {
            FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
            }])
        }
        FileSystemSandboxKind::Restricted => {
            for entry in &mut file_system_sandbox_policy.entries {
                if entry.access == FileSystemAccessMode::Write {
                    entry.access = FileSystemAccessMode::Read;
                }
            }
            file_system_sandbox_policy
        }
    }
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
    fn workflow_coder_overlay_confines_writes_to_workflow_dir() {
        let tmp = TempDir::new().unwrap();
        let workflow_dir = tmp.path().join(".codex/workflows/pr-triage");
        std::fs::create_dir_all(workflow_dir.join("src")).unwrap();
        std::fs::write(workflow_dir.join("workflow.yaml"), "id: pr-triage\n").unwrap();
        std::fs::write(workflow_dir.join("DESIGN.md"), "design\n").unwrap();
        let cwd = AbsolutePathBuf::try_from(workflow_dir.clone()).unwrap();
        let workflow_src = workflow_dir.join("src/workflow.ts");
        let design_md = workflow_dir.join("DESIGN.md");
        let workspace_readme = tmp.path().join("README.md");

        let policy = TurnToolPolicy::for_turn(&workflow_session_source("workflow-coder"), &cwd);
        let overlaid =
            policy.apply_file_system_overlay(FileSystemSandboxPolicy::unrestricted(), &cwd);

        assert!(overlaid.can_write_path_with_cwd(workflow_src.as_path(), cwd.as_path()));
        assert!(!overlaid.can_write_path_with_cwd(design_md.as_path(), cwd.as_path()));
        assert!(!overlaid.can_write_path_with_cwd(workspace_readme.as_path(), cwd.as_path()));
    }

    #[test]
    fn workflow_architect_overlay_confines_writes_to_workflow_dir() {
        let tmp = TempDir::new().unwrap();
        let workflow_dir = tmp.path().join(".codex/workflows/pr-triage");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(workflow_dir.join("workflow.yaml"), "id: pr-triage\n").unwrap();
        let cwd = AbsolutePathBuf::try_from(workflow_dir.clone()).unwrap();
        let design_md = workflow_dir.join("DESIGN.md");
        let workspace_readme = tmp.path().join("README.md");

        let policy = TurnToolPolicy::for_turn(&workflow_session_source("workflow-architect"), &cwd);
        let overlaid =
            policy.apply_file_system_overlay(FileSystemSandboxPolicy::unrestricted(), &cwd);

        assert!(overlaid.can_write_path_with_cwd(design_md.as_path(), cwd.as_path()));
        assert!(!overlaid.can_write_path_with_cwd(workspace_readme.as_path(), cwd.as_path()));
    }

    #[test]
    fn workflow_coder_without_workflow_dir_cannot_write_workspace_root() {
        let tmp = TempDir::new().unwrap();
        let cwd = AbsolutePathBuf::try_from(tmp.path().to_path_buf()).unwrap();
        let root_workflow_yaml = tmp.path().join("workflow.yaml");

        let policy = TurnToolPolicy::for_turn(&workflow_session_source("workflow-coder"), &cwd);
        let overlaid = policy.apply_file_system_overlay(writable_root_policy(&cwd), &cwd);

        assert_eq!(
            policy.workflow_design().map(|policy| policy.role),
            Some(WorkflowDesignPolicyRole::Coder)
        );
        assert!(!overlaid.can_write_path_with_cwd(root_workflow_yaml.as_path(), cwd.as_path()));
    }

    #[test]
    fn workflow_coder_from_nested_cwd_preserves_nested_writable_root() {
        let tmp = TempDir::new().unwrap();
        let workflow_dir = tmp.path().join(".codex/workflows/pr-triage");
        let workflow_src_dir = workflow_dir.join("src");
        std::fs::create_dir_all(&workflow_src_dir).unwrap();
        std::fs::write(workflow_dir.join("workflow.yaml"), "id: pr-triage\n").unwrap();
        let cwd = AbsolutePathBuf::try_from(workflow_src_dir.clone()).unwrap();
        let workflow_src = workflow_src_dir.join("workflow.ts");
        let workflow_package = workflow_dir.join("package.json");
        let workspace_readme = tmp.path().join("README.md");

        let policy = TurnToolPolicy::for_turn(&workflow_session_source("workflow-coder"), &cwd);
        let overlaid = policy.apply_file_system_overlay(writable_root_policy(&cwd), &cwd);

        assert!(overlaid.can_write_path_with_cwd(workflow_src.as_path(), cwd.as_path()));
        assert!(!overlaid.can_write_path_with_cwd(workflow_package.as_path(), cwd.as_path()));
        assert!(!overlaid.can_write_path_with_cwd(workspace_readme.as_path(), cwd.as_path()));
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
