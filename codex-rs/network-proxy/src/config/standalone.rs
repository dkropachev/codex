use super::NetworkMode;
use super::NetworkProxyConfig;
use crate::ConfigState;
use crate::NetworkProxyConstraints;
use crate::Platform;
use crate::build_config_state;
use crate::validate_policy_against_constraints;
use anyhow::Result;
use anyhow::ensure;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

/// A resolved policy, including the ceilings applied by managed requirements.
///
/// Keep this document outside the workload's writable filesystem. It is not an
/// attestation of enforcement: the host must separately ensure that all workload
/// egress traverses the proxy and bind readiness to the current environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StaticNetworkProxyConfig {
    pub network: NetworkProxyConfig,
    /// Standalone configurations may omit constraints. Managed callers must export
    /// the resolved requirements and use `from_slice_strict` to require this field.
    pub constraints: NetworkProxyConstraints,
}

impl<'de> Deserialize<'de> for StaticNetworkProxyConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Document {
            network: NetworkProxyConfig,
            #[serde(default)]
            constraints: NetworkProxyConstraints,
        }

        let config: Document = deserialize_document(deserializer, /*allow_metadata*/ true)?;
        Ok(Self {
            network: config.network,
            constraints: config.constraints,
        })
    }
}

// Only the top-level metadata object is extensible. Ignoring a matcher key can widen access.
fn deserialize_document<'de, D, T>(
    deserializer: D,
    allow_metadata: bool,
) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let mut unknown = None;
    let config = serde_ignored::deserialize(deserializer, |path| {
        let path = path.to_string();
        if unknown.is_none() && !(allow_metadata && path == "metadata") {
            unknown = Some(path);
        }
    })?;
    if let Some(path) = unknown {
        return Err(serde::de::Error::custom(format!("unknown field: {path}")));
    }
    Ok(config)
}

impl StaticNetworkProxyConfig {
    /// Parse standalone policy, allowing optional constraints and top-level metadata.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice::<Self>(bytes)?.normalize())
    }

    /// Require explicit constraints and reject unknown fields at every typed level.
    pub fn from_slice_strict(bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct RequiredConstraints {
            network: NetworkProxyConfig,
            constraints: NetworkProxyConstraints,
        }

        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let config: RequiredConstraints =
            deserialize_document(&mut deserializer, /*allow_metadata*/ false)?;
        deserializer.end()?;
        Ok(Self {
            network: config.network,
            constraints: config.constraints,
        }
        .normalize())
    }

    fn normalize(mut self) -> Self {
        self.network.mitm |=
            self.network.mode == NetworkMode::Limited || !self.network.mitm_hooks.is_empty();
        self
    }

    /// Validate before creating listeners or loading credential/CA material.
    pub fn build_state(self, platform: Platform) -> Result<ConfigState> {
        let config = self.normalize();
        ensure!(
            config.network.enabled,
            "standalone network proxy requires network.enabled = true"
        );
        validate_policy_against_constraints(&config.network, &config.constraints)?;
        build_config_state(config.network, config.constraints, platform)
    }
}

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
