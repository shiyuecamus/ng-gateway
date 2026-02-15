/// Benchmark scenario kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioKind {
    /// Periodic data collection (采集) workload.
    Collect,
    /// Collection under load plus control-plane downlink latency via **driver** (下发) workload.
    CollectAndDownlink,
    /// Pure API write-point latency test (no local driver/channel setup).
    ///
    /// The bench acts as a plain HTTP client issuing `POST /api/point/write`
    /// requests against a running gateway instance.  Collection metrics are
    /// **not** recorded because they belong to the gateway process.
    ApiWritePoint,
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

/// Maximum scenario id supported (inclusive).
pub const MAX_SCENARIO_ID: u8 = 8;

impl Scenario {
    /// Create a scenario from your spec (1..=8).
    ///
    /// | id | kind                | description                                |
    /// |----|---------------------|--------------------------------------------|
    /// | 1  | Collect             | 1 ch · 10 dev · 1k pts · 1000 ms          |
    /// | 2  | Collect             | 5 ch · 10 dev · 1k pts · 1000 ms          |
    /// | 3  | Collect             | 10 ch · 10 dev · 1k pts · 1000 ms         |
    /// | 4  | Collect             | 1 ch · 1 dev · 1k pts · 100 ms            |
    /// | 5  | Collect             | 5 ch · 1 dev · 1k pts · 100 ms            |
    /// | 6  | Collect             | 10 ch · 1 dev · 1k pts · 100 ms           |
    /// | 7  | CollectAndDownlink  | 10 ch · 10 dev · 1k pts · 1000 ms + write |
    /// | 8  | ApiWritePoint       | pure HTTP API write_point latency test     |
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
            // Scenario 8: API-only write point.  Channel/device/point counts
            // are meaningless here; they are controlled via CLI args.
            8 => Some(Self {
                id,
                kind: ScenarioKind::ApiWritePoint,
                channel_count: 0,
                devices_per_channel: 0,
                points_per_device: 0,
                period_ms: 0,
                total_points: 0,
            }),
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
