use backoff::{backoff::Backoff, ExponentialBackoff};
use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Unified retry policy configuration with exponential backoff and max attempts.
///
/// This struct combines backoff parameters with attempt limits to provide
/// a comprehensive retry strategy for both drivers and northward plugins.
///
/// # Design Philosophy
/// - Unified: Single config type for all retry scenarios (drivers, northward, etc.)
/// - Flexible: Supports both time-based (max_elapsed_time) and count-based (max_attempts) limits
/// - Safe defaults: Reasonable values that work for most use cases
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    /// Maximum number of retry attempts.
    ///
    /// Semantics (MUST be consistent across southward/northward):
    /// - `Some(0)`: no retries (fail immediately after the first failure)
    /// - `Some(n)`: retry at most `n` times
    /// - `None`: unlimited retries (use with caution)
    #[serde(default = "RetryPolicy::default_max_attempts")]
    #[serde(alias = "max_attempts")]
    pub max_attempts: Option<u32>,

    /// Initial retry interval in milliseconds
    #[serde(default = "RetryPolicy::default_initial_interval_ms")]
    #[serde(alias = "initial_interval_ms")]
    pub initial_interval_ms: u64,

    /// Maximum retry interval cap in milliseconds
    #[serde(default = "RetryPolicy::default_max_interval_ms")]
    #[serde(alias = "max_interval_ms")]
    pub max_interval_ms: u64,

    /// Randomization factor in range [0.0, 1.0]. Example: 0.2 means ±20% jitter
    #[serde(default = "RetryPolicy::default_randomization_factor")]
    #[serde(alias = "randomization_factor")]
    pub randomization_factor: f64,

    /// Multiplicative factor for each retry step. Typically 2.0 for exponential backoff
    #[serde(default = "RetryPolicy::default_multiplier")]
    pub multiplier: f64,

    /// Optional maximum total elapsed time in milliseconds (None = no time limit)
    ///
    /// Note: If both max_attempts and max_elapsed_time are set, whichever is reached first stops retries
    #[serde(default = "RetryPolicy::default_max_elapsed_time_ms")]
    #[serde(alias = "max_elapsed_time_ms")]
    pub max_elapsed_time_ms: Option<u64>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: Self::default_max_attempts(),
            initial_interval_ms: Self::default_initial_interval_ms(),
            max_interval_ms: Self::default_max_interval_ms(),
            randomization_factor: Self::default_randomization_factor(),
            multiplier: Self::default_multiplier(),
            max_elapsed_time_ms: Self::default_max_elapsed_time_ms(),
        }
    }
}

impl RetryPolicy {
    fn default_max_attempts() -> Option<u32> {
        None // Default to unlimited retries (explicit budgets are opt-in)
    }

    fn default_initial_interval_ms() -> u64 {
        1_000 // 1 second
    }

    fn default_max_interval_ms() -> u64 {
        30_000 // 30 seconds
    }

    fn default_randomization_factor() -> f64 {
        0.2 // ±20% jitter
    }

    fn default_multiplier() -> f64 {
        2.0 // Exponential backoff
    }

    fn default_max_elapsed_time_ms() -> Option<u64> {
        None // No time limit by default
    }

    /// Create a retry policy with no retries (fail immediately)
    pub fn no_retry() -> Self {
        Self {
            max_attempts: Some(0),
            ..Default::default()
        }
    }

    /// Create a retry policy with unlimited attempts (use with caution!)
    pub fn unlimited() -> Self {
        Self {
            max_attempts: None,
            max_elapsed_time_ms: None,
            ..Default::default()
        }
    }

    /// Create a retry policy with specific max attempts
    pub fn with_max_attempts(max_attempts: u32) -> Self {
        Self {
            max_attempts: Some(max_attempts),
            ..Default::default()
        }
    }
}

impl sea_orm::IntoActiveValue<RetryPolicy> for RetryPolicy {
    fn into_active_value(self) -> sea_orm::ActiveValue<RetryPolicy> {
        sea_orm::ActiveValue::Set(self)
    }
}

/// Build an ExponentialBackoff from RetryPolicy.
///
/// Safety & performance notes:
/// - This is a one-time builder per retry loop, avoiding allocations on hot path
/// - `max_elapsed_time` controls time-based retry limit
/// - Caller must separately check `max_attempts` if needed
pub fn build_exponential_backoff(policy: &RetryPolicy) -> ExponentialBackoff {
    ExponentialBackoff {
        initial_interval: Duration::from_millis(policy.initial_interval_ms.max(1)),
        max_interval: Duration::from_millis(policy.max_interval_ms.max(policy.initial_interval_ms)),
        randomization_factor: policy.randomization_factor.clamp(0.0, 1.0),
        multiplier: policy.multiplier.max(1.0),
        max_elapsed_time: policy.max_elapsed_time_ms.map(Duration::from_millis),
        ..ExponentialBackoff::default()
    }
}

/// A unified retry budget controller that enforces BOTH:
/// - `max_attempts` (count-based budget)
/// - `max_elapsed_time` (time-based budget via backoff's `next_backoff == None`)
///
/// This avoids subtle off-by-one mismatches across drivers.
#[derive(Debug, Clone)]
pub struct RetryController {
    backoff: ExponentialBackoff,
    max_attempts: Option<u32>,
    retries_used: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    RetryAfter(Duration),
    Exhausted,
}

impl RetryController {
    #[inline]
    pub fn new(policy: &RetryPolicy) -> Self {
        Self {
            backoff: build_exponential_backoff(policy),
            max_attempts: policy.max_attempts,
            retries_used: 0,
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.backoff.reset();
        self.retries_used = 0;
    }

    /// Call this once per failure to decide whether to retry.
    ///
    /// - If `max_attempts = Some(0)`, this returns `Exhausted` immediately.
    /// - Otherwise, if the time budget is exhausted (`next_backoff == None`), returns `Exhausted`.
    #[inline]
    pub fn on_failure(&mut self) -> RetryDecision {
        if let Some(max) = self.max_attempts {
            if self.retries_used >= max {
                return RetryDecision::Exhausted;
            }
        }
        match self.backoff.next_backoff() {
            Some(dur) => {
                self.retries_used = self.retries_used.saturating_add(1);
                RetryDecision::RetryAfter(dur)
            }
            None => RetryDecision::Exhausted,
        }
    }

    #[inline]
    pub fn retries_used(&self) -> u32 {
        self.retries_used
    }
}
