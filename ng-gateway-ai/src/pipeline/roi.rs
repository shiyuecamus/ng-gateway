//! Region of Interest (ROI) cropping.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded::{DecodedFrame, FrameView};
    use bytes::Bytes;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::types::RegionOfInterest;

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
            let mut data = Vec::with_capacity(DecodedFrame::expected_len(
                self.plan.width,
                self.plan.height,
            ));

            for y in self.plan.y1..self.plan.y2 {
                let row_start = (y * self.frame.width + self.plan.x1) as usize * 3;
                let row_end = row_start + self.plan.width as usize * 3;
                data.extend_from_slice(&self.frame.data[row_start..row_end]);
            }

            DecodedFrame {
                data: Bytes::from(data),
                width: self.plan.width,
                height: self.plan.height,
            }
        }
    }

    /// Crop a decoded frame to the specified ROI.
    ///
    /// Returns a new `DecodedFrame` containing only the pixels within the ROI.
    /// Coordinates are normalized `[0.0, 1.0]` and clamped to frame bounds.
    pub fn crop_frame(
        frame: &DecodedFrame,
        roi: &RegionOfInterest,
    ) -> Result<DecodedFrame, AiEngineError> {
        let plan = RoiPlan::from_roi(frame, roi)?;
        let view = RoiView {
            frame: frame.view(),
            plan,
        };
        Ok(view.materialize())
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
