//! AI module domain models.
//!
//! Contains all AI-related domain types: pipeline definitions, model metadata,
//! algorithm ABI contracts, data types, and the public engine API trait.
//! These types form the public contract between the AI engine implementation
//! (`ng-gateway-ai`) and its consumers (drivers, web handlers, core gateway).
//!
//! Error types live in [`ng_gateway_error::ai`], configuration types live in
//! [`crate::settings`].

pub mod algorithm;
pub mod api;
pub mod model;
pub mod pipeline;
pub mod types;
