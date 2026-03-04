//! ROI coordinate remapping — maps sub-frame results back to full frame space.

use crate::pipeline::context::PipelineContext;
use ng_gateway_models::{
    domain::prelude::{BoundingBox, SegmentationMask},
    entities::ai::pipeline::RegionOfInterest,
};

/// Remap all results in a pipeline context from ROI-local space to full frame space.
pub(super) fn remap_context_to_full_frame(
    context: &mut PipelineContext,
    roi: &RegionOfInterest,
    full_width: u32,
    full_height: u32,
) {
    for det in &mut context.detections {
        det.bbox = remap_bbox_to_full_frame(det.bbox, roi);
    }
    for kpd in &mut context.keypoint_detections {
        kpd.bbox = remap_bbox_to_full_frame(kpd.bbox, roi);
        for kp in &mut kpd.keypoints {
            kp.x = remap_scalar_to_full_frame(kp.x, roi.x_min, roi.x_max);
            kp.y = remap_scalar_to_full_frame(kp.y, roi.y_min, roi.y_max);
        }
    }
    for mask in &mut context.segmentation_masks {
        remap_segmentation_mask_to_full_frame_in_place(mask, roi, full_width, full_height);
    }
}

#[inline]
fn remap_bbox_to_full_frame(bbox: BoundingBox, roi: &RegionOfInterest) -> BoundingBox {
    BoundingBox {
        x_min: remap_scalar_to_full_frame(bbox.x_min, roi.x_min, roi.x_max),
        y_min: remap_scalar_to_full_frame(bbox.y_min, roi.y_min, roi.y_max),
        x_max: remap_scalar_to_full_frame(bbox.x_max, roi.x_min, roi.x_max),
        y_max: remap_scalar_to_full_frame(bbox.y_max, roi.y_min, roi.y_max),
    }
}

#[inline]
fn remap_scalar_to_full_frame(v: f32, min: f32, max: f32) -> f32 {
    (min + v * (max - min)).clamp(0.0, 1.0)
}

fn remap_segmentation_mask_to_full_frame_in_place(
    mask: &mut SegmentationMask,
    roi: &RegionOfInterest,
    full_width: u32,
    full_height: u32,
) {
    let fw = full_width as usize;
    let fh = full_height as usize;
    if fw == 0 || fh == 0 || mask.width == 0 || mask.height == 0 {
        return;
    }
    let mut full_mask = vec![0u8; fw * fh];
    let x_start = (roi.x_min * full_width as f32)
        .floor()
        .clamp(0.0, full_width as f32) as u32;
    let y_start = (roi.y_min * full_height as f32)
        .floor()
        .clamp(0.0, full_height as f32) as u32;
    let x_end = (roi.x_max * full_width as f32)
        .ceil()
        .clamp(0.0, full_width as f32) as u32;
    let y_end = (roi.y_max * full_height as f32)
        .ceil()
        .clamp(0.0, full_height as f32) as u32;
    let roi_w = (x_end.saturating_sub(x_start)).max(1);
    let roi_h = (y_end.saturating_sub(y_start)).max(1);
    let mw = mask.width as usize;
    let mh = mask.height as usize;
    if mask.mask.len() != mw * mh {
        return;
    }
    for gy in y_start..y_end {
        for gx in x_start..x_end {
            let local_x = gx.saturating_sub(x_start) as f32 / roi_w as f32;
            let local_y = gy.saturating_sub(y_start) as f32 / roi_h as f32;
            let mx = ((local_x * mask.width as f32).floor() as u32).min(mask.width - 1) as usize;
            let my = ((local_y * mask.height as f32).floor() as u32).min(mask.height - 1) as usize;
            let class_idx = mask.mask[my * mw + mx];
            full_mask[gy as usize * fw + gx as usize] = class_idx;
        }
    }
    mask.mask = full_mask;
    mask.width = full_width;
    mask.height = full_height;
}
