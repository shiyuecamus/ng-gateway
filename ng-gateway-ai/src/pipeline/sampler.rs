//! Frame sampling strategies.
//!
//! Determines which frames from the video stream are submitted to the
//! AI pipeline for processing.

use ng_gateway_models::enums::ai::SamplingStrategy;
use std::time::Instant;

/// Stateful frame sampler that implements the configured sampling strategy.
pub struct FrameSampler {
    strategy: SamplingStrategy,
    /// Timestamp of the last processed frame (for target FPS mode).
    last_process_time: Option<Instant>,
    /// Baseline target interval derived from configured target FPS.
    base_interval_secs: f64,
    /// Current adaptive target interval between processed frames.
    current_interval_secs: f64,
    /// Minimum interval clamp for adaptive mode.
    min_interval_secs: f64,
    /// Maximum interval clamp for adaptive mode.
    max_interval_secs: f64,
}

impl FrameSampler {
    /// Create a new sampler from a strategy configuration.
    pub fn new(strategy: &SamplingStrategy) -> Self {
        let base_interval_secs = match strategy {
            SamplingStrategy::TargetFps { fps } => 1.0 / (*fps as f64).max(0.1),
            _ => 0.0,
        };
        Self {
            strategy: strategy.clone(),
            last_process_time: None,
            base_interval_secs,
            current_interval_secs: base_interval_secs,
            min_interval_secs: base_interval_secs * 0.4,
            max_interval_secs: base_interval_secs * 4.0,
        }
    }

    /// Determine whether a frame should be processed.
    ///
    /// - `seq`: monotonic frame sequence number.
    /// - `is_keyframe`: whether this frame is a keyframe (IDR). Derived from
    ///   GStreamer's `GST_BUFFER_FLAG_DELTA_UNIT` flag by the extractor.
    ///   For `KeyFrameOnly` strategy, only keyframes pass the gate.
    ///
    /// For `TargetFps`, uses wall-clock time rather than sequence numbers
    /// to handle variable source frame rates correctly.
    pub fn should_process(&mut self, seq: u64, is_keyframe: bool) -> bool {
        match &self.strategy {
            SamplingStrategy::EveryFrame => true,

            SamplingStrategy::FixedInterval { every_n_frames } => {
                seq.is_multiple_of(*every_n_frames as u64)
            }

            SamplingStrategy::KeyFrameOnly => is_keyframe,

            SamplingStrategy::TargetFps { .. } => {
                let now = Instant::now();
                match self.last_process_time {
                    None => {
                        self.last_process_time = Some(now);
                        true
                    }
                    Some(last) => {
                        let elapsed = now.duration_since(last).as_secs_f64();
                        if elapsed >= self.current_interval_secs {
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
        if matches!(self.strategy, SamplingStrategy::TargetFps { .. }) {
            self.current_interval_secs = self.base_interval_secs;
        }
    }

    /// Provide runtime feedback so `TargetFps` can adapt to inference load.
    ///
    /// Behaviour:
    /// - On backpressure: decrease sampling rate quickly.
    /// - On high utilization: decrease sampling rate gradually.
    /// - On low utilization: increase sampling rate gradually.
    pub fn on_feedback(&mut self, inference_latency_secs: Option<f64>, backpressure: bool) {
        if !matches!(self.strategy, SamplingStrategy::TargetFps { .. }) {
            return;
        }

        if backpressure {
            self.current_interval_secs = (self.current_interval_secs * 1.2)
                .clamp(self.min_interval_secs, self.max_interval_secs);
            return;
        }

        let Some(latency) = inference_latency_secs else {
            return;
        };

        let utilization = latency / self.current_interval_secs.max(1e-6);
        if utilization > 0.9 {
            self.current_interval_secs = (self.current_interval_secs * 1.08)
                .clamp(self.min_interval_secs, self.max_interval_secs);
        } else if utilization < 0.5 {
            self.current_interval_secs = (self.current_interval_secs * 0.92)
                .clamp(self.min_interval_secs, self.max_interval_secs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_fps_adapts_to_backpressure() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::TargetFps { fps: 10.0 });
        let before = sampler.current_interval_secs;
        sampler.on_feedback(None, true);
        assert!(sampler.current_interval_secs > before);
    }

    #[test]
    fn target_fps_adapts_up_when_underutilized() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::TargetFps { fps: 5.0 });
        let before = sampler.current_interval_secs;
        sampler.on_feedback(Some(0.01), false);
        assert!(sampler.current_interval_secs < before);
    }

    #[test]
    fn fixed_interval_samples_every_n() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::FixedInterval { every_n_frames: 5 });
        let sampled: Vec<u64> = (0..20)
            .filter(|&seq| sampler.should_process(seq, true))
            .collect();
        assert_eq!(sampled, vec![0, 5, 10, 15]);
    }

    #[test]
    fn every_frame_always_processes() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::EveryFrame);
        for seq in 0..100 {
            assert!(sampler.should_process(seq, true));
        }
    }

    #[test]
    fn keyframe_only_passes_keyframes() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::KeyFrameOnly);
        assert!(sampler.should_process(0, true), "keyframes should pass");
        assert!(
            !sampler.should_process(1, false),
            "delta frames should be rejected"
        );
        assert!(sampler.should_process(2, true), "next keyframe should pass");
    }

    #[test]
    fn reset_clears_target_fps_state() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::TargetFps { fps: 10.0 });
        assert!(sampler.should_process(0, true));
        assert!(sampler.last_process_time.is_some());

        sampler.reset();
        assert!(sampler.last_process_time.is_none());
        assert!(
            (sampler.current_interval_secs - sampler.base_interval_secs).abs() < 1e-9,
            "reset should restore the baseline interval"
        );
    }

    #[test]
    fn target_fps_interval_clamped_to_bounds() {
        let mut sampler = FrameSampler::new(&SamplingStrategy::TargetFps { fps: 10.0 });
        for _ in 0..100 {
            sampler.on_feedback(None, true);
        }
        assert!(
            sampler.current_interval_secs <= sampler.max_interval_secs + 1e-9,
            "interval should not exceed max bound"
        );

        for _ in 0..100 {
            sampler.on_feedback(Some(0.001), false);
        }
        assert!(
            sampler.current_interval_secs >= sampler.min_interval_secs - 1e-9,
            "interval should not go below min bound"
        );
    }
}
