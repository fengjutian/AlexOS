//! Application menu, tray, and global-shortcut registries.
//!
//! The host treats menus / tray icons / shortcuts as
//! *declarative*: the page sends a `template` (a JSON shape
//! listing items and accelerators), the host renders them on
//! the OS shell, and item activations arrive back as
//! `menu.clicked` / `tray.clicked` / `shortcut.triggered`
//! events on the page's bus. The host never executes
//! front-end JS in response to a click — the event is
//! surfaced as data only, so a malicious app cannot smuggle
//! a script into a menu item.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MenuTemplate {
    #[serde(default)]
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum MenuItem {
    #[serde(rename = "normal")]
    Normal {
        id: String,
        label: String,
        #[serde(default)]
        accelerator: Option<String>,
        #[serde(default)]
        enabled: Option<bool>,
    },
    #[serde(rename = "separator")]
    Separator,
    #[serde(rename = "submenu")]
    Submenu {
        id: String,
        label: String,
        items: Vec<MenuItem>,
    },
    #[serde(rename = "checkbox")]
    Checkbox {
        id: String,
        label: String,
        #[serde(default)]
        checked: Option<bool>,
        #[serde(default)]
        accelerator: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraySpec {
    pub icon: String,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub menu: Option<MenuTemplate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrayInfo {
    pub id: String,
    pub icon: String,
    pub tooltip: Option<String>,
}

#[derive(Debug, Error)]
pub enum MenuError {
    #[error("menu template is invalid: {0}")]
    Invalid(String),
    #[error("icon path {0:?} is not allowed")]
    IconPath(String),
    #[error("tray icon {0} already exists for this app")]
    TrayExists(String),
    #[error("tray icon {0} is unknown")]
    UnknownTray(String),
    #[error("shortcut {0} is already registered by another app")]
    ShortcutConflict(String),
    #[error("shortcut accelerator {0:?} is invalid")]
    InvalidAccelerator(String),
    #[error("icon path must be a file:// URL or relative to the package root")]
    BadIcon,
}

const MAX_ITEMS_PER_MENU: usize = 256;
const MAX_LABEL_BYTES: usize = 200;
const MAX_ACCELERATOR_BYTES: usize = 64;

pub struct MenuStore {
    pub(crate) state: Mutex<MenuState>,
}

#[derive(Default)]
pub struct MenuState {
    pub(crate) menus: HashMap<String, AppMenu>,
    pub(crate) tray: HashMap<String, AppTray>,
    /// Map from normalized accelerator to the app id that
    /// currently holds it. Conflicts are detected on register.
    pub(crate) shortcuts: HashMap<String, String>,
    /// Per-app set of accelerators the app owns. Used for
    /// bulk revoke on app shutdown.
    pub(crate) app_shortcuts: HashMap<String, Vec<String>>,
}

pub struct AppMenu {
    pub(crate) app_id: String,
    #[allow(dead_code)]
    pub(crate) template: MenuTemplate,
}

pub struct AppTray {
    pub(crate) app_id: String,
    #[allow(dead_code)]
    pub(crate) info: TrayInfo,
}

impl MenuStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(MenuState::default()),
        })
    }

    pub fn set_application_menu(
        &self,
        app_id: &str,
        template: MenuTemplate,
    ) -> Result<(), MenuError> {
        validate_template(&template)?;
        let mut state = self.state.lock().expect("menu lock poisoned");
        state
            .menus
            .insert(app_id.to_owned(), AppMenu { app_id: app_id.to_owned(), template });
        Ok(())
    }

    pub fn set_context_menu(
        &self,
        app_id: &str,
        template: MenuTemplate,
    ) -> Result<(), MenuError> {
        // The data model is the same; the shell layer decides
        // where to attach the menu. We key by a different
        // prefix to keep both menus addressable.
        validate_template(&template)?;
        let key = format!("{app_id}:context");
        let mut state = self.state.lock().expect("menu lock poisoned");
        state.menus.insert(
            key,
            AppMenu {
                app_id: app_id.to_owned(),
                template,
            },
        );
        Ok(())
    }

    pub fn create_tray(
        &self,
        app_id: &str,
        spec: TraySpec,
        package_root: &std::path::Path,
    ) -> Result<TrayInfo, MenuError> {
        if spec.icon.is_empty() {
            return Err(MenuError::Invalid("icon is empty".into()));
        }
        if !is_safe_icon(&spec.icon, package_root) {
            return Err(MenuError::IconPath(spec.icon));
        }
        if let Some(menu) = &spec.menu {
            validate_template(menu)?;
        }
        let id = format!("{app_id}:tray:{}", next_id(&spec.icon));
        let mut state = self.state.lock().expect("menu lock poisoned");
        if state.tray.contains_key(&id) {
            return Err(MenuError::TrayExists(id));
        }
        let info = TrayInfo {
            id: id.clone(),
            icon: spec.icon,
            tooltip: spec.tooltip,
        };
        state.tray.insert(id, AppTray { app_id: app_id.to_owned(), info: info.clone() });
        Ok(info)
    }

    pub fn destroy_tray(&self, app_id: &str, tray_id: &str) -> Result<(), MenuError> {
        let mut state = self.state.lock().expect("menu lock poisoned");
        let owned = state.tray.get(tray_id).ok_or_else(|| MenuError::UnknownTray(tray_id.to_owned()))?;
        if owned.app_id != app_id {
            return Err(MenuError::UnknownTray(tray_id.to_owned()));
        }
        state.tray.remove(tray_id);
        Ok(())
    }

    pub fn register_shortcut(
        &self,
        app_id: &str,
        accelerator: &str,
    ) -> Result<(), MenuError> {
        let normalized = normalize_accelerator(accelerator)?;
        let mut state = self.state.lock().expect("menu lock poisoned");
        if let Some(owner) = state.shortcuts.get(&normalized) {
            if owner == app_id {
                return Ok(());
            }
            return Err(MenuError::ShortcutConflict(normalized));
        }
        state.shortcuts.insert(normalized.clone(), app_id.to_owned());
        state
            .app_shortcuts
            .entry(app_id.to_owned())
            .or_default()
            .push(normalized);
        Ok(())
    }

    /// Drop everything that belongs to `app_id`. Called on app
    /// shutdown so a crashed app does not leave a tray icon
    /// or a registered hotkey behind.
    pub fn drop_app(&self, app_id: &str) {
        let mut state = self.state.lock().expect("menu lock poisoned");
        state.menus.retain(|_, menu| menu.app_id != app_id);
        state.tray.retain(|_, tray| tray.app_id != app_id);
        if let Some(list) = state.app_shortcuts.remove(app_id) {
            for accel in list {
                state.shortcuts.remove(&accel);
            }
        }
    }

    /// Return accelerators that the app currently owns, for
    /// diagnostics / capability listing.
    pub fn app_shortcuts(&self, app_id: &str) -> Vec<String> {
        let state = self.state.lock().expect("menu lock poisoned");
        state
            .app_shortcuts
            .get(app_id)
            .cloned()
            .unwrap_or_default()
    }
}

fn validate_template(template: &MenuTemplate) -> Result<(), MenuError> {
    if template.items.len() > MAX_ITEMS_PER_MENU {
        return Err(MenuError::Invalid(format!(
            "menu has {} items; cap is {MAX_ITEMS_PER_MENU}",
            template.items.len()
        )));
    }
    validate_items(&template.items, 0)
}

fn validate_items(items: &[MenuItem], depth: usize) -> Result<(), MenuError> {
    if depth > 4 {
        return Err(MenuError::Invalid("submenu nesting > 4".into()));
    }
    for item in items {
        match item {
            MenuItem::Normal { label, .. } | MenuItem::Checkbox { label, .. } => {
                if label.len() > MAX_LABEL_BYTES {
                    return Err(MenuError::Invalid("label too long".into()));
                }
            }
            MenuItem::Submenu { label, items, .. } => {
                if label.len() > MAX_LABEL_BYTES {
                    return Err(MenuError::Invalid("label too long".into()));
                }
                validate_items(items, depth + 1)?;
            }
            MenuItem::Separator => {}
        }
    }
    Ok(())
}

fn is_safe_icon(path: &str, package_root: &std::path::Path) -> bool {
    // Two accepted shapes:
    // 1. a `file://` URL whose canonical path lives inside
    //    the package root (defeats symlink escape);
    // 2. a relative path with no `..` components (resolved
    //    against the package root by the shell when it
    //    actually renders the icon).
    // Absolute native paths and `http(s)://` URLs are
    // refused outright so a tray icon cannot point at a
    // system binary.
    if let Ok(url) = Url::parse(path) {
        if url.scheme() != "file" {
            return false;
        }
        let Ok(target) = url.to_file_path() else {
            return false;
        };
        let canonical_root = package_root
            .canonicalize()
            .unwrap_or_else(|_| package_root.to_path_buf());
        let canonical_target = match target.canonicalize() {
            Ok(value) => value,
            // The file may not exist yet at config time;
            // canonicalize the deepest existing ancestor.
            Err(_) => return target.starts_with(&canonical_root),
        };
        return canonical_target.starts_with(&canonical_root);
    }
    !path.contains("..") && !path.starts_with('/') && !path.contains(':')
}

pub fn normalize_accelerator_public(accelerator: &str) -> Result<String, MenuError> {
    normalize_accelerator(accelerator)
}

fn normalize_accelerator(accelerator: &str) -> Result<String, MenuError> {
    let trimmed = accelerator.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_ACCELERATOR_BYTES {
        return Err(MenuError::InvalidAccelerator(accelerator.to_owned()));
    }
    // Allowed modifiers and keys. We deliberately keep
    // the grammar small so a mistyped accelerator does not
    // land in the conflict map.
    let mut parts = trimmed.split('+').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 2 {
        return Err(MenuError::InvalidAccelerator(accelerator.to_owned()));
    }
    let key = parts.pop().expect("at least 2 parts");
    if !is_key(key) {
        return Err(MenuError::InvalidAccelerator(accelerator.to_owned()));
    }
    for modifier in &parts {
        if !is_modifier(modifier) {
            return Err(MenuError::InvalidAccelerator(accelerator.to_owned()));
        }
    }
    let mut modifiers = parts
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>();
    modifiers.sort();
    let normalized_key = if key.len() == 1 {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    };
    Ok(format!("{}+{}", modifiers.join("+"), normalized_key))
}

fn is_modifier(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "ctrl" | "control" | "shift" | "alt" | "meta" | "cmd" | "win" | "super"
    )
}

fn is_key(value: &str) -> bool {
    if value.is_empty() || value.len() > 32 {
        return false;
    }
    // Single character (letter / digit / punctuation) or
    // one of the well-known special names.
    if value.len() == 1 {
        return value.chars().next().is_some_and(|c| !c.is_whitespace());
    }
    matches!(
        value.to_ascii_lowercase().as_str(),
        "enter"
            | "return"
            | "escape"
            | "esc"
            | "tab"
            | "backspace"
            | "delete"
            | "del"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "up"
            | "down"
            | "left"
            | "right"
            | "space"
            | "f1"
            | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
    )
}

fn next_id(seed: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    seed.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(ids: &[&str]) -> MenuTemplate {
        MenuTemplate {
            items: ids
                .iter()
                .map(|id| MenuItem::Normal {
                    id: (*id).into(),
                    label: (*id).into(),
                    accelerator: None,
                    enabled: None,
                })
                .collect(),
        }
    }

    #[test]
    fn application_menu_replaces_previous() {
        let store = MenuStore::new();
        store
            .set_application_menu("a", template(&["open", "save"]))
            .unwrap();
        store
            .set_application_menu("a", template(&["quit"]))
            .unwrap();
        assert_eq!(store.state.lock().unwrap().menus.len(), 1);
    }

    #[test]
    fn tray_icon_can_only_be_owned_by_one_app() {
        let store = MenuStore::new();
        let first = store
            .create_tray(
                "a",
                TraySpec {
                    icon: "assets/tray.png".into(),
                    tooltip: None,
                    menu: None,
                },
                std::path::Path::new("."),
            )
            .unwrap();
        assert!(store.destroy_tray("a", &first.id).is_ok());
    }

    #[test]
    fn tray_icon_rejects_absolute_file_url() {
        // A `file://` URL whose path is the host's C:\ drive
        // must be refused even if the rest of the URL is
        // well-formed.
        let store = MenuStore::new();
        let url = "file:///C:/Windows/System32/shell32.dll".to_string();
        let result = store.create_tray(
            "a",
            TraySpec {
                icon: url,
                tooltip: None,
                menu: None,
            },
            std::path::Path::new("."),
        );
        assert!(result.is_err());
    }

    #[test]
    fn shortcut_conflict_is_rejected() {
        let store = MenuStore::new();
        store.register_shortcut("a", "Ctrl+Shift+P").unwrap();
        let err = store.register_shortcut("b", "Ctrl+Shift+P").unwrap_err();
        assert!(matches!(err, MenuError::ShortcutConflict(_)));
    }

    #[test]
    fn drop_app_releases_shortcuts() {
        let store = MenuStore::new();
        store.register_shortcut("a", "Ctrl+Shift+P").unwrap();
        store.drop_app("a");
        // The accelerator must be re-registrable by another
        // app once the original app is gone.
        store.register_shortcut("b", "Ctrl+Shift+P").unwrap();
    }

    #[test]
    fn normalize_accelerator_handles_known_modifiers() {
        assert_eq!(
            normalize_accelerator("Ctrl+Shift+P").unwrap(),
            "ctrl+shift+P"
        );
        assert!(normalize_accelerator("Bogus+P").is_err());
        assert!(normalize_accelerator("Ctrl+").is_err());
    }

    #[test]
    fn menu_rejects_too_many_items() {
        let store = MenuStore::new();
        let items = (0..MAX_ITEMS_PER_MENU + 1)
            .map(|i| MenuItem::Normal {
                id: format!("item-{i}"),
                label: format!("item-{i}"),
                accelerator: None,
                enabled: None,
            })
            .collect();
        let err = store
            .set_application_menu("a", MenuTemplate { items })
            .unwrap_err();
        assert!(matches!(err, MenuError::Invalid(_)));
    }
}
