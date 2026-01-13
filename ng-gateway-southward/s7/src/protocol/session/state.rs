use super::super::{error::Error, frame::CpuType};
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

/// Public lifecycle state exposed to API consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifecycleState {
    /// Initial idle state before any connection attempt
    Idle,
    /// Transport connecting
    Connecting,
    /// Protocol handshaking (TPKT/COTP/S7)
    Handshaking,
    /// Fully active/established
    Active,
    /// Graceful closing in progress
    Closing,
    /// Fully closed/disconnected
    Closed,
    /// Failed/backoff state after error
    Failed,
}

/// Public session events for observability.
#[derive(Debug, Clone, Copy)]
pub enum SessionEvent {
    /// Lifecycle changed notification
    LifecycleChanged(SessionLifecycleState),
    /// Transport level error occurred (connect/reset/IO)
    TransportError,
    /// Fragment reassembly dropped due to parse error or invalid segment
    FragmentReassemblyDrop,
}

/// Session state transition event
#[derive(Debug)]
pub enum StateEvent {
    /// Start connection process
    Connect,
    /// TCP connection established
    TcpConnected,
    /// Handshake completed successfully
    HandshakeComplete,
    /// Handshake failed
    HandshakeFailed(Error),
    /// Connection lost or error occurred
    ConnectionLost(Error),
    /// Start graceful shutdown
    Shutdown,
    /// Force immediate shutdown
    ForceShutdown,
    /// Retry after backoff period
    Retry,
    /// Reset to disconnected state
    Reset,
}

/// Session configuration for S7 connection handshake and runtime behavior.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Remote PLC address (host:port)
    pub socket_addr: SocketAddr,
    /// CPU type
    pub cpu: CpuType,
    /// Explicit TSAP override values. If provided, override `tsap`.
    pub tsap_src: u16,
    pub tsap_dst: u16,
    /// Preferred S7 PDU size (bytes); if None, accept peer
    pub preferred_pdu_size: Option<u16>,
    /// Preferred AmQ Caller & Callee; if None, accept peer
    pub preferred_amq_caller: Option<u16>,
    pub preferred_amq_callee: Option<u16>,
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
            socket_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 102)),
            cpu: CpuType::S71500,
            tsap_src: 0x0100,
            tsap_dst: 0x0100,
            preferred_pdu_size: Some(960),
            preferred_amq_caller: Some(8),
            preferred_amq_callee: Some(80),
            connect_timeout: Duration::from_millis(10_000),
            read_timeout: Duration::from_millis(5_000),
            write_timeout: Duration::from_millis(5_000),
            send_queue_capacity: 256,
            max_concurrent_requests: 32,
            tcp_nodelay: true,
        }
    }
}
