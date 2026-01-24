use super::SouthwardTransportMeter;

/// A no-op transport meter.
///
/// This is useful for unit tests and environments where observability is disabled.
#[derive(Debug, Default)]
pub struct NoopSouthwardTransportMeter;

impl SouthwardTransportMeter for NoopSouthwardTransportMeter {
    #[inline]
    fn add_bytes_in(&self, _channel_id: i32, _driver: &str, _device_id: Option<i32>, _bytes: u64) {}

    #[inline]
    fn add_bytes_out(&self, _channel_id: i32, _driver: &str, _device_id: Option<i32>, _bytes: u64) {
    }
}
