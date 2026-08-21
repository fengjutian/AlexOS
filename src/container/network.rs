//! Service-mode network allocator.

use std::net::TcpListener;

use thiserror::Error;

pub const SERVICE_PORT_RANGE_START: u16 = 28000;
pub const SERVICE_PORT_RANGE_END: u16 = 28999;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("no free port available in service range {start}-{end}", start = SERVICE_PORT_RANGE_START, end = SERVICE_PORT_RANGE_END)]
    NoFreePort,
}

pub fn allocate_loopback_port() -> Result<u16, NetworkError> {
    for candidate in SERVICE_PORT_RANGE_START..=SERVICE_PORT_RANGE_END {
        if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return Ok(candidate);
        }
    }
    Err(NetworkError::NoFreePort)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_returns_a_port_in_range() {
        let port = allocate_loopback_port().expect("range should have at least one free");
        assert!((SERVICE_PORT_RANGE_START..=SERVICE_PORT_RANGE_END).contains(&port));
    }
}
