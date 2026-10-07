use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::path::Component;
use std::path::Path;

use anyhow::Context;
use anyhow::bail;

use super::is_windows_reparse_point;

const MAX_DEPENDENCY_LINK_DEPTH: usize = 32;

pub(super) fn validate_dependency_link(
    node_modules: &Path,
    link: &Path,
    target: &Path,
) -> anyhow::Result<()> {
    if target.is_absolute() || target.as_os_str().is_empty() {
        bail!("dependency link must use a relative target");
    }
    let target_text = target
        .to_str()
        .context("dependency link target must be UTF-8")?;
    if !target_text.is_ascii() || target_text.contains(':') {
        bail!("dependency link target is not portable");
    }
    let mut current = link
        .parent()
        .context("dependency link has no parent")?
        .to_path_buf();
    let mut pending = target
        .components()
        .map(OwnedComponent::try_from)
        .collect::<anyhow::Result<VecDeque<_>>>()?;
    let mut depth = 0;
    while let Some(component) = pending.pop_front() {
        match component {
            OwnedComponent::Current => {}
            OwnedComponent::Parent => {
                if current == node_modules {
                    bail!("dependency link escapes node_modules");
                }
                current.pop();
            }
            OwnedComponent::Normal(component) => {
                current.push(component);
                let metadata =
                    fs::symlink_metadata(&current).context("dependency link is dangling")?;
                if metadata.file_type().is_symlink() {
                    depth += 1;
                    if depth > MAX_DEPENDENCY_LINK_DEPTH {
                        bail!("dependency link resolution exceeds depth limit");
                    }
                    let nested =
                        fs::read_link(&current).context("failed to read dependency link")?;
                    if nested.is_absolute() {
                        bail!("dependency link has an absolute nested target");
                    }
                    current.pop();
                    for component in nested
                        .components()
                        .map(OwnedComponent::try_from)
                        .collect::<anyhow::Result<Vec<_>>>()?
                        .into_iter()
                        .rev()
                    {
                        pending.push_front(component);
                    }
                } else if is_windows_reparse_point(&metadata) {
                    bail!("dependency link resolves through a reparse point");
                } else if !metadata.is_dir() && !pending.is_empty() {
                    bail!("dependency link resolves through a non-directory");
                }
            }
        }
    }
    if !current.starts_with(node_modules) {
        bail!("dependency link escapes node_modules");
    }
    Ok(())
}

enum OwnedComponent {
    Current,
    Parent,
    Normal(OsString),
}

impl TryFrom<Component<'_>> for OwnedComponent {
    type Error = anyhow::Error;

    fn try_from(component: Component<'_>) -> anyhow::Result<Self> {
        Ok(match component {
            Component::CurDir => Self::Current,
            Component::ParentDir => Self::Parent,
            Component::Normal(name) => Self::Normal(name.to_os_string()),
            Component::Prefix(_) | Component::RootDir => {
                bail!("dependency link target is absolute")
            }
        })
    }
}
