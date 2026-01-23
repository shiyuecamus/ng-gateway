//! System-level metrics updated at scrape time.

use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::core::metrics::SystemInfoSnapshot;
use prometheus::{Gauge, IntCounter, Registry};
use std::{path::Path, sync::Mutex};
use sysinfo::{get_current_pid, Disks, Networks, ProcessesToUpdate, System};
use tracing::warn;

/// Internal system state guarded by a mutex.
#[derive(Debug)]
struct SystemState {
    sys: System,
    last_net_sent: u64,
    last_net_recv: u64,
}

/// System-level metrics owned by `NGMetricsHub`.
///
/// # Notes
/// - Holds a cached `sysinfo::System` to avoid reallocating on every scrape.
/// - Registers metrics into the provided Prometheus registry at construction time.
#[derive(Debug)]
pub(crate) struct SystemMetrics {
    /// Cached `sysinfo::System`.
    state: Mutex<SystemState>,
    cpu_usage_ratio: Gauge,
    memory_usage_ratio: Gauge,
    disk_usage_ratio: Gauge,
    process_cpu_usage_ratio: Gauge,
    process_memory_rss_bytes: Gauge,
    network_bytes_sent_total: IntCounter,
    network_bytes_received_total: IntCounter,
}

impl SystemMetrics {
    /// Create and register system metrics into the given registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        Ok(Self {
            state: Mutex::new(SystemState {
                sys: System::new_all(),
                last_net_sent: 0,
                last_net_recv: 0,
            }),
            cpu_usage_ratio: register_gauge(
                registry,
                "system_cpu_usage_ratio",
                "System-wide CPU usage ratio in [0,1].",
            )?,
            memory_usage_ratio: register_gauge(
                registry,
                "system_memory_usage_ratio",
                "System-wide memory usage ratio in [0,1].",
            )?,
            disk_usage_ratio: register_gauge(
                registry,
                "system_disk_usage_ratio",
                "Root filesystem disk usage ratio in [0,1].",
            )?,
            process_cpu_usage_ratio: register_gauge(
                registry,
                "process_cpu_usage_ratio",
                "Gateway process CPU usage ratio in [0,1] (best-effort).",
            )?,
            process_memory_rss_bytes: register_gauge(
                registry,
                "process_memory_rss_bytes",
                "Gateway process memory RSS in bytes (best-effort; sysinfo units).",
            )?,
            network_bytes_sent_total: register_int_counter(
                registry,
                "network_bytes_sent_total",
                "Total bytes sent across all network interfaces since process start (best-effort).",
            )?,
            network_bytes_received_total: register_int_counter(
                registry,
                "network_bytes_received_total",
                "Total bytes received across all network interfaces since process start (best-effort).",
            )?,
        })
    }

    /// Update system metrics at scrape time.
    pub(crate) fn refresh(&self) {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(e) => {
                warn!(error=%e, "System metrics lock poisoned; skipping this scrape update");
                return;
            }
        };

        state.sys.refresh_all();

        // `global_cpu_usage()` returns percentage in [0,100].
        self.cpu_usage_ratio
            .set((state.sys.global_cpu_usage() as f64) / 100.0);

        let total = state.sys.total_memory() as f64;
        if total > 0.0 {
            self.memory_usage_ratio
                .set((state.sys.used_memory() as f64) / total);
        }

        let disks = Disks::new_with_refreshed_list();
        if let Some(root_disk) = disks
            .list()
            .iter()
            .find(|d| d.mount_point() == Path::new("/"))
        {
            let total = root_disk.total_space() as f64;
            if total > 0.0 {
                let used = (root_disk.total_space() - root_disk.available_space()) as f64;
                self.disk_usage_ratio.set(used / total);
            }
        }

        // Process metrics (best-effort; sysinfo APIs vary by platform).
        if let Ok(pid) = get_current_pid() {
            state
                .sys
                .refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
            if let Some(proc_) = state.sys.process(pid) {
                // sysinfo returns percentage in [0,100] for process cpu_usage.
                self.process_cpu_usage_ratio
                    .set((proc_.cpu_usage() as f64) / 100.0);
                // NOTE: sysinfo memory units are platform-dependent; treat as bytes best-effort.
                self.process_memory_rss_bytes.set(proc_.memory() as f64);
            }
        }

        // Network bytes (best-effort): accumulate deltas since last refresh into counters.
        let networks = Networks::new_with_refreshed_list();
        let (sent, recv) = networks.iter().fold((0u64, 0u64), |acc, (_name, data)| {
            (
                acc.0 + data.total_transmitted(),
                acc.1 + data.total_received(),
            )
        });

        let delta_sent = sent.saturating_sub(state.last_net_sent);
        let delta_recv = recv.saturating_sub(state.last_net_recv);
        state.last_net_sent = sent;
        state.last_net_recv = recv;

        if delta_sent > 0 {
            self.network_bytes_sent_total.inc_by(delta_sent);
        }
        if delta_recv > 0 {
            self.network_bytes_received_total.inc_by(delta_recv);
        }
    }

    /// Snapshot system information for REST/WS consumers.
    ///
    /// # Notes
    /// - This refreshes the underlying `sysinfo::System` so values are real-time.
    /// - This is called on control-plane paths (status endpoints), not hot paths.
    pub(crate) fn snapshot_system_info(&self) -> SystemInfoSnapshot {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(e) => {
                warn!(error=%e, "System metrics lock poisoned; returning zeroed system snapshot");
                return SystemInfoSnapshot {
                    os_type: "Unknown".to_string(),
                    os_arch: "Unknown".to_string(),
                    hostname: None,
                    cpu_cores: 0,
                    total_memory: 0,
                    used_memory: 0,
                    memory_usage_percent: 0.0,
                    cpu_usage_percent: 0.0,
                    total_disk: 0,
                    used_disk: 0,
                    disk_usage_percent: 0.0,
                };
            }
        };

        state.sys.refresh_all();

        // OS information
        let os_type = System::name().unwrap_or_else(|| "Unknown".to_string());
        let os_arch = System::cpu_arch();
        let hostname = System::host_name();

        // CPU information
        let cpu_cores = state.sys.cpus().len();
        let cpu_usage_percent = state.sys.global_cpu_usage() as f64;

        // Memory information
        let total_memory = state.sys.total_memory();
        let used_memory = state.sys.used_memory();
        let memory_usage_percent = if total_memory > 0 {
            (used_memory as f64 / total_memory as f64) * 100.0
        } else {
            0.0
        };

        // Disk information (aggregate)
        let disks = Disks::new_with_refreshed_list();
        let (total_disk, used_disk) = disks.list().iter().fold((0u64, 0u64), |acc, disk| {
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            (acc.0 + total, acc.1 + used)
        });
        let disk_usage_percent = if total_disk > 0 {
            (used_disk as f64 / total_disk as f64) * 100.0
        } else {
            0.0
        };

        SystemInfoSnapshot {
            os_type,
            os_arch,
            hostname,
            cpu_cores,
            total_memory,
            used_memory,
            memory_usage_percent,
            cpu_usage_percent,
            total_disk,
            used_disk,
            disk_usage_percent,
        }
    }

    /// Snapshot network bytes counters (best-effort).
    #[inline]
    pub(crate) fn snapshot_network_bytes(&self) -> (u64, u64) {
        (
            self.network_bytes_sent_total.get(),
            self.network_bytes_received_total.get(),
        )
    }
}

fn register_gauge(registry: &Registry, name: &'static str, help: &'static str) -> NGResult<Gauge> {
    let gauge = Gauge::new(name, help).map_err(|e| {
        NGError::from(format!(
            "Failed to create Prometheus gauge (name={name}): {e}"
        ))
    })?;

    registry.register(Box::new(gauge.clone())).map_err(|e| {
        NGError::from(format!(
            "Failed to register Prometheus gauge (name={name}): {e}"
        ))
    })?;

    Ok(gauge)
}

fn register_int_counter(
    registry: &Registry,
    name: &'static str,
    help: &'static str,
) -> NGResult<IntCounter> {
    let c = IntCounter::new(name, help).map_err(|e| {
        NGError::from(format!(
            "Failed to create Prometheus int counter (name={name}): {e}"
        ))
    })?;
    registry.register(Box::new(c.clone())).map_err(|e| {
        NGError::from(format!(
            "Failed to register Prometheus int counter (name={name}): {e}"
        ))
    })?;
    Ok(c)
}
