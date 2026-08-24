//! Opt-in Windows desktop smoke test.
//! Run with `ALEX_RUN_NATIVE_GUI_TESTS=1 cargo test --test native_gui -- --nocapture`.

#![cfg(windows)]

#[test]
#[ignore = "requires an interactive Windows desktop; run with --ignored"]
fn production_shell_creates_a_real_top_level_window() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{Foundation::{BOOL, HWND, LPARAM}, UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE}},
    };

    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/desktop-api");
    let mut child = Command::new(env!("CARGO_BIN_EXE_alex"))
        .arg("run")
        .arg(package)
        .spawn()
        .expect("launch production shell");
    struct Search { pid: u32, hwnd: Option<HWND> }
    unsafe extern "system" fn find_owned_window(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)); }
        if pid == search.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            search.hwnd = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let hwnd = loop {
        let mut search = Search { pid: child.id(), hwnd: None };
        let _ = unsafe { EnumWindows(Some(find_owned_window), LPARAM((&mut search as *mut Search) as isize)) };
        if let Some(hwnd) = search.hwnd {
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
