use crate::{
    codec::EthernetIpCodec,
    supervisor::{SessionEntry, SessionSupervisor, SharedSession},
    types::{EthernetIpChannel, EthernetIpDevice, EthernetIpPoint},
};
use async_trait::async_trait;
use chrono::Utc;
use ng_gateway_sdk::{
    AccessMode, AttributeData, DataPointType, Driver, DriverError, DriverHealth, DriverResult,
    ExecuteOutcome, ExecuteResult, HealthStatus, NGValue, NorthwardData, PointValue, RuntimeAction,
    RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardConnectionState, SouthwardInitContext,
    TelemetryData, WriteOutcome, WriteResult,
};
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

pub struct EthernetIpDriver {
    inner: Arc<EthernetIpChannel>,
    /// Shared session (Single Supervisor Pattern)
    shared: SharedSession,
    /// Started flag
    started: AtomicBool,
    /// Cancel token for supervisor
    cancel_token: CancellationToken,

    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,

    /// Temporary storage for the reconnect receiver during initialization
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,

    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
}

impl EthernetIpDriver {
    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        let inner = ctx
            .runtime_channel
            .downcast_arc::<EthernetIpChannel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid EthernetIpChannel".to_string())
            })?;

        let (conn_tx, conn_rx) = watch::channel(SouthwardConnectionState::Disconnected);
        let (reconnect_tx, reconnect_rx) = mpsc::channel(1);

        Ok(Self {
            inner,
            shared: Arc::new(SessionEntry::new_empty(reconnect_tx)),
            started: AtomicBool::new(false),
            cancel_token: CancellationToken::new(),
            conn_tx,
            conn_rx,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        })
    }

    /// Update metrics helper
    fn update_metrics(&self, success: bool, elapsed_ms: u64) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
            let prev = self.last_avg_response_time_ms.load(Ordering::Acquire);
            let new_avg = if prev == 0 {
                elapsed_ms
            } else {
                (prev.saturating_mul(9) + elapsed_ms) / 10
            };
            self.last_avg_response_time_ms
                .store(new_avg, Ordering::Release);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[async_trait]
impl Driver for EthernetIpDriver {
    #[instrument(level = "info", skip_all)]
    async fn start(&self) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let rx = self.reconnect_rx.lock().unwrap().take().ok_or_else(|| {
            DriverError::ConfigurationError("Driver already started or invalid state".into())
        })?;

        let cancel = self.cancel_token.child_token();
        let shared = Arc::clone(&self.shared);

        let supervisor = SessionSupervisor::new(shared, cancel, self.conn_tx.clone(), rx);

        let channel = Arc::clone(&self.inner);
        supervisor.run(channel).await?;

        info!("Ethernet/IP driver started");
        Ok(())
    }

    #[instrument(level = "info", skip_all)]
    async fn stop(&self) -> DriverResult<()> {
        info!("Stopping Ethernet/IP driver...");
        self.cancel_token.cancel();
        self.started.store(false, Ordering::Release);
        Ok(())
    }

    #[instrument(level = "debug", skip_all)]
    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        let device =
            device
                .downcast_ref::<EthernetIpDevice>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimeDevice is not EthernetIpDevice".to_string(),
                ))?;

        if data_points.is_empty() {
            return Ok(Vec::new());
        }

        // Use shared session directly (No lazy loading)
        let client_opt = self.shared.client.load_full();
        let client_mutex = match client_opt {
            Some(c) => c,
            None => {
                return Err(DriverError::ServiceUnavailable);
            }
        };

        // Prepare tags
        let points: Vec<&EthernetIpPoint> = data_points
            .iter()
            .filter_map(|p| p.downcast_ref::<EthernetIpPoint>())
            // Filter readable points
            .filter(|p| matches!(p.access_mode, AccessMode::Read | AccessMode::ReadWrite))
            .collect();

        if points.is_empty() {
            return Ok(Vec::new());
        }

        const BATCH_SIZE: usize = 50;
        let mut telemetry_values = Vec::with_capacity(points.len());
        let mut attribute_values = Vec::with_capacity(points.len());

        let start_ts = Instant::now();
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
                        let point = chunk[i];
                        match res {
                            Ok(plc_value) => match EthernetIpCodec::to_ng_value(
                                plc_value,
                                point.data_type,
                                point.scale,
                            ) {
                                Ok(val) => {
                                    let pv = PointValue {
                                        point_id: point.id,
                                        point_key: Arc::from(point.key.as_str()),
                                        value: val,
                                    };
                                    match point.r#type {
                                        DataPointType::Telemetry => telemetry_values.push(pv),
                                        DataPointType::Attribute => attribute_values.push(pv),
                                    }
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
                    // Trigger reconnect
                    self.shared.healthy.store(false, Ordering::Release);
                    let _ = self.shared.reconnect_tx.try_send(());
                    break;
                }
                Err(_) => {
                    warn!("Batch read timeout");
                    overall_success = false;
                    self.shared.healthy.store(false, Ordering::Release);
                    let _ = self.shared.reconnect_tx.try_send(());
                    break;
                }
            }
        }

        self.update_metrics(overall_success, start_ts.elapsed().as_millis() as u64);

        if !overall_success && telemetry_values.is_empty() && attribute_values.is_empty() {
            return Err(DriverError::ExecutionError(
                "All batch reads failed".to_string(),
            ));
        }

        let mut out = Vec::new();
        if !telemetry_values.is_empty() {
            out.push(NorthwardData::Telemetry(TelemetryData::new(
                device.id,
                device.device_name.clone(),
                telemetry_values,
            )));
        }
        if !attribute_values.is_empty() {
            out.push(NorthwardData::Attributes(
                AttributeData::new_client_attributes(
                    device.id,
                    device.device_name.clone(),
                    attribute_values,
                ),
            ));
        }

        Ok(out)
    }

    #[instrument(level = "debug", skip_all)]
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

        let client_opt = self.shared.client.load_full();
        let client_mutex = client_opt.ok_or(DriverError::ServiceUnavailable)?;

        let mut results = Vec::new();
        let start_ts = Instant::now();
        let mut overall_success = true;

        for (param, value) in parameters {
            let eth_param = param
                .downcast_ref::<crate::types::EthernetIpParameter>()
                .ok_or(DriverError::ConfigurationError(
                    "Invalid Parameter Type".into(),
                ))?;

            if eth_param.tag_name.is_empty() {
                warn!("Parameter {} has no tag_name, skipping", eth_param.name);
                continue;
            }

            let plc_value = EthernetIpCodec::to_plc_value(&value, eth_param.data_type)?;

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
                    self.shared.healthy.store(false, Ordering::Release);
                    let _ = self.shared.reconnect_tx.try_send(());
                    break;
                }
            }
        }

        self.update_metrics(overall_success, start_ts.elapsed().as_millis() as u64);

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
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        // Validation removed for brevity, assuming similar to execute_action
        let point =
            point
                .downcast_ref::<EthernetIpPoint>()
                .ok_or(DriverError::ConfigurationError(
                    "RuntimePoint is not EthernetIpPoint".to_string(),
                ))?;

        let client_opt = self.shared.client.load_full();
        let client_mutex = client_opt.ok_or(DriverError::ServiceUnavailable)?;

        let plc_value = EthernetIpCodec::to_plc_value(&value, point.data_type)?;
        let timeout_dur = StdDuration::from_millis(timeout_ms.unwrap_or(self.inner.config.timeout));

        let op_res = timeout(timeout_dur, async {
            let mut client = client_mutex.lock().await;
            client.write_tag(&point.tag_name, plc_value).await
        })
        .await;

        match op_res {
            Ok(Ok(_)) => Ok(WriteResult {
                outcome: WriteOutcome::Applied,
                applied_value: Some(value),
            }),
            Ok(Err(e)) => Err(DriverError::ExecutionError(e.to_string())),
            Err(_) => Err(DriverError::Timeout(timeout_dur)),
        }
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }

    async fn health_check(&self) -> DriverResult<DriverHealth> {
        let is_healthy = self.shared.healthy.load(Ordering::Acquire);

        Ok(DriverHealth {
            status: if is_healthy {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            last_activity: Utc::now(),
            error_count: self.failed_requests.load(Ordering::Acquire),
            success_rate: {
                let total = self.total_requests.load(Ordering::Acquire) as f64;
                if total > 0.0 {
                    self.successful_requests.load(Ordering::Acquire) as f64 / total
                } else {
                    0.0
                }
            },
            average_response_time: StdDuration::from_millis(
                self.last_avg_response_time_ms.load(Ordering::Acquire),
            ),
            details: None,
        })
    }
}
