use crate::{
    codec::DIResponseParser,
    protocol::{
        error::ProtocolError,
        frame::defs::DataIdentifier,
        session::{Cjt188Session, ReadDataParams},
    },
    supervisor::{Cjt188Supervisor, SessionEntry, SharedSession},
    types::{Cjt188Channel, Cjt188Device, Cjt188Point},
};
use async_trait::async_trait;
use ng_gateway_sdk::{
    AttributeData, DataPointType, Driver, DriverError, DriverHealth, DriverResult, ExecuteResult,
    HealthStatus, NGValue, NorthwardData, PointValue, RuntimeAction, RuntimeDevice,
    RuntimeParameter, RuntimePoint, SouthwardConnectionState, SouthwardInitContext, TelemetryData,
    WriteResult,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;

/// Production-ready CJ/T 188 driver.
pub struct Cjt188Driver {
    inner: Arc<Cjt188Channel>,
    session: SharedSession,
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,
    cancel: CancellationToken,
    conn_tx: watch::Sender<SouthwardConnectionState>,
    conn_rx: watch::Receiver<SouthwardConnectionState>,
    started: AtomicBool,
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    last_avg_response_time_ms: AtomicU64,
}

impl Cjt188Driver {
    pub fn with_context(ctx: SouthwardInitContext) -> DriverResult<Self> {
        // Try to downcast to Cjt188Channel.
        // Note: This requires Cjt188Channel to be registered in the loader/models.
        let inner = ctx
            .runtime_channel
            .downcast_arc::<Cjt188Channel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid Cjt188Channel runtime type".to_string())
            })?;

        let (conn_tx, conn_rx) = watch::channel(SouthwardConnectionState::Disconnected);
        let (reconnect_tx, reconnect_rx) = mpsc::channel::<()>(1);

        // Use channel config for timeout-driven reconnect policy.
        //
        // Semantics:
        // - max_timeouts = 0: disable "timeout-triggered reconnect" (still reconnect on IO/transport errors)
        // - max_timeouts > 0: reconnect when consecutive request timeouts >= max_timeouts
        let threshold = inner.config.max_timeouts;
        let shared = Arc::new(SessionEntry::new_empty(reconnect_tx, threshold));
        let cancel = CancellationToken::new();

        Ok(Self {
            inner,
            session: shared,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            cancel,
            conn_tx,
            conn_rx,
            started: AtomicBool::new(false),
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            last_avg_response_time_ms: AtomicU64::new(0),
        })
    }

    fn update_response_metrics(&self, elapsed_ms: u64, success: bool) {
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

    async fn run_op<T, F, Fut>(
        &self,
        op_timeout: Duration,
        op_label: &'static str,
        op: F,
    ) -> DriverResult<T>
    where
        F: FnOnce(Arc<dyn Cjt188Session>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, crate::protocol::error::ProtocolError>>
            + Send
            + 'static,
        T: Send + 'static,
    {
        let session_shared = self.session.clone();

        let (res, elapsed_ms) = tokio::spawn(async move {
            let start = Instant::now();
            let session_opt = session_shared.session.load_full();
            let handle = match session_opt {
                Some(h) => h,
                None => {
                    session_shared.healthy.store(false, Ordering::Release);
                    let _ = session_shared.reconnect_tx.try_send(());
                    return (Err(DriverError::ServiceUnavailable), 0);
                }
            };

            let sess = Arc::clone(&handle.0);

            // Wrap operation in timeout
            let op_res = match tokio::time::timeout(op_timeout, op(sess)).await {
                Ok(res) => res,
                Err(_) => Err(ProtocolError::Timeout(format!(
                    "Operation timed out after {:?}",
                    op_timeout
                ))),
            };

            let res = match op_res {
                Ok(v) => {
                    session_shared
                        .consecutive_timeouts
                        .store(0, Ordering::Release);
                    Ok(v)
                }
                Err(e) => {
                    // Simple error handling/reconnect logic
                    match &e {
                        ProtocolError::Timeout(_) => {
                            tracing::warn!(op = op_label, "CJ/T 188 operation timeout");
                            let count = session_shared
                                .consecutive_timeouts
                                .fetch_add(1, Ordering::Relaxed)
                                + 1;
                            let threshold = session_shared.timeout_reconnect_threshold as u64;
                            if threshold != 0 && count >= threshold {
                                session_shared.healthy.store(false, Ordering::Release);
                                tracing::warn!(
                                    op = op_label,
                                    timeout_count = count,
                                    threshold,
                                    "CJ/T 188 timeout threshold reached, requesting reconnect"
                                );
                                let _ = session_shared.reconnect_tx.try_send(());
                            }
                        }
                        ProtocolError::Transport(msg) => {
                            tracing::warn!(op = op_label, error = %msg, "CJ/T 188 transport error");
                            session_shared.healthy.store(false, Ordering::Release);
                            let _ = session_shared.reconnect_tx.try_send(());
                        }
                        ProtocolError::Io(err) => {
                            tracing::warn!(op = op_label, error = %err, "CJ/T 188 IO error");
                            session_shared.healthy.store(false, Ordering::Release);
                            let _ = session_shared.reconnect_tx.try_send(());
                        }
                        _ => {}
                    }
                    Err(DriverError::ExecutionError(e.to_string()))
                }
            };

            (res, start.elapsed().as_millis() as u64)
        })
        .await
        .map_err(|e| DriverError::ExecutionError(e.to_string()))?;

        self.update_response_metrics(elapsed_ms, res.is_ok());
        res
    }
}

#[async_trait]
impl Driver for Cjt188Driver {
    async fn start(&self) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let channel = Arc::clone(&self.inner);
        let shared = Arc::clone(&self.session);
        let cancel = self.cancel.child_token();
        let mut rx_guard = self.reconnect_rx.lock().await;
        let reconnect_rx = rx_guard
            .take()
            .ok_or(DriverError::ExecutionError("reconnect rx consumed".into()))?;
        let supervisor = Cjt188Supervisor::new(shared, cancel, self.conn_tx.clone(), reconnect_rx);

        supervisor.run(channel).await?;

        Ok(())
    }

    async fn stop(&self) -> DriverResult<()> {
        self.cancel.cancel();
        self.session.shutdown.store(true, Ordering::Release);
        Ok(())
    }

    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>> {
        let d = device
            .downcast_ref::<Cjt188Device>()
            .ok_or(DriverError::InvalidEntity(
                "Device is not a CJ/T 188 device in this driver".to_string(),
            ))?;

        let address = d.address_struct()?;
        let meter_type = d.meter_type;

        let concrete_points = data_points
            .iter()
            .filter_map(|p| p.downcast_ref::<Cjt188Point>())
            .collect::<Vec<_>>();

        if concrete_points.is_empty() {
            return Ok(Vec::new());
        }

        // ===== DI Grouping Optimization =====
        // Group points by DI to minimize protocol requests
        // Multiple points with the same DI will only trigger ONE request
        let mut di_groups: HashMap<u16, Vec<&Cjt188Point>> = HashMap::new();
        for point in &concrete_points {
            di_groups.entry(point.di).or_default().push(point);
        }

        tracing::debug!(
            device_id = d.id,
            total_points = concrete_points.len(),
            unique_dis = di_groups.len(),
            "Grouped points by DI for optimized reading"
        );

        // Store point values indexed by point_id
        let mut point_values: HashMap<i32, NGValue> = HashMap::new();

        // Read each DI once and distribute values to corresponding points
        let timeout = Duration::from_millis(self.inner.connection_policy.read_timeout_ms.max(1));

        for (di, points_in_group) in di_groups {
            let start = Instant::now();
            let di_enum = DataIdentifier::from(di);

            let res = self
                .run_op(timeout, "read_data", move |session| {
                    let addr = address;
                    let mt = meter_type;
                    async move {
                        let resp = session
                            .read_data(
                                ReadDataParams {
                                    meter_type: mt,
                                    address: addr,
                                    di: di_enum,
                                },
                                timeout,
                            )
                            .await?;

                        Ok(resp)
                    }
                })
                .await;

            let elapsed_ms = start.elapsed().as_millis() as u64;

            match res {
                Ok(response) => {
                    self.update_response_metrics(elapsed_ms, true);

                    // Parse DI response and directly produce final point values
                    match DIResponseParser::parse(
                        di,
                        meter_type,
                        &response.payload,
                        &points_in_group,
                    ) {
                        Ok(parsed) => {
                            tracing::debug!(
                                device_id = d.id,
                                di = format!("0x{:04X}", di),
                                values = parsed.value_count(),
                                elapsed_ms,
                                "Successfully parsed DI response"
                            );

                            // Move all produced values into the global map.
                            //
                            // This avoids cloning `NGValue` per point. `NGValue` can contain
                            // shared payloads (`Arc<str>`, `Bytes`), but avoiding clone is still
                            // best-practice on hot paths.
                            let produced = parsed.point_values;

                            // Emit warnings for missing points (if any).
                            for point in points_in_group {
                                if !produced.contains_key(&point.id) {
                                    // Either:
                                    // - point.field_key does not exist in the DI schema
                                    // - coercion failed due to datatype/scale mismatch
                                    // - device returned an unexpected payload shape
                                    tracing::warn!(
                                        device_id = d.id,
                                        point_id = point.id,
                                        point_key = %point.key,
                                        field_key = %point.field_key,
                                        di = format!("0x{:04X}", di),
                                        "Point value not produced from DI response"
                                    );
                                }
                            }

                            point_values.extend(produced);
                        }
                        Err(e) => {
                            tracing::error!(
                                device_id = d.id,
                                di = format!("0x{:04X}", di),
                                error = %e,
                                "Failed to parse DI response"
                            );
                            // Skip all points in this group if parsing fails
                        }
                    }
                }
                Err(e) => {
                    self.update_response_metrics(elapsed_ms, false);
                    tracing::warn!(
                        device_id = d.id,
                        di = format!("0x{:04X}", di),
                        error = %e,
                        "Failed to read DI"
                    );
                    // Skip all points in this group if read fails
                }
            }
        }

        // Convert point values to NorthwardData
        let mut results = Vec::new();
        for point in concrete_points {
            if let Some(value) = point_values.get(&point.id) {
                let pv = PointValue {
                    point_id: point.id,
                    point_key: Arc::<str>::from(point.key.as_str()),
                    value: value.clone(),
                };

                match point.r#type {
                    DataPointType::Telemetry => {
                        results.push(NorthwardData::Telemetry(TelemetryData::new(
                            d.id,
                            d.device_name.clone(),
                            vec![pv],
                        )));
                    }
                    DataPointType::Attribute => {
                        results.push(NorthwardData::Attributes(
                            AttributeData::new_client_attributes(
                                d.id,
                                d.device_name.clone(),
                                vec![pv],
                            ),
                        ));
                    }
                }
            }
        }

        Ok(results)
    }

    async fn execute_action(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        _action: Arc<dyn RuntimeAction>,
        _parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        Err(DriverError::ConfigurationError(
            "CJ/T 188 driver does not support execute_action (downlink is not implemented)"
                .to_string(),
        ))
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        _point: Arc<dyn RuntimePoint>,
        _value: NGValue,
        _timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        Err(DriverError::ConfigurationError(
            "CJ/T 188 driver does not support write_point (downlink is not implemented)"
                .to_string(),
        ))
    }

    fn subscribe_connection_state(&self) -> watch::Receiver<SouthwardConnectionState> {
        self.conn_rx.clone()
    }

    async fn health_check(&self) -> DriverResult<DriverHealth> {
        Ok(DriverHealth {
            status: HealthStatus::Healthy,
            last_activity: chrono::Utc::now(),
            error_count: self.failed_requests.load(Ordering::Relaxed),
            success_rate: 1.0,
            average_response_time: Duration::from_millis(
                self.last_avg_response_time_ms.load(Ordering::Relaxed),
            ),
            details: None,
        })
    }
}
