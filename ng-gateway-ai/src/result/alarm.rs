//! Alarm rule evaluation engine.

#[cfg(feature = "engine")]
mod inner {
    use crate::pipeline::context::PipelineContext;
    use ng_gateway_models::ai::{
        pipeline::{AlarmCondition, AlarmRule, CrossingDirection},
        types::{AlarmEvent, Detection},
    };
    use std::sync::Arc;

    /// Evaluate alarm rules against the pipeline context and produce alarm events.
    pub fn evaluate_alarm_rules(rules: &[AlarmRule], context: &PipelineContext) -> Vec<AlarmEvent> {
        let mut alarms = Vec::new();

        for rule in rules {
            if let Some(alarm) = evaluate_single_rule(rule, context) {
                alarms.push(alarm);
            }
        }

        alarms
    }

    fn evaluate_single_rule(rule: &AlarmRule, context: &PipelineContext) -> Option<AlarmEvent> {
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
            } => evaluate_line_crossing(context, line, class.as_deref(), *direction),

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

    /// Evaluate line-crossing for tracked detections.
    ///
    /// A crossing is detected when a tracked object's center point transitions
    /// from one side of the line to the other between the previous and current
    /// positions. This requires the Tracker stage to populate `track_id`.
    ///
    /// For Phase 1 without a full trajectory store, we approximate by checking
    /// if any detection's center is very close to (within ~2% of) the line,
    /// combined with the track_id being present.
    fn evaluate_line_crossing(
        context: &PipelineContext,
        line: &[(f32, f32); 2],
        class_filter: Option<&str>,
        direction: Option<CrossingDirection>,
    ) -> (bool, Vec<Detection>) {
        let (x1, y1) = line[0];
        let (x2, y2) = line[1];

        let matches: Vec<Detection> = context
            .detections
            .iter()
            .filter(|d| {
                if let Some(cls) = class_filter {
                    if d.class.as_ref() != cls {
                        return false;
                    }
                }
                // Require tracking ID for line crossing
                if d.track_id.is_none() {
                    return false;
                }

                let cx = (d.bbox.x_min + d.bbox.x_max) / 2.0;
                let cy = (d.bbox.y_min + d.bbox.y_max) / 2.0;

                // Signed distance from point to line (cross product method).
                // Positive = left side, negative = right side.
                let cross = (x2 - x1) * (cy - y1) - (y2 - y1) * (cx - x1);
                let line_len_sq = (x2 - x1).powi(2) + (y2 - y1).powi(2);
                if line_len_sq <= 0.0 {
                    return false;
                }
                let normalized_dist = cross.abs() / line_len_sq.sqrt();

                // Proximity threshold: within ~2% of frame diagonal
                let is_near_line = normalized_dist < 0.02;
                if !is_near_line {
                    return false;
                }

                // Apply direction filter if specified
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
