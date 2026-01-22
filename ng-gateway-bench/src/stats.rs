use std::time::Duration;

/// A lightweight latency/elapsed-time statistics collector.
///
/// # Notes
/// - We avoid external histogram crates to keep dependencies minimal.
/// - This is good enough for the requested min/max/avg outputs.
#[derive(Debug, Default, Clone)]
pub struct DurationStats {
    count: u64,
    sum_ns: u128,
    min_ns: Option<u128>,
    max_ns: Option<u128>,
}

impl DurationStats {
    /// Record a duration sample.
    #[inline]
    pub fn record(&mut self, d: Duration) {
        let ns = d.as_nanos();
        self.count = self.count.saturating_add(1);
        self.sum_ns = self.sum_ns.saturating_add(ns);
        self.min_ns = Some(self.min_ns.map(|x| x.min(ns)).unwrap_or(ns));
        self.max_ns = Some(self.max_ns.map(|x| x.max(ns)).unwrap_or(ns));
    }

    /// Merge another stats collector into this one.
    #[inline]
    pub fn merge_from(&mut self, other: &DurationStats) {
        self.count = self.count.saturating_add(other.count);
        self.sum_ns = self.sum_ns.saturating_add(other.sum_ns);
        if let Some(min) = other.min_ns {
            self.min_ns = Some(self.min_ns.map(|x| x.min(min)).unwrap_or(min));
        }
        if let Some(max) = other.max_ns {
            self.max_ns = Some(self.max_ns.map(|x| x.max(max)).unwrap_or(max));
        }
    }

    /// Return number of samples.
    #[inline]
    #[allow(unused)]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Return min duration.
    #[inline]
    pub fn min(&self) -> Option<Duration> {
        self.min_ns.map(|ns| Duration::from_nanos(ns as u64))
    }

    /// Return max duration.
    #[inline]
    pub fn max(&self) -> Option<Duration> {
        self.max_ns.map(|ns| Duration::from_nanos(ns as u64))
    }

    /// Return average duration.
    #[inline]
    pub fn avg(&self) -> Option<Duration> {
        if self.count == 0 {
            return None;
        }
        let avg_ns = self.sum_ns / (self.count as u128);
        Some(Duration::from_nanos(avg_ns as u64))
    }
}

/// Format a duration for human-friendly table output.
#[inline]
pub fn fmt_duration_ms(d: Option<Duration>) -> String {
    match d {
        Some(v) => format!("{:.3} ms", (v.as_secs_f64() * 1000.0)),
        None => "-".to_string(),
    }
}
