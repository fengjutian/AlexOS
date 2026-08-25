//! Runtime capability view generated from the Desktop API IDL.

pub fn available(native: crate::native::NativeHostCapabilities) -> Vec<String> {
    let mut result = super::idl_generated::ALWAYS
        .iter()
        .map(|v| (*v).to_owned())
        .collect::<Vec<_>>();
    for method in super::idl_generated::NATIVE_DESKTOP {
        let enabled = if method.starts_with("window.") {
            native.secondary_windows
        } else if method.starts_with("menu.") {
            native.menus
        } else if method.starts_with("tray.") {
            native.tray
        } else if method.starts_with("shortcuts.") {
            native.shortcuts
        } else {
            false
        };
        if enabled {
            result.push((*method).to_owned());
        }
    }
    result
}

pub fn experimental() -> Vec<String> {
    super::idl_generated::EXPERIMENTAL
        .iter()
        .map(|v| (*v).to_owned())
        .collect()
}
