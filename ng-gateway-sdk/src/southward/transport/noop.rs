use super::SouthwardTransportMeter;

/// A no-op transport meter.
///
/// This is useful for unit tests and environments where observability is disabled.
#[derive(Debug, Default)]
pub struct NoopSouthwardTransportMeter;

impl SouthwardTransportMeter for NoopSouthwardTransportMeter {
    #[inline]
    fn add_bytes_in(&self, _bytes: u64) {}

    #[inline]
    fn add_bytes_out(&self, _bytes: u64) {}
}
