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

mod metered_stream;
mod metered_udp;
mod noop;

pub use metered_stream::MeteredStream;
pub use metered_udp::MeteredUdpSocket;
pub use noop::NoopSouthwardTransportMeter;

use std::{
    fmt::Debug, future::Future, io::Result as IoResult, net::SocketAddr, pin::Pin, sync::Arc,
};
use tokio::net::TcpStream;

/// Host-injected: aggregates measured transport bytes into an authoritative hub.
///
/// # Contract
/// - Implementations must be **fast** and must not allocate on hot paths.
/// - Label cardinality must remain bounded (device_id is NOT a Prometheus label).
pub trait SouthwardTransportMeter: Send + Sync + Debug {
    fn add_bytes_in(&self, channel_id: i32, driver: &str, device_id: Option<i32>, bytes: u64);
    fn add_bytes_out(&self, channel_id: i32, driver: &str, device_id: Option<i32>, bytes: u64);
}

/// Unified instrumentation-aware transport factory.
///
/// # Notes
/// This is primarily used at connection creation sites to ensure **all** transports
/// are wrapped consistently. It is intentionally async to support DNS and OS I/O.
pub trait InstrumentedTransportFactory: Send + Sync + Debug {
    fn connect_tcp(
        &self,
        channel_id: i32,
        driver: Arc<str>,
        device_id: Option<i32>,
        addr: SocketAddr,
        meter: Arc<dyn SouthwardTransportMeter>,
    ) -> Pin<Box<dyn Future<Output = IoResult<MeteredStream<TcpStream>>> + Send>>;
}

/// Default transport factory implementation for gateway-hosted drivers.
///
/// This implementation creates a `tokio::net::TcpStream` and wraps it into `MeteredStream`
/// with the provided meter and identity.
#[derive(Debug, Default)]
pub struct NGTransportFactory;

impl InstrumentedTransportFactory for NGTransportFactory {
    #[inline]
    fn connect_tcp(
        &self,
        channel_id: i32,
        driver: Arc<str>,
        device_id: Option<i32>,
        addr: SocketAddr,
        meter: Arc<dyn SouthwardTransportMeter>,
    ) -> Pin<Box<dyn Future<Output = IoResult<MeteredStream<TcpStream>>> + Send>> {
        Box::pin(async move {
            let stream = TcpStream::connect(addr).await?;
            Ok(MeteredStream::new(
                stream, meter, channel_id, driver, device_id,
            ))
        })
    }
}

/// Backward-compatible alias.
pub type NGInstrumentedTransportFactory = NGTransportFactory;
