//! Frame sampling strategies.
//!
//! Determines which frames from the video stream are submitted to the
//! AI pipeline for processing.

use ng_gateway_models::ai::pipeline::SamplingStrategy;
use std::time::Instant;

/// Stateful frame sampler that implements the configured sampling strategy.
pub struct FrameSampler {
    strategy: SamplingStrategy,
    /// Timestamp of the last processed frame (for target FPS mode).
    last_process_time: Option<Instant>,
    /// Target interval between processed frames.
    target_interval_secs: f64,
}

impl FrameSampler {
    /// Create a new sampler from a strategy configuration.
    pub fn new(strategy: &SamplingStrategy) -> Self {
        let target_interval_secs = match strategy {
            SamplingStrategy::TargetFps { fps } => 1.0 / (*fps as f64).max(0.1),
            _ => 0.0,
        };
        Self {
            strategy: strategy.clone(),
            last_process_time: None,
            target_interval_secs,
        }
    }

    /// Determine whether a frame at the given sequence number should be processed.
    ///
    /// For `TargetFps`, uses wall-clock time rather than sequence numbers
    /// to handle variable source frame rates correctly.
    pub fn should_process(&mut self, seq: u64) -> bool {
        match &self.strategy {
            SamplingStrategy::EveryFrame => true,

            SamplingStrategy::FixedInterval { every_n_frames } => {
                seq.is_multiple_of(*every_n_frames as u64)
            }

            SamplingStrategy::KeyFrameOnly => {
                // Key frame detection is handled upstream by the camera driver;
                // this sampler always returns true and relies on the driver to
                // only submit key frames.
                true
            }

            SamplingStrategy::TargetFps { .. } => {
                let now = Instant::now();
                match self.last_process_time {
                    None => {
                        self.last_process_time = Some(now);
                        true
                    }
                    Some(last) => {
                        let elapsed = now.duration_since(last).as_secs_f64();
                        if elapsed >= self.target_interval_secs {
                            self.last_process_time = Some(now);
                            true
                        } else {
                            false
                        }
                    }
                }
            }
        }
    }

    /// Reset the sampler state (e.g., on reconnection).
    pub fn reset(&mut self) {
        self.last_process_time = None;
    }
}
