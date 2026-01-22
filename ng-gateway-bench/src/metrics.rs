use std::time::{Duration, Instant};
use sysinfo::{Networks, Pid, ProcessesToUpdate, System};

/// A snapshot of system/process metrics used by benchmark reports.
#[derive(Debug, Clone)]
pub struct MetricsSummary {
    /// Average CPU usage of current process in percent.
    pub avg_process_cpu_pct: f32,
    /// Peak resident memory of current process (bytes).
    pub peak_process_rss_bytes: u64,
    /// Average network receive rate across all interfaces (bytes/sec).
    pub avg_net_rx_bps: f64,
    /// Average network transmit rate across all interfaces (bytes/sec).
    pub avg_net_tx_bps: f64,
}

impl MetricsSummary {
    /// Format bytes as MiB string.
    pub fn fmt_mib(bytes: u64) -> String {
        format!("{:.2} MiB", (bytes as f64) / (1024.0 * 1024.0))
    }

    /// Format bytes/sec as kB/s string (decimal, 1 kB = 1000 bytes).
    pub fn fmt_kb_per_sec(bps: f64) -> String {
        format!("{:.0}kB/s", bps / 1000.0)
    }
}

/// A low-overhead sampler for process CPU/memory and system network I/O.
///
/// # Notes
/// - CPU usage in sysinfo is computed between refreshes; we sample at a fixed interval.
/// - Network stats are system-wide (not per-process), but this is sufficient for relative benchmarks.
pub struct MetricsSampler {
    pid: Pid,
    system: System,
    networks: Networks,
    start: Instant,

    cpu_sum: f64,
    cpu_count: u64,
    rss_peak: u64,

    net_rx_start: u64,
    net_tx_start: u64,
}

impl MetricsSampler {
    /// Create a new sampler for the current process.
    pub fn new() -> anyhow::Result<Self> {
        let pid = sysinfo::get_current_pid()
            .map_err(|e| anyhow::anyhow!("get_current_pid failed: {e}"))?;
        let mut system = System::new();
        let pids = [pid];
        let _ = system.refresh_processes(ProcessesToUpdate::Some(&pids), true);

        let mut networks = Networks::new_with_refreshed_list();
        networks.refresh(false);
        let (rx, tx) = total_net_bytes(&networks);

        Ok(Self {
            pid,
            system,
            networks,
            start: Instant::now(),
            cpu_sum: 0.0,
            cpu_count: 0,
            rss_peak: 0,
            net_rx_start: rx,
            net_tx_start: tx,
        })
    }

    /// Sample once.
    pub fn sample(&mut self) {
        let pids = [self.pid];
        let _ = self
            .system
            .refresh_processes(ProcessesToUpdate::Some(&pids), true);
        if let Some(p) = self.system.process(self.pid) {
            // CPU usage is in percent across all cores.
            let cpu = p.cpu_usage() as f64;
            self.cpu_sum += cpu;
            self.cpu_count = self.cpu_count.saturating_add(1);

            // `Process::memory()` returns resident set size (RSS) in bytes.
            let rss_bytes = p.memory();
            self.rss_peak = self.rss_peak.max(rss_bytes);
        }

        self.networks.refresh(false);
    }

    /// Build a summary from collected samples.
    pub fn finish(mut self) -> MetricsSummary {
        // Final refresh for network totals.
        self.networks.refresh(false);
        let (rx_end, tx_end) = total_net_bytes(&self.networks);
        let elapsed = self.start.elapsed().as_secs_f64().max(1e-9);

        let avg_cpu = if self.cpu_count == 0 {
            0.0
        } else {
            (self.cpu_sum / (self.cpu_count as f64)) as f32
        };

        let rx_bps = (rx_end.saturating_sub(self.net_rx_start) as f64) / elapsed;
        let tx_bps = (tx_end.saturating_sub(self.net_tx_start) as f64) / elapsed;

        MetricsSummary {
            avg_process_cpu_pct: avg_cpu,
            peak_process_rss_bytes: self.rss_peak,
            avg_net_rx_bps: rx_bps,
            avg_net_tx_bps: tx_bps,
        }
    }
}

/// Run a sampler loop for the given duration.
pub async fn sample_for(duration: Duration, interval: Duration) -> anyhow::Result<MetricsSummary> {
    let mut sampler = MetricsSampler::new()?;
    let start = Instant::now();

    while start.elapsed() < duration {
        sampler.sample();
        tokio::time::sleep(interval).await;
    }

    Ok(sampler.finish())
}

#[inline]
fn total_net_bytes(networks: &Networks) -> (u64, u64) {
    let mut rx: u64 = 0;
    let mut tx: u64 = 0;
    for (_name, data) in networks.iter() {
        rx = rx.saturating_add(data.received());
        tx = tx.saturating_add(data.transmitted());
    }
    (rx, tx)
}
