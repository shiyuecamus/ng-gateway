use std::{io, result::Result as StdResult};
use thiserror::Error as ThisError;

/// Unified MC protocol result type.
///
/// This mirrors the S7 `Error` design but is significantly simpler for the
/// initial MC implementation. Protocol layers (codec/session) should prefer
/// returning this type instead of bare `io::Error` so that the driver can
/// distinguish transport, protocol and PLC-level errors.
pub type Result<T> = StdResult<T, Error>;

/// MC protocol error type.
#[allow(unused)]
#[derive(Debug, ThisError)]
pub enum Error {
    /// Underlying I/O error from the transport.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Request timed out at the session layer.
    #[error("request timeout")]
    RequestTimeout,

    /// Frame-level validation failed (e.g. header length mismatch).
    #[error("invalid frame")]
    InvalidFrame,

    /// Protocol contract violated (e.g. reserved/invalid field values).
    #[error("protocol violation: {context}")]
    ProtocolViolation { context: &'static str },

    /// Encode error for wire-format serialization failures.
    #[error("encode error: {context}")]
    Encode { context: &'static str },

    /// Decode error for wire-format parsing failures that are not purely I/O.
    #[error("decode error: {context}")]
    Decode { context: &'static str },

    /// Feature or command is recognised but not yet implemented by this driver.
    #[error("unsupported feature: {feature}")]
    UnsupportedFeature { feature: &'static str },

    /// MC Ack header-level error with specific `end_code`.
    ///
    /// This is returned when a PLC responds with a non-zero completion code in
    /// the 3E/4E acknowledge header. Callers can inspect the raw `end_code`
    /// for diagnostics or mapping to higher-level error categories.
    #[error("MC end_code error: {code:#06x}")]
    EndCode { code: u16 },
}
