use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactSource {
    pub path: PathBuf,
    pub kind: String,
    pub sha256: String,
}

impl ArtifactSource {
    pub fn new(path: PathBuf, kind: impl Into<String>, sha256: String) -> Self {
        Self {
            path,
            kind: kind.into(),
            sha256,
        }
    }
}

pub fn source_key(sources: &[ArtifactSource]) -> String {
    let mut hasher = Sha256::new();
    for source in sources {
        hasher.update(b"path\0");
        hasher.update(normalized_relative_path(&source.path).as_bytes());
        hasher.update(b"\0kind\0");
        hasher.update(source.kind.as_bytes());
        hasher.update(b"\0sha256\0");
        hasher.update(source.sha256.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

pub fn changed_source_paths(
    learned_sources: &[ArtifactSource],
    current_sources: &[ArtifactSource],
) -> Vec<PathBuf> {
    let learned_by_path = learned_sources
        .iter()
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    let current_by_path = current_sources
        .iter()
        .map(|source| (source.path.clone(), source))
        .collect::<BTreeMap<_, _>>();
    let mut changed = Vec::new();
    for learned in learned_sources {
        match current_by_path.get(&learned.path) {
            Some(current) if *current == learned => {}
            Some(_) | None => changed.push(learned.path.clone()),
        }
    }
    for current in current_sources {
        if !learned_by_path.contains_key(&current.path) {
            changed.push(current.path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
}

pub fn file_sha256(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(data);
    Some(format!("{:x}", hasher.finalize()))
}

pub fn sharded_state_dir(root: &Path, scope_key: &str, source_key: &str) -> PathBuf {
    scope_artifacts_dir(root, scope_key).join(source_key)
}

pub fn scope_artifacts_dir(root: &Path, scope_key: &str) -> PathBuf {
    let first = scope_key.get(..2).unwrap_or(scope_key);
    let second = scope_key.get(2..4).unwrap_or("");
    root.join(first).join(second).join(scope_key)
}

pub(crate) fn normalized_relative_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::RootDir | Component::Prefix(_) => {
                parts.push(component.as_os_str().to_string_lossy().to_string());
            }
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn source_key_changes_with_source_hash() {
        let first = vec![source("Cargo.toml", "aaa", "build_manifest")];
        let second = vec![source("Cargo.toml", "bbb", "build_manifest")];

        assert_ne!(source_key(&first), source_key(&second));
    }

    #[test]
    fn changed_source_paths_tracks_added_removed_and_modified_sources() {
        let learned = vec![
            source("Cargo.toml", "aaa", "build_manifest"),
            source("Cargo.lock", "old", "lockfile"),
            source("justfile", "same", "tooling"),
        ];
        let current = vec![
            source("Cargo.toml", "bbb", "build_manifest"),
            source("justfile", "same", "tooling"),
            source("package.json", "new", "build_manifest"),
        ];

        assert_eq!(
            changed_source_paths(&learned, &current),
            vec![
                PathBuf::from("Cargo.lock"),
                PathBuf::from("Cargo.toml"),
                PathBuf::from("package.json"),
            ]
        );
    }

    #[test]
    fn sharded_state_dir_uses_scope_key_prefixes() {
        assert_eq!(
            sharded_state_dir(Path::new("/tmp/artifacts"), "abcdef", "source"),
            PathBuf::from("/tmp/artifacts/ab/cd/abcdef/source")
        );
    }

    fn source(path: &str, sha256: &str, kind: &str) -> ArtifactSource {
        ArtifactSource::new(PathBuf::from(path), kind, sha256.to_string())
    }
}
