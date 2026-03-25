//! ThingsBoard data-plane handle implementation.
//!
//! `ThingsBoardHandle` is published by the SDK supervisor when the plugin is Connected/Ready.
//! It implements `ng_gateway_sdk::NorthwardHandle` for high-throughput uplink publishing.
//!
//! # Design
//!
//! All MQTT publish calls use `try_publish()` (non-blocking) instead of `publish().await`
//! (blocking). This keeps `process_data` CPU-only and consistent with the Kafka/Pulsar
//! outbound-queue pattern:
//!
//! - rumqttc `AsyncClient` has an internal bounded channel (sized by `outbound_queue_capacity`).
//! - `try_publish` enqueues a message without waiting; returns immediately if full.
//! - The `EventLoop::poll()` in `Session::run()` drains the channel and performs actual I/O.

use super::{
    config::{MessageFormat, ThingsBoardPluginConfig},
    payload,
    topics::Topics,
};
use ng_gateway_sdk::{NorthwardData, NorthwardError, NorthwardHandle, NorthwardResult, TargetType};
use rumqttc::{AsyncClient, ClientError, QoS};
use serde_json::json;
use std::sync::Arc;
use tracing::{info, warn};

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

    /// Map a rumqttc `ClientError` to a `NorthwardError`.
    ///
    /// `TryRequest` indicates the internal channel rejected the message (typically full),
    /// which maps to `PublishFailed` (backpressure). Other errors are transport-level.
    #[inline]
    fn map_publish_error(err: ClientError) -> NorthwardError {
        match err {
            ClientError::TryRequest(_) => NorthwardError::PublishFailed {
                platform: "thingsboard".to_string(),
                reason: "MQTT outbound queue full".to_string(),
            },
            other => NorthwardError::MqttError {
                reason: other.to_string(),
            },
        }
    }

    /// Hot-path telemetry publisher (Gateway API) that enforces `max_payload_bytes`.
    ///
    /// Payload shape (per-point ts grouping):
    /// `{ "<device>": [ {"ts":<ms>,"values":{k1:v1,...}}, {"ts":<ms2>,"values":{...}} ] }`
    ///
    /// Points with the same effective timestamp are grouped into a single
    /// `{"ts":..., "values":{...}}` entry. Per-point source timestamps (`pv.ts`)
    /// take precedence; `batch_ts_ms` is used as fallback when `pv.ts` is `None`.
    fn publish_telemetry_chunked(
        &self,
        topic: &str,
        device_name: &str,
        batch_ts_ms: i64,
        point_values: &[ng_gateway_sdk::PointValue],
    ) -> NorthwardResult<()> {
        let qos = self.qos();
        let retain = self.retain();
        let max_bytes = self.max_payload_bytes();

        let mut chunker = payload::TelemetryChunker::new(device_name, max_bytes)?;
        for pv in point_values.iter() {
            let ts_ms = pv.ts.map(|t| t.timestamp_millis()).unwrap_or(batch_ts_ms);
            if let Some(out) = chunker.push(pv.point_key.as_ref(), &pv.value, ts_ms)? {
                self.client
                    .try_publish(topic, qos, retain, out)
                    .map_err(Self::map_publish_error)?;
            }
        }
        if let Some(bytes) = chunker.finish() {
            self.client
                .try_publish(topic, qos, retain, bytes)
                .map_err(Self::map_publish_error)?;
        }

        Ok(())
    }

    /// Hot-path attributes publisher (Gateway API) that enforces `max_payload_bytes`.
    ///
    /// Payload shape:
    /// `{ "<device>": { k1:v1, k2:v2, ... } }`
    fn publish_attributes_chunked(
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
                    .try_publish(topic, qos, retain, out)
                    .map_err(Self::map_publish_error)?;
            }
        }
        if let Some(bytes) = chunker.finish() {
            self.client
                .try_publish(topic, qos, retain, bytes)
                .map_err(Self::map_publish_error)?;
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
                    )?;
                }
                NorthwardData::Attributes(attributes) => {
                    let topic = Topics::gateway_attributes();
                    self.publish_attributes_chunked(
                        &topic,
                        &attributes.device_name,
                        &attributes.client_attributes,
                    )?;
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
                        .try_publish(Topics::gateway_connect(), self.qos(), self.retain(), bytes)
                        .map_err(Self::map_publish_error)?;
                }
                NorthwardData::DeviceDisconnected(device_disconnected) => {
                    let bytes = serde_json::to_vec(&json!({
                        "device": device_disconnected.device_name
                    }))
                    .map_err(|e| NorthwardError::SerializationError {
                        reason: e.to_string(),
                    })?;

                    self.client
                        .try_publish(
                            Topics::gateway_disconnect(),
                            self.qos(),
                            self.retain(),
                            bytes,
                        )
                        .map_err(Self::map_publish_error)?;
                }
                NorthwardData::WritePointResponse(resp) => {
                    // ThingsBoard northward currently does not require a write-point reply.
                    // Best practice: keep the control-plane closed-loop inside gateway, and log results here.
                    info!(
                        request_id = resp.request_id,
                        point_id = resp.point_id,
                        device_id = resp.device_id,
                        device_name = resp.device_name.as_ref(),
                        point_key = resp.point_key.as_ref(),
                        status = ?resp.status,
                        error = resp.error.as_ref().map(|e| e.message.as_str()),
                        "WritePointResponse received"
                    );
                }
                NorthwardData::RpcResponse(resp) => {
                    // ThingsBoard Server-side RPC for devices behind the gateway:
                    // - Topic: `v1/gateway/rpc`
                    // - Payload: {"device":"Device A","id":$request_id,"data":{...}}
                    //
                    // Also support gateway-level RPC response:
                    // - Topic: `v1/devices/me/rpc/response/<request_id>`
                    // - Payload: arbitrary JSON (best-effort).
                    info!(
                        request_id = resp.request_id,
                        target_type = ?resp.target_type,
                        "RpcResponse received"
                    );
                    match resp.target_type {
                        TargetType::SubDevice => {
                            let Some(device_name) = resp.device_name.as_deref() else {
                                warn!(
                                    request_id = resp.request_id,
                                    "RpcResponse missing device_name for SubDevice target; dropped"
                                );
                                return Ok(());
                            };

                            // ThingsBoard expects an integer request id. Our pipeline uses a String for portability.
                            let request_id: i64 = match resp.request_id.parse::<i64>() {
                                Ok(v) => v,
                                Err(e) => {
                                    warn!(
                                        request_id = resp.request_id,
                                        error = %e,
                                        "RpcResponse request_id is not numeric; dropped"
                                    );
                                    return Ok(());
                                }
                            };

                            let data = if resp.is_success() {
                                // Keep `success: true` aligned with ThingsBoard docs; include result when present.
                                match resp.result.as_ref() {
                                    Some(result) => json!({ "success": true, "result": result }),
                                    None => json!({ "success": true }),
                                }
                            } else {
                                json!({
                                    "success": false,
                                    "error": resp.error.as_deref().unwrap_or("unknown error")
                                })
                            };

                            let bytes = serde_json::to_vec(&json!({
                                "device": device_name,
                                "id": request_id,
                                "data": data
                            }))
                            .map_err(|e| {
                                NorthwardError::SerializationError {
                                    reason: e.to_string(),
                                }
                            })?;

                            self.client
                                .try_publish(Topics::gateway_rpc(), self.qos(), false, bytes)
                                .map_err(Self::map_publish_error)?;
                        }
                        TargetType::Gateway => {
                            let topic = Topics::device_rpc_response_topic(&resp.request_id);
                            let body = if resp.is_success() {
                                resp.result.clone().unwrap_or(json!({ "success": true }))
                            } else {
                                json!({
                                    "success": false,
                                    "error": resp.error.as_deref().unwrap_or("unknown error")
                                })
                            };

                            let bytes = serde_json::to_vec(&body).map_err(|e| {
                                NorthwardError::SerializationError {
                                    reason: e.to_string(),
                                }
                            })?;

                            self.client
                                .try_publish(topic, self.qos(), false, bytes)
                                .map_err(Self::map_publish_error)?;
                        }
                    }
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
