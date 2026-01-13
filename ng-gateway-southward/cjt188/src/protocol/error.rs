use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Frame too short: {0}")]
    FrameTooShort(String),

    #[error("Invalid frame: {0}")]
    InvalidFrame(String),

    #[error("Checksum mismatch: expected {expected:#04X}, calculated {calculated:#04X}")]
    ChecksumMismatch { expected: u8, calculated: u8 },

    #[error("Semantic error: {0}")]
    Semantic(String),

    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Timeout awaiting response")]
    Timeout(String),
}
