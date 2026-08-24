use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
};

use windows::Win32::{
    Foundation::{ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE},
    Storage::FileSystem::PIPE_ACCESS_DUPLEX,
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    },
};
use windows::core::PCWSTR;

use crate::daemon::{ControlRequest, ControlResponse, DaemonService};

const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;

pub fn run_server(service: DaemonService, pipe_name: &str) -> std::io::Result<()> {
    validate_pipe_name(pipe_name)?;
    loop {
        let file = create_connected_pipe(pipe_name)?;
        if let Err(error) = serve_connection(&service, file) {
            eprintln!("alexd: client connection failed: {error}");
        }
    }
}

fn create_connected_pipe(pipe_name: &str) -> std::io::Result<File> {
    let name: Vec<u16> = std::ffi::OsStr::new(pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    if let Err(error) = unsafe { ConnectNamedPipe(handle, None) }
        && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
    {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        return Err(std::io::Error::other(error.to_string()));
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

fn serve_connection(service: &DaemonService, file: File) -> std::io::Result<()> {
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
        let response = if read > MAX_CONTROL_LINE_BYTES || !line.ends_with('\n') {
            ControlResponse::failure("unknown", "control request exceeds 1 MiB")
        } else {
            match serde_json::from_str::<ControlRequest>(line.trim_end()) {
                Ok(request) => service.handle(request),
                Err(error) => {
                    ControlResponse::failure("unknown", format!("invalid request: {error}"))
                }
            }
        };
        serde_json::to_writer(&mut reader.get_mut(), &response)?;
        reader.get_mut().write_all(b"\n")?;
        reader.get_mut().flush()?;
        if read > MAX_CONTROL_LINE_BYTES || !line.ends_with('\n') {
            return Ok(());
        }
    }
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
