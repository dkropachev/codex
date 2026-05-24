use std::fmt;

use anyhow::Result;
use anyhow::anyhow;

use crate::repair::types::WorkflowRepairActionKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRepairMode {
    None,
    Metadata,
    Structural,
    Full,
    Threshold(u32),
}

impl WorkflowRepairMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "none" => Ok(Self::None),
            "metadata" => Ok(Self::Metadata),
            "structural" => Ok(Self::Structural),
            "full" => Ok(Self::Full),
            _ => {
                let Some(value) = raw.strip_prefix("threshold:") else {
                    return Err(anyhow!("unsupported repair mode `{raw}`"));
                };
                let threshold = value
                    .parse::<u32>()
                    .map_err(|err| anyhow!("invalid threshold repair mode `{raw}`: {err}"))?;
                Ok(Self::Threshold(threshold))
            }
        }
    }

    pub fn allows_action(&self, kind: WorkflowRepairActionKind) -> bool {
        match self {
            Self::None => false,
            Self::Metadata => matches!(
                kind,
                WorkflowRepairActionKind::NormalizeValidationMetadata
                    | WorkflowRepairActionKind::RepairReadme
                    | WorkflowRepairActionKind::RepairDesign
                    | WorkflowRepairActionKind::RepairPackageManifest
            ),
            Self::Structural => matches!(
                kind,
                WorkflowRepairActionKind::NormalizeValidationMetadata
                    | WorkflowRepairActionKind::RepairReadme
                    | WorkflowRepairActionKind::RepairDesign
                    | WorkflowRepairActionKind::RepairLayout
                    | WorkflowRepairActionKind::RepairPackageManifest
                    | WorkflowRepairActionKind::RepairTsconfig
                    | WorkflowRepairActionKind::ScaffoldWorkflowSource
                    | WorkflowRepairActionKind::ScaffoldWorkflowTests
                    | WorkflowRepairActionKind::AddCoverageMarkers
            ),
            Self::Full | Self::Threshold(_) => true,
        }
    }
}

impl fmt::Display for WorkflowRepairMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Metadata => f.write_str("metadata"),
            Self::Structural => f.write_str("structural"),
            Self::Full => f.write_str("full"),
            Self::Threshold(value) => write!(f, "threshold:{value}"),
        }
    }
}
