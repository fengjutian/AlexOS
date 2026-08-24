//! Opt-in Windows desktop smoke test.
//! Run with `ALEX_RUN_NATIVE_GUI_TESTS=1 cargo test --test native_gui -- --nocapture`.

#![cfg(windows)]

#[test]
fn production_shell_creates_a_real_top_level_window() {
    if std::env::var_os("ALEX_RUN_NATIVE_GUI_TESTS").is_none() {
        eprintln!("skipped: set ALEX_RUN_NATIVE_GUI_TESTS=1 in an interactive Windows session");
        return;
    }
    use std::{
        os::windows::ffi::OsStrExt,
        process::Command,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE},
        core::PCWSTR,
    };

    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/desktop-api");
    let mut child = Command::new(env!("CARGO_BIN_EXE_alex"))
        .arg("run")
        .arg(package)
        .spawn()
        .expect("launch production shell");
    let title: Vec<u16> = std::ffi::OsStr::new("Desktop API Demo")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    let hwnd = loop {
        if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) } {
            break hwnd;
        }
        assert!(
            Instant::now() < deadline,
            "Alex shell window did not appear within 15 seconds"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default()) };
    let _ = child.wait();
}
