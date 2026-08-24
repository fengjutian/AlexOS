use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
        LocalFree,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::PIPE_ACCESS_DUPLEX,
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    },
};
use windows::core::{PCWSTR, PWSTR};

use crate::daemon::{ControlRequest, ControlResponse, DaemonService};

const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONCURRENT_CLIENTS: usize = 32;

pub fn run_server(service: DaemonService, pipe_name: &str) -> std::io::Result<()> {
    validate_pipe_name(pipe_name)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicUsize::new(0));
    while !shutdown.load(Ordering::Acquire) {
        let file = create_connected_pipe(pipe_name)?;
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CLIENTS {
            active.fetch_sub(1, Ordering::AcqRel);
            reject_busy(file);
            continue;
        }
        let service = service.clone();
        let shutdown = Arc::clone(&shutdown);
        let active = Arc::clone(&active);
        let pipe_name = pipe_name.to_owned();
        std::thread::spawn(move || {
            let result = serve_connection(&service, file, &shutdown);
            active.fetch_sub(1, Ordering::AcqRel);
            if let Err(error) = result {
                eprintln!("alexd: client connection failed: {error}");
            }
            if shutdown.load(Ordering::Acquire) {
                let _ = connect_client(&pipe_name);
            }
        });
    }
    Ok(())
}

pub fn send_request(pipe_name: &str, request: &ControlRequest) -> std::io::Result<ControlResponse> {
    validate_pipe_name(pipe_name)?;
    let mut pipe = connect_client(pipe_name)?;
    serde_json::to_writer(&mut pipe, request)?;
    pipe.write_all(b"\n")?;
    pipe.flush()?;

    let mut reader = BufReader::new(pipe);
    let mut line = String::new();
    let read = reader
        .by_ref()
        .take((MAX_CONTROL_LINE_BYTES + 1) as u64)
        .read_line(&mut line)?;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "alexd closed the pipe without a response",
        ));
    }
    if read > MAX_CONTROL_LINE_BYTES || !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "alexd response exceeds 1 MiB or is incomplete",
        ));
    }
    serde_json::from_str(line.trim_end()).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid alexd response: {error}"),
        )
    })
}

fn connect_client(pipe_name: &str) -> std::io::Result<File> {
    let started = Instant::now();
    loop {
        match OpenOptions::new().read(true).write(true).open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            Err(error) if started.elapsed() < CONNECT_TIMEOUT => {
                // ERROR_FILE_NOT_FOUND (daemon is creating the first instance)
                // and ERROR_PIPE_BUSY (an instance is serving another client)
                // are both transient during the bounded connection window.
                if matches!(error.raw_os_error(), Some(2) | Some(231)) {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
                return Err(error);
            }
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!("cannot connect to alexd at {pipe_name}: {error}"),
                ));
            }
        }
    }
}

fn create_connected_pipe(pipe_name: &str) -> std::io::Result<File> {
    let name: Vec<u16> = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let security = PipeSecurity::for_current_user()?;
    let attributes = security.attributes();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            Some(&attributes),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    if let Err(error) = unsafe { ConnectNamedPipe(handle, None) }
        && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
    {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(std::io::Error::other(error.to_string()));
    }
    if let Err(error) = verify_same_user_client(handle) {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(error);
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
}

impl PipeSecurity {
    fn for_current_user() -> std::io::Result<Self> {
        let sid = current_user_sid_string()?;
        // Protected DACL: only LocalSystem and the daemon's current user get
        // generic-all access. No inherited or broad Authenticated Users ACE.
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})");
        let wide: Vec<u16> = std::ffi::OsStr::new(&sddl)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(win32_io)?;
        Ok(Self { descriptor })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

fn current_user_sid_string() -> std::io::Result<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(win32_io)?;
    let result = token_user_sid_string(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

fn verify_same_user_client(pipe: HANDLE) -> std::io::Result<()> {
    let mut pid = 0;
    unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) }.map_err(win32_io)?;
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(win32_io)?;
    let mut token = HANDLE::default();
    let token_result = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    unsafe {
        let _ = CloseHandle(process);
    }
    token_result.map_err(win32_io)?;
    let client_sid = token_user_sid_string(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    if client_sid? == current_user_sid_string()? {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "alexd rejected a named-pipe client owned by another user",
        ))
    }
}

fn token_user_sid_string(token: HANDLE) -> std::io::Result<String> {
    let mut required = 0;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
    if required == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; required as usize];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )
    }
    .map_err(win32_io)?;
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_string = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) }.map_err(win32_io)?;
    let result = unsafe { sid_string.to_string() }.map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Windows SID is not valid UTF-16: {error}"),
        )
    });
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sid_string.0.cast())));
    }
    result
}

fn win32_io(error: windows::core::Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn serve_connection(
    service: &DaemonService,
    file: File,
    shutdown: &AtomicBool,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(file);
    loop {
        let mut line = String::new();
        let read = reader
            .by_ref()
            .take((MAX_CONTROL_LINE_BYTES + 1) as u64)
            .read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        let (response, requests_shutdown) =
            if read > MAX_CONTROL_LINE_BYTES || !line.ends_with('\n') {
                (
                    ControlResponse::failure("unknown", "control request exceeds 1 MiB"),
                    false,
                )
            } else {
                match serde_json::from_str::<ControlRequest>(line.trim_end()) {
                    Ok(request) => {
                        let requests_shutdown =
                            matches!(request.command, crate::daemon::ControlCommand::Shutdown);
                        (service.handle(request), requests_shutdown)
                    }
                    Err(error) => (
                        ControlResponse::failure("unknown", format!("invalid request: {error}")),
                        false,
                    ),
                }
            };
        serde_json::to_writer(&mut reader.get_mut(), &response)?;
        reader.get_mut().write_all(b"\n")?;
        reader.get_mut().flush()?;
        if requests_shutdown && response.ok {
            shutdown.store(true, Ordering::Release);
            return Ok(());
        }
        if read > MAX_CONTROL_LINE_BYTES || !line.ends_with('\n') {
            return Ok(());
        }
    }
}

fn reject_busy(mut file: File) {
    let response = ControlResponse::failure("unknown", "alexd has too many concurrent clients");
    let _ = serde_json::to_writer(&mut file, &response);
    let _ = file.write_all(b"\n");
    let _ = file.flush();
}

fn validate_pipe_name(pipe_name: &str) -> std::io::Result<()> {
    if pipe_name.starts_with(r"\\.\pipe\")
        && pipe_name.len() <= 240
        && !pipe_name[9..].is_empty()
        && !pipe_name.contains('\0')
    {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pipe name must be a local \\\\.\\pipe\\ name",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_are_local_and_bounded() {
        assert!(validate_pipe_name(super::super::DEFAULT_PIPE_NAME).is_ok());
        assert!(validate_pipe_name(r"\\server\pipe\alex").is_err());
        assert!(validate_pipe_name(r"\\.\pipe\").is_err());
    }
}
