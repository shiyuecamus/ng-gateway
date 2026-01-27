//! Northward events bus (plugins/apps -> gateway event processor).
//!
//! This module provides a swappable sender handle so we can adjust the northward events
//! queue capacity at runtime (rebuild bounded channel) without restarting the process.

use arc_swap::ArcSwap;
use ng_gateway_common::metrics::channel::InstrumentedSender;
use ng_gateway_sdk::NorthwardEvent;
use std::sync::Arc;

/// A swappable northward events sender shared by all producers.
///
/// Producers should load the sender per send (or per small batch) so a runtime swap
/// takes effect quickly and does not pin the old sender for long.
#[derive(Debug)]
pub struct NorthwardEventsBus {
    sender: ArcSwap<InstrumentedSender<(i32, NorthwardEvent)>>,
}

impl NorthwardEventsBus {
    #[inline]
    pub fn new(sender: InstrumentedSender<(i32, NorthwardEvent)>) -> Self {
        Self {
            sender: ArcSwap::from_pointee(sender),
        }
    }

    #[inline]
    pub fn sender(&self) -> Arc<InstrumentedSender<(i32, NorthwardEvent)>> {
        self.sender.load_full()
    }

    #[inline]
    pub fn swap_sender(&self, sender: InstrumentedSender<(i32, NorthwardEvent)>) {
        self.sender.store(Arc::new(sender));
    }
}
