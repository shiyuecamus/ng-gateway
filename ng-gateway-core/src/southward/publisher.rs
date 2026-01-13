use ng_gateway_sdk::{NorthwardData, NorthwardError, NorthwardPublisher, NorthwardResult};
use std::sync::Arc;
use tokio::sync::mpsc::{error::TrySendError, Sender};

/// High-performance publisher backed by a bounded mpsc channel.
///
/// This implementation is non-blocking and backpressure-aware. It attempts to
/// send batches directly to the gateway's forwarding queue using `try_send` so
/// that producers (drivers) can implement their own retry/aggregation strategy
/// on saturation without blocking async tasks.
#[derive(Debug)]
pub struct MpscNorthwardPublisher {
    tx: Sender<Arc<NorthwardData>>,
}

impl MpscNorthwardPublisher {
    /// Create a new publisher from the gateway's batch sender.
    pub fn new(tx: Sender<Arc<NorthwardData>>) -> Self {
        Self { tx }
    }
}

impl NorthwardPublisher for MpscNorthwardPublisher {
    #[inline]
    fn try_publish(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        self.tx.try_send(data).map_err(|e| match e {
            TrySendError::Full(_) => NorthwardError::QueueFull,
            TrySendError::Closed(_) => NorthwardError::DataSendError {
                message: "Channel closed".to_string(),
            },
        })
    }
}
