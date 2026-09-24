use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

pub const WORKFLOW_API_VERSION: u32 = 1;
pub const MAX_WORKFLOW_YAML_BYTES: u64 = 64 * 1024;
pub const MAX_PACKAGE_JSON_BYTES: u64 = 1024 * 1024;
pub const MAX_WORKFLOW_SOURCE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowManifest {
    pub api_version: u32,
    pub id: String,
    pub title: String,
    pub callable_name: String,
    pub description: String,
    pub validation: ValidationPolicy,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationPolicy {
    pub commands: Vec<ValidationCommand>,
    pub coverage: ValidationCoverage,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationCommand {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationCoverage {
    pub positive: bool,
    pub load: bool,
    pub autocomplete: bool,
    pub negative: bool,
    #[serde(default)]
    pub recovery: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowPackage {
    pub(crate) root: PathBuf,
    pub manifest: WorkflowManifest,
    pub(crate) package_json: Value,
}

impl WorkflowPackage {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        if !fs::symlink_metadata(root)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            bail!(
                "workflow package root {} must be a regular directory",
                root.display()
            );
        }
        let workflow_yaml = root.join("workflow.yaml");
        if !is_package_regular_file(root, Path::new("workflow.yaml")) {
            bail!(
                "missing required regular package file {}",
                workflow_yaml.display()
            );
        }
        let contents = read_bounded_utf8(&workflow_yaml, MAX_WORKFLOW_YAML_BYTES)?;
        let raw = serde_yaml::from_str::<serde_yaml::Value>(&contents)
            .with_context(|| format!("failed to parse {}", workflow_yaml.display()))?;
        if let Some(field) = first_legacy_field(&raw) {
            bail!(legacy_migration_message(field));
        }
        let manifest = serde_yaml::from_str::<WorkflowManifest>(&contents).with_context(|| {
            format!("invalid canonical metadata in {}", workflow_yaml.display())
        })?;
        if manifest.api_version != WORKFLOW_API_VERSION {
            bail!(
                "unsupported workflow apiVersion {}; expected {WORKFLOW_API_VERSION}",
                manifest.api_version
            );
        }
        let normalized_id = crate::normalize_workflow_id(&manifest.id)?;
        if normalized_id != manifest.id {
            bail!("workflow id `{}` is not canonical", manifest.id);
        }
        if manifest.title.trim().is_empty() {
            bail!("workflow title must not be empty");
        }
        if manifest.description.trim().is_empty() {
            bail!("workflow description must not be empty");
        }
        if !manifest
            .callable_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            || !manifest
                .callable_name
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
        {
            bail!(
                "workflow callableName `{}` is invalid",
                manifest.callable_name
            );
        }

        let package_path = root.join("package.json");
        if !is_package_regular_file(root, Path::new("package.json")) {
            bail!(
                "missing required regular package file {}",
                package_path.display()
            );
        }
        let package_json = serde_json::from_str::<Value>(&read_bounded_utf8(
            &package_path,
            MAX_PACKAGE_JSON_BYTES,
        )?)
        .with_context(|| format!("failed to parse {}", package_path.display()))?;
        if !package_json.is_object() {
            bail!("{} must contain a JSON object", package_path.display());
        }
        let source_path = root.join("src/workflow.ts");
        if !is_package_regular_file(root, Path::new("src/workflow.ts")) {
            bail!(
                "{} must be a regular file inside the package",
                source_path.display()
            );
        }
        read_bounded_utf8(&source_path, MAX_WORKFLOW_SOURCE_BYTES)?;

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
            package_json,
        })
    }

    pub fn load_executable(root: &Path) -> anyhow::Result<Self> {
        let package = Self::load(root)?;
        crate::validation::validate_executable_package(&package)?;
        Ok(package)
    }
}

pub(crate) fn read_bounded_utf8(path: &Path, maximum_bytes: u64) -> anyhow::Result<String> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut contents = String::new();
    file.take(maximum_bytes + 1)
        .read_to_string(&mut contents)
        .with_context(|| format!("failed to read {} as UTF-8", path.display()))?;
    if contents.len() as u64 > maximum_bytes {
        bail!("{} exceeds the {maximum_bytes}-byte limit", path.display());
    }
    Ok(contents)
}

pub(crate) fn is_package_regular_file(root: &Path, relative: &Path) -> bool {
    package_path_metadata(root, relative).is_some_and(|metadata| metadata.is_file())
}

pub(crate) fn is_package_regular_directory(root: &Path, relative: &Path) -> bool {
    package_path_metadata(root, relative).is_some_and(|metadata| metadata.is_dir())
}

fn package_path_metadata(root: &Path, relative: &Path) -> Option<fs::Metadata> {
    let root_metadata = fs::symlink_metadata(root).ok()?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return None;
    }
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() || (components.peek().is_some() && !metadata.is_dir())
        {
            return None;
        }
        if components.peek().is_none() {
            return Some(metadata);
        }
    }
    None
}

pub(crate) fn first_legacy_field(value: &serde_yaml::Value) -> Option<&'static str> {
    let mapping = value.as_mapping()?;
    ["command", "userDescription", "api", "usage"]
        .into_iter()
        .find(|key| mapping.contains_key(serde_yaml::Value::String((*key).to_string())))
}

pub(crate) fn legacy_migration_message(field: &str) -> String {
    format!(
        "legacy workflow metadata field `{field}` is not executable; migrate to workflow package v1 with `apiVersion`, `callableName`, explicit Draft 2020-12 schemas, and a default `defineWorkflow(...)` export"
    )
}
