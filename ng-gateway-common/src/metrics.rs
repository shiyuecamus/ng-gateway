use opentelemetry::{
    metrics::{Meter, MeterProvider},
    KeyValue,
};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use std::{path::Path, sync::Arc};
use sysinfo::{Disks, System};
use tokio_cron_scheduler::{JobScheduler, JobSchedulerError};

/// System metrics collector that periodically collects CPU and memory usage
pub struct SystemMetricsCollector {
    meter: Meter,
}

impl SystemMetricsCollector {
    /// Creates a new system metrics collector
    pub async fn new(
        scheduler: JobScheduler,
        provider: &Arc<SdkMeterProvider>,
    ) -> Result<Self, JobSchedulerError> {
        let meter = provider.meter("system_metrics");
        let cpu_usage = meter
            .f64_observable_up_down_counter("system.cpu.usage")
            .with_description("CPU usage percentage")
            .with_callback(|observer| {
                let mut sys = System::new_all();
                sys.refresh_all();

                observer.observe(
                    sys.global_cpu_usage() as f64,
                    &[KeyValue::new("type", "cpu")],
                );
            })
            .build();
        let memory_usage = meter
            .f64_observable_up_down_counter("system.memory.usage")
            .with_description("Memory usage in bytes")
            .with_callback(|observer| {
                let mut sys = System::new_all();
                sys.refresh_all();

                observer.observe(sys.used_memory() as f64, &[KeyValue::new("type", "memory")]);
            })
            .build();
        let disk_usage = meter
            .f64_observable_up_down_counter("system.disk.usage")
            .with_description("Disk usage in bytes")
            .with_callback(|observer| {
                let disks = Disks::new_with_refreshed_list();
                if let Some(root_disk) = disks
                    .list()
                    .iter()
                    .find(|d| d.mount_point() == Path::new("/"))
                {
                    observer.observe(
                        (root_disk.total_space() - root_disk.available_space()) as f64,
                        &[KeyValue::new("type", "disk")],
                    );
                }
            })
            .build();

        Ok(Self { meter })
    }
}
