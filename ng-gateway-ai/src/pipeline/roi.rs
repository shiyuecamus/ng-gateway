//! Region of Interest (ROI) cropping.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded::{DecodedFrame, FrameView};
    use bytes::Bytes;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::entities::ai::pipeline::RegionOfInterest;

    /// Cropping plan for one ROI on a source frame.
    #[derive(Debug, Clone, Copy)]
    pub struct RoiPlan {
        /// Clamped pixel-space left bound.
        pub x1: u32,
        /// Clamped pixel-space top bound.
        pub y1: u32,
        /// Clamped pixel-space right bound (exclusive).
        pub x2: u32,
        /// Clamped pixel-space bottom bound (exclusive).
        pub y2: u32,
        /// Output width.
        pub width: u32,
        /// Output height.
        pub height: u32,
    }

    /// Borrowed ROI view over one source frame.
    #[derive(Debug, Clone, Copy)]
    pub struct RoiView<'a> {
        /// Source frame view.
        pub frame: FrameView<'a>,
        /// Planned ROI bounds.
        pub plan: RoiPlan,
    }

    impl RoiPlan {
        /// Build a clamped ROI plan from normalized coordinates.
        pub fn from_roi(
            frame: &DecodedFrame,
            roi: &RegionOfInterest,
        ) -> Result<Self, AiEngineError> {
            let x1 = ((roi.x_min * frame.width as f32).round() as u32).min(frame.width);
            let y1 = ((roi.y_min * frame.height as f32).round() as u32).min(frame.height);
            let x2 = ((roi.x_max * frame.width as f32).round() as u32).min(frame.width);
            let y2 = ((roi.y_max * frame.height as f32).round() as u32).min(frame.height);

            if x2 <= x1 || y2 <= y1 {
                return Err(AiEngineError::PreprocessError(
                    "ROI has zero area after clamping".into(),
                ));
            }

            Ok(Self {
                x1,
                y1,
                x2,
                y2,
                width: x2 - x1,
                height: y2 - y1,
            })
        }
    }

    impl<'a> RoiView<'a> {
        /// Materialize ROI bytes into an owned RGB24 frame.
        pub fn materialize(&self) -> DecodedFrame {
            let mut data = Vec::with_capacity(DecodedFrame::expected_rgb24_len(
                self.plan.width,
                self.plan.height,
            ));

            for y in self.plan.y1..self.plan.y2 {
                let row_start = (y * self.frame.width + self.plan.x1) as usize * 3;
                let row_end = row_start + self.plan.width as usize * 3;
                data.extend_from_slice(&self.frame.data[row_start..row_end]);
            }

            DecodedFrame::from_rgb24(Bytes::from(data), self.plan.width, self.plan.height)
        }
    }

    /// Crop a decoded frame to the specified ROI.
    ///
    /// Returns a new CPU-resident `DecodedFrame` containing only the pixels
    /// within the ROI. For DMA-buf or device frames, the data is first
    /// materialized to CPU via [`FrameMemory::to_cpu()`].
    ///
    /// Coordinates are normalized `[0.0, 1.0]` and clamped to frame bounds.
    ///
    /// # Future optimization
    ///
    /// On platforms with hardware crop support (e.g. RGA on RK3588), this
    /// could be replaced with a DMA-to-DMA crop that stays entirely in
    /// device memory. For now, we always produce a CPU-resident output.
    pub fn crop_frame(
        frame: &DecodedFrame,
        roi: &RegionOfInterest,
    ) -> Result<DecodedFrame, AiEngineError> {
        let plan = RoiPlan::from_roi(frame, roi)?;

        // Ensure we have CPU-accessible data for the crop.
        // For non-CPU frame memory, keep the materialized bytes alive locally
        // and borrow from it directly to avoid an extra to_vec() copy.
        let cpu_materialized;
        let cpu_data = if let Some(data) = frame.memory.as_cpu_slice() {
            data
        } else {
            cpu_materialized = frame.memory.to_cpu()?;
            cpu_materialized.as_ref()
        };

        let view = FrameView {
            data: cpu_data,
            width: frame.width,
            height: frame.height,
        };
        let roi_view = RoiView { frame: view, plan };
        Ok(roi_view.materialize())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_utils::*;
        use ng_gateway_models::entities::ai::pipeline::RegionOfInterest;

        #[test]
        fn roi_plan_from_normalized_coords() {
            let frame = make_solid_frame(1920, 1080, 0, 0, 0);
            let roi = RegionOfInterest {
                x_min: 0.25,
                y_min: 0.25,
                x_max: 0.75,
                y_max: 0.75,
            };

            let plan = RoiPlan::from_roi(&frame, &roi).expect("valid ROI");

            assert_eq!(plan.x1, 480);
            assert_eq!(plan.y1, 270);
            assert_eq!(plan.x2, 1440);
            assert_eq!(plan.y2, 810);
            assert_eq!(plan.width, 960);
            assert_eq!(plan.height, 540);
        }

        #[test]
        fn roi_plan_clamps_to_frame_bounds() {
            let frame = make_solid_frame(1920, 1080, 0, 0, 0);
            let roi = RegionOfInterest {
                x_min: 0.0,
                y_min: 0.0,
                x_max: 1.5,
                y_max: 1.5,
            };

            let plan = RoiPlan::from_roi(&frame, &roi).expect("clamped ROI");

            assert_eq!(plan.x2, 1920, "x2 should clamp to frame width");
            assert_eq!(plan.y2, 1080, "y2 should clamp to frame height");
        }

        #[test]
        fn roi_plan_full_frame() {
            let frame = make_solid_frame(1920, 1080, 0, 0, 0);
            let roi = RegionOfInterest {
                x_min: 0.0,
                y_min: 0.0,
                x_max: 1.0,
                y_max: 1.0,
            };

            let plan = RoiPlan::from_roi(&frame, &roi).expect("full frame ROI");

            assert_eq!(plan.width, frame.width);
            assert_eq!(plan.height, frame.height);
        }

        #[test]
        fn roi_plan_zero_area_returns_error() {
            let frame = make_solid_frame(1920, 1080, 0, 0, 0);
            let roi = RegionOfInterest {
                x_min: 0.5,
                y_min: 0.5,
                x_max: 0.5,
                y_max: 0.5,
            };

            let result = RoiPlan::from_roi(&frame, &roi);
            assert!(result.is_err(), "zero-area ROI must produce an error");
        }

        #[test]
        fn crop_frame_output_dimensions_match_plan() {
            let frame = make_solid_frame(1920, 1080, 0, 0, 0);
            let roi = RegionOfInterest {
                x_min: 0.25,
                y_min: 0.25,
                x_max: 0.75,
                y_max: 0.75,
            };

            let cropped = crop_frame(&frame, &roi).expect("crop should succeed");

            assert_eq!(cropped.width, 960);
            assert_eq!(cropped.height, 540);
        }

        #[test]
        fn crop_frame_solid_color_preserves_pixels() {
            let frame = make_solid_frame(100, 100, 255, 0, 0);
            let roi = RegionOfInterest {
                x_min: 0.1,
                y_min: 0.2,
                x_max: 0.8,
                y_max: 0.9,
            };

            let cropped = crop_frame(&frame, &roi).expect("crop should succeed");
            let data = cropped.memory.as_cpu_slice().expect("CPU data");

            // Every pixel in a solid red crop must remain (255, 0, 0).
            for chunk in data.chunks_exact(3) {
                assert_eq!(chunk, [255, 0, 0], "all pixels should be red");
            }
        }

        #[test]
        fn crop_frame_gradient_pixel_correctness() {
            let width = 100u32;
            let height = 100u32;
            let frame = make_gradient_frame(width, height);
            // Crop top-left quarter: (0.0, 0.0) → (0.5, 0.5).
            let roi = RegionOfInterest {
                x_min: 0.0,
                y_min: 0.0,
                x_max: 0.5,
                y_max: 0.5,
            };

            let cropped = crop_frame(&frame, &roi).expect("crop should succeed");
            let data = cropped.memory.as_cpu_slice().expect("CPU data");
            let cw = cropped.width;

            // Pixel at source (0, 0): gradient formula yields 0 for both x=0 and y=0.
            assert_eq!(data[0], 0u8);
            assert_eq!(data[1], 0u8);
            assert_eq!(data[2], 128);

            // Verify pixel at (10, 5) in the crop matches source (10, 5).
            let idx = ((5 * cw + 10) as usize) * 3;
            let expected_r = ((10u32 * 255) / width.max(1)) as u8;
            let expected_g = ((5u32 * 255) / height.max(1)) as u8;
            assert_eq!(data[idx], expected_r);
            assert_eq!(data[idx + 1], expected_g);
            assert_eq!(data[idx + 2], 128);
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
