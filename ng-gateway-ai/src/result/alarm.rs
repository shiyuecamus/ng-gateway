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

            AlarmCondition::ZoneDwell { .. } => {
                // ZoneDwell requires trajectory state (TrajectoryCache).
                // Without trajectory context, this condition cannot be
                // evaluated — it will be handled by the trajectory-aware
                // evaluator in the engine layer.
                (false, Vec::new())
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
            trajectory: None,
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
            trajectory: None,
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pipeline::context::PipelineContext;
        use crate::test_utils::*;
        use ng_gateway_models::domain::prelude::AlarmRuleInfo;
        use ng_gateway_models::entities::ai::alarm_rule::AlarmCondition;
        use ng_gateway_models::enums::ai::{AlarmSeverity, CrossingDirection};

        /// Build a minimal `AlarmRuleInfo` for testing.
        fn make_rule(condition: AlarmCondition) -> AlarmRuleInfo {
            AlarmRuleInfo {
                id: 1,
                name: "test_rule".to_string(),
                pipeline_id: 1,
                rule_order: 0,
                severity: AlarmSeverity::Warning,
                condition,
                cooldown_secs: 0,
                min_duration_secs: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }
        }

        /// Build a `PipelineContext` with pre-populated detections.
        fn context_with_detections(
            detections: Vec<ng_gateway_models::domain::prelude::Detection>,
        ) -> PipelineContext {
            let frame = make_solid_frame(640, 480, 0, 0, 0);
            let mut ctx = PipelineContext::new(frame);
            ctx.detections = detections;
            ctx
        }

        // ── ClassDetected ────────────────────────────────────────────

        #[test]
        fn class_detected_triggers_when_present() {
            let rule = make_rule(AlarmCondition::ClassDetected {
                class: "person".into(),
                min_confidence: 0.5,
            });
            let ctx =
                context_with_detections(vec![make_detection("person", 0, 0.1, 0.1, 0.5, 0.5, 0.8)]);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert_eq!(alarms.len(), 1);
        }

        #[test]
        fn class_detected_below_confidence_no_alarm() {
            let rule = make_rule(AlarmCondition::ClassDetected {
                class: "person".into(),
                min_confidence: 0.5,
            });
            let ctx =
                context_with_detections(vec![make_detection("person", 0, 0.1, 0.1, 0.5, 0.5, 0.3)]);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert!(alarms.is_empty(), "low confidence should not trigger");
        }

        #[test]
        fn class_detected_wrong_class_no_alarm() {
            let rule = make_rule(AlarmCondition::ClassDetected {
                class: "person".into(),
                min_confidence: 0.5,
            });
            let ctx =
                context_with_detections(vec![make_detection("car", 1, 0.1, 0.1, 0.5, 0.5, 0.9)]);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert!(alarms.is_empty(), "wrong class should not trigger");
        }

        // ── CountExceeds ─────────────────────────────────────────────

        #[test]
        fn count_exceeds_triggers() {
            let rule = make_rule(AlarmCondition::CountExceeds {
                class: Some("person".into()),
                threshold: 3,
            });
            let dets: Vec<_> = (0..5)
                .map(|i| {
                    make_detection(
                        "person",
                        0,
                        i as f32 * 0.1,
                        0.1,
                        i as f32 * 0.1 + 0.08,
                        0.5,
                        0.9,
                    )
                })
                .collect();
            let ctx = context_with_detections(dets);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert_eq!(alarms.len(), 1, "5 > 3 should trigger");
        }

        #[test]
        fn count_exceeds_at_boundary_no_alarm() {
            let rule = make_rule(AlarmCondition::CountExceeds {
                class: Some("person".into()),
                threshold: 3,
            });
            let dets: Vec<_> = (0..3)
                .map(|i| {
                    make_detection(
                        "person",
                        0,
                        i as f32 * 0.1,
                        0.1,
                        i as f32 * 0.1 + 0.08,
                        0.5,
                        0.9,
                    )
                })
                .collect();
            let ctx = context_with_detections(dets);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert!(
                alarms.is_empty(),
                "count == threshold (3) should NOT trigger (strictly >)"
            );
        }

        // ── ZoneIntrusion ────────────────────────────────────────────

        #[test]
        fn zone_intrusion_inside_triggers() {
            // Square zone covering (0.2, 0.2) → (0.8, 0.8).
            let zone = vec![(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
            let rule = make_rule(AlarmCondition::ZoneIntrusion {
                zone,
                class: Some("person".into()),
            });
            // Detection centered at (0.3, 0.3) — inside the zone.
            let ctx =
                context_with_detections(vec![make_detection("person", 0, 0.2, 0.2, 0.4, 0.4, 0.9)]);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert_eq!(alarms.len(), 1, "detection inside zone should trigger");
        }

        #[test]
        fn zone_intrusion_outside_no_alarm() {
            let zone = vec![(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
            let rule = make_rule(AlarmCondition::ZoneIntrusion {
                zone,
                class: Some("person".into()),
            });
            // Detection centered at (0.05, 0.05) — outside the zone.
            let ctx =
                context_with_detections(vec![make_detection("person", 0, 0.0, 0.0, 0.1, 0.1, 0.9)]);

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert!(
                alarms.is_empty(),
                "detection outside zone should not trigger"
            );
        }

        // ── LineCrossing (with history) ──────────────────────────────

        #[test]
        fn line_crossing_with_history_triggers() {
            // Vertical line at x = 0.5 (from top to bottom).
            let rule = make_rule(AlarmCondition::LineCrossing {
                line: [(0.5, 0.0), (0.5, 1.0)],
                class: None,
                direction: Some(CrossingDirection::Any),
            });

            // Frame 1: track #1 at center (0.3, 0.5) — left of the line.
            let prev_det = with_track_id(make_detection("person", 0, 0.2, 0.4, 0.4, 0.6, 0.9), 1);
            let prev_ctx = context_with_detections(vec![prev_det]);

            let mut history = TrackHistory::default();
            history.update_from_context(&prev_ctx);

            // Frame 2: track #1 moved to center (0.7, 0.5) — right of the line.
            let curr_det = with_track_id(make_detection("person", 0, 0.6, 0.4, 0.8, 0.6, 0.9), 1);
            let curr_ctx = context_with_detections(vec![curr_det]);

            let alarms = evaluate_alarm_rules_with_history(&[rule], &curr_ctx, &history);
            assert_eq!(alarms.len(), 1, "track crossing the line should trigger");
        }

        // ── AnomalyDetected ─────────────────────────────────────────

        #[test]
        fn anomaly_detected_above_threshold() {
            use ng_gateway_models::domain::prelude::AnomalyMap;

            let rule = make_rule(AlarmCondition::AnomalyDetected { min_score: 0.5 });
            let frame = make_solid_frame(640, 480, 0, 0, 0);
            let mut ctx = PipelineContext::new(frame);
            ctx.anomaly_maps.push(AnomalyMap {
                score: 0.8,
                heatmap: None,
                heatmap_width: 0,
                heatmap_height: 0,
                is_anomalous: true,
                threshold: 0.5,
            });

            let alarms = evaluate_alarm_rules(&[rule], &ctx);
            assert_eq!(alarms.len(), 1, "anomaly above threshold should trigger");
        }

        // ── TrackHistory ─────────────────────────────────────────────

        #[test]
        fn track_history_records_and_retrieves() {
            let det = with_track_id(make_detection("person", 0, 0.2, 0.3, 0.4, 0.5, 0.9), 42);
            let ctx = context_with_detections(vec![det]);

            let mut history = TrackHistory::default();
            history.update_from_context(&ctx);

            let center = history.prev_center(42).expect("track 42 should exist");
            // Center = ((0.2 + 0.4) / 2, (0.3 + 0.5) / 2) = (0.3, 0.4).
            assert!(
                (center.0 - 0.3).abs() < 1e-5 && (center.1 - 0.4).abs() < 1e-5,
                "expected center (0.3, 0.4), got ({}, {})",
                center.0,
                center.1,
            );

            assert!(
                history.prev_center(999).is_none(),
                "unknown track should return None",
            );
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
