use ng_gateway_sdk::{NorthwardData, NorthwardPublisher};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// A minimal, non-blocking publisher for benchmarking.
///
/// # Design
/// - Drivers may require a `NorthwardPublisher` in `SouthwardInitContext` (e.g. OPC UA subscribe mode).
/// - For benchmarks, we usually use `read_mode = Read`, so this publisher is effectively a sink.
/// - We still keep simple counters for observability.
#[derive(Debug, Default)]
pub struct NullPublisher {
    published_items: AtomicU64,
}

impl NorthwardPublisher for NullPublisher {
    #[inline]
    fn try_publish(&self, _data: Arc<NorthwardData>) -> ng_gateway_sdk::NorthwardResult<()> {
        self.published_items.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
