use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use semver::Version;

use super::DEPENDENCY_SECTIONS;
use super::ValidatedDependencySources;
use super::resolve_local_path;
use super::valid_package_name;
use super::validate_override_tree;
use super::validate_specifier;

const MAX_BUN_LOCK_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "used by managed dependency installation")]
pub(in crate::managed) enum ManagedBunLockfile {
    NotRequired,
    TextValidated,
    BinaryRequiresSandboxInspection,
}

#[allow(dead_code, reason = "used by managed dependency installation")]
pub(super) fn validate(
    package: &crate::WorkflowPackage,
    sources: &ValidatedDependencySources,
) -> anyhow::Result<ManagedBunLockfile> {
    if !sources.has_dependencies {
        return Ok(ManagedBunLockfile::NotRequired);
    }

    let text = root_lock_metadata(&package.root, "bun.lock")?;
    let binary = root_lock_metadata(&package.root, "bun.lockb")?;
    let (path, format) = match (text, binary) {
        (Some(_), Some(_)) => bail!("managed workflow must contain exactly one Bun lockfile"),
        (None, None) => bail!("managed workflow dependencies require bun.lock or bun.lockb"),
        (Some(_), None) => (package.root.join("bun.lock"), LockFormat::Text),
        (None, Some(metadata)) => {
            if metadata.len() > MAX_BUN_LOCK_BYTES {
                bail!(
                    "{} exceeds the {MAX_BUN_LOCK_BYTES}-byte limit",
                    package.root.join("bun.lockb").display()
                );
            }
            (package.root.join("bun.lockb"), LockFormat::Binary)
        }
    };

    match format {
        LockFormat::Text => {
            let contents = crate::manifest::read_bounded_utf8(&path, MAX_BUN_LOCK_BYTES)?;
            validate_text_lock(package, sources, &contents)
                .with_context(|| format!("invalid managed Bun lockfile {}", path.display()))?;
            Ok(ManagedBunLockfile::TextValidated)
        }
        LockFormat::Binary => Ok(ManagedBunLockfile::BinaryRequiresSandboxInspection),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LockFormat {
    Text,
    Binary,
}

fn root_lock_metadata(root: &Path, name: &str) -> anyhow::Result<Option<fs::Metadata>> {
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Ok(Some(metadata))
        }
        Ok(_) => bail!("{} must be a regular file", path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn validate_text_lock(
    package: &crate::WorkflowPackage,
    sources: &ValidatedDependencySources,
    contents: &str,
) -> anyhow::Result<()> {
    let value = crate::managed::jsonc::parse_unique_jsonc(contents)
        .context("bun.lock must contain valid JSONC without duplicate keys")?;
    let object = value
        .as_object()
        .context("bun.lock must contain an object")?;

    if !matches!(
        object
            .get("lockfileVersion")
            .and_then(|value| value.as_u64()),
        Some(0..=2)
    ) {
        bail!("bun.lock uses an unsupported lockfile version");
    }
    if !matches!(
        object.get("configVersion").map(serde_json::Value::as_u64),
        None | Some(Some(0..=1))
    ) {
        bail!("bun.lock uses an unsupported configuration version");
    }
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "lockfileVersion" | "configVersion" | "workspaces" | "packages" | "overrides"
        ) {
            bail!("bun.lock top-level field `{key}` is unsupported");
        }
    }
    if let Some(overrides) = object.get("overrides") {
        validate_override_tree("overrides", overrides)?;
    }
    let allowed_local = sources
        .local_packages
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let workspaces = object
        .get("workspaces")
        .and_then(serde_json::Value::as_object)
        .context("bun.lock workspaces must contain an object")?;
    if workspaces.len() != 1 || !workspaces.contains_key("") {
        bail!("bun.lock may contain only the root workspace");
    }
    let workspace = workspaces[""]
        .as_object()
        .context("bun.lock root workspace must contain an object")?;
    validate_info_fields(workspace, InfoKind::Workspace)?;
    for section in DEPENDENCY_SECTIONS {
        let expected = package.package_json.get(section);
        let actual = workspace.get(section);
        if dependency_object(actual)? != dependency_object(expected)? {
            bail!("bun.lock root workspace `{section}` does not match package.json");
        }
        if let Some(value) = actual {
            validate_dependency_map(value, Path::new(""), LocalSpecs::Allowed, &allowed_local)
                .with_context(|| format!("invalid bun.lock root workspace `{section}`"))?;
        }
    }

    let packages = object
        .get("packages")
        .and_then(serde_json::Value::as_object)
        .context("bun.lock packages must contain an object")?;
    let mut resolved_local = BTreeSet::new();
    for (key, value) in packages {
        let tuple = value
            .as_array()
            .with_context(|| format!("bun.lock package `{key}` must contain a tuple"))?;
        let resolution = tuple
            .first()
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("bun.lock package `{key}` has no string resolution"))?;
        match classify_resolution(resolution, &allowed_local)? {
            PackageResolution::Registry => {
                if tuple.len() != 4 || tuple.get(1).and_then(serde_json::Value::as_str) != Some("")
                {
                    bail!("bun.lock registry package `{key}` must use the public-registry tuple");
                }
                let metadata = tuple[2].as_object().with_context(|| {
                    format!("bun.lock registry package `{key}` has invalid metadata")
                })?;
                tuple[3].as_str().with_context(|| {
                    format!("bun.lock registry package `{key}` has invalid integrity metadata")
                })?;
                validate_tuple_metadata(key, metadata, None, &allowed_local)?;
            }
            PackageResolution::Local(path) => {
                if tuple.len() != 2 {
                    bail!("bun.lock local package `{key}` has an unsupported tuple");
                }
                let metadata = tuple[1].as_object().with_context(|| {
                    format!("bun.lock local package `{key}` has invalid metadata")
                })?;
                validate_tuple_metadata(key, metadata, Some(&path), &allowed_local)?;
                resolved_local.insert(path);
            }
        }
    }
    if resolved_local != allowed_local {
        bail!(
            "bun.lock local package targets do not match package.json: expected {allowed_local:?}, resolved {resolved_local:?}"
        );
    }
    Ok(())
}

fn dependency_object(
    value: Option<&serde_json::Value>,
) -> anyhow::Result<Option<&serde_json::Map<String, serde_json::Value>>> {
    match value {
        None => Ok(None),
        Some(serde_json::Value::Object(object)) if object.is_empty() => Ok(None),
        Some(serde_json::Value::Object(object)) => Ok(Some(object)),
        Some(_) => bail!("dependency section must contain an object"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PackageResolution {
    Registry,
    Local(PathBuf),
}

fn classify_resolution(
    resolution: &str,
    allowed_local: &BTreeSet<PathBuf>,
) -> anyhow::Result<PackageResolution> {
    if let Some((name, relative)) = resolution.split_once("@file:") {
        if !valid_package_name(name) {
            bail!("bun.lock local package resolution `{resolution}` has an invalid name");
        }
        let path = resolve_local_path(Path::new(""), relative).with_context(|| {
            format!("bun.lock local package resolution `{resolution}` is unsafe")
        })?;
        if !allowed_local.contains(&path) {
            bail!("bun.lock contains undeclared local package target {path:?}");
        }
        return Ok(PackageResolution::Local(path));
    }

    let exact = resolution.strip_prefix("npm:").unwrap_or(resolution);
    let exact = if let Some((alias, target)) = exact.split_once("@npm:") {
        if !valid_package_name(alias) {
            bail!("bun.lock registry resolution `{resolution}` has an invalid alias");
        }
        target
    } else {
        exact
    };
    let separator = exact
        .rfind('@')
        .filter(|separator| *separator > 0)
        .with_context(|| format!("bun.lock package resolution `{resolution}` is not exact"))?;
    let (name, version) = exact.split_at(separator);
    if !valid_package_name(name) || Version::parse(&version[1..]).is_err() {
        bail!("bun.lock package resolution `{resolution}` is not an exact registry SemVer");
    }
    Ok(PackageResolution::Registry)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalSpecs {
    Allowed,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InfoKind {
    Workspace,
    Package,
}

fn validate_info_fields(
    info: &serde_json::Map<String, serde_json::Value>,
    kind: InfoKind,
) -> anyhow::Result<()> {
    for field in info.keys() {
        let base = matches!(
            field.as_str(),
            "name"
                | "version"
                | "bin"
                | "binDir"
                | "dependencies"
                | "devDependencies"
                | "optionalDependencies"
                | "peerDependencies"
                | "optionalPeers"
        );
        if !base && !(kind == InfoKind::Package && matches!(field.as_str(), "os" | "cpu" | "libc"))
        {
            bail!("bun.lock metadata field `{field}` is unsupported");
        }
    }
    Ok(())
}

fn validate_tuple_metadata(
    key: &str,
    metadata: &serde_json::Map<String, serde_json::Value>,
    local_base: Option<&Path>,
    allowed_local: &BTreeSet<PathBuf>,
) -> anyhow::Result<()> {
    validate_info_fields(metadata, InfoKind::Package)?;
    for section in DEPENDENCY_SECTIONS {
        if let Some(value) = metadata.get(section) {
            validate_dependency_map(
                value,
                local_base.unwrap_or_else(|| Path::new("")),
                if local_base.is_some() {
                    LocalSpecs::Allowed
                } else {
                    LocalSpecs::Rejected
                },
                allowed_local,
            )
            .with_context(|| format!("invalid bun.lock package `{key}` metadata `{section}`"))?;
        }
    }
    Ok(())
}

fn validate_dependency_map(
    value: &serde_json::Value,
    base: &Path,
    local_specs: LocalSpecs,
    allowed_local: &BTreeSet<PathBuf>,
) -> anyhow::Result<()> {
    let dependencies = value
        .as_object()
        .context("dependency metadata must contain an object")?;
    for (name, value) in dependencies {
        if !valid_package_name(name) {
            bail!("dependency name `{name}` is invalid");
        }
        let specifier = value
            .as_str()
            .with_context(|| format!("dependency `{name}` must use a string specifier"))?;
        let local = validate_specifier(base, name, specifier)?;
        match (local_specs, local) {
            (LocalSpecs::Rejected, Some(_)) => {
                bail!("registry package dependency `{name}` may not use a local file target");
            }
            (LocalSpecs::Allowed, Some(path)) if !allowed_local.contains(&path) => {
                bail!("dependency `{name}` contains undeclared local package target {path:?}");
            }
            (LocalSpecs::Allowed | LocalSpecs::Rejected, None) | (LocalSpecs::Allowed, Some(_)) => {
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "lockfile_tests.rs"]
mod tests;
