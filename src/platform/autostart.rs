//! Per-user Windows login startup integration.
//!
//! Entries live under HKCU so enabling startup never requires elevation. Each
//! application receives its own deterministic value and starts through the
//! Alex CLI/daemon rather than executing package code directly.

use std::io;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn value_name(app_id: &str) -> io::Result<String> {
    if app_id.is_empty()
        || !app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "application id is not safe for an autostart value",
        ));
    }
    Ok(format!("Alex.{app_id}"))
}

#[cfg(windows)]
pub fn is_enabled(app_id: &str) -> io::Result<bool> {
    let name = value_name(app_id)?;
    let status = std::process::Command::new("reg.exe")
        .args(["query", RUN_KEY, "/v", &name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(status.success())
}

#[cfg(windows)]
pub fn set_enabled(app_id: &str, enabled: bool) -> io::Result<()> {
    let name = value_name(app_id)?;
    if !enabled {
        if !is_enabled(app_id)? {
            return Ok(());
        }
        let status = std::process::Command::new("reg.exe")
            .args(["delete", RUN_KEY, "/v", &name, "/f"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        return status
            .success()
            .then_some(())
            .ok_or_else(|| io::Error::other("reg.exe could not remove the autostart entry"));
    }

    let executable = std::env::current_exe()?;
    let command = format!("\"{}\" start {}", executable.display(), app_id);
    let status = std::process::Command::new("reg.exe")
        .args([
            "add", RUN_KEY, "/v", &name, "/t", "REG_SZ", "/d", &command, "/f",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("reg.exe could not create the autostart entry"))
}

#[cfg(not(windows))]
pub fn is_enabled(app_id: &str) -> io::Result<bool> {
    let _ = value_name(app_id)?;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "autostart is currently supported only on Windows",
    ))
}

#[cfg(not(windows))]
pub fn set_enabled(app_id: &str, _enabled: bool) -> io::Result<()> {
    let _ = value_name(app_id)?;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "autostart is currently supported only on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_names_are_deterministic_and_reject_injection() {
        assert_eq!(
            value_name("com.example.notes").unwrap(),
            "Alex.com.example.notes"
        );
        assert!(value_name("bad /v injected").is_err());
        assert!(value_name("").is_err());
    }
}
