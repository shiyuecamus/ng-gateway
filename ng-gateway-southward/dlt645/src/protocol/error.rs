use std::time::Duration;
use thiserror::Error;

/// DL/T645 exception code carried in the data field when D6 = 1.
///
/// When a slave responds with the exception flag set, the first byte of the
/// data field usually encodes a more specific reason for the failure. This
/// enumeration models a conservative subset of commonly used codes and keeps a
/// raw variant for vendor-specific extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dl645ExceptionCode {
    /// Illegal command or unsupported function.
    IllegalCommand,
    /// Illegal data or parameter out of range.
    IllegalData,
    /// Operation not allowed in the current meter state.
    OperationNotAllowed,
    /// Authentication or password failure.
    AuthenticationFailed,
    /// Requested data object is not supported.
    ObjectNotSupported,
    /// Vendor-specific error code in the range 0x80..=0xFF.
    Vendor(u8),
    /// Any other unrecognized error code.
    Unknown(u8),
}

impl Dl645ExceptionCode {
    /// Decode an 8-bit value into an exception code.
    ///
    /// The mapping is intentionally conservative; codes that are not widely
    /// standardized are treated as `Unknown` while the upper vendor range is
    /// preserved using the `Vendor` variant.
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Dl645ExceptionCode::IllegalCommand,
            0x02 => Dl645ExceptionCode::IllegalData,
            0x03 => Dl645ExceptionCode::OperationNotAllowed,
            0x04 => Dl645ExceptionCode::AuthenticationFailed,
            0x05 => Dl645ExceptionCode::ObjectNotSupported,
            other @ 0x80..=0xFF => Dl645ExceptionCode::Vendor(other),
            other => Dl645ExceptionCode::Unknown(other),
        }
    }
}

/// Protocol-level error type for DL/T 645.
///
/// This error is used inside the protocol module only. It intentionally
/// distinguishes between codec/structural failures, device-level exceptions,
/// semantic violations and transport/timeout issues so that higher layers can
/// map them into their own error domains as needed.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Frame has invalid structure (header, length, or tail).
    #[error("Invalid frame: {0}")]
    InvalidFrame(String),
    /// Checksum does not match the payload.
    #[error("Checksum mismatch")]
    ChecksumMismatch,
    /// Unsupported or invalid control code.
    #[error("Invalid control code: {0:#X}")]
    InvalidControl(u8),
    /// Frame exceeds configured size limit.
    #[error("Frame size exceeds limit: {0}")]
    FrameTooLarge(usize),
    /// Device responded with an application-level exception.
    #[error("DL/T645 response exception: {0:?}")]
    Exception(Dl645ExceptionCode),
    /// Semantic protocol violation in a structurally valid frame sequence.
    ///
    /// Examples include mismatched addresses between subsequent frames of a
    /// logical response or unsupported function codes in this context.
    #[error("DL/T645 semantic error: {0}")]
    Semantic(String),
    /// Per-request timeout while waiting for a frame on the wire.
    #[error("DL/T645 timeout after {0:?}")]
    Timeout(Duration),
    /// Transport or IO-level failure (serial/TCP).
    #[error("DL/T645 transport error: {0}")]
    Transport(String),
    /// Underlying IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
