use super::{
    frame::asdu::{CauseOfTransmission, TypeID},
    session::Request,
};
use anyhow::anyhow;
use std::result::Result as StdResult;
use thiserror::Error;

pub type Result<T> = StdResult<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("connect timeout")]
    ErrConnectTimeout,
    #[error("asdu: [type identifier: {0:?}] doesn't match call or time tag")]
    ErrTypeIDNotMatch(TypeID),
    #[error("asdu: [cause of transmission: {0:?}] for command not standard requirement")]
    ErrCmdCause(CauseOfTransmission),

    #[error("Invalid frame")]
    ErrInvalidFrame,

    #[error("SendError {0}")]
    ErrSendRequest(#[from] tokio::sync::mpsc::error::SendError<Request>),

    #[error("send request timed out")]
    ErrSendRequestTimeout,

    #[error("send request cancelled")]
    ErrSendRequestCancelled,

    #[error("")]
    ErrUseClosedConnection,
    #[error("")]
    ErrNotActive,

    #[error("anyhow error")]
    ErrAnyHow(#[from] anyhow::Error),
}

impl From<()> for Error {
    fn from(_: ()) -> Self {
        Error::ErrAnyHow(anyhow!("conversion error"))
    }
}
