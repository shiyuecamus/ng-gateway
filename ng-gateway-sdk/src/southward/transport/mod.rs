//! Southward transport instrumentation.
//!
//! This module provides **measured** transport byte accounting for southward drivers.
//! The byte semantics are strictly defined as:
//! - bytes_out: number of bytes successfully written to the underlying I/O (user-space write buffer size)
//! - bytes_in:  number of bytes successfully read from the underlying I/O (user-space read buffer size)
//!
//! The intent is to keep the hot path allocation-free and low overhead:
//! - instrumentation is done at the I/O boundary (AsyncRead/AsyncWrite wrappers)
//! - metering callbacks are expected to be cheap (atomics / lock-free ring buffer)

mod noop;
mod stream;
mod udp;

pub use noop::NoopSouthwardTransportMeter;
use std::{
    fmt::Debug,
    io::{Error, ErrorKind, Result as IoResult},
    net::SocketAddr,
    sync::Arc,
};
pub use stream::MeteredStream;
use tokio::{
    net::{TcpStream, UdpSocket},
    time::{timeout, Duration as TokioDuration},
};
use tokio_serial::{DataBits, Parity, SerialPortBuilderExt, SerialStream, StopBits};
pub use udp::MeteredUdpSocket;

/// Host-injected: aggregates measured transport bytes into an authoritative hub.
///
/// # Contract
/// - Implementations must be **fast** and must not allocate on hot paths.
/// - Label cardinality must remain bounded (do NOT add per-device labels).
pub trait SouthwardTransportMeter: Send + Sync + Debug {
    /// Add measured inbound bytes (field -> gateway).
    ///
    /// # Contract
    /// - Must be fast and allocation-free.
    /// - Called on I/O hot paths.
    fn add_bytes_in(&self, bytes: u64);

    /// Add measured outbound bytes (gateway -> field).
    ///
    /// # Contract
    /// - Must be fast and allocation-free.
    /// - Called on I/O hot paths.
    fn add_bytes_out(&self, bytes: u64);
}

/// Connect a TCP stream and wrap it with metering.
///
/// # Best practice (dynamic drivers)
/// When running southward drivers as `cdylib` plugins, **do not** call a host-provided async
/// transport factory from inside the plugin runtime. That would create a Future implemented by
/// the host binary and poll it on the plugin Tokio runtime, which can panic with
/// \"there is no reactor running\" due to Tokio runtime context isolation across dylibs.
///
/// Prefer calling this helper directly inside the plugin so the returned Future is implemented
/// by the plugin-linked `tokio` and is polled by the plugin's own Tokio runtime.
#[inline]
#[allow(unused)]
pub async fn connect_tcp_metered(
    addr: SocketAddr,
    meter: Arc<dyn SouthwardTransportMeter>,
) -> IoResult<MeteredStream<TcpStream>> {
    let stream = TcpStream::connect(addr).await?;
    Ok(MeteredStream::new(stream, meter))
}

/// Same as [`connect_tcp_metered`] but enforces a connect timeout.
#[inline]
#[allow(unused)]
pub async fn connect_tcp_metered_with_timeout(
    addr: SocketAddr,
    meter: Arc<dyn SouthwardTransportMeter>,
    connect_timeout_ms: u64,
) -> IoResult<MeteredStream<TcpStream>> {
    let d = TokioDuration::from_millis(connect_timeout_ms.max(1));
    match timeout(d, connect_tcp_metered(addr, meter)).await {
        Ok(r) => r,
        Err(_elapsed) => Err(Error::new(ErrorKind::TimedOut, "tcp connect timeout")),
    }
}

/// Bind a UDP socket and wrap it with metering.
///
/// This should be called from inside the plugin runtime, for the same reason as TCP:
/// keep the Future implementation and the Tokio runtime in the same dylib \"world\".
#[inline]
#[allow(unused)]
pub async fn bind_udp_metered(
    bind_addr: SocketAddr,
    meter: Arc<dyn SouthwardTransportMeter>,
) -> IoResult<MeteredUdpSocket> {
    let sock = UdpSocket::bind(bind_addr).await?;
    Ok(MeteredUdpSocket::new(sock, meter))
}

/// Bind a UDP socket with timeout and wrap it with metering.
#[inline]
#[allow(unused)]
pub async fn bind_udp_metered_with_timeout(
    bind_addr: SocketAddr,
    meter: Arc<dyn SouthwardTransportMeter>,
    bind_timeout_ms: u64,
) -> IoResult<MeteredUdpSocket> {
    let d = TokioDuration::from_millis(bind_timeout_ms.max(1));
    match timeout(d, bind_udp_metered(bind_addr, meter)).await {
        Ok(r) => r,
        Err(_elapsed) => Err(Error::new(ErrorKind::TimedOut, "udp bind timeout")),
    }
}

/// Serial open configuration for `connect_serial_metered`.
///
/// This intentionally uses `tokio_serial` types so drivers can pass through their
/// already-resolved serial parameters without re-mapping enums in the SDK.
#[derive(Clone, Debug)]
pub struct SerialConnectConfig {
    pub port: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub stop_bits: StopBits,
    pub parity: Parity,
}

/// Open a serial port and wrap it with metering.
///
/// Notes:
/// - `open_native_async()` is a synchronous OS call and cannot be meaningfully timed out.
/// - This helper keeps serial metering setup consistent across drivers.
#[inline]
#[allow(unused)]
pub fn connect_serial_metered(
    cfg: SerialConnectConfig,
    meter: Arc<dyn SouthwardTransportMeter>,
) -> IoResult<MeteredStream<SerialStream>> {
    let stream = tokio_serial::new(cfg.port, cfg.baud_rate)
        .data_bits(cfg.data_bits)
        .stop_bits(cfg.stop_bits)
        .parity(cfg.parity)
        .open_native_async()?;
    Ok(MeteredStream::new(stream, meter))
}
