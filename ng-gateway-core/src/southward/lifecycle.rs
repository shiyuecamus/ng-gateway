//! Southward lifecycle management.
//!
//! This module contains channel lifecycle operations for `NGSouthwardManager`:
//! - start/stop/restart channels
//! - wait for connection final state
//! - shutdown the southward runtime
//!
//! # Concurrency & safety
//! - Never hold a `DashMap` guard across `.await`.
//! - Prefer snapshotting `Arc` handles (driver, labels) before awaiting.

use super::{
    super::lifecycle::{start_with_policy, StartPolicy},
    NGSouthwardManager, SouthwardDataBus,
};
use chrono::Utc;
use futures::future::join_all;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::entities::prelude::ChannelModel;
use ng_gateway_sdk::{ConnectionState, Driver, FailureKind, FailurePhase, FailureReport, Phase};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

impl NGSouthwardManager {
    /// Wait until a driver's connection reaches Connected or Failed, with timeout.
    ///
    /// This is a thin wrapper used by lifecycle helpers to provide a unified "start + wait" semantic.
    pub async fn wait_for_final(&self, driver: &Arc<dyn Driver>, timeout_ms: u64) -> NGResult<()> {
        let mut rx = driver.subscribe_connection_state();

        match timeout(Duration::from_millis(timeout_ms), async move {
            rx.wait_for(|state| matches!(state.phase, Phase::Connected | Phase::Failed))
                .await
                .map(|r| r.clone())
        })
        .await
        {
            Ok(Ok(state)) => {
                if state.phase == Phase::Connected {
                    return Ok(());
                }
                if state.phase == Phase::Failed {
                    let reason = state
                        .last_failure
                        .as_ref()
                        .map(|r| r.summary.as_ref())
                        .unwrap_or("unknown failure");
                    return Err(NGError::DriverError(format!(
                        "Driver connection failed: {reason}"
                    )));
                }
                Err(NGError::DriverError("Invalid connection phase".to_string()))
            }
            Ok(Err(_)) => Err(NGError::DriverError(
                "Driver connection state channel closed".to_string(),
            )),
            Err(_) => Err(NGError::DriverError(format!(
                "Driver connection timeout after {} ms",
                timeout_ms
            ))),
        }
    }

    /// Start a channel by id using its current runtime context.
    pub async fn start_channel(&self, channel_id: i32, policy: StartPolicy) -> NGResult<()> {
        // Snapshot the driver handle first; NEVER hold DashMap guards across `.await`.
        let driver = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver));
        let Some(driver) = driver else {
            return Ok(());
        };

        // Bridge driver lifecycle into the generic start helper.
        let driver_for_start = Arc::clone(&driver);
        let start_fn = move || async move {
            driver_for_start
                .start()
                .await
                .map_err(|e| NGError::DriverError(e.to_string()))
        };

        let this = self;
        let driver_for_wait = Arc::clone(&driver);
        let wait_fn = move |timeout_ms: u64| async move {
            this.wait_for_final(&driver_for_wait, timeout_ms).await
        };

        start_with_policy(policy, start_fn, wait_fn).await?;

        // Update bookkeeping (last_activity) after start operation.
        if let Some(mut entry) = self.runtime.index.channels.get_mut(&channel_id) {
            entry.touch_activity(Utc::now());
        }

        Ok(())
    }

    /// Create a channel, commit it, and start with provided policy.
    pub async fn create_and_start_channel(
        &self,
        config: &ChannelModel,
        southward_data_bus: &Arc<SouthwardDataBus>,
        policy: StartPolicy,
    ) -> NGResult<()> {
        // Prepare instance (driver created but not started) and commit.
        let instance = self
            .create_channel_instance(config, southward_data_bus)
            .await?;
        let channel_id = instance.config.id();
        self.runtime
            .index
            .channels
            .insert(instance.config.id(), instance.clone());

        // Start according to policy via by-id path.
        match self.start_channel(channel_id, policy).await {
            Ok(()) => Ok(()),
            Err(e) => {
                match policy {
                    StartPolicy::SyncWaitConnected { .. } => {
                        // On sync start failure, clean up driver and remove channel entry.
                        let _ = instance.driver.stop().await;
                    }
                    StartPolicy::AsyncFireAndForget => {
                        // For async, keep instance but mark as failed.
                        if let Some(mut entry) = self.runtime.index.channels.get_mut(&channel_id) {
                            let report = Arc::new(FailureReport {
                                phase: FailurePhase::Connect,
                                kind: FailureKind::Fatal,
                                summary: Arc::<str>::from("async start failed"),
                                code: Some(Arc::<str>::from("async_start_failed")),
                            });
                            entry.state = ConnectionState::arc_now_with_failure(
                                Phase::Failed,
                                0,
                                Some(report),
                            );
                        }
                    }
                }
                Err(e)
            }
        }
    }

    /// Start all enabled channels (fire-and-forget).
    pub async fn start_channels(&self) -> NGResult<()> {
        // Collect channel ids first to avoid holding iter guards across await.
        let ids = self.get_enabled_channel_ids();
        for id in ids.into_iter() {
            self.start_channel(id, StartPolicy::AsyncFireAndForget)
                .await?;
        }
        // Ensure hub manager snapshot baseline is up-to-date after start.
        self.refresh_manager_snapshot_from_index().await;
        Ok(())
    }

    /// Stop a channel's runtime and optionally remove all runtime mappings.
    pub async fn stop_channel(&self, channel_id: i32, remove: bool) {
        // Snapshot driver label early (used for metrics unregister when removing).
        // IMPORTANT: do not hold DashMap guards across await.
        let driver_label = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|instance| Arc::clone(&instance.driver_label));

        // Stop driver if channel exists.
        let driver = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|instance| Arc::clone(&instance.driver));
        if let Some(driver) = driver {
            let _ = driver.stop().await;
        }

        if remove {
            if let Some(driver_label) = driver_label.as_ref() {
                // Remove per-channel labeled metrics to avoid "zombie" series.
                self.metrics_hub
                    .unregister_southward_channel_metrics(channel_id, driver_label);
                self.metrics_hub
                    .unregister_control_channel_metrics(channel_id, driver_label);
            }

            if let Some((_, device_ids)) = self.runtime.index.channel_devices.remove(&channel_id) {
                for device_id in device_ids.iter().copied() {
                    if let Some((_, dev)) = self.runtime.index.devices.remove(&device_id) {
                        // Best-effort remove device name index.
                        let name = dev.config.device_name();
                        self.runtime
                            .index
                            .device_name_index
                            .remove_if(name, |_, v| *v == device_id);
                    }
                    self.runtime.index.device_points.remove(&device_id);
                    self.runtime.index.device_actions.remove(&device_id);
                }
            }
            // Remove channel entry itself after stopping.
            self.runtime.index.channels.remove(&channel_id);
        }

        // Ensure hub manager snapshot baseline is up-to-date after stop/remove.
        self.refresh_manager_snapshot_from_index().await;
    }

    /// Rebind all devices under a channel to the channel's current driver handle.
    pub fn rebind_channel_devices(&self, channel_id: i32) {
        let new_driver = match self.runtime.index.channels.get(&channel_id) {
            Some(chan) => Arc::clone(&chan.driver),
            None => return,
        };
        if let Some(ids) = self
            .runtime
            .index
            .channel_devices
            .get(&channel_id)
            .map(|e| e.value().iter().copied().collect::<Vec<i32>>())
        {
            for device_id in ids.into_iter() {
                if let Some(mut dev) = self.runtime.index.devices.get_mut(&device_id) {
                    dev.driver = Arc::clone(&new_driver);
                }
            }
        }
    }

    /// Replace channel instance without starting (for Disabled channels or pre-start update).
    pub async fn replace_channel_instance(
        &self,
        config: &ChannelModel,
        southward_data_bus: &Arc<SouthwardDataBus>,
    ) -> NGResult<()> {
        let instance = self
            .create_channel_instance(config, southward_data_bus)
            .await?;
        self.runtime
            .index
            .channels
            .insert(instance.config.id(), instance);
        self.rebind_channel_devices(config.id);
        self.refresh_manager_snapshot_from_index().await;
        Ok(())
    }

    /// Restart a channel atomically with new configuration.
    pub async fn restart_channel(
        &self,
        config: &ChannelModel,
        southward_data_bus: &Arc<SouthwardDataBus>,
        timeout_ms: u64,
    ) -> NGResult<()> {
        let channel_id = config.id;
        // Stop previous runtime and clean runtime entries.
        self.stop_channel(channel_id, false).await;
        // Create and start synchronously.
        self.create_and_start_channel(
            config,
            southward_data_bus,
            StartPolicy::SyncWaitConnected { timeout_ms },
        )
        .await?;
        // Rebind devices to the new channel driver.
        self.rebind_channel_devices(channel_id);
        Ok(())
    }

    /// Shutdown the southward manager.
    pub async fn shutdown(&self) -> NGResult<()> {
        // Stop all channels concurrently using unified stop logic.
        let ids = self.get_channel_ids();
        let futures_iter = ids.into_iter().map(|id| self.stop_channel(id, true));
        let _ = timeout(Duration::from_secs(6), join_all(futures_iter)).await;

        // Clear caches and mappings.
        self.runtime.clear();

        // Reset hub manager snapshot state to zeroed baseline.
        self.refresh_manager_snapshot_from_index().await;

        Ok(())
    }
}
