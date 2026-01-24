use super::SouthwardTransportMeter;
use std::{io::Result as IoResult, net::SocketAddr, sync::Arc};
use tokio::net::UdpSocket;

/// A thin wrapper around `tokio::net::UdpSocket` that meters `send_to`/`recv_from` bytes.
///
/// # Notes
/// - UDP is message-oriented; bytes are counted by the number of bytes reported by the socket APIs.
/// - This wrapper does not attempt to interpret protocol payload boundaries.
#[derive(Debug)]
pub struct MeteredUdpSocket {
    inner: tokio::net::UdpSocket,
    meter: Arc<dyn SouthwardTransportMeter>,
    channel_id: i32,
    driver: Arc<str>,
    device_id: Option<i32>,
}

impl MeteredUdpSocket {
    #[inline]
    pub fn new(
        inner: tokio::net::UdpSocket,
        meter: Arc<dyn SouthwardTransportMeter>,
        channel_id: i32,
        driver: Arc<str>,
        device_id: Option<i32>,
    ) -> Self {
        Self {
            inner,
            meter,
            channel_id,
            driver,
            device_id,
        }
    }

    #[inline]
    pub fn into_inner(self) -> UdpSocket {
        self.inner
    }

    #[inline]
    pub async fn send_to(&self, buf: &[u8], target: SocketAddr) -> IoResult<usize> {
        let n = self.inner.send_to(buf, target).await?;
        if n > 0 {
            self.meter.add_bytes_out(
                self.channel_id,
                self.driver.as_ref(),
                self.device_id,
                n as u64,
            );
        }
        Ok(n)
    }

    #[inline]
    pub async fn recv_from(&self, buf: &mut [u8]) -> IoResult<(usize, SocketAddr)> {
        let (n, addr) = self.inner.recv_from(buf).await?;
        if n > 0 {
            self.meter.add_bytes_in(
                self.channel_id,
                self.driver.as_ref(),
                self.device_id,
                n as u64,
            );
        }
        Ok((n, addr))
    }
}
