use ng_gateway_common::metrics::{
    channel::InstrumentedSender, southward::SouthwardChannelMetricHandles,
};
use ng_gateway_sdk::{NorthwardData, NorthwardError, NorthwardPublisher, NorthwardResult};
use std::sync::Arc;
use tokio::sync::mpsc::error::TrySendError;

/// High-performance publisher backed by a bounded mpsc channel.
///
/// This implementation is non-blocking and backpressure-aware. It attempts to
/// send batches directly to the gateway's forwarding queue using `try_send` so
/// that producers (drivers) can implement their own retry/aggregation strategy
/// on saturation without blocking async tasks.
#[derive(Debug)]
pub struct MpscNorthwardPublisher {
    tx: InstrumentedSender<Arc<NorthwardData>>,
    prom: Arc<SouthwardChannelMetricHandles>,
}

impl MpscNorthwardPublisher {
    /// Create a publisher and bind it to a southward channel metrics handle.
    ///
    /// This enables per-channel **report/push** metrics for drivers that actively publish
    /// data via `publisher.try_publish(...)` (Report/Subscribe mode).
    pub fn new(
        tx: InstrumentedSender<Arc<NorthwardData>>,
        prom: Arc<SouthwardChannelMetricHandles>,
    ) -> Self {
        Self { tx, prom }
    }
}

impl NorthwardPublisher for MpscNorthwardPublisher {
    #[inline]
    fn try_publish(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        // Best practice: only count Report publish for actual uplink data
        // (Telemetry/Attributes/Alarm), not for control-plane responses.
        let should_count = matches!(
            data.as_ref(),
            NorthwardData::Telemetry(_) | NorthwardData::Attributes(_) | NorthwardData::Alarm(_)
        );
        let device_id = if should_count { data.device_id() } else { 0 };
        match self.tx.try_send(data) {
            Ok(()) => {
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                if should_count {
                    self.prom.record_device_report_success(device_id, now_ms);
                }
                Ok(())
            }
            Err(e) => {
                let err = match e {
                    TrySendError::Full(_) => NorthwardError::QueueFull,
                    TrySendError::Closed(_) => NorthwardError::DataSendError {
                        message: "Channel closed".to_string(),
                    },
                };
                let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                if should_count {
                    match &err {
                        NorthwardError::QueueFull => {
                            self.prom.record_device_report_dropped(device_id, now_ms);
                        }
                        _ => {
                            self.prom.record_device_report_fail(device_id, now_ms);
                        }
                    }
                }
                Err(err)
            }
        }
    }
}
