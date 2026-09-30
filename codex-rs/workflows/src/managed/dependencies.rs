use std::collections::BTreeSet;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;

mod lockfile;

pub(in crate::managed) use lockfile::ManagedBunLockfile;

const DEPENDENCY_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];
const MAX_LOCAL_PACKAGES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::managed) struct ValidatedDependencySources {
    pub(in crate::managed) has_dependencies: bool,
    pub(in crate::managed) local_packages: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::managed) struct ValidatedManagedDependencies {
    pub(in crate::managed) sources: ValidatedDependencySources,
    pub(in crate::managed) lockfile: ManagedBunLockfile,
}

pub(in crate::managed) fn validate_managed_dependencies(
    package: &crate::WorkflowPackage,
) -> anyhow::Result<ValidatedManagedDependencies> {
    let sources = validate_managed_dependency_sources(package)?;
    let lockfile = lockfile::validate(package, &sources)?;
    Ok(ValidatedManagedDependencies { sources, lockfile })
}

pub(in crate::managed) fn validate_managed_dependency_sources(
    package: &crate::WorkflowPackage,
) -> anyhow::Result<ValidatedDependencySources> {
    let mut pending = vec![(PathBuf::new(), package.package_json.clone())];
    let mut discovered = BTreeSet::from([PathBuf::new()]);
    let mut has_dependencies = false;
    while let Some((base, manifest)) = pending.pop() {
        let object = manifest
            .as_object()
            .context("managed workflow package.json must contain an object")?;
        reject_unsupported_package_features(object)?;
        for section in DEPENDENCY_SECTIONS {
            let Some(value) = object.get(section) else {
                continue;
            };
            let dependencies = value
                .as_object()
                .with_context(|| format!("managed workflow `{section}` must be an object"))?;
            has_dependencies |= !dependencies.is_empty();
            for (name, value) in dependencies {
                if !valid_package_name(name) {
                    bail!("managed workflow dependency name `{name}` is invalid");
                }
                let specifier = value.as_str().with_context(|| {
                    format!("managed workflow dependency `{name}` must be a string")
                })?;
                let Some(local) = validate_specifier(&base, name, specifier)? else {
                    continue;
                };
                if discovered.contains(&local) {
                    continue;
                }
                if discovered.len() > MAX_LOCAL_PACKAGES {
                    bail!(
                        "managed workflow exceeds {MAX_LOCAL_PACKAGES} local dependency packages"
                    );
                }
                if !crate::manifest::is_package_regular_directory(&package.root, &local) {
                    bail!(
                        "managed workflow local dependency `{name}` must be a regular directory inside the package"
                    );
                }
                let manifest_path = local.join("package.json");
                if !crate::manifest::is_package_regular_file(&package.root, &manifest_path) {
                    bail!("managed workflow local dependency `{name}` has no regular package.json");
                }
                let contents = crate::manifest::read_bounded_utf8(
                    &package.root.join(&manifest_path),
                    crate::manifest::MAX_PACKAGE_JSON_BYTES,
                )?;
                let value = serde_json::from_str(&contents).with_context(|| {
                    format!(
                        "invalid local dependency manifest {}",
                        manifest_path.display()
                    )
                })?;
                discovered.insert(local.clone());
                pending.push((local, value));
            }
        }
        let mut override_field = None;
        for section in ["overrides", "resolutions"] {
            if let Some(value) = object.get(section) {
                let values = validate_override_map(section, value)?;
                if !values.is_empty() && override_field.replace(section).is_some() {
                    bail!("managed workflow may not combine `overrides` and `resolutions`");
                }
            }
        }
    }
    let local_packages = discovered
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    Ok(ValidatedDependencySources {
        has_dependencies,
        local_packages,
    })
}

fn reject_unsupported_package_features(
    object: &serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    for field in [
        "workspaces",
        "catalog",
        "catalogs",
        "patchedDependencies",
        "trustedDependencies",
    ] {
        if object.get(field).is_some_and(|value| !value.is_null()) {
            bail!("managed workflow package field `{field}` is unsupported");
        }
    }
    Ok(())
}

fn validate_override_map<'a>(
    field: &str,
    value: &'a serde_json::Value,
) -> anyhow::Result<&'a serde_json::Map<String, serde_json::Value>> {
    let values = value
        .as_object()
        .with_context(|| format!("managed workflow `{field}` must be an object"))?;
    for (name, value) in values {
        if !valid_package_name(name) {
            bail!("managed workflow override package `{name}` is invalid");
        }
        let specifier = value.as_str().with_context(|| {
            format!("managed workflow override `{name}` must use a registry specifier")
        })?;
        validate_registry_specifier(name, specifier)?;
    }
    Ok(values)
}

fn validate_specifier(base: &Path, name: &str, specifier: &str) -> anyhow::Result<Option<PathBuf>> {
    if let Some(relative) = specifier.strip_prefix("file:") {
        return resolve_local_path(base, relative)
            .with_context(|| {
                format!("managed workflow local dependency `{name}` has an unsafe path")
            })
            .map(Some);
    }
    validate_registry_specifier(name, specifier)?;
    Ok(None)
}

fn resolve_local_path(base: &Path, value: &str) -> anyhow::Result<PathBuf> {
    if value.is_empty()
        || value.starts_with(['/', '\\'])
        || value.contains(['\\', ':', '%'])
        || !value.is_ascii()
    {
        bail!("unsafe local dependency path");
    }
    let mut resolved = base.to_path_buf();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !resolved.pop() {
                    bail!("local dependency escapes the workflow package");
                }
            }
            part if safe_path_component(part) => resolved.push(part),
            _ => bail!("unsafe local dependency path"),
        }
    }
    if resolved.as_os_str().is_empty()
        || resolved == base
        || resolved
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe local dependency path");
    }
    Ok(resolved)
}

fn validate_registry_specifier(name: &str, specifier: &str) -> anyhow::Result<()> {
    let valid = specifier
        .strip_prefix("npm:")
        .map_or_else(|| valid_registry_range(specifier), valid_npm_alias);
    if !valid {
        bail!(
            "managed workflow dependency `{name}` must use the public registry or an in-package `file:` directory"
        );
    }
    Ok(())
}

fn valid_registry_range(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._*+~^<>=|, -".contains(&byte))
        && value
            .bytes()
            .any(|byte| byte.is_ascii_alphanumeric() || byte == b'*')
}

fn valid_npm_alias(alias: &str) -> bool {
    let Some(separator) = alias.rfind('@') else {
        return false;
    };
    let (package, version) = alias.split_at(separator);
    valid_package_name(package) && valid_registry_range(&version[1..])
}

fn valid_package_name(name: &str) -> bool {
    let valid_part = |part: &str| {
        !part.is_empty()
            && !matches!(part, "." | "..")
            && part.is_ascii()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
    };
    if let Some(scoped) = name.strip_prefix('@') {
        scoped.split_once('/').is_some_and(|(scope, package)| {
            valid_part(scope) && valid_part(package) && !package.contains('/')
        })
    } else {
        valid_part(name) && !name.contains('/')
    }
}

fn safe_path_component(part: &str) -> bool {
    part.len() <= 255
        && part
            .bytes()
            .all(|byte| byte > b' ' && byte != 0x7f && !b"<>\"|?*".contains(&byte))
}

#[cfg(test)]
#[path = "dependencies_tests.rs"]
mod tests;
