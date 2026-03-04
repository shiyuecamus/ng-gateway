//! Alarm rule evaluation engine.

#[cfg(feature = "engine")]
mod inner {
    use crate::pipeline::context::PipelineContext;
    use ng_gateway_models::{
        domain::prelude::{AlarmEvent, AlarmRuleInfo, Detection},
        entities::ai::alarm_rule::AlarmCondition,
        enums::ai::CrossingDirection,
    };
    use std::{collections::HashMap, sync::Arc};

    /// Per-track position history for line-crossing detection.
    ///
    /// Stores the previous frame's center point for each tracked object,
    /// enabling true cross-product sign-change detection between frames.
    #[derive(Debug, Default)]
    pub struct TrackHistory {
        /// Previous center position (cx, cy) per track_id.
        prev_centers: HashMap<u64, (f32, f32)>,
    }

    impl TrackHistory {
        /// Record current detection centers for the next frame's comparison.
        pub fn update_from_context(&mut self, context: &PipelineContext) {
            self.prev_centers.clear();
            for det in &context.detections {
                if let Some(track_id) = det.track_id {
                    let cx = (det.bbox.x_min + det.bbox.x_max) / 2.0;
                    let cy = (det.bbox.y_min + det.bbox.y_max) / 2.0;
                    self.prev_centers.insert(track_id, (cx, cy));
                }
            }
        }

        /// Get the previous center for a given track_id.
        fn prev_center(&self, track_id: u64) -> Option<(f32, f32)> {
            self.prev_centers.get(&track_id).copied()
        }
    }

    /// Evaluate alarm rules against the pipeline context and produce alarm events.
    pub fn evaluate_alarm_rules(
        rules: &[AlarmRuleInfo],
        context: &PipelineContext,
    ) -> Vec<AlarmEvent> {
        let mut alarms = Vec::new();

        for rule in rules {
            if let Some(alarm) = evaluate_single_rule(rule, context) {
                alarms.push(alarm);
            }
        }

        alarms
    }

    /// Evaluate alarm rules with track history for trajectory-based conditions.
    pub fn evaluate_alarm_rules_with_history(
        rules: &[AlarmRuleInfo],
        context: &PipelineContext,
        history: &TrackHistory,
    ) -> Vec<AlarmEvent> {
        let mut alarms = Vec::new();

        for rule in rules {
            if let Some(alarm) = evaluate_single_rule_with_history(rule, context, history) {
                alarms.push(alarm);
            }
        }

        alarms
    }

    fn evaluate_single_rule(rule: &AlarmRuleInfo, context: &PipelineContext) -> Option<AlarmEvent> {
        let (triggered, related) = match &rule.condition {
            AlarmCondition::ClassDetected {
                class,
                min_confidence,
            } => {
                let matches: Vec<Detection> = context
                    .detections
                    .iter()
                    .filter(|d| d.class.as_ref() == class && d.confidence >= *min_confidence)
                    .cloned()
                    .collect();
                (!matches.is_empty(), matches)
            }

            AlarmCondition::CountExceeds { class, threshold } => {
                let count = match class {
                    Some(cls) => context
                        .detections
                        .iter()
                        .filter(|d| d.class.as_ref() == cls.as_str())
                        .count(),
                    None => context.detections.len(),
                };
                (count > *threshold as usize, Vec::new())
            }

            AlarmCondition::ZoneIntrusion { zone, class } => {
                if zone.len() < 3 {
                    return None;
                }
                let matches: Vec<Detection> = context
                    .detections
                    .iter()
                    .filter(|d| {
                        let class_match = class
                            .as_ref()
                            .is_none_or(|c| d.class.as_ref() == c.as_str());
                        if !class_match {
                            return false;
                        }
                        let cx = (d.bbox.x_min + d.bbox.x_max) / 2.0;
                        let cy = (d.bbox.y_min + d.bbox.y_max) / 2.0;
                        point_in_polygon(cx, cy, zone)
                    })
                    .cloned()
                    .collect();
                (!matches.is_empty(), matches)
            }

            AlarmCondition::LineCrossing {
                line,
                class,
                direction,
            } => evaluate_line_crossing_proximity(context, line, class.as_deref(), *direction),

            AlarmCondition::AnomalyDetected { min_score } => {
                let triggered = context
                    .anomaly_maps
                    .iter()
                    .any(|a| a.is_anomalous && a.score >= *min_score);
                (triggered, Vec::new())
            }

            AlarmCondition::CustomWasm { .. } => {
                // Phase 2: WASM-based alarm evaluation
                (false, Vec::new())
            }
        };

        if !triggered {
            return None;
        }

        Some(AlarmEvent {
            alarm_type: Arc::from(rule.name.as_str()),
            description: Arc::from(format!("Alarm rule '{}' triggered", rule.name)),
            severity: rule.severity,
            related_detections: related,
            snapshot: None,
        })
    }

    /// Evaluate line-crossing with trajectory history.
    ///
    /// Uses the `TrackHistory` to compare each tracked detection's current
    /// center against its previous-frame center. A crossing is detected when
    /// the signed cross-product relative to the line changes sign between
    /// frames, indicating the object moved from one side to the other.
    fn evaluate_line_crossing_with_history(
        context: &PipelineContext,
        line: &[(f32, f32); 2],
        class_filter: Option<&str>,
        direction: Option<CrossingDirection>,
        history: &TrackHistory,
    ) -> (bool, Vec<Detection>) {
        let (lx1, ly1) = line[0];
        let (lx2, ly2) = line[1];
        let line_dx = lx2 - lx1;
        let line_dy = ly2 - ly1;

        if line_dx.powi(2) + line_dy.powi(2) <= 0.0 {
            return (false, Vec::new());
        }

        let matches: Vec<Detection> = context
            .detections
            .iter()
            .filter(|d| {
                if let Some(cls) = class_filter {
                    if d.class.as_ref() != cls {
                        return false;
                    }
                }
                let track_id = match d.track_id {
                    Some(id) => id,
                    None => return false,
                };
                let prev = match history.prev_center(track_id) {
                    Some(p) => p,
                    None => return false,
                };

                let cx = (d.bbox.x_min + d.bbox.x_max) / 2.0;
                let cy = (d.bbox.y_min + d.bbox.y_max) / 2.0;

                let cross_prev = line_dx * (prev.1 - ly1) - line_dy * (prev.0 - lx1);
                let cross_curr = line_dx * (cy - ly1) - line_dy * (cx - lx1);

                let sign_changed = cross_prev * cross_curr < 0.0;
                if !sign_changed {
                    return false;
                }

                match direction {
                    Some(CrossingDirection::LeftToRight) => cross_prev > 0.0 && cross_curr < 0.0,
                    Some(CrossingDirection::RightToLeft) => cross_prev < 0.0 && cross_curr > 0.0,
                    Some(CrossingDirection::Any) | None => true,
                }
            })
            .cloned()
            .collect();

        (!matches.is_empty(), matches)
    }

    /// Fallback line-crossing using proximity heuristic (no history available).
    fn evaluate_line_crossing_proximity(
        context: &PipelineContext,
        line: &[(f32, f32); 2],
        class_filter: Option<&str>,
        direction: Option<CrossingDirection>,
    ) -> (bool, Vec<Detection>) {
        let (x1, y1) = line[0];
        let (x2, y2) = line[1];
        let line_len_sq = (x2 - x1).powi(2) + (y2 - y1).powi(2);
        if line_len_sq <= 0.0 {
            return (false, Vec::new());
        }

        let matches: Vec<Detection> = context
            .detections
            .iter()
            .filter(|d| {
                if let Some(cls) = class_filter {
                    if d.class.as_ref() != cls {
                        return false;
                    }
                }
                if d.track_id.is_none() {
                    return false;
                }

                let cx = (d.bbox.x_min + d.bbox.x_max) / 2.0;
                let cy = (d.bbox.y_min + d.bbox.y_max) / 2.0;
                let cross = (x2 - x1) * (cy - y1) - (y2 - y1) * (cx - x1);
                let normalized_dist = cross.abs() / line_len_sq.sqrt();

                if normalized_dist >= 0.02 {
                    return false;
                }

                match direction {
                    Some(CrossingDirection::LeftToRight) => cross < 0.0,
                    Some(CrossingDirection::RightToLeft) => cross > 0.0,
                    Some(CrossingDirection::Any) | None => true,
                }
            })
            .cloned()
            .collect();

        (!matches.is_empty(), matches)
    }

    /// Dispatch to single rule evaluation with history awareness.
    fn evaluate_single_rule_with_history(
        rule: &AlarmRuleInfo,
        context: &PipelineContext,
        history: &TrackHistory,
    ) -> Option<AlarmEvent> {
        let (triggered, related) = match &rule.condition {
            AlarmCondition::LineCrossing {
                line,
                class,
                direction,
            } => evaluate_line_crossing_with_history(
                context,
                line,
                class.as_deref(),
                *direction,
                history,
            ),
            _ => {
                return evaluate_single_rule(rule, context);
            }
        };

        if !triggered {
            return None;
        }

        Some(AlarmEvent {
            alarm_type: Arc::from(rule.name.as_str()),
            description: Arc::from(format!("Alarm rule '{}' triggered", rule.name)),
            severity: rule.severity,
            related_detections: related,
            snapshot: None,
        })
    }

    /// Ray-casting point-in-polygon test.
    fn point_in_polygon(px: f32, py: f32, polygon: &[(f32, f32)]) -> bool {
        let n = polygon.len();
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = polygon[i];
            let (xj, yj) = polygon[j];
            if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
                inside = !inside;
            }
            j = i;
        }
        inside
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
