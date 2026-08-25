//! Long-running Alex Runtime control plane.
//!
//! Transport implementations (Windows named pipe and Unix domain socket) sit
//! above this module. The protocol and durable desired state deliberately do
//! not depend on WebView or the native shell.

mod protocol;
mod proxy_client;
mod service;
mod state;
mod transport;

pub use protocol::{
    ControlCommand, ControlRequest, ControlResponse, MAX_PROXY_BODY_BYTES, PROTOCOL_VERSION,
    StreamControlError,
};
pub use proxy_client::{open_service_websocket, proxy_service_http};
pub use service::{DaemonService, RecoveryFailure, RecoveryReport};
pub use state::{
    AppControlState, DaemonState, DaemonStateError, DaemonStateStore, DesiredState, ObservedState,
    ServiceControlState,
};
pub use transport::{DEFAULT_PIPE_NAME, run_server, send_request};
