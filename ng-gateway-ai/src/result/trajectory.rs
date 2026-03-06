//! Trajectory-based alarm engine.
//!
//! Provides per-track trajectory state management and evaluation functions
//! for trajectory-dependent alarm conditions: line-crossing with proper
//! debouncing and zone-dwell with the three-event model (Entered /
//! DwellTimeout / Exited).
//!
//! # Architecture
//!
//! ```text
//! ┌── Per-Channel ────────────────────────────────────────────┐
//! │  TrajectoryCache                                          │
//! │    tracks: HashMap<u64, TrackTrajectory>                  │
//! │                                                           │
//! │    ┌── TrackTrajectory (per track_id) ──────────────────┐ │
//! │    │  positions: VecDeque<(ts, cx, cy)>  (ring buffer)  │ │
//! │    │  velocity: (vx, vy)                                │ │
//! │    │  direction_deg: f32                                 │ │
//! │    │  zone_entry: HashMap<rule_id, entry_ts>            │ │
//! │    │  rule_cooldowns: HashMap<rule_id, last_trigger_ts> │ │
//! │    └────────────────────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────────────┘
//! ```

#[cfg(feature = "engine")]
mod inner {
    use ng_gateway_models::{
        domain::prelude::{AlarmEvent, Detection, TrajectoryContext},
        entities::ai::alarm_rule::AlarmCondition,
        enums::ai::{AlarmSeverity, CrossingDirection},
    };
    use std::{
        collections::{HashMap, VecDeque},
        sync::Arc,
    };

    // ── Zone event types ──────────────────────────────────────────

    /// Zone event types for trajectory-based zone alarms.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ZoneEvent {
        /// Track entered the zone (first frame inside).
        Entered,
        /// Track has been inside the zone longer than `dwell_timeout`.
        DwellTimeout,
        /// Track left the zone (first frame outside after being inside).
        Exited,
    }

    // ── TrackTrajectory ───────────────────────────────────────────

    /// Per-track trajectory state maintained across frames.
    ///
    /// The alarm engine uses this to evaluate trajectory-based rules
    /// (line crossing, zone dwell) that require temporal context beyond
    /// a single frame.
    #[derive(Debug)]
    pub struct TrackTrajectory {
        /// Track identifier.
        pub track_id: u64,
        /// Object class (e.g. "person", "vehicle").
        pub class: Arc<str>,
        /// Recent positions: `(timestamp_ms, center_x, center_y)`.
        /// Maintained as a bounded ring buffer of the last N frames.
        pub positions: VecDeque<(i64, f32, f32)>,
        /// Smoothed velocity vector (normalized pixels/second).
        pub velocity: (f32, f32),
        /// Direction in degrees `[0, 360)`, north=0, clockwise.
        pub direction_deg: f32,
        /// Timestamp when this track entered each zone (keyed by rule_id).
        pub zone_entry: HashMap<i32, i64>,
        /// Last trigger timestamp per rule_id (for cooldown enforcement).
        pub rule_cooldowns: HashMap<i32, i64>,
    }

    impl TrackTrajectory {
        /// Create a new trajectory for a track with its first observed position.
        fn new(track_id: u64, class: Arc<str>, ts: i64, cx: f32, cy: f32) -> Self {
            let mut positions = VecDeque::with_capacity(64);
            positions.push_back((ts, cx, cy));
            Self {
                track_id,
                class,
                positions,
                velocity: (0.0, 0.0),
                direction_deg: 0.0,
                zone_entry: HashMap::new(),
                rule_cooldowns: HashMap::new(),
            }
        }

        /// Append a new position and recompute velocity / direction.
        fn push_position(&mut self, ts: i64, cx: f32, cy: f32, max_history: usize) {
            if let Some(&(prev_ts, prev_cx, prev_cy)) = self.positions.back() {
                let dt = (ts - prev_ts).max(1) as f32 / 1000.0;
                let dx = cx - prev_cx;
                let dy = cy - prev_cy;
                let raw_vx = dx / dt;
                let raw_vy = dy / dt;

                // Exponential moving average for smoothing (alpha = 0.3).
                const ALPHA: f32 = 0.3;
                self.velocity.0 = ALPHA * raw_vx + (1.0 - ALPHA) * self.velocity.0;
                self.velocity.1 = ALPHA * raw_vy + (1.0 - ALPHA) * self.velocity.1;

                // Direction: atan2 converted to [0, 360) degrees.
                let angle_rad = self.velocity.1.atan2(self.velocity.0);
                let mut deg = angle_rad.to_degrees();
                if deg < 0.0 {
                    deg += 360.0;
                }
                self.direction_deg = deg;
            }
            self.positions.push_back((ts, cx, cy));
            while self.positions.len() > max_history {
                self.positions.pop_front();
            }
        }

        /// Build a trajectory context snapshot for alarm payloads.
        fn to_context(&self) -> TrajectoryContext {
            TrajectoryContext {
                track_id: self.track_id,
                polyline: self.positions.iter().copied().collect(),
                velocity_at_trigger: self.velocity,
                direction_at_trigger: self.direction_deg,
            }
        }

        /// Check if a rule is within its cooldown window.
        fn is_in_cooldown(&self, rule_id: i32, cooldown_ms: i64, current_ts: i64) -> bool {
            if cooldown_ms <= 0 {
                return false;
            }
            self.rule_cooldowns
                .get(&rule_id)
                .is_some_and(|&last| current_ts - last < cooldown_ms)
        }
    }

    // ── TrajectoryCache ───────────────────────────────────────────

    /// Trajectory cache for all active tracks within a channel.
    ///
    /// One instance per channel, owned by the engine's per-channel
    /// processing state. Thread-safe access is guaranteed by the
    /// channel's single-threaded processing loop.
    #[derive(Debug)]
    pub struct TrajectoryCache {
        /// Track ID → trajectory state.
        tracks: HashMap<u64, TrackTrajectory>,
        /// Maximum positions to retain per track.
        max_history: usize,
        /// Evict tracks not seen for this many milliseconds.
        eviction_timeout_ms: i64,
    }

    impl TrajectoryCache {
        /// Create a new trajectory cache.
        pub fn new(max_history: usize, eviction_timeout_ms: i64) -> Self {
            Self {
                tracks: HashMap::new(),
                max_history: max_history.max(2),
                eviction_timeout_ms,
            }
        }

        /// Update trajectories from the current frame's detections.
        ///
        /// Only detections with a `track_id` (assigned by the Tracker stage)
        /// are incorporated. Detections without tracking are ignored.
        pub fn update_from_detections(&mut self, detections: &[Detection], current_ts: i64) {
            for det in detections {
                let track_id = match det.track_id {
                    Some(id) => id,
                    None => continue,
                };
                let cx = (det.bbox.x_min + det.bbox.x_max) / 2.0;
                let cy = (det.bbox.y_min + det.bbox.y_max) / 2.0;

                match self.tracks.get_mut(&track_id) {
                    Some(traj) => {
                        traj.push_position(current_ts, cx, cy, self.max_history);
                        traj.class = det.class.clone();
                    }
                    None => {
                        let traj =
                            TrackTrajectory::new(track_id, det.class.clone(), current_ts, cx, cy);
                        self.tracks.insert(track_id, traj);
                    }
                }
            }
        }

        /// Remove tracks that haven't been observed for longer than
        /// `eviction_timeout_ms`.
        pub fn evict_stale(&mut self, current_ts: i64) {
            self.tracks.retain(|_, traj| {
                traj.positions
                    .back()
                    .is_some_and(|&(ts, _, _)| current_ts - ts < self.eviction_timeout_ms)
            });
        }

        /// Evaluate all trajectory-dependent alarm rules against the
        /// current trajectory state.
        ///
        /// Only `LineCrossing` and `ZoneDwell` conditions are handled here.
        /// Other conditions (ClassDetected, CountExceeds, etc.) should be
        /// evaluated separately by the standard alarm evaluator.
        pub fn evaluate_trajectory_rules(
            &mut self,
            rules: &[(i32, &str, AlarmSeverity, &AlarmCondition, i64)],
            current_ts: i64,
        ) -> Vec<AlarmEvent> {
            let mut alarms = Vec::new();

            for &(rule_id, rule_name, severity, condition, rule_cooldown_ms) in rules {
                match condition {
                    AlarmCondition::LineCrossing {
                        line,
                        class,
                        direction,
                    } => {
                        for traj in self.tracks.values_mut() {
                            if let Some(cls) = class {
                                if traj.class.as_ref() != cls.as_str() {
                                    continue;
                                }
                            }
                            if let Some(event) = evaluate_line_crossing(
                                traj,
                                line,
                                *direction,
                                rule_id,
                                rule_cooldown_ms,
                                current_ts,
                            ) {
                                alarms.push(event.into_alarm_event(rule_name, severity));
                            }
                        }
                    }

                    AlarmCondition::ZoneDwell {
                        zone,
                        class,
                        dwell_timeout_ms,
                        cooldown_ms,
                    } => {
                        if zone.len() < 3 {
                            continue;
                        }
                        let effective_cooldown = if *cooldown_ms > 0 {
                            *cooldown_ms
                        } else {
                            rule_cooldown_ms
                        };

                        for traj in self.tracks.values_mut() {
                            if let Some(cls) = class {
                                if traj.class.as_ref() != cls.as_str() {
                                    continue;
                                }
                            }
                            if let Some(zone_event) = evaluate_zone_trajectory(
                                traj,
                                zone,
                                rule_id,
                                *dwell_timeout_ms,
                                effective_cooldown,
                                current_ts,
                            ) {
                                let desc = match zone_event {
                                    ZoneEvent::Entered => format!(
                                        "Track {} entered zone (rule '{}')",
                                        traj.track_id, rule_name
                                    ),
                                    ZoneEvent::DwellTimeout => format!(
                                        "Track {} dwell timeout in zone (rule '{}')",
                                        traj.track_id, rule_name
                                    ),
                                    ZoneEvent::Exited => format!(
                                        "Track {} exited zone (rule '{}')",
                                        traj.track_id, rule_name
                                    ),
                                };
                                alarms.push(AlarmEvent {
                                    alarm_type: Arc::from(rule_name),
                                    description: Arc::from(desc),
                                    severity,
                                    related_detections: Vec::new(),
                                    snapshot: None,
                                    trajectory: Some(traj.to_context()),
                                });
                            }
                        }
                    }

                    _ => {} // Non-trajectory conditions handled elsewhere.
                }
            }

            alarms
        }

        /// Number of active tracks.
        pub fn track_count(&self) -> usize {
            self.tracks.len()
        }
    }

    impl Default for TrajectoryCache {
        fn default() -> Self {
            Self::new(64, 5000)
        }
    }

    // ── Line-crossing evaluation ──────────────────────────────────

    /// Result of a line-crossing detection.
    struct LineCrossingResult {
        track_id: u64,
        _class: Arc<str>,
        direction: CrossingDirection,
        _crossing_point: (f32, f32),
        context: TrajectoryContext,
    }

    impl LineCrossingResult {
        fn into_alarm_event(self, rule_name: &str, severity: AlarmSeverity) -> AlarmEvent {
            let desc = format!(
                "Track {} crossed line {:?} (rule '{}')",
                self.track_id, self.direction, rule_name,
            );
            AlarmEvent {
                alarm_type: Arc::from(rule_name),
                description: Arc::from(desc),
                severity,
                related_detections: Vec::new(),
                snapshot: None,
                trajectory: Some(self.context),
            }
        }
    }

    /// Evaluate whether a track has crossed a line segment.
    ///
    /// Uses the sign change of the cross product between consecutive
    /// positions to detect when a track's trajectory crosses the line.
    fn evaluate_line_crossing(
        track: &mut TrackTrajectory,
        line: &[(f32, f32); 2],
        direction_filter: Option<CrossingDirection>,
        rule_id: i32,
        cooldown_ms: i64,
        current_ts: i64,
    ) -> Option<LineCrossingResult> {
        if track.positions.len() < 2 {
            return None;
        }

        let len = track.positions.len();
        let (_, prev_x, prev_y) = track.positions[len - 2];
        let (_, curr_x, curr_y) = track.positions[len - 1];

        let (ax, ay) = line[0];
        let (bx, by) = line[1];
        let line_dx = bx - ax;
        let line_dy = by - ay;

        if line_dx.powi(2) + line_dy.powi(2) <= 0.0 {
            return None;
        }

        // Cross product sign: which side of the line the point is on.
        let cross_prev = line_dx * (prev_y - ay) - line_dy * (prev_x - ax);
        let cross_curr = line_dx * (curr_y - ay) - line_dy * (curr_x - ax);

        // Sign change means the track crossed the line.
        if cross_prev * cross_curr >= 0.0 {
            return None;
        }

        let crossing_direction = if cross_prev > 0.0 {
            CrossingDirection::LeftToRight
        } else {
            CrossingDirection::RightToLeft
        };

        // Direction filter.
        match direction_filter {
            Some(CrossingDirection::LeftToRight)
                if crossing_direction != CrossingDirection::LeftToRight =>
            {
                return None;
            }
            Some(CrossingDirection::RightToLeft)
                if crossing_direction != CrossingDirection::RightToLeft =>
            {
                return None;
            }
            _ => {}
        }

        // Cooldown check.
        if track.is_in_cooldown(rule_id, cooldown_ms, current_ts) {
            return None;
        }

        // Record trigger timestamp for cooldown.
        track.rule_cooldowns.insert(rule_id, current_ts);

        Some(LineCrossingResult {
            track_id: track.track_id,
            _class: track.class.clone(),
            direction: crossing_direction,
            _crossing_point: ((prev_x + curr_x) / 2.0, (prev_y + curr_y) / 2.0),
            context: track.to_context(),
        })
    }

    // ── Zone dwell evaluation ─────────────────────────────────────

    /// Evaluate zone-based trajectory events using the three-event model.
    ///
    /// For each tracked object, determines:
    /// 1. Has it just entered the zone? → `ZoneEvent::Entered`
    /// 2. Has it been inside for too long? → `ZoneEvent::DwellTimeout`
    /// 3. Has it just left the zone? → `ZoneEvent::Exited`
    fn evaluate_zone_trajectory(
        track: &mut TrackTrajectory,
        polygon: &[(f32, f32)],
        rule_id: i32,
        dwell_timeout_ms: i64,
        cooldown_ms: i64,
        current_ts: i64,
    ) -> Option<ZoneEvent> {
        let &(_, cx, cy) = track.positions.back()?;
        let inside = point_in_polygon(cx, cy, polygon);

        match (track.zone_entry.get(&rule_id), inside) {
            (None, true) => {
                // Just entered.
                track.zone_entry.insert(rule_id, current_ts);
                Some(ZoneEvent::Entered)
            }
            (Some(&entry_ts), true) => {
                // Still inside — check dwell timeout.
                let dwell_ms = current_ts - entry_ts;
                if dwell_ms >= dwell_timeout_ms
                    && !track.is_in_cooldown(rule_id, cooldown_ms, current_ts)
                {
                    track.rule_cooldowns.insert(rule_id, current_ts);
                    Some(ZoneEvent::DwellTimeout)
                } else {
                    None
                }
            }
            (Some(_), false) => {
                // Just exited.
                track.zone_entry.remove(&rule_id);
                Some(ZoneEvent::Exited)
            }
            (None, false) => None,
        }
    }

    // ── Geometry helpers ──────────────────────────────────────────

    /// Ray-casting point-in-polygon test (reusable across alarm modules).
    pub fn point_in_polygon(px: f32, py: f32, polygon: &[(f32, f32)]) -> bool {
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

        fn make_detection(
            class: &str,
            track_id: u64,
            x_min: f32,
            y_min: f32,
            x_max: f32,
            y_max: f32,
        ) -> Detection {
            use ng_gateway_models::domain::prelude::BoundingBox;
            Detection {
                class: Arc::from(class),
                class_id: 0,
                confidence: 0.9,
                bbox: BoundingBox {
                    x_min,
                    y_min,
                    x_max,
                    y_max,
                },
                track_id: Some(track_id),
            }
        }

        // ── TrajectoryCache basics ────────────────────────────────

        #[test]
        fn cache_updates_from_detections() {
            let mut cache = TrajectoryCache::new(64, 5000);
            let dets = vec![make_detection("person", 1, 0.1, 0.1, 0.3, 0.3)];
            cache.update_from_detections(&dets, 1000);
            assert_eq!(cache.track_count(), 1);
        }

        #[test]
        fn cache_evicts_stale_tracks() {
            let mut cache = TrajectoryCache::new(64, 1000);
            let dets = vec![make_detection("person", 1, 0.1, 0.1, 0.3, 0.3)];
            cache.update_from_detections(&dets, 1000);
            assert_eq!(cache.track_count(), 1);

            cache.evict_stale(3000);
            assert_eq!(cache.track_count(), 0, "track not seen for 2s > 1s timeout");
        }

        #[test]
        fn cache_retains_active_tracks() {
            let mut cache = TrajectoryCache::new(64, 5000);
            let dets = vec![make_detection("person", 1, 0.1, 0.1, 0.3, 0.3)];
            cache.update_from_detections(&dets, 1000);
            cache.evict_stale(3000);
            assert_eq!(cache.track_count(), 1, "track seen 2s ago < 5s timeout");
        }

        // ── Line crossing ─────────────────────────────────────────

        #[test]
        fn line_crossing_detects_sign_change() {
            let mut cache = TrajectoryCache::new(64, 5000);

            // Frame 1: track at (0.3, 0.5) — left of vertical line x=0.5.
            let dets1 = vec![make_detection("person", 1, 0.2, 0.4, 0.4, 0.6)];
            cache.update_from_detections(&dets1, 1000);

            // Frame 2: track at (0.7, 0.5) — right of vertical line x=0.5.
            let dets2 = vec![make_detection("person", 1, 0.6, 0.4, 0.8, 0.6)];
            cache.update_from_detections(&dets2, 2000);

            let line = [(0.5, 0.0), (0.5, 1.0)];
            let cond = AlarmCondition::LineCrossing {
                line,
                class: None,
                direction: Some(CrossingDirection::Any),
            };
            let rules = vec![(
                1_i32,
                "line_test",
                AlarmSeverity::Warning,
                &cond,
                60_000_i64,
            )];
            let alarms = cache.evaluate_trajectory_rules(&rules, 2000);
            assert_eq!(alarms.len(), 1, "crossing should be detected");
            assert!(
                alarms[0].trajectory.is_some(),
                "trajectory context should be present"
            );
        }

        #[test]
        fn line_crossing_respects_direction_filter() {
            let mut cache = TrajectoryCache::new(64, 5000);

            // Track moves left-to-right (0.3 → 0.7).
            let dets1 = vec![make_detection("person", 1, 0.2, 0.4, 0.4, 0.6)];
            cache.update_from_detections(&dets1, 1000);
            let dets2 = vec![make_detection("person", 1, 0.6, 0.4, 0.8, 0.6)];
            cache.update_from_detections(&dets2, 2000);

            let line = [(0.5, 0.0), (0.5, 1.0)];
            let cond = AlarmCondition::LineCrossing {
                line,
                class: None,
                direction: Some(CrossingDirection::RightToLeft),
            };
            let rules = vec![(1_i32, "line_rtl", AlarmSeverity::Warning, &cond, 60_000_i64)];
            let alarms = cache.evaluate_trajectory_rules(&rules, 2000);
            assert!(
                alarms.is_empty(),
                "RightToLeft filter should block LTR crossing"
            );
        }

        #[test]
        fn line_crossing_cooldown_prevents_retrigger() {
            let mut cache = TrajectoryCache::new(64, 5000);

            // First crossing.
            let dets1 = vec![make_detection("person", 1, 0.2, 0.4, 0.4, 0.6)];
            cache.update_from_detections(&dets1, 1000);
            let dets2 = vec![make_detection("person", 1, 0.6, 0.4, 0.8, 0.6)];
            cache.update_from_detections(&dets2, 2000);

            let line = [(0.5, 0.0), (0.5, 1.0)];
            let cond = AlarmCondition::LineCrossing {
                line,
                class: None,
                direction: Some(CrossingDirection::Any),
            };
            let rules = vec![(1_i32, "line_cd", AlarmSeverity::Warning, &cond, 60_000_i64)];
            let alarms = cache.evaluate_trajectory_rules(&rules, 2000);
            assert_eq!(alarms.len(), 1);

            // Second crossing within cooldown window — should not trigger.
            let dets3 = vec![make_detection("person", 1, 0.2, 0.4, 0.4, 0.6)];
            cache.update_from_detections(&dets3, 3000);
            let dets4 = vec![make_detection("person", 1, 0.6, 0.4, 0.8, 0.6)];
            cache.update_from_detections(&dets4, 4000);

            let alarms2 = cache.evaluate_trajectory_rules(&rules, 4000);
            assert!(alarms2.is_empty(), "cooldown should prevent re-trigger");
        }

        // ── Zone dwell ────────────────────────────────────────────

        #[test]
        fn zone_dwell_three_event_model() {
            let mut cache = TrajectoryCache::new(64, 5000);
            let zone = vec![(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)];
            let condition = AlarmCondition::ZoneDwell {
                zone: zone.clone(),
                class: None,
                dwell_timeout_ms: 3000,
                cooldown_ms: 60_000,
            };

            // Frame 1: track outside zone.
            let dets1 = vec![make_detection("person", 1, 0.0, 0.0, 0.1, 0.1)];
            cache.update_from_detections(&dets1, 1000);
            let rules = vec![(
                1_i32,
                "zone_test",
                AlarmSeverity::Warning,
                &condition,
                60_000_i64,
            )];
            let alarms = cache.evaluate_trajectory_rules(&rules, 1000);
            assert!(alarms.is_empty(), "outside zone → no event");

            // Frame 2: track enters zone.
            let dets2 = vec![make_detection("person", 1, 0.4, 0.4, 0.6, 0.6)];
            cache.update_from_detections(&dets2, 2000);
            let alarms = cache.evaluate_trajectory_rules(&rules, 2000);
            assert_eq!(alarms.len(), 1, "should fire Entered event");

            // Frame 3: still inside, before dwell timeout.
            let dets3 = vec![make_detection("person", 1, 0.4, 0.4, 0.6, 0.6)];
            cache.update_from_detections(&dets3, 3000);
            let alarms = cache.evaluate_trajectory_rules(&rules, 3000);
            assert!(alarms.is_empty(), "dwell not yet reached");

            // Frame 4: dwell timeout reached (entry at 2000, now 5500 → 3500ms > 3000ms).
            let dets4 = vec![make_detection("person", 1, 0.4, 0.4, 0.6, 0.6)];
            cache.update_from_detections(&dets4, 5500);
            let alarms = cache.evaluate_trajectory_rules(&rules, 5500);
            assert_eq!(alarms.len(), 1, "should fire DwellTimeout event");

            // Frame 5: track exits zone.
            let dets5 = vec![make_detection("person", 1, 0.0, 0.0, 0.1, 0.1)];
            cache.update_from_detections(&dets5, 6000);
            let alarms = cache.evaluate_trajectory_rules(&rules, 6000);
            assert_eq!(alarms.len(), 1, "should fire Exited event");
        }

        // ── Point in polygon ──────────────────────────────────────

        #[test]
        fn point_inside_square() {
            let polygon = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
            assert!(point_in_polygon(0.5, 0.5, &polygon));
        }

        #[test]
        fn point_outside_square() {
            let polygon = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
            assert!(!point_in_polygon(1.5, 0.5, &polygon));
        }

        #[test]
        fn velocity_and_direction_computed() {
            let mut cache = TrajectoryCache::new(64, 5000);

            let dets1 = vec![make_detection("person", 1, 0.0, 0.0, 0.1, 0.1)];
            cache.update_from_detections(&dets1, 0);

            let dets2 = vec![make_detection("person", 1, 0.5, 0.0, 0.6, 0.1)];
            cache.update_from_detections(&dets2, 1000);

            let traj = cache.tracks.get(&1).expect("track should exist");
            assert!(traj.velocity.0 > 0.0, "should have positive x velocity");
            assert_eq!(traj.positions.len(), 2);
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
