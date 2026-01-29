//! ThingsBoard data-plane handle implementation.
//!
//! `ThingsBoardHandle` is published by the SDK supervisor when the plugin is Connected/Ready.
//! It implements `ng_gateway_sdk::NorthwardHandle` for high-throughput uplink publishing.

use super::config::{MessageFormat, ThingsBoardPluginConfig};
use super::topics::Topics;
use ng_gateway_sdk::{
    NGValueJsonOptions, NorthwardData, NorthwardError, NorthwardHandle, NorthwardResult,
};
use rumqttc::{AsyncClient, QoS};
use serde_json::{json, Map, Value};
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
}

#[async_trait::async_trait]
impl NorthwardHandle for ThingsBoardHandle {
    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
        let qos = self.qos();

        let (topic, payload) = match self.config.communication.message_format {
            MessageFormat::Json => match data.as_ref() {
                NorthwardData::Telemetry(telemetry) => {
                    let topic = Topics::gateway_telemetry();
                    let mut root = Map::with_capacity(1);
                    let mut values = Map::with_capacity(telemetry.values.len());
                    for pv in telemetry.values.iter() {
                        let key = pv.point_key.as_ref().to_string();
                        let v = pv.value.to_json_value(NGValueJsonOptions::default());
                        values.insert(key, v);
                    }
                    let entry = json!({
                        "ts": telemetry.timestamp.timestamp_millis(),
                        "values": Value::Object(values)
                    });
                    root.insert(telemetry.device_name.clone(), Value::Array(vec![entry]));
                    let bytes = serde_json::to_vec(&Value::Object(root)).map_err(|e| {
                        NorthwardError::SerializationError {
                            reason: e.to_string(),
                        }
                    })?;
                    (topic, bytes)
                }
                NorthwardData::Attributes(attributes) => {
                    let topic = Topics::gateway_attributes();
                    let mut root = Map::with_capacity(1);
                    let mut device_attrs = Map::with_capacity(attributes.client_attributes.len());
                    for pv in attributes.client_attributes.iter() {
                        let key = pv.point_key.as_ref().to_string();
                        let v = pv.value.to_json_value(NGValueJsonOptions::default());
                        device_attrs.insert(key, v);
                    }
                    root.insert(attributes.device_name.clone(), Value::Object(device_attrs));
                    let bytes = serde_json::to_vec(&Value::Object(root)).map_err(|e| {
                        NorthwardError::SerializationError {
                            reason: e.to_string(),
                        }
                    })?;
                    (topic, bytes)
                }
                NorthwardData::DeviceConnected(device_connected) => {
                    let topic = Topics::gateway_connect();
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_connected.device_name,
                        "type": device_connected.device_type
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;
                    (topic, bytes)
                }
                NorthwardData::DeviceDisconnected(device_disconnected) => {
                    let topic = Topics::gateway_disconnect();
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_disconnected.device_name
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;
                    (topic, bytes)
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
        };

        self.client
            .publish(topic, qos, false, payload)
            .await
            .map_err(|e| NorthwardError::MqttError {
                reason: e.to_string(),
            })?;

        Ok(())
    }
}
