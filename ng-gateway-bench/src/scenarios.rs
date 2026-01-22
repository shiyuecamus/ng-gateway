/// Benchmark scenario kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioKind {
    /// Periodic data collection (采集) workload.
    Collect,
    /// Collection under load plus control-plane downlink latency (下发) workload.
    CollectAndDownlink,
}

/// A benchmark scenario definition.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub id: u8,
    pub kind: ScenarioKind,
    pub channel_count: usize,
    pub devices_per_channel: usize,
    pub points_per_device: usize,
    pub period_ms: u32,
    pub total_points: usize,
}

impl Scenario {
    /// Create a scenario from your spec (1..=7).
    pub fn from_id(id: u8) -> Option<Self> {
        let points_per_device = 1000usize;
        match id {
            1 => Some(Self::new(
                id,
                ScenarioKind::Collect,
                1,
                10,
                points_per_device,
                1000,
            )),
            2 => Some(Self::new(
                id,
                ScenarioKind::Collect,
                5,
                10,
                points_per_device,
                1000,
            )),
            3 => Some(Self::new(
                id,
                ScenarioKind::Collect,
                10,
                10,
                points_per_device,
                1000,
            )),
            4 => Some(Self::new(
                id,
                ScenarioKind::Collect,
                1,
                1,
                points_per_device,
                100,
            )),
            5 => Some(Self::new(
                id,
                ScenarioKind::Collect,
                5,
                1,
                points_per_device,
                100,
            )),
            6 => Some(Self::new(
                id,
                ScenarioKind::Collect,
                10,
                1,
                points_per_device,
                100,
            )),
            7 => Some(Self::new(
                id,
                ScenarioKind::CollectAndDownlink,
                10,
                10,
                points_per_device,
                1000,
            )),
            _ => None,
        }
    }

    fn new(
        id: u8,
        kind: ScenarioKind,
        channel_count: usize,
        devices_per_channel: usize,
        points_per_device: usize,
        period_ms: u32,
    ) -> Self {
        let total_points = channel_count
            .saturating_mul(devices_per_channel)
            .saturating_mul(points_per_device);
        Self {
            id,
            kind,
            channel_count,
            devices_per_channel,
            points_per_device,
            period_ms,
            total_points,
        }
    }
}
