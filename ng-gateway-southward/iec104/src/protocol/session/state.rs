use super::super::frame::{apci::SeqPending, asdu::Asdu};
use chrono::{DateTime, Utc};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU8, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLifecycleState {
    Idle,
    Connecting,
    Handshaking,
    Active,
    Inactive,
    Closing,
    Closed,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    LifecycleChanged(SessionLifecycleState),
    WindowFullDrop,
    LowPriorityMerged,
    MemoryBudgetDrop,
    FrameTooLargeDrop,
    T1Timeout,
}

/// SessionConfig holds all timing and capacity parameters for an IEC104 session.
///
/// - t0: Connection establishment timeout
/// - t1: Acknowledge timeout for I-frames
/// - t2: Acknowledge aggregation timeout for S-frames
/// - t3: Idle test frame interval
/// - k: Maximum number of unacknowledged I-frames (window size)
/// - w: Acknowledge aggregation threshold (number of I-frames)
/// - send_queue_capacity: Capacity of outbound request queue
/// - auto_reconnect: Whether to auto reconnect when connection lost
/// - reconnect_min_delay_ms: Minimum backoff delay in milliseconds
/// - reconnect_max_delay_ms: Maximum backoff delay in milliseconds
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// Connection establishment timeout (ms)
    pub connection_timeout_ms: u64,
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub t2_ms: u64,
    pub t3_ms: u64,
    pub k_window: u16,
    pub w_threshold: u16,
    pub send_queue_capacity: usize,
    // network tuning
    pub tcp_nodelay: bool,
    // max bytes allowed for unsent queues (high+low)
    pub max_pending_asdu_bytes: usize,
    // discard low-priority I-frames when window is full
    pub discard_low_priority_when_window_full: bool,
    // coalesce low-priority items by replacing last
    pub merge_low_priority: bool,
    // low-priority flush age threshold; if Some and exceeded, force send once when window allows
    pub low_prio_flush_max_age_ms: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            connection_timeout_ms: 10000,
            t0_ms: 10000,
            t1_ms: 15000,
            t2_ms: 10000,
            t3_ms: 20000,
            k_window: 12,
            w_threshold: 8,
            send_queue_capacity: 1024,
            tcp_nodelay: true,
            max_pending_asdu_bytes: 1024 * 1024,
            discard_low_priority_when_window_full: true,
            merge_low_priority: true,
            low_prio_flush_max_age_ms: 2000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationState {
    Inactive = 0,
    Active = 1,
}

#[derive(Debug)]
pub struct ApplicationStateCell(AtomicU8);

impl ApplicationStateCell {
    pub fn new(initial: ApplicationState) -> Self {
        Self(AtomicU8::new(initial as u8))
    }

    pub fn load(&self) -> ApplicationState {
        match self.0.load(Ordering::Relaxed) {
            1 => ApplicationState::Active,
            _ => ApplicationState::Inactive,
        }
    }

    pub fn store(&self, state: ApplicationState) {
        self.0.store(state as u8, Ordering::Relaxed);
    }

    pub fn is_active(&self) -> bool {
        matches!(self.load(), ApplicationState::Active)
    }
}

/// SessionState encapsulates counters and timers for an active link-layer session.
#[derive(Debug)]
pub struct SessionState {
    pub send_sn: u16,
    pub ack_sendsn: u16,
    pub rcv_sn: u16,
    pub ack_rcvsn: u16,

    pub idle_since: DateTime<Utc>,
    pub test_sent_since: DateTime<Utc>,
    pub start_sent_since: DateTime<Utc>,
    pub stop_sent_since: DateTime<Utc>,
    pub unacked_rcv_since: DateTime<Utc>,
    pub consecutive_t1_timeouts: u32,

    pub pending: VecDeque<SeqPending>,
    pub pending_bytes: usize,
    pub last_low_prio: Option<Asdu>,
    pub last_low_prio_since: Option<DateTime<Utc>>,
}

impl SessionState {
    /// Construct initial session state
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            send_sn: 0,
            ack_sendsn: 0,
            rcv_sn: 0,
            ack_rcvsn: 0,
            idle_since: now,
            test_sent_since: DateTime::<Utc>::MAX_UTC,
            start_sent_since: DateTime::<Utc>::MAX_UTC,
            stop_sent_since: DateTime::<Utc>::MAX_UTC,
            unacked_rcv_since: DateTime::<Utc>::MAX_UTC,
            consecutive_t1_timeouts: 0,
            pending: VecDeque::new(),
            pending_bytes: 0,
            last_low_prio: None,
            last_low_prio_since: None,
        }
    }

    /// Return number of outstanding I-frames in-flight (unacknowledged)
    pub fn inflight_count(&self) -> u16 {
        Self::seq_distance(self.ack_sendsn, self.send_sn)
    }

    /// Whether window has available slots to send new I-frames
    pub fn window_has_capacity(&self, k_window: u16) -> bool {
        self.inflight_count() < k_window
    }

    /// Number of available slots before reaching window limit
    pub fn window_available_slots(&self, k_window: u16) -> u16 {
        k_window.saturating_sub(self.inflight_count())
    }

    /// Push a new pending sequence with timestamp
    pub fn push_pending(&mut self, seq: u16, now: DateTime<Utc>, size_bytes: usize) {
        self.pending.push_back(SeqPending {
            seq,
            send_time: now,
            size_bytes,
        });
        self.pending_bytes = self.pending_bytes.saturating_add(size_bytes);
    }

    /// Pop pending until and including `up_to_seq` (exclusive semantics depend on comparison)
    /// Returns number of removed items
    pub fn pop_pending_until(&mut self, up_to_seq: u16) -> usize {
        let mut removed = 0usize;
        while let Some(front) = self.pending.front() {
            if Self::seq_lte(front.seq, up_to_seq) {
                if let Some(item) = self.pending.pop_front() {
                    self.pending_bytes = self.pending_bytes.saturating_sub(item.size_bytes);
                }
                removed += 1;
            } else {
                break;
            }
        }
        removed
    }

    /// Clear all pending; returns removed count
    pub fn clear_pending(&mut self) -> usize {
        let n = self.pending.len();
        self.pending.clear();
        self.pending_bytes = 0;
        n
    }

    /// Update send acknowledge numbers given remote ack `rcv_sn` in incoming I/S frames.
    /// Returns true if the ack is within valid window and state advanced, false if invalid.
    pub fn update_send_ack(&mut self, ack_rcv_no: u16) -> bool {
        // Accept only acks within [ack_sendsn, send_sn]
        if !Self::seq_in_range_inclusive(self.ack_sendsn, self.send_sn, ack_rcv_no) {
            return false;
        }
        // remove pending up to ack_rcv_no - 1
        let removed = self.pop_pending_until(Self::seq_add(ack_rcv_no, 32767));
        let _ = removed; // keep for clarity; could export metrics
        self.ack_sendsn = ack_rcv_no;
        true
    }

    /// Advance local receive sequence on incoming I-frame with `send_sn`.
    /// Returns true if sequence is expected, false otherwise.
    pub fn advance_receive_seq(&mut self, incoming_send_sn: u16) -> bool {
        if incoming_send_sn != self.rcv_sn {
            return false;
        }
        self.rcv_sn = Self::seq_add(self.rcv_sn, 1);
        true
    }

    /// Whether we should send aggregated S-ACK now, based on t2 timer or w threshold.
    pub fn should_send_s_ack(&self, now: DateTime<Utc>, t2_ms: u64, w_threshold: u16) -> bool {
        if self.ack_rcvsn == self.rcv_sn {
            return false;
        }
        let unacked = Self::seq_distance(self.ack_rcvsn, self.rcv_sn);
        if unacked >= w_threshold {
            return true;
        }
        // t2 check
        now.signed_duration_since(self.unacked_rcv_since)
            .num_milliseconds() as u64
            >= t2_ms
    }

    /// Check front pending for t1 timeout and handle it by dropping one and advancing ack_sendsn.
    /// Returns Some(dropped_seq) when timeout occurred and state mutated, None otherwise.
    pub fn check_and_handle_t1_timeout(&mut self, now: DateTime<Utc>, t1_ms: u64) -> Option<u16> {
        if self.ack_sendsn == self.send_sn {
            self.consecutive_t1_timeouts = 0;
            return None;
        }
        if let Some(front) = self.pending.front() {
            if now
                .signed_duration_since(front.send_time)
                .num_milliseconds() as u64
                >= t1_ms
            {
                let dropped = front.seq;
                self.pending.pop_front();
                self.ack_sendsn = Self::seq_add(self.ack_sendsn, 1);
                self.consecutive_t1_timeouts = self.consecutive_t1_timeouts.saturating_add(1);
                return Some(dropped);
            }
        }
        // no timeout this tick; decay counter
        if self.consecutive_t1_timeouts > 0 {
            self.consecutive_t1_timeouts -= 1;
        }
        None
    }

    /// Mark that we received data and have unacked rx
    pub fn mark_unacked_receive(&mut self, now: DateTime<Utc>) {
        if self.ack_rcvsn == self.rcv_sn {
            self.unacked_rcv_since = now;
        }
    }

    /// Advance ack_rcvsn to rcv_sn when S-ACK sent
    pub fn mark_s_ack_sent(&mut self) {
        self.ack_rcvsn = self.rcv_sn;
    }

    /// Add with sequence wrap modulo 32768
    pub fn seq_add(seq: u16, delta: u16) -> u16 {
        ((seq as u32 + delta as u32) % 32768) as u16
    }

    /// Distance from `from` to `to` in modulo space [0, 32767]
    pub fn seq_distance(from: u16, to: u16) -> u16 {
        ((to as i32 - from as i32 + 32768) % 32768) as u16
    }

    /// Is x within [start, end] in modulo space inclusive on end
    pub fn seq_in_range_inclusive(start: u16, end: u16, x: u16) -> bool {
        Self::seq_distance(start, x) <= Self::seq_distance(start, end)
    }

    /// Less or equal in modulo order relative to 0-based
    pub fn seq_lte(a: u16, b: u16) -> bool {
        Self::seq_distance(0, a) <= Self::seq_distance(0, b)
    }

    /// Get total bytes pending in-flight I-frames
    pub fn total_pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Replace last low-priority ASDU to be sent when window allows
    pub fn set_last_low_prio(&mut self, asdu: Asdu, now: DateTime<Utc>) {
        self.last_low_prio = Some(asdu);
        self.last_low_prio_since = Some(now);
    }

    /// Take and clear the stored low-priority ASDU
    pub fn take_last_low_prio(&mut self) -> Option<Asdu> {
        self.last_low_prio_since = None;
        self.last_low_prio.take()
    }

    /// Whether low-priority buffered ASDU should be flushed due to age
    pub fn should_flush_low_prio(&self, now: DateTime<Utc>, max_age_ms: u64) -> bool {
        match (self.last_low_prio.as_ref(), self.last_low_prio_since) {
            (Some(_), Some(since)) => {
                (now.signed_duration_since(since).num_milliseconds() as u64) >= max_age_ms
            }
            _ => false,
        }
    }
}
