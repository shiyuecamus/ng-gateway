//! Multi-object tracking stage implementation (SORT / DeepSORT-style).
//!
//! This module provides stateful per-channel tracking that assigns stable
//! `track_id` values to detections and keypoint detections across frames.

#[cfg(feature = "engine")]
mod inner {
    use ng_gateway_models::ai::{
        pipeline::TrackerAlgorithm,
        types::{BoundingBox, Detection, KeypointDetection},
    };

    /// Stateful tracker runtime bound to one channel pipeline.
    pub struct TrackerRuntime {
        algorithm: TrackerAlgorithm,
        max_age: u32,
        min_iou_for_match: f32,
        frame_index: u64,
        next_track_id: u64,
        detection_tracks: Vec<TrackState>,
        keypoint_tracks: Vec<TrackState>,
    }

    #[derive(Debug, Clone)]
    struct TrackState {
        id: u64,
        class_id: u32,
        bbox: BoundingBox,
        embedding: [f32; 4],
        last_seen_frame: u64,
    }

    impl TrackerRuntime {
        /// Create a tracker runtime for one pipeline tracker stage.
        pub fn new(algorithm: TrackerAlgorithm, max_age: u32) -> Self {
            Self {
                algorithm,
                max_age: max_age.max(1),
                min_iou_for_match: 0.1,
                frame_index: 0,
                next_track_id: 1,
                detection_tracks: Vec::with_capacity(128),
                keypoint_tracks: Vec::with_capacity(128),
            }
        }

        /// Whether this runtime can be reused for the given tracker configuration.
        #[inline]
        pub fn is_compatible(&self, algorithm: &TrackerAlgorithm, max_age: u32) -> bool {
            self.algorithm == *algorithm && self.max_age == max_age.max(1)
        }

        /// Apply tracking to detections and keypoint detections in-place.
        pub fn update(
            &mut self,
            detections: &mut [Detection],
            keypoint_detections: &mut [KeypointDetection],
        ) {
            self.frame_index = self.frame_index.saturating_add(1);
            let frame_index = self.frame_index;

            self.update_detections(frame_index, detections);
            self.update_keypoint_detections(frame_index, keypoint_detections);
            self.prune_stale_tracks(frame_index);
        }

        fn update_detections(&mut self, frame_index: u64, detections: &mut [Detection]) {
            let mut track_used = vec![false; self.detection_tracks.len()];

            for detection in detections.iter_mut() {
                let det_embedding = embedding_from_bbox(&detection.bbox);
                let match_idx = self.find_best_track_match(
                    &self.detection_tracks,
                    &track_used,
                    detection.class_id,
                    &detection.bbox,
                    det_embedding,
                );

                if let Some(idx) = match_idx {
                    let track = &mut self.detection_tracks[idx];
                    track.bbox = detection.bbox;
                    track.embedding = det_embedding;
                    track.last_seen_frame = frame_index;
                    detection.track_id = Some(track.id);
                    track_used[idx] = true;
                } else {
                    let new_id = self.allocate_track_id();
                    detection.track_id = Some(new_id);
                    self.detection_tracks.push(TrackState {
                        id: new_id,
                        class_id: detection.class_id,
                        bbox: detection.bbox,
                        embedding: det_embedding,
                        last_seen_frame: frame_index,
                    });
                    track_used.push(true);
                }
            }
        }

        fn update_keypoint_detections(
            &mut self,
            frame_index: u64,
            keypoint_detections: &mut [KeypointDetection],
        ) {
            let mut track_used = vec![false; self.keypoint_tracks.len()];

            for detection in keypoint_detections.iter_mut() {
                let det_embedding = embedding_from_bbox(&detection.bbox);
                let match_idx = self.find_best_track_match(
                    &self.keypoint_tracks,
                    &track_used,
                    detection.class_id,
                    &detection.bbox,
                    det_embedding,
                );

                if let Some(idx) = match_idx {
                    let track = &mut self.keypoint_tracks[idx];
                    track.bbox = detection.bbox;
                    track.embedding = det_embedding;
                    track.last_seen_frame = frame_index;
                    detection.track_id = Some(track.id);
                    track_used[idx] = true;
                } else {
                    let new_id = self.allocate_track_id();
                    detection.track_id = Some(new_id);
                    self.keypoint_tracks.push(TrackState {
                        id: new_id,
                        class_id: detection.class_id,
                        bbox: detection.bbox,
                        embedding: det_embedding,
                        last_seen_frame: frame_index,
                    });
                    track_used.push(true);
                }
            }
        }

        fn prune_stale_tracks(&mut self, frame_index: u64) {
            let max_age = self.max_age as u64;
            self.detection_tracks
                .retain(|t| frame_index.saturating_sub(t.last_seen_frame) <= max_age);
            self.keypoint_tracks
                .retain(|t| frame_index.saturating_sub(t.last_seen_frame) <= max_age);
        }

        fn find_best_track_match(
            &self,
            tracks: &[TrackState],
            track_used: &[bool],
            class_id: u32,
            bbox: &BoundingBox,
            embedding: [f32; 4],
        ) -> Option<usize> {
            let mut best_idx = None;
            let mut best_score = f32::MIN;

            for (idx, track) in tracks.iter().enumerate() {
                if track_used[idx] || track.class_id != class_id {
                    continue;
                }

                let iou = track.bbox.iou(bbox);
                if iou < self.min_iou_for_match {
                    continue;
                }

                let score = match &self.algorithm {
                    TrackerAlgorithm::Sort => iou,
                    TrackerAlgorithm::DeepSort { reid_model_id: _ } => {
                        // DeepSORT uses IoU + appearance affinity. We approximate
                        // appearance affinity with a geometry embedding until ReID
                        // model inference is introduced in the dedicated ReID phase.
                        let appearance = cosine_similarity(track.embedding, embedding);
                        (0.6 * iou) + (0.4 * appearance)
                    }
                };

                if score > best_score {
                    best_score = score;
                    best_idx = Some(idx);
                }
            }

            best_idx
        }

        #[inline]
        fn allocate_track_id(&mut self) -> u64 {
            let id = self.next_track_id;
            self.next_track_id = self.next_track_id.saturating_add(1);
            id
        }
    }

    #[inline]
    fn embedding_from_bbox(bbox: &BoundingBox) -> [f32; 4] {
        let cx = (bbox.x_min + bbox.x_max) * 0.5;
        let cy = (bbox.y_min + bbox.y_max) * 0.5;
        let w = (bbox.x_max - bbox.x_min).max(1e-6);
        let h = (bbox.y_max - bbox.y_min).max(1e-6);
        [cx, cy, w, h]
    }

    #[inline]
    fn cosine_similarity(a: [f32; 4], b: [f32; 4]) -> f32 {
        let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
        let a_norm = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2] + a[3] * a[3]).sqrt();
        let b_norm = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2] + b[3] * b[3]).sqrt();
        if a_norm <= 0.0 || b_norm <= 0.0 {
            0.0
        } else {
            (dot / (a_norm * b_norm)).clamp(-1.0, 1.0)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::Arc;

        fn det(class_id: u32, x1: f32, y1: f32, x2: f32, y2: f32) -> Detection {
            Detection {
                bbox: BoundingBox {
                    x_min: x1,
                    y_min: y1,
                    x_max: x2,
                    y_max: y2,
                },
                class: Arc::from("person"),
                class_id,
                confidence: 0.9,
                track_id: None,
            }
        }

        #[test]
        fn sort_keeps_stable_track_ids() {
            let mut tracker = TrackerRuntime::new(TrackerAlgorithm::Sort, 30);
            let mut frame1 = vec![det(0, 0.10, 0.10, 0.30, 0.30)];
            let mut frame2 = vec![det(0, 0.11, 0.10, 0.31, 0.30)];

            tracker.update(&mut frame1, &mut []);
            tracker.update(&mut frame2, &mut []);

            assert!(frame1[0].track_id.is_some());
            assert_eq!(frame1[0].track_id, frame2[0].track_id);
        }

        #[test]
        fn tracks_expire_after_max_age() {
            let mut tracker = TrackerRuntime::new(TrackerAlgorithm::Sort, 1);
            let mut frame1 = vec![det(0, 0.10, 0.10, 0.30, 0.30)];
            tracker.update(&mut frame1, &mut []);
            let first_id = frame1[0].track_id;

            tracker.update(&mut [], &mut []);
            tracker.update(&mut [], &mut []);

            let mut frame4 = vec![det(0, 0.10, 0.10, 0.30, 0.30)];
            tracker.update(&mut frame4, &mut []);
            assert_ne!(first_id, frame4[0].track_id);
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
