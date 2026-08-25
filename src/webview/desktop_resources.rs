//! Shared Windows application-menu, context-menu, tray, and shortcut host state.

use std::{collections::HashMap, path::PathBuf};

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use muda::{
    CheckMenuItem, ContextMenu, Menu, MenuItem as NativeMenuItem, PredefinedMenuItem, Submenu,
};
use tao::platform::windows::WindowExtWindows;
use tao::window::Window;
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::{
    api::ApiRouter,
    menu_tray::{MenuItem, MenuTemplate, TraySpec},
    native::NativeError,
};

pub struct DesktopResources {
    application_menu: Option<Menu>,
    context_menu: Option<Menu>,
    tray_icons: HashMap<String, TrayIcon>,
    hotkey_manager: GlobalHotKeyManager,
    hotkeys: HashMap<u32, (HotKey, String)>,
}

impl DesktopResources {
    pub fn new() -> Result<Self, NativeError> {
        Ok(Self {
            application_menu: None,
            context_menu: None,
            tray_icons: HashMap::new(),
            hotkey_manager: GlobalHotKeyManager::new()
                .map_err(|error| failed("failed to initialize shortcut manager", error))?,
            hotkeys: HashMap::new(),
        })
    }

    pub fn set_application_menu(
        &mut self,
        window: &Window,
        template: &MenuTemplate,
    ) -> Result<(), NativeError> {
        let menu = build_menu(template)?;
        if let Some(previous) = self.application_menu.take() {
            let _ = unsafe { previous.remove_for_hwnd(window.hwnd() as isize) };
        }
        unsafe { menu.init_for_hwnd(window.hwnd() as isize) }
            .map_err(|error| failed("failed to attach application menu", error))?;
        self.application_menu = Some(menu);
        Ok(())
    }

    pub fn set_context_menu(&mut self, template: &MenuTemplate) -> Result<(), NativeError> {
        self.context_menu = Some(build_menu(template)?);
        Ok(())
    }

    pub fn show_context_menu(&self, window: &Window) {
        if let Some(menu) = &self.context_menu {
            unsafe { menu.show_context_menu_for_hwnd(window.hwnd(), None) };
        }
    }

    pub fn create_tray(
        &mut self,
        id: String,
        spec: TraySpec,
        root: PathBuf,
    ) -> Result<(), NativeError> {
        let icon_path = url::Url::parse(&spec.icon)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok())
            .unwrap_or_else(|| root.join(&spec.icon));
        let icon = tray_icon::Icon::from_path(&icon_path, None).map_err(|error| {
            failed(
                &format!("failed to load tray icon {}", icon_path.display()),
                error,
            )
        })?;
        let mut builder = TrayIconBuilder::new().with_id(id.clone()).with_icon(icon);
        if let Some(tooltip) = spec.tooltip {
            builder = builder.with_tooltip(tooltip);
        }
        if let Some(template) = spec.menu {
            builder = builder.with_menu(Box::new(build_menu(&template)?));
        }
        let tray = builder
            .build()
            .map_err(|error| failed("failed to create tray icon", error))?;
        self.tray_icons.insert(id, tray);
        Ok(())
    }

    pub fn destroy_tray(&mut self, id: &str) -> Result<(), NativeError> {
        self.tray_icons
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| NativeError::Failed(format!("unknown tray icon {id}")))
    }

    pub fn register_shortcut(&mut self, accelerator: String) -> Result<(), NativeError> {
        let hotkey = accelerator
            .parse::<HotKey>()
            .map_err(|error| failed("invalid accelerator", error))?;
        self.hotkey_manager
            .register(hotkey)
            .map_err(|error| failed("shortcut registration failed", error))?;
        self.hotkeys.insert(hotkey.id(), (hotkey, accelerator));
        Ok(())
    }

    pub fn unregister_shortcut(&mut self, accelerator: &str) -> Result<(), NativeError> {
        let Some((id, (hotkey, _))) = self
            .hotkeys
            .iter()
            .find(|(_, (_, value))| value == accelerator)
            .map(|(id, value)| (*id, value.clone()))
        else {
            return Err(NativeError::Failed(format!(
                "shortcut is not registered: {accelerator}"
            )));
        };
        self.hotkey_manager
            .unregister(hotkey)
            .map_err(|error| failed("shortcut unregister failed", error))?;
        self.hotkeys.remove(&id);
        Ok(())
    }

    pub fn drain_events(&self, router: &ApiRouter) {
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            router
                .event_bus()
                .deliver("menu.clicked", &serde_json::json!({ "id": event.id().0 }));
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            router
                .event_bus()
                .deliver("tray.clicked", &serde_json::json!({ "id": event.id().0 }));
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed
                && let Some((_, accelerator)) = self.hotkeys.get(&event.id)
            {
                router.event_bus().deliver(
                    "shortcut.triggered",
                    &serde_json::json!({ "accelerator": accelerator }),
                );
            }
        }
    }
}

fn build_menu(template: &MenuTemplate) -> Result<Menu, NativeError> {
    let menu = Menu::new();
    append_menu_items(&menu, &template.items)?;
    Ok(menu)
}

fn append_menu_items(parent: &dyn MenuAppender, items: &[MenuItem]) -> Result<(), NativeError> {
    for item in items {
        match item {
            MenuItem::Normal {
                id,
                label,
                accelerator,
                enabled,
            } => {
                let accelerator = accelerator
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|error| failed("invalid menu accelerator", error))?;
                parent
                    .append_item(&NativeMenuItem::with_id(
                        id,
                        label,
                        enabled.unwrap_or(true),
                        accelerator,
                    ))
                    .map_err(|error| failed("failed to append menu item", error))?;
            }
            MenuItem::Checkbox {
                id,
                label,
                checked,
                accelerator,
            } => {
                let accelerator = accelerator
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(|error| failed("invalid menu accelerator", error))?;
                parent
                    .append_item(&CheckMenuItem::with_id(
                        id,
                        label,
                        true,
                        checked.unwrap_or(false),
                        accelerator,
                    ))
                    .map_err(|error| failed("failed to append menu item", error))?;
            }
            MenuItem::Separator => parent
                .append_item(&PredefinedMenuItem::separator())
                .map_err(|error| failed("failed to append menu separator", error))?,
            MenuItem::Submenu { id, label, items } => {
                let submenu = Submenu::with_id(id, label, true);
                append_menu_items(&submenu, items)?;
                parent
                    .append_item(&submenu)
                    .map_err(|error| failed("failed to append submenu", error))?;
            }
        }
    }
    Ok(())
}

trait MenuAppender {
    fn append_item(&self, item: &dyn muda::IsMenuItem) -> muda::Result<()>;
}

impl MenuAppender for Menu {
    fn append_item(&self, item: &dyn muda::IsMenuItem) -> muda::Result<()> {
        self.append(item)
    }
}

impl MenuAppender for Submenu {
    fn append_item(&self, item: &dyn muda::IsMenuItem) -> muda::Result<()> {
        self.append(item)
    }
}

fn failed(context: &str, error: impl std::fmt::Display) -> NativeError {
    NativeError::Failed(format!("{context}: {error}"))
}
