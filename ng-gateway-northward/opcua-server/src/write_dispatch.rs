use crate::config::OpcuaServerPluginConfig;
use crate::node_cache::NodeCache;
use chrono::{DateTime as ChronoDateTime, Utc};
use ng_gateway_sdk::{
    AccessMode, DataType, NGValue, NorthwardError, NorthwardEvent, NorthwardResult,
    NorthwardRuntimeApi, PointMeta, WritePoint, WritePointErrorKind, WritePointResponse,
    WritePointStatus,
};
use opcua::types::{ByteString, Variant};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot, Mutex};
use uuid::Uuid;

/// Write callback dispatcher: OPC UA Write -> gateway southward action execution.
#[derive(Clone)]
pub struct WriteDispatcher {
    config: Arc<OpcuaServerPluginConfig>,
    runtime: Arc<dyn NorthwardRuntimeApi>,
    node_cache: Arc<NodeCache>,
    events_tx: mpsc::Sender<NorthwardEvent>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<WritePointResponse>>>>,
}

impl WriteDispatcher {
    pub fn new(
        config: Arc<OpcuaServerPluginConfig>,
        runtime: Arc<dyn NorthwardRuntimeApi>,
        node_cache: Arc<NodeCache>,
        events_tx: mpsc::Sender<NorthwardEvent>,
    ) -> Self {
        Self {
            config,
            runtime,
            node_cache,
            events_tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Dispatch a write for a node id (ns=1;s=...) and value.
    ///
    /// NOTE: Proper OPC UA StatusCode mapping is done by server integration layer.
    pub async fn dispatch_write(&self, node_id: &str, value: &Variant) -> NorthwardResult<()> {
        let point_id =
            self.node_cache
                .get_point_id(node_id)
                .ok_or_else(|| NorthwardError::NotFound {
                    entity: format!("node_id:{node_id}"),
                })?;
        let meta =
            self.runtime
                .get_point_meta(point_id)
                .ok_or_else(|| NorthwardError::NotFound {
                    entity: format!("point:{point_id}"),
                })?;

        ensure_writeable(meta.as_ref())?;
        let ng_value = variant_to_value(meta.as_ref(), value)?;

        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        let req = WritePoint {
            request_id: request_id.clone(),
            point_id: meta.point_id,
            value: ng_value,
            timestamp: Utc::now(),
            timeout_ms: Some(self.config.write_timeout_ms),
        };

        // Send request to gateway via events channel.
        if let Err(e) = self.events_tx.send(NorthwardEvent::WritePoint(req)).await {
            let mut pending = self.pending.lock().await;
            pending.remove(&request_id);
            return Err(NorthwardError::DataSendError {
                message: format!("Failed to send WritePoint event: {e}"),
            });
        }

        // Wait for async response.
        let timeout_ms = self.config.write_timeout_ms;
        let resp = match tokio::time::timeout(Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_closed)) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&request_id);
                return Err(NorthwardError::GatewayError {
                    reason: "WritePoint response channel closed".to_string(),
                });
            }
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&request_id);
                return Err(NorthwardError::Timeout {
                    timeout_ms,
                    operation: "opcua write".to_string(),
                });
            }
        };

        if resp.status == WritePointStatus::Success {
            return Ok(());
        }
        let Some(err) = resp.error else {
            return Err(NorthwardError::GatewayError {
                reason: "WritePoint failed without error details".to_string(),
            });
        };
        Err(map_write_point_error(
            &err.kind,
            &err.message,
            resp.point_id,
            resp.device_id,
        ))
    }

    /// Handle an async `WritePointResponse` from gateway and wake the corresponding pending caller.
    pub async fn on_write_point_response(&self, resp: WritePointResponse) {
        let tx = { self.pending.lock().await.remove(&resp.request_id) };
        if let Some(tx) = tx {
            let _ = tx.send(resp);
        }
    }
}

fn ensure_writeable(meta: &PointMeta) -> Result<(), NorthwardError> {
    if matches!(meta.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
        Ok(())
    } else {
        Err(NorthwardError::ValidationFailed {
            reason: "point is not writeable".to_string(),
        })
    }
}

fn variant_to_value(meta: &PointMeta, v: &Variant) -> Result<NGValue, NorthwardError> {
    let type_mismatch = || NorthwardError::ValidationFailed {
        reason: format!("type mismatch: expected {:?}, got {:?}", meta.data_type, v),
    };

    Ok(match meta.data_type {
        DataType::Boolean => match v {
            Variant::Boolean(b) => NGValue::Boolean(*b),
            _ => return Err(type_mismatch()),
        },
        DataType::Int8 => match v {
            Variant::SByte(x) => NGValue::Int8(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::UInt8 => match v {
            Variant::Byte(x) => NGValue::UInt8(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::Int16 => match v {
            Variant::Int16(x) => NGValue::Int16(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::UInt16 => match v {
            Variant::UInt16(x) => NGValue::UInt16(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::Int32 => match v {
            Variant::Int32(x) => NGValue::Int32(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::UInt32 => match v {
            Variant::UInt32(x) => NGValue::UInt32(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::Int64 => match v {
            Variant::Int64(x) => NGValue::Int64(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::UInt64 => match v {
            Variant::UInt64(x) => NGValue::UInt64(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::Float32 => match v {
            Variant::Float(x) => NGValue::Float32(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::Float64 => match v {
            Variant::Double(x) => NGValue::Float64(*x),
            _ => return Err(type_mismatch()),
        },
        DataType::String => match v {
            Variant::String(s) => NGValue::String(Arc::<str>::from(s.to_string())),
            _ => return Err(type_mismatch()),
        },
        DataType::Binary => match v {
            Variant::ByteString(bs) => {
                let bytes: Vec<u8> = bytestring_to_vec(bs);
                NGValue::Binary(bytes::Bytes::from(bytes))
            }
            _ => return Err(type_mismatch()),
        },
        DataType::Timestamp => match v {
            Variant::DateTime(dt) => {
                let utc: ChronoDateTime<Utc> = ChronoDateTime::<Utc>::from(**dt);
                let ms = utc.timestamp_millis();
                NGValue::Timestamp(ms)
            }
            _ => return Err(type_mismatch()),
        },
    })
}

fn bytestring_to_vec(bs: &ByteString) -> Vec<u8> {
    bs.as_ref().to_vec()
}

fn map_write_point_error(
    kind: &WritePointErrorKind,
    message: &str,
    point_id: i32,
    device_id: i32,
) -> NorthwardError {
    match kind {
        WritePointErrorKind::NotFound => NorthwardError::NotFound {
            entity: format!("point:{point_id}"),
        },
        WritePointErrorKind::NotWriteable => NorthwardError::ValidationFailed {
            reason: "point is not writeable".to_string(),
        },
        WritePointErrorKind::TypeMismatch => NorthwardError::ValidationFailed {
            reason: format!("type mismatch: {message}"),
        },
        WritePointErrorKind::OutOfRange => NorthwardError::ValidationFailed {
            reason: format!("out of range: {message}"),
        },
        WritePointErrorKind::NotConnected => NorthwardError::NotConnected,
        WritePointErrorKind::QueueTimeout => NorthwardError::Timeout {
            timeout_ms: 0,
            operation: "write queue timeout".to_string(),
        },
        WritePointErrorKind::DriverError => NorthwardError::GatewayError {
            reason: format!("driver error (device {device_id}): {message}"),
        },
    }
}
