use std::path::PathBuf;

use codex_utils_absolute_path::AbsolutePathBuf;

use crate::function_tool::FunctionCallError;

pub(crate) struct EnvironmentPath {
    pub(crate) environment_id: String,
    pub(crate) path: AbsolutePathBuf,
}

pub(crate) fn format_oai_env_uri(environment_id: &str, path: &AbsolutePathBuf) -> String {
    format!("oai://{environment_id}{}", path.as_path().display())
}

pub(crate) fn parse_oai_env_uri(value: &str) -> Result<Option<EnvironmentPath>, FunctionCallError> {
    if !value.starts_with("oai://") {
        return Ok(None);
    }

    let url = url::Url::parse(value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid environment path URI: {err}"))
    })?;
    let environment_id = url.host_str().filter(|id| !id.is_empty()).ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "environment path URI must include an environment id".to_string(),
        )
    })?;
    let path = PathBuf::from(url.path());
    let path = AbsolutePathBuf::from_absolute_path(&path).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid environment path URI path: {err}"))
    })?;

    Ok(Some(EnvironmentPath {
        environment_id: environment_id.to_string(),
        path,
    }))
}
