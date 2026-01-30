//! ThingsBoard data-plane handle implementation.
//!
//! `ThingsBoardHandle` is published by the SDK supervisor when the plugin is Connected/Ready.
//! It implements `ng_gateway_sdk::NorthwardHandle` for high-throughput uplink publishing.

use super::{
    config::{MessageFormat, ThingsBoardPluginConfig},
    payload,
    topics::Topics,
};
use ng_gateway_sdk::{NorthwardData, NorthwardError, NorthwardHandle, NorthwardResult};
use rumqttc::{AsyncClient, QoS};
use serde_json::json;
use std::sync::Arc;

/// ThingsBoard data-plane handle.
///
/// This handle is only published when the MQTT session is Ready.
pub struct ThingsBoardHandle {
    pub(crate) config: Arc<ThingsBoardPluginConfig>,
    pub(crate) client: AsyncClient,
}

impl ThingsBoardHandle {
    /// Create a new handle from config and an active MQTT client.
    pub fn new(config: Arc<ThingsBoardPluginConfig>, client: AsyncClient) -> Self {
        Self { config, client }
    }

    /// Borrow the underlying MQTT client.
    #[inline]
    pub fn client(&self) -> &AsyncClient {
        &self.client
    }

    #[inline]
    fn qos(&self) -> QoS {
        self.config.communication.qos()
    }

    #[inline]
    fn retain(&self) -> bool {
        self.config.communication.retain_messages
    }

    #[inline]
    fn max_payload_bytes(&self) -> usize {
        // Ensure non-zero to avoid infinite loops.
        self.config.communication.max_payload_bytes.max(256)
    }

    /// Hot-path telemetry publisher (Gateway API) that enforces `max_payload_bytes`.
    ///
    /// Payload shape:
    /// `{ "<device>": [ { "ts": <ms>, "values": { k1:v1, k2:v2, ... } } ] }`
    ///
    /// # Performance notes (best practice)
    /// - Single pass over point values.
    /// - No backtracking / no repeated full-map serialization.
    /// - Only per-point temporary serialization into a reusable scratch buffer.
    async fn publish_telemetry_chunked(
        &self,
        topic: &str,
        device_name: &str,
        ts_ms: i64,
        point_values: &[ng_gateway_sdk::PointValue],
    ) -> NorthwardResult<()> {
        let qos = self.qos();
        let retain = self.retain();
        let max_bytes = self.max_payload_bytes();

        let mut chunker = payload::TelemetryChunker::new(device_name, ts_ms, max_bytes)?;
        for pv in point_values.iter() {
            if let Some(out) = chunker.push(pv.point_key.as_ref(), &pv.value)? {
                self.client
                    .publish(topic, qos, retain, out)
                    .await
                    .map_err(|e| NorthwardError::MqttError {
                        reason: e.to_string(),
                    })?;
            }
        }
        if let Some(bytes) = chunker.finish() {
            self.client
                .publish(topic, qos, retain, bytes)
                .await
                .map_err(|e| NorthwardError::MqttError {
                    reason: e.to_string(),
                })?;
        }

        Ok(())
    }

    /// Hot-path attributes publisher (Gateway API) that enforces `max_payload_bytes`.
    ///
    /// Payload shape:
    /// `{ "<device>": { k1:v1, k2:v2, ... } }`
    async fn publish_attributes_chunked(
        &self,
        topic: &str,
        device_name: &str,
        point_values: &[ng_gateway_sdk::PointValue],
    ) -> NorthwardResult<()> {
        let qos = self.qos();
        let retain = self.retain();
        let max_bytes = self.max_payload_bytes();

        let mut chunker = payload::AttributesChunker::new(device_name, max_bytes)?;
        for pv in point_values.iter() {
            if let Some(out) = chunker.push(pv.point_key.as_ref(), &pv.value)? {
                self.client
                    .publish(topic, qos, retain, out)
                    .await
                    .map_err(|e| NorthwardError::MqttError {
                        reason: e.to_string(),
                    })?;
            }
        }
        if let Some(bytes) = chunker.finish() {
            self.client
                .publish(topic, qos, retain, bytes)
                .await
                .map_err(|e| NorthwardError::MqttError {
                    reason: e.to_string(),
                })?;
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl NorthwardHandle for ThingsBoardHandle {
    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        match self.config.communication.message_format {
            MessageFormat::Json => match data.as_ref() {
                NorthwardData::Telemetry(telemetry) => {
                    let topic = Topics::gateway_telemetry();
                    let ts = telemetry.timestamp.timestamp_millis();
                    self.publish_telemetry_chunked(
                        &topic,
                        &telemetry.device_name,
                        ts,
                        &telemetry.values,
                    )
                    .await?;
                }
                NorthwardData::Attributes(attributes) => {
                    let topic = Topics::gateway_attributes();
                    self.publish_attributes_chunked(
                        &topic,
                        &attributes.device_name,
                        &attributes.client_attributes,
                    )
                    .await?;
                }
                NorthwardData::DeviceConnected(device_connected) => {
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_connected.device_name,
                        "type": device_connected.device_type
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;

                    self.client
                        .publish(Topics::gateway_connect(), self.qos(), self.retain(), bytes)
                        .await
                        .map_err(|e| NorthwardError::MqttError {
                            reason: e.to_string(),
                        })?;
                }
                NorthwardData::DeviceDisconnected(device_disconnected) => {
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_disconnected.device_name
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;

                    self.client
                        .publish(
                            Topics::gateway_disconnect(),
                            self.qos(),
                            self.retain(),
                            bytes,
                        )
                        .await
                        .map_err(|e| NorthwardError::MqttError {
                            reason: e.to_string(),
                        })?;
                }
                _ => {
                    return Err(NorthwardError::RuntimeError {
                        reason: "Unsupported NorthwardData type for ThingsBoard".to_string(),
                    })
                }
            },
            MessageFormat::Protobuf => {
                return Err(NorthwardError::RuntimeError {
                    reason: "ThingsBoard proto format is not implemented".to_string(),
                })
            }
        }

        Ok(())
    }
}
