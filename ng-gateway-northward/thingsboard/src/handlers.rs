//! ThingsBoard MQTT message handlers
//!
//! This module contains handlers for processing incoming MQTT messages
//! from ThingsBoard platform, including RPC requests, attribute updates, etc.

use crate::types::{TbAttributeChanged, TbGatewayRpcRequest, TbSubDeviceRpcRequest};
use chrono::Utc;
use ng_gateway_sdk::{
    mqtt::router::HandlerResult, Command, NGValue, NorthwardError, NorthwardEvent,
    NorthwardRuntimeApi, TargetType, WritePoint,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;
use uuid::Uuid;

/// Handle gateway attributes (shared attributes) changed
///
/// Topic: `v1/devices/me/attributes`
#[inline]
pub async fn handle_device_attributes(
    _topic: &str,
    payload: &[u8],
    events_tx: &mpsc::Sender<NorthwardEvent>,
    runtime: &Arc<dyn NorthwardRuntimeApi>,
) -> HandlerResult {
    debug!(
        "Received device attributes changed: {}",
        String::from_utf8_lossy(payload)
    );

    let attr_changed: TbAttributeChanged =
        serde_json::from_slice(payload).map_err(|e| NorthwardError::InvalidMessageFormat {
            reason: format!("Failed to parse device attributes changed: {e}"),
        })?;

    // Map attribute key -> point_id (best-effort).
    let metas = runtime.list_point_meta();
    for (k, v) in attr_changed.data.into_iter() {
        let Some(meta) = metas
            .iter()
            .find(|m| m.device_name.as_ref() == attr_changed.device && m.point_key.as_ref() == k)
        else {
            debug!(
                "No point mapping for device={}, key={}",
                attr_changed.device, k
            );
            continue;
        };
        let Some(ngv) = NGValue::try_from_json_scalar(meta.data_type, &v) else {
            debug!(
                "Unsupported attribute value type for device={}, key={}, expected={:?}",
                attr_changed.device, k, meta.data_type
            );
            continue;
        };
        let req = WritePoint {
            request_id: Uuid::new_v4().to_string(),
            point_id: meta.point_id,
            value: ngv,
            timestamp: chrono::Utc::now(),
            timeout_ms: None,
        };
        let _ = events_tx.send(NorthwardEvent::WritePoint(req)).await;
    }

    Ok(())
}

/// Handle device attributes response
///
/// Topic: `v1/devices/me/attributes/response/+`
#[inline]
pub async fn handle_device_attributes_response(
    _topic: &str,
    _payload: &[u8],
    _events_tx: &mpsc::Sender<NorthwardEvent>,
) -> HandlerResult {
    debug!("Handling device attributes response");
    // TODO: Implement attribute response handling if needed
    Ok(())
}

/// Handle device RPC request
///
/// Topic: `v1/devices/me/rpc/request/+`
#[inline]
pub async fn handle_device_rpc_request(
    topic: &str,
    payload: &[u8],
    events_tx: &mpsc::Sender<NorthwardEvent>,
) -> HandlerResult {
    debug!("Handling device RPC request on topic: {topic}");

    // Extract request ID from topic
    let request_id = topic
        .split('/')
        .next_back()
        .ok_or(ng_gateway_sdk::NorthwardError::InvalidMessageFormat {
            reason: "Request ID not found in topic".to_string(),
        })?
        .to_string();

    // Parse RPC request
    let rpc_request: TbGatewayRpcRequest = serde_json::from_slice(payload).map_err(|e| {
        ng_gateway_sdk::NorthwardError::InvalidMessageFormat {
            reason: format!("Failed to parse device RPC request: {e}"),
        }
    })?;

    // Send internal event
    let _ = events_tx
        .send(NorthwardEvent::CommandReceived(Command {
            command_id: request_id,
            key: rpc_request.method,
            target_type: TargetType::Gateway,
            device_id: None,
            device_name: Some("gateway".to_string()),
            params: rpc_request.params,
            timeout_ms: None,
            timestamp: Utc::now(),
        }))
        .await;

    Ok(())
}

/// Handle device RPC response
///
/// Topic: `v1/devices/me/rpc/response/+`
#[inline]
pub async fn handle_device_rpc_response(
    _topic: &str,
    _payload: &[u8],
    _events_tx: &mpsc::Sender<NorthwardEvent>,
) -> HandlerResult {
    debug!("Handling device RPC response");
    // TODO: Implement RPC response handling if needed
    Ok(())
}

/// Handle gateway RPC (subdevice RPC)
///
/// Topic: `v1/gateway/rpc`
#[inline]
pub async fn handle_gateway_rpc(
    _topic: &str,
    payload: &[u8],
    events_tx: &mpsc::Sender<NorthwardEvent>,
) -> HandlerResult {
    debug!("Handling gateway RPC (subdevice RPC)");

    let rpc_request: TbSubDeviceRpcRequest = serde_json::from_slice(payload).map_err(|e| {
        ng_gateway_sdk::NorthwardError::InvalidMessageFormat {
            reason: format!("Failed to parse gateway RPC request: {e}"),
        }
    })?;

    let _ = events_tx
        .send(NorthwardEvent::CommandReceived(Command {
            command_id: rpc_request.data.id.to_string(),
            key: rpc_request.data.method,
            target_type: TargetType::SubDevice,
            device_id: None,
            device_name: Some(rpc_request.device),
            params: rpc_request.data.params,
            timeout_ms: None,
            timestamp: chrono::Utc::now(),
        }))
        .await;

    Ok(())
}
