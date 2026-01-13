use super::super::types::{McFrameVariant, McSeries};
use std::{net::SocketAddr, time::Duration};

/// MC session lifecycle state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifecycleState {
    /// Initial idle state before any connection attempt
    Idle,
    /// Transport connecting
    Connecting,
    /// Fully active/established
    Active,
    /// Graceful closing in progress
    Closing,
    /// Fully closed/disconnected
    Closed,
    /// Failed/backoff state after error
    Failed,
}

/// Public session events for observability
#[derive(Debug, Clone, Copy)]
pub enum SessionEvent {
    /// Lifecycle changed notification
    LifecycleChanged(SessionLifecycleState),
    /// Transport level error occurred (connect/reset/IO)
    TransportError,
}

/// MC Session configuration
///
/// Simplified from S7, no TSAP/PDU negotiation, only keeping MC-specific parameters.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Remote PLC address (host:port)
    pub socket_addr: SocketAddr,
    /// PLC series (A/QnA/Q/L/IQ-R) used to derive frame behaviour and subcommands.
    pub series: McSeries,
    /// Frame variant (1E/3E/4E + Binary/ASCII)
    pub frame_variant: McFrameVariant,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Read timeout
    pub read_timeout: Duration,
    /// Write timeout
    pub write_timeout: Duration,
    /// Outbound queue capacity
    pub send_queue_capacity: usize,
    /// Maximum concurrent requests (back pressure control)
    pub max_concurrent_requests: usize,
    /// TCP_NODELAY option. Defaults to true for low-latency small PDUs
    pub tcp_nodelay: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            socket_addr: "127.0.0.1:5007".parse().unwrap(),
            series: McSeries::QnA,
            frame_variant: McFrameVariant::Frame3EBinary,
            connect_timeout: Duration::from_millis(10_000),
            read_timeout: Duration::from_millis(5_000),
            write_timeout: Duration::from_millis(5_000),
            send_queue_capacity: 256,
            max_concurrent_requests: 1,
            tcp_nodelay: true,
        }
    }
}
