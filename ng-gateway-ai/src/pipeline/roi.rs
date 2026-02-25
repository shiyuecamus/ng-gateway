//! Region of Interest (ROI) cropping.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded_frame::DecodedFrame;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::types::RegionOfInterest;

    /// Crop a decoded frame to the specified ROI.
    ///
    /// Returns a new `DecodedFrame` containing only the pixels within the ROI.
    /// Coordinates are normalized `[0.0, 1.0]` and clamped to frame bounds.
    pub fn crop_frame(
        frame: &DecodedFrame,
        roi: &RegionOfInterest,
    ) -> Result<DecodedFrame, AiEngineError> {
        let x1 = ((roi.x_min * frame.width as f32).round() as u32).min(frame.width);
        let y1 = ((roi.y_min * frame.height as f32).round() as u32).min(frame.height);
        let x2 = ((roi.x_max * frame.width as f32).round() as u32).min(frame.width);
        let y2 = ((roi.y_max * frame.height as f32).round() as u32).min(frame.height);

        if x2 <= x1 || y2 <= y1 {
            return Err(AiEngineError::PreprocessError(
                "ROI has zero area after clamping".into(),
            ));
        }

        let new_w = x2 - x1;
        let new_h = y2 - y1;
        let mut data = Vec::with_capacity(DecodedFrame::expected_len(new_w, new_h));

        for y in y1..y2 {
            let row_start = (y * frame.width + x1) as usize * 3;
            let row_end = row_start + new_w as usize * 3;
            data.extend_from_slice(&frame.data[row_start..row_end]);
        }

        Ok(DecodedFrame {
            data,
            width: new_w,
            height: new_h,
        })
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
