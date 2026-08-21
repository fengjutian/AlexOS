use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{AlexError, permission::Permission};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppManifest {
    pub schema_version: u32,
    #[serde(default, rename = "kind")]
    pub kind: PackageKind,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<Author>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<Icons>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub frontend: Frontend,
    #[serde(default)]
    pub backend: Option<Backend>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// Plugin 静态声明的扩展点(命令 / 面板 / 菜单)。
    /// 0.1 切片 3:只解析和聚合,host 不主动调用(那是 0.2 的事)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_points: Option<Vec<ExtensionPoint>>,
}

/// 0.1 引入的字段。`App` 是默认(向后兼容),`Plugin` 启用扩展点挂载。
/// schemaVersion 不 bump — 老 manifest 仍然能跑。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    #[default]
    App,
    Plugin,
}

/// Plugin 静态声明的扩展点。`entry` 是 plugin backend 暴露的方法名,
/// host 通过 system permission 调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionPoint {
    pub kind: ExtensionKind,
    pub id: String,
    pub label: String,
    pub entry: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionKind {
    Command,
    Panel,
    Menu,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, transparent)]
pub struct Icons {
    pub entries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Author {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
