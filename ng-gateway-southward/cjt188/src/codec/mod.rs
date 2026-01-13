//! CJ/T 188 Codec Layer
//!
//! This module provides encoding and decoding functionality for CJ/T 188 protocol data.
//!
//! # Module Organization
//!
//! - `di_types`: DI response type definitions
//! - `di_schema`: DI response schema registry
//! - `di_parser`: DI response parser implementation
//!
//! # Architecture
//!
//! The codec layer sits between the protocol layer (which handles frame structure)
//! and the driver layer (which handles business logic). It's responsible for:
//!
//! - Converting between protocol bytes and typed values (`NGValue`)
//! - Schema-driven parsing of DI responses
//! - BCD/Binary/DateTime encoding/decoding
//!
//! This layer depends on `ng_gateway_sdk` for the `NGValue` type, which is
//! the unified value representation used throughout the gateway.

pub mod di_parser;
pub mod di_schema;
pub mod di_types;

// Re-export commonly used types
pub use di_parser::{DIResponseParser, ParsedDIResponse, ParsedDIValue};
pub use di_schema::{get_di_schema, DISchemaKey, DI_SCHEMA_REGISTRY};
pub use di_types::{DIResponse, DIResponseSchema, DataFormat, ResponseField};
