//! Ethernet/IP southward data-plane handle.
//!
//! This is the **only** hot-path object published by the SDK supervision loop.
//! It wraps an `EipClient` guarded by an async mutex and provides:
//! - batched tag reads (uplink)
//! - tag writes (downlink)

use super::{
    codec::EthernetIpCodec,
    types::{EthernetIpChannel, EthernetIpDevice, EthernetIpParameter, EthernetIpPoint},
};
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    AccessMode, CollectItem, CollectionGroupKey, DeviceBuffers, DriverError, DriverResult,
    ExecuteOutcome, ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction, RuntimeDelta,
    RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, WriteOutcome, WriteResult,
};
use rust_ethernet_ip::EipClient;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration as StdDuration,
};
use tokio::{sync::Mutex, time::timeout};
use tracing::{error, warn};

/// Ethernet/IP data-plane handle.
pub struct EthernetIpHandle {
    inner: Arc<EthernetIpChannel>,
    client: ArcSwapOption<Mutex<EipClient>>,
    reconnect: OnceLock<ng_gateway_sdk::supervision::ReconnectHandle>,
}

impl EthernetIpHandle {
    /// ASCII: "ENCH"
    const KIND_ETH_CHANNEL: u32 = 0x454E_4348;

    #[inline]
    pub fn new(inner: Arc<EthernetIpChannel>) -> Self {
        Self {
            inner,
            client: ArcSwapOption::from(None),
            reconnect: OnceLock::new(),
        }
    }

    #[inline]
    pub(crate) fn set_reconnect(&self, reconnect: ng_gateway_sdk::supervision::ReconnectHandle) {
        let _ = self.reconnect.set(reconnect);
    }

    #[inline]
    pub(crate) fn attach_client(&self, client: EipClient) {
        self.client.store(Some(Arc::new(Mutex::new(client))));
    }

    #[inline]
    pub(crate) fn detach_client(&self) {
        self.client.store(None);
    }

    #[inline]
    fn try_request_reconnect(&self, reason: &'static str) {
        if let Some(h) = self.reconnect.get() {
            let _ = h.try_request_reconnect(reason);
        }
    }

    #[inline]
    fn load_client(&self) -> DriverResult<Arc<Mutex<EipClient>>> {
        self.client
            .load_full()
            .ok_or(DriverError::ServiceUnavailable)
    }
}

#[async_trait]
impl SouthwardHandle for EthernetIpHandle {
    fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        device
            .downcast_ref::<EthernetIpDevice>()
            .map(|d| CollectionGroupKey::from_u64(Self::KIND_ETH_CHANNEL, d.channel_id as u64))
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        if items.is_empty() {
            return Err(DriverError::ValidationError(
                "collect_data called with empty items".to_string(),
            ));
        }

        let mut buffers = HashMap::with_capacity(items.len());
        let mut points = Vec::new();

        for (dev_any, points_any) in items.iter() {
            let dev = dev_any.downcast_ref::<EthernetIpDevice>().ok_or(
                DriverError::ConfigurationError(
                    "RuntimeDevice is not EthernetIpDevice".to_string(),
                ),
            )?;

            buffers
                .entry(dev.id)
                .or_insert_with(|| DeviceBuffers::new(dev.device_name.clone()));

            for p_any in points_any.iter() {
                let Ok(p) = Arc::clone(p_any).downcast_arc::<EthernetIpPoint>() else {
                    continue;
                };
                if !matches!(p.access_mode, AccessMode::Read | AccessMode::ReadWrite) {
                    continue;
                }
                points.push(p);
            }
        }

        if points.is_empty() {
            return Ok(Vec::new());
        }

        let client_mutex = self.load_client()?;
        const BATCH_SIZE: usize = 50;

        let mut overall_success = true;

        for chunk in points.chunks(BATCH_SIZE) {
            let tag_names: Vec<&str> = chunk.iter().map(|p| p.tag_name.as_str()).collect();
            let op_res = timeout(StdDuration::from_millis(self.inner.config.timeout), async {
                let mut client = client_mutex.lock().await;
                client.read_tags_batch(&tag_names).await
            })
            .await;

            match op_res {
                Ok(Ok(results)) => {
                    for (i, (_tag_name, res)) in results.into_iter().enumerate() {
                        let point = &chunk[i];
                        let Some(buf) = buffers.get_mut(&point.device_id) else {
                            continue;
                        };
                        match res {
                            Ok(plc_value) => match EthernetIpCodec::to_ng_value(
                                plc_value,
                                point.logical_data_type(),
                                &point.transform,
                            ) {
                                Ok(val) => {
                                    buf.push(
                                        point.r#type,
                                        PointValue {
                                            point_id: point.id,
                                            point_key: Arc::from(point.key.as_str()),
                                            value: val,
                                        },
                                    );
                                }
                                Err(e) => {
                                    warn!("Codec error for point {}: {}", point.tag_name, e);
                                }
                            },
                            Err(e) => {
                                warn!("Error reading point {}: {}", point.tag_name, e);
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    warn!("Batch read failed: {}", e);
                    overall_success = false;
                    self.try_request_reconnect("ethernetip batch read failed");
                    break;
                }
                Err(_) => {
                    warn!("Batch read timeout");
                    overall_success = false;
                    self.try_request_reconnect("ethernetip batch read timeout");
                    break;
                }
            }
        }

        let any_data = buffers.values().any(|b| !b.is_empty());
        if !overall_success && !any_data {
            return Err(DriverError::ExecutionError(
                "All batch reads failed".to_string(),
            ));
        }

        let ts = Utc::now();
        let mut device_ids: Vec<i32> = buffers.keys().copied().collect();
        device_ids.sort_unstable();
        let mut out = Vec::with_capacity(device_ids.len() * 2);
        for device_id in device_ids {
            let Some(buf) = buffers.remove(&device_id) else {
                continue;
            };
            out.extend(buf.into_northward(device_id, ts));
        }
        Ok(out)
    }

    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        _action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let _device =
            device
                .downcast_ref::<EthernetIpDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not EthernetIpDevice".to_string(),
                ))?;

        if parameters.is_empty() {
            return Err(DriverError::ValidationError(
                "No parameters provided for write".into(),
            ));
        }

        let client_mutex = self.load_client()?;
        let mut results = Vec::new();
        let mut overall_success = true;

        for (param, value) in parameters {
            let eth_param = param.downcast_ref::<EthernetIpParameter>().ok_or(
                DriverError::ConfigurationError("Invalid Parameter Type".into()),
            )?;

            if eth_param.tag_name.is_empty() {
                warn!("Parameter {} has no tag_name, skipping", eth_param.name);
                continue;
            }

            let plc_value = EthernetIpCodec::to_plc_value(&value, eth_param.wire_data_type())?;
            let op_res = timeout(StdDuration::from_millis(self.inner.config.timeout), async {
                let mut client = client_mutex.lock().await;
                client.write_tag(&eth_param.tag_name, plc_value).await
            })
            .await;

            match op_res {
                Ok(Ok(_)) => {
                    results.push(format!("Wrote {:?} to {}", value, eth_param.tag_name));
                }
                Ok(Err(e)) => {
                    overall_success = false;
                    error!("Write tag {} failed: {}", eth_param.tag_name, e);
                }
                Err(_) => {
                    overall_success = false;
                    self.try_request_reconnect("ethernetip write timeout");
                    break;
                }
            }
        }

        if !overall_success {
            return Err(DriverError::ExecutionError(
                "One or more writes failed".into(),
            ));
        }

        Ok(ExecuteResult {
            outcome: ExecuteOutcome::Completed,
            payload: Some(json!({"status": "success", "details": results})),
        })
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        let point =
            point
                .downcast_ref::<EthernetIpPoint>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimePoint is not EthernetIpPoint".to_string(),
                ))?;

        let client_mutex = self.load_client()?;
        let plc_value = EthernetIpCodec::to_plc_value(value, point.wire_data_type())?;
        let timeout_dur = StdDuration::from_millis(timeout_ms.unwrap_or(self.inner.config.timeout));

        let op_res = timeout(timeout_dur, async {
            let mut client = client_mutex.lock().await;
            client.write_tag(&point.tag_name, plc_value).await
        })
        .await;

        match op_res {
            Ok(Ok(_)) => Ok(WriteResult {
                outcome: WriteOutcome::Applied,
                applied_value: Some(value.clone()),
            }),
            Ok(Err(e)) => Err(DriverError::ExecutionError(e.to_string())),
            Err(_) => Err(DriverError::Timeout(timeout_dur)),
        }
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
}
