use crate::southward::index::RuntimeIndex;
use chrono::Utc;
use dashmap::DashMap;
use ng_gateway_sdk::{
    DeviceConnectedData, DeviceDisconnectedData, NorthwardData, SouthwardConnectionState,
};
use std::sync::Arc;
use tokio::{sync::mpsc::Sender, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Per-channel monitor component that manages lifecycle and event emission
#[derive(Clone)]
pub struct ChannelMonitor {
    index: Arc<RuntimeIndex>,
    tokens: Arc<DashMap<i32, CancellationToken>>,
    tasks: Arc<DashMap<i32, JoinHandle<()>>>,
}

impl ChannelMonitor {
    pub fn new(index: Arc<RuntimeIndex>) -> Self {
        Self {
            index,
            tokens: Arc::new(DashMap::new()),
            tasks: Arc::new(DashMap::new()),
        }
    }

    /// Spawn a connection monitor for a specific channel
    pub fn spawn(&self, channel_id: i32, data_tx: &Sender<Arc<NorthwardData>>) {
        if self.tasks.contains_key(&channel_id) {
            return;
        }

        let monitor_channels = Arc::clone(&self.index.channels);
        let monitor_driver = match self.index.channels.get(&channel_id) {
            Some(chan) => Arc::clone(&chan.driver),
            None => return,
        };
        let monitor_channel_name = match self.index.channels.get(&channel_id) {
            Some(chan) => chan.config.name().to_string(),
            None => return,
        };
        let monitor_data_tx = data_tx.clone();
        let device_map = Arc::clone(&self.index.channel_devices);
        let devices_table = Arc::clone(&self.index.devices);

        let token = CancellationToken::new();
        self.tokens.insert(channel_id, token.clone());
        let tasks_ref = Arc::clone(&self.tasks);
        let tokens_cleanup = Arc::clone(&self.tokens);
        let handle = tokio::spawn(async move {
            // Shared last state across event and polling paths to avoid duplicate work
            let mut last_state = SouthwardConnectionState::Connecting;

            // Unified state transition handler: updates channel state and builds northward events
            let mut handle_transition =
                |state: SouthwardConnectionState| -> Option<Vec<Arc<NorthwardData>>> {
                    if state == last_state {
                        return None;
                    }
                    last_state = state.clone();

                    match state {
                        SouthwardConnectionState::Connected => {
                            if let Some(mut entry) = monitor_channels.get_mut(&channel_id) {
                                entry.state = SouthwardConnectionState::Connected;
                                entry.last_activity = Utc::now();
                            }
                            info!("Channel [{monitor_channel_name}] connected");

                            if let Some(set_ref) = device_map.get(&channel_id) {
                                let mut events: Vec<Arc<NorthwardData>> =
                                    Vec::with_capacity(set_ref.len());
                                for device_id in set_ref.iter().copied() {
                                    if let Some(dev) = devices_table.get(&device_id) {
                                        events.push(Arc::new(NorthwardData::DeviceConnected(
                                            DeviceConnectedData {
                                                device_id,
                                                device_name: dev.config.device_name().to_string(),
                                                device_type: dev.config.device_type().to_string(),
                                            },
                                        )));
                                    }
                                }
                                if !events.is_empty() {
                                    return Some(events);
                                }
                            }
                            None
                        }
                        SouthwardConnectionState::Disconnected
                        | SouthwardConnectionState::Failed(_) => {
                            if let Some(mut entry) = monitor_channels.get_mut(&channel_id) {
                                entry.state = SouthwardConnectionState::Disconnected;
                                entry.last_activity = Utc::now();
                            }
                            info!("Channel [{monitor_channel_name}] disconnected");

                            if let Some(set_ref) = device_map.get(&channel_id) {
                                let mut events: Vec<Arc<NorthwardData>> =
                                    Vec::with_capacity(set_ref.len());
                                for device_id in set_ref.iter().copied() {
                                    if let Some(dev) = devices_table.get(&device_id) {
                                        events.push(Arc::new(NorthwardData::DeviceDisconnected(
                                            DeviceDisconnectedData {
                                                device_id,
                                                device_name: dev.config.device_name().to_string(),
                                                device_type: dev.config.device_type().to_string(),
                                            },
                                        )));
                                    }
                                }
                                if !events.is_empty() {
                                    return Some(events);
                                }
                            }
                            None
                        }
                        _ => {
                            info!("Channel [{monitor_channel_name}] in state {state}");
                            if let Some(mut entry) = monitor_channels.get_mut(&channel_id) {
                                entry.state = state;
                                entry.last_activity = Utc::now();
                            }
                            None
                        }
                    }
                };

            // Subscribe and drive connection state updates exclusively
            let mut rx = monitor_driver.subscribe_connection_state();

            // Emit initial transition based on current state to avoid missing first events
            let init_state = rx.borrow().clone();
            if let Some(events) = handle_transition(init_state) {
                for event in events.into_iter() {
                    let _ = monitor_data_tx.send(event).await;
                }
            }
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    r = rx.changed() => {
                        if r.is_err() { break; }
                        let state = rx.borrow().clone();
                        if let Some(events) = handle_transition(state) {
                            for event in events.into_iter() {
                                let _ = monitor_data_tx.send(event).await;
                            }
                        }
                    }
                }
            }
            // cleanup: remove task and token entries when exit
            tasks_ref.remove(&channel_id);
            tokens_cleanup.remove(&channel_id);
        });
        self.tasks.insert(channel_id, handle);
    }

    /// Spawn monitors for all channel ids
    pub fn spawn_all(&self, channel_ids: Vec<i32>, data_tx: &Sender<Arc<NorthwardData>>) {
        for id in channel_ids.into_iter() {
            self.spawn(id, data_tx);
        }
    }

    /// Cancel a running monitor if present
    pub fn cancel(&self, channel_id: i32) {
        if let Some((_, token)) = self.tokens.remove(&channel_id) {
            token.cancel();
        }
        if let Some((_, handle)) = self.tasks.remove(&channel_id) {
            if !handle.is_finished() {
                handle.abort();
            }
        }
    }

    /// Cancel all monitors
    pub fn cancel_all(&self) {
        let ids: Vec<i32> = self.tasks.iter().map(|e| *e.key()).collect();
        for id in ids {
            self.cancel(id);
        }
    }
}
