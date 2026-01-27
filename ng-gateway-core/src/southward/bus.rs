//! Southward data bus (collector/southward -> gateway forwarding).
//!
//! This module provides a swappable sender handle so we can adjust the outbound queue capacity
//! at runtime without restarting the whole process.

use arc_swap::ArcSwap;
use ng_gateway_common::metrics::channel::InstrumentedSender;
use ng_gateway_sdk::NorthwardData;
use std::sync::Arc;

/// A swappable southward outbound sender shared by all producers.
///
/// # Design
/// - Producers hold an `Arc<SouthwardDataBus>` and load the current sender per send.
/// - On capacity change, the gateway swaps to a new sender; old senders naturally drain.
#[derive(Debug)]
pub struct SouthwardDataBus {
    sender: ArcSwap<InstrumentedSender<Arc<NorthwardData>>>,
}

impl SouthwardDataBus {
    #[inline]
    pub fn new(sender: InstrumentedSender<Arc<NorthwardData>>) -> Self {
        Self {
            sender: ArcSwap::from_pointee(sender),
        }
    }

    /// Load the current sender (cheap Arc clone).
    #[inline]
    pub fn sender(&self) -> Arc<InstrumentedSender<Arc<NorthwardData>>> {
        self.sender.load_full()
    }

    /// Swap the sender to a new instrumented channel.
    #[inline]
    pub fn swap_sender(&self, sender: InstrumentedSender<Arc<NorthwardData>>) {
        self.sender.store(Arc::new(sender));
    }
}
