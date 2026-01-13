//! DL/T 645 protocol primitives.
//!
//! This module groups all low-level protocol types used by the driver, such as
//! frame encoding/decoding, DI utilities, protocol-level codecs and the
//! session abstraction.

pub mod codec;
pub mod error;
pub mod frame;
pub mod session;
