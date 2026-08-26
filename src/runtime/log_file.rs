//! Compatibility facade for the standalone `alex-logging` package.
//!
//! Runtime code can keep importing this module while new consumers depend on
//! `alex-logging` directly.

pub use alex_logging::{LOG_FILE_MAX_BYTES, LogFileWriter, ServiceLogSink, redact_secrets};
