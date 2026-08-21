use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{AlexError, permission::Permission};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub frontend: Frontend,
    #[serde(default)]
    pub backend: Option<Backend>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontend {
    pub entry: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backend {
    pub runtime: RuntimeKind,
    pub entry: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Node,
}

impl AppManifest {
    pub fn validate(&self, root: &Path) -> Result<(), AlexError> {
        if self.schema_version != 1 {
            return Err(AlexError::Validation(format!(
                "unsupported schemaVersion {}; expected 1",
                self.schema_version
            )));
        }
        if !valid_id(&self.id) {
            return Err(AlexError::Validation(format!(
                "invalid package id {:?}; use reverse-domain components",
                self.id
            )));
        }
        validate_relative_entry(root, &self.frontend.entry, "frontend")?;
        if let Some(backend) = &self.backend {
            validate_relative_entry(root, &backend.entry, "backend")?;
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    id.contains('.')
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn validate_relative_entry(root: &Path, entry: &str, kind: &str) -> Result<(), AlexError> {
    let entry_path = Path::new(entry);
    if entry_path.is_absolute()
        || entry_path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(AlexError::Validation(format!(
            "{kind} entry must stay inside the package"
        )));
    }
    if !root.join(entry_path).is_file() {
        return Err(AlexError::Validation(format!(
            "{kind} entry does not exist: {entry}"
        )));
    }
    Ok(())
}
