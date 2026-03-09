//! Southward supervision observers (core-owned).
//!
//! This module implements the "ObserverConsumer" design for southward channels:
//! the SDK supervision loop owns lifecycle semantics, while core attaches side effects
//! (metrics updates, index updates, and northward device events) through `Observer`.
//!
//! # Performance & safety
//! - Called on **control-plane** transitions only (low frequency).
//! - MUST be non-blocking: uses `try_send` for event emission.
//! - Avoids allocations on hot paths; minor allocations are acceptable here.

use super::{bus::SouthwardDataBus, index::RuntimeIndex};
use crate::observer::{CompositeObserver, CoreObserverFactory};
use chrono::Utc;
use ng_gateway_common::metrics::{southward::SouthwardChannelMetricHandles, NGMetricsHub};
use ng_gateway_sdk::{
    supervision::{Observer, ObserverFactory, RetryBudgetSnapshot, SouthwardObserverLabels},
    ConnectionState, DeviceConnectedData, DeviceDisconnectedData, NorthwardData, Phase,
    SouthwardTransportMeter,
};
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

/// Per-channel bound transport meter (hot-path friendly).
///
/// This avoids any map lookups or string allocations on I/O hot paths by binding the
/// meter to pre-resolved channel metric handles.
#[derive(Debug)]
pub struct ChannelBoundTransportMeter {
    prom: Arc<SouthwardChannelMetricHandles>,
}

impl ChannelBoundTransportMeter {
    #[inline]
    pub fn new(prom: Arc<SouthwardChannelMetricHandles>) -> Self {
        Self { prom }
    }
}

impl SouthwardTransportMeter for ChannelBoundTransportMeter {
    #[inline]
    fn add_bytes_in(&self, bytes: u64) {
        self.prom.add_bytes_received(bytes);
    }

    #[inline]
    fn add_bytes_out(&self, bytes: u64) {
        self.prom.add_bytes_sent(bytes);
    }
}

/// Per-channel observer factory for southward drivers.
#[derive(Clone)]
pub(crate) struct SouthwardChannelObserverFactory {
    channel_id: i32,
    prom: Arc<SouthwardChannelMetricHandles>,
    metrics_hub: Arc<NGMetricsHub>,
    index: Arc<RuntimeIndex>,
    outbound: Arc<SouthwardDataBus>,
}

impl SouthwardChannelObserverFactory {
    /// Create a per-channel observer factory.
    pub(crate) fn new(
        channel_id: i32,
        prom: Arc<SouthwardChannelMetricHandles>,
        metrics_hub: Arc<NGMetricsHub>,
        index: Arc<RuntimeIndex>,
        outbound: Arc<SouthwardDataBus>,
    ) -> Self {
        Self {
            channel_id,
            prom,
            metrics_hub,
            index,
            outbound,
        }
    }
}

impl ObserverFactory for SouthwardChannelObserverFactory {
    fn create_southward(&self, labels: SouthwardObserverLabels) -> Arc<dyn Observer> {
        // Best-effort sanity check without panicking.
        if labels.channel_id != self.channel_id {
            tracing::warn!(
                expected_channel_id = self.channel_id,
                got_channel_id = labels.channel_id,
                "southward observer labels mismatch"
            );
        }
        let side_effects: Arc<dyn Observer> = Arc::new(SouthwardChannelObserver {
            channel_id: self.channel_id,
            prom: Arc::clone(&self.prom),
            metrics_hub: Arc::clone(&self.metrics_hub),
            index: Arc::clone(&self.index),
            outbound: Arc::clone(&self.outbound),
            last_phase: AtomicU8::new(u8::from(Phase::Disconnected)),
            last_connected: AtomicU8::new(0),
        });

        let logging: Arc<dyn Observer> = CoreObserverFactory::new().create_southward(labels);
        Arc::new(CompositeObserver::new(vec![logging, side_effects]))
    }
}

struct SouthwardChannelObserver {
    channel_id: i32,
    prom: Arc<SouthwardChannelMetricHandles>,
    metrics_hub: Arc<NGMetricsHub>,
    index: Arc<RuntimeIndex>,
    outbound: Arc<SouthwardDataBus>,
    last_phase: AtomicU8,
    last_connected: AtomicU8,
}

impl Observer for SouthwardChannelObserver {
    fn on_state(&self, state: &ConnectionState) {
        let now = u8::from(state.phase);
        let prev = self.last_phase.swap(now, Ordering::AcqRel);
        if prev == now {
            return;
        }

        // Update manager snapshot state & activity timestamps (best-effort).
        if let Some(mut entry) = self.index.channels.get_mut(&self.channel_id) {
            entry.set_state(Arc::new(state.clone()));
            entry.touch_activity(Utc::now());
        }

        let is_connected = state.is_connected();
        let was_connected = self
            .last_connected
            .swap(if is_connected { 1 } else { 0 }, Ordering::AcqRel)
            != 0;
        if is_connected && !was_connected {
            self.metrics_hub.inc_southward_connected_channels();
        } else if !is_connected && was_connected {
            self.metrics_hub.dec_southward_connected_channels();
        }

        // Update Prometheus gauges and reconnect counters.
        let now_ms = Utc::now().timestamp_millis().max(0) as u64;
        self.prom.set_connected(is_connected);
        self.prom.set_state_value(state.as_value());
        self.prom.record_state_change_ms(now_ms);

        if state.is_reconnecting() && prev != u8::from(Phase::Reconnecting) {
            self.prom.inc_reconnect();
        }
        if state.is_failed() && prev != u8::from(Phase::Failed) {
            self.prom.inc_connect_failed();
        }
        if was_connected && (state.is_disconnected() || state.is_failed()) {
            self.prom.inc_disconnect();
        }

        // Emit device lifecycle events on boundary transitions.
        match state.phase {
            Phase::Connected => {
                self.emit_device_events(true);
            }
            Phase::Disconnected | Phase::Failed => {
                self.emit_device_events(false);
            }
            _ => {}
        }
    }

    fn on_failure(&self, _report: &ng_gateway_sdk::FailureReport) {
        // Failure counts are handled by phase transitions; keep this as a hook for future use.
    }

    fn on_backoff(&self, _delay: Duration, _budget: &RetryBudgetSnapshot) {}
}

impl SouthwardChannelObserver {
    #[inline]
    fn emit_device_events(&self, connected: bool) {
        let Some(set_ref) = self.index.channel_devices.get(&self.channel_id) else {
            return;
        };

        let tx = self.outbound.sender();
        for device_id in set_ref.iter().copied() {
            let Some(dev) = self.index.devices.get(&device_id) else {
                continue;
            };
            let (device_name, device_type) = (
                dev.config.device_name().to_string(),
                dev.config.device_type().to_string(),
            );

            let event = if connected {
                NorthwardData::DeviceConnected(DeviceConnectedData {
                    device_id,
                    device_name,
                    device_type,
                })
            } else {
                NorthwardData::DeviceDisconnected(DeviceDisconnectedData {
                    device_id,
                    device_name,
                    device_type,
                })
            };

            // Best-effort, non-blocking.
            let _ = tx.try_send(Arc::new(event));
        }
    }
}
