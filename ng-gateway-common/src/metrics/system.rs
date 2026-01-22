//! System-level metrics updated at scrape time.

use once_cell::sync::Lazy;
use prometheus::Gauge;
use std::{path::Path, sync::Mutex};
use sysinfo::{Disks, System};
use tracing::warn;

use super::REGISTRY;

struct SystemMetrics {
    /// A cached `sysinfo::System` to avoid reallocating on every scrape.
    sys: Mutex<System>,
    cpu_usage_ratio: Option<Gauge>,
    memory_usage_ratio: Option<Gauge>,
    disk_usage_ratio: Option<Gauge>,
}

static SYSTEM_METRICS: Lazy<SystemMetrics> = Lazy::new(|| SystemMetrics {
    sys: Mutex::new(System::new_all()),
    cpu_usage_ratio: register_gauge(
        "ng_gateway_system_cpu_usage_ratio",
        "System-wide CPU usage ratio in [0,1].",
    ),
    memory_usage_ratio: register_gauge(
        "ng_gateway_system_memory_usage_ratio",
        "System-wide memory usage ratio in [0,1].",
    ),
    disk_usage_ratio: register_gauge(
        "ng_gateway_system_disk_usage_ratio",
        "Root filesystem disk usage ratio in [0,1].",
    ),
});

fn register_gauge(name: &'static str, help: &'static str) -> Option<Gauge> {
    let gauge = match Gauge::new(name, help) {
        Ok(gauge) => gauge,
        Err(e) => {
            warn!(metric_name=name, error=%e, "Failed to create Prometheus gauge");
            return None;
        }
    };

    if let Err(e) = REGISTRY.register(Box::new(gauge.clone())) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name=name, error=%e, "Failed to register Prometheus gauge");
    }

    Some(gauge)
}

pub(super) fn update_system_metrics() {
    let mut sys = match SYSTEM_METRICS.sys.lock() {
        Ok(guard) => guard,
        Err(e) => {
            warn!(error=%e, "System metrics lock poisoned; skipping this scrape update");
            return;
        }
    };

    sys.refresh_all();

    if let Some(ref gauge) = SYSTEM_METRICS.cpu_usage_ratio {
        // `global_cpu_usage()` returns percentage in [0,100].
        gauge.set((sys.global_cpu_usage() as f64) / 100.0);
    }

    if let Some(ref gauge) = SYSTEM_METRICS.memory_usage_ratio {
        let total = sys.total_memory() as f64;
        if total > 0.0 {
            gauge.set((sys.used_memory() as f64) / total);
        }
    }

    if let Some(ref gauge) = SYSTEM_METRICS.disk_usage_ratio {
        let disks = Disks::new_with_refreshed_list();
        if let Some(root_disk) = disks
            .list()
            .iter()
            .find(|d| d.mount_point() == Path::new("/"))
        {
            let total = root_disk.total_space() as f64;
            if total > 0.0 {
                let used = (root_disk.total_space() - root_disk.available_space()) as f64;
                gauge.set(used / total);
            }
        }
    }
}
