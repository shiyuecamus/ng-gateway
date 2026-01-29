//! Unified supervision foundation for southward drivers and northward plugins.
//!
//! This module provides the shared building blocks for a high-performance gateway:
//! - A structured, cheap-to-clone connection state model (`ConnectionState`)
//! - A lock-free handle publication mechanism (`HandleCell`)
//! - Low-frequency model conversion contracts (`model_convert`)
//!
//! The main goal is to ensure **one authoritative lifecycle semantics** across all
//! connection-oriented components while keeping the data-plane hot path:
//! - lock-free
//! - allocation-free (besides cheap `Arc` clones)
//! - predictable under load

pub mod converter;
pub mod handle;
pub mod r#loop;
pub mod observer;
pub mod protocol;
pub mod state;

pub use handle::HandleCell;
pub use observer::{
    NoopObserver, NoopObserverFactory, NorthwardObserverLabels, Observer, ObserverFactory,
    SouthwardObserverLabels,
};
pub use protocol::{Connector, ReconnectHandle, RunOutcome, Session, SessionContext};
pub use r#loop::{SupervisorHandle, SupervisorLoop, SupervisorParams};
pub use state::{
    ConnectionState, FailureKind, FailurePhase, FailureReport, Phase, RetryBudgetSnapshot,
};
