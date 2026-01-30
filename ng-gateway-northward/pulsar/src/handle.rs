//! Pulsar data-plane handle implementation.
//!
//! `PulsarHandle` is published by the SDK supervisor when the plugin attempt is Ready.
//! It MUST keep `process_data()` CPU-only and non-blocking: all Pulsar I/O is offloaded
//! to the session publisher task via a bounded queue.

use super::config::{EventUplink, PulsarPluginConfig};
use async_trait::async_trait;
use ng_gateway_sdk::{
    northward::{
        payload::{build_context_ref, encode_uplink_payload_ref, UplinkEventKind},
        runtime_api::NorthwardRuntimeApi,
        template::render_template_serde,
    },
    NorthwardData, NorthwardError, NorthwardHandle, NorthwardResult,
};
use pulsar::{producer, SerializeMessage};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;

/// Internal outbound message representation (produced on hot path).
#[derive(Debug, Clone)]
pub(crate) struct OutboundMessage {
    pub(crate) payload: Vec<u8>,
    pub(crate) partition_key: Option<String>,
    pub(crate) properties: HashMap<String, String>,
    pub(crate) event_time: Option<u64>,
}

impl SerializeMessage for OutboundMessage {
    fn serialize_message(input: Self) -> Result<producer::Message, pulsar::Error> {
        Ok(producer::Message {
            payload: input.payload,
            properties: input.properties,
            partition_key: input.partition_key,
            event_time: input.event_time,
            ..Default::default()
        })
    }
}

/// Internal outbound publish request (topic + message).
#[derive(Debug, Clone)]
pub(crate) struct OutboundPublish {
    pub(crate) topic: String,
    pub(crate) msg: OutboundMessage,
}

/// Pulsar data-plane handle (hot path).
pub struct PulsarHandle {
    config: Arc<PulsarPluginConfig>,
    app_id: i32,
    app_name: Arc<str>,
    plugin_type: Arc<str>,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    outbound_tx: mpsc::Sender<OutboundPublish>,
}

impl PulsarHandle {
    /// Create a new handle for a single supervision attempt.
    pub fn new(
        config: Arc<PulsarPluginConfig>,
        app_id: i32,
        app_name: String,
        runtime: Arc<dyn NorthwardRuntimeApi>,
        outbound_tx: mpsc::Sender<OutboundPublish>,
    ) -> Self {
        Self {
            config,
            app_id,
            app_name: Arc::from(app_name),
            plugin_type: Arc::<str>::from("pulsar"),
            runtime,
            outbound_tx,
        }
    }

    #[inline]
    fn select_uplink_mapping(
        &self,
        data: &NorthwardData,
    ) -> Option<(UplinkEventKind, &EventUplink)> {
        match data {
            NorthwardData::DeviceConnected(_) => Some((
                UplinkEventKind::DeviceConnected,
                &self.config.uplink.device_connected,
            )),
            NorthwardData::DeviceDisconnected(_) => Some((
                UplinkEventKind::DeviceDisconnected,
                &self.config.uplink.device_disconnected,
            )),
            NorthwardData::Telemetry(_) => {
                Some((UplinkEventKind::Telemetry, &self.config.uplink.telemetry))
            }
            NorthwardData::Attributes(_) => {
                Some((UplinkEventKind::Attributes, &self.config.uplink.attributes))
            }
            _ => None,
        }
    }
}

#[async_trait]
impl NorthwardHandle for PulsarHandle {
    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        if !self.config.uplink.enabled {
            return Ok(());
        }
        let Some((event_kind, mapping)) = self.select_uplink_mapping(data.as_ref()) else {
            return Ok(());
        };
        if !mapping.enabled {
            return Ok(());
        }

        let Some(ctx) = build_context_ref(
            self.app_id,
            &self.app_name,
            &self.plugin_type,
            event_kind,
            data.as_ref(),
            &self.runtime,
        ) else {
            return Ok(());
        };

        let topic = render_template_serde(mapping.topic.as_str(), &ctx);
        let key_rendered = render_template_serde(mapping.key.as_str(), &ctx);
        let key = if key_rendered.trim().is_empty() {
            None
        } else {
            Some(key_rendered)
        };

        let payload =
            encode_uplink_payload_ref(&mapping.payload, &ctx, data.as_ref(), &self.runtime)
                .map_err(|e| NorthwardError::SerializationError {
                    reason: e.to_string(),
                })?;

        let ts = ctx.ts.timestamp_millis() as u64;
        let msg = OutboundMessage {
            payload,
            partition_key: key,
            properties: ctx.to_properties_map(),
            event_time: Some(ts),
        };

        self.outbound_tx
            .try_send(OutboundPublish { topic, msg })
            .map_err(|e| NorthwardError::PublishFailed {
                platform: "pulsar".to_string(),
                reason: format!("outbound queue rejected: {e}"),
            })
    }
}
