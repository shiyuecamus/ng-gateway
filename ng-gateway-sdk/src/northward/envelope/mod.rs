//! Northward envelope protocol definitions.
//!
//! This module is intentionally split into two layers:
//! - **wire**: stable, versioned JSON shape used on the boundary between gateway and plugins.
//! - **northward**: strongly-typed payloads with automatic kind derivation and validation.
//!
//! # Design goals
//! - Keep the **wire format stable** (no enum tags inside `payload.data`).
//! - Use an explicit discriminator (`event.kind`) for routing and mixed-topic scenarios.
//! - Preserve forward compatibility by allowing unknown kinds to round-trip as raw JSON.

mod northward;
mod wire;

pub use northward::*;
pub use wire::*;
