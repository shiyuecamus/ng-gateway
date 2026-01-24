use super::SouthwardTransportMeter;
use std::{
    io::{IoSlice, Result as IoResult},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// An `AsyncRead + AsyncWrite` wrapper that meters transport bytes.
///
/// # Semantics
/// - `bytes_out`: incremented by the number of bytes returned by `poll_write`/`poll_write_vectored`
/// - `bytes_in`: incremented by the number of bytes appended into `ReadBuf` by `poll_read`
///
/// # Performance
/// - No allocations on the hot path.
/// - Meter callbacks should be cheap (atomics).
#[derive(Debug)]
pub struct MeteredStream<T> {
    inner: T,
    meter: Arc<dyn SouthwardTransportMeter>,
    channel_id: i32,
    driver: Arc<str>,
    device_id: Option<i32>,
}

impl<T> MeteredStream<T> {
    #[inline]
    pub fn new(
        inner: T,
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
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for MeteredStream<T> {
    #[inline]
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<IoResult<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let after = buf.filled().len();
            let n = after.saturating_sub(before) as u64;
            if n > 0 {
                self.meter
                    .add_bytes_in(self.channel_id, self.driver.as_ref(), self.device_id, n);
            }
        }
        poll
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for MeteredStream<T> {
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<IoResult<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &poll {
            let n = *n as u64;
            if n > 0 {
                self.meter
                    .add_bytes_out(self.channel_id, self.driver.as_ref(), self.device_id, n);
            }
        }
        poll
    }

    #[inline]
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    #[inline]
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<IoResult<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    #[inline]
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<IoResult<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = &poll {
            let n = *n as u64;
            if n > 0 {
                self.meter
                    .add_bytes_out(self.channel_id, self.driver.as_ref(), self.device_id, n);
            }
        }
        poll
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug, Default)]
    struct TestMeter {
        bytes_in: AtomicU64,
        bytes_out: AtomicU64,
    }

    impl SouthwardTransportMeter for TestMeter {
        fn add_bytes_in(
            &self,
            _channel_id: i32,
            _driver: &str,
            _device_id: Option<i32>,
            bytes: u64,
        ) {
            self.bytes_in.fetch_add(bytes, Ordering::Relaxed);
        }

        fn add_bytes_out(
            &self,
            _channel_id: i32,
            _driver: &str,
            _device_id: Option<i32>,
            bytes: u64,
        ) {
            self.bytes_out.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn metered_stream_counts_write_and_read() {
        let meter_impl = Arc::new(TestMeter::default());
        let meter: Arc<dyn SouthwardTransportMeter> = meter_impl.clone();
        let (a, mut b) = tokio::io::duplex(64 * 1024);

        let mut metered = MeteredStream::new(a, Arc::clone(&meter), 1, Arc::from("test"), None);

        // write path (out)
        let payload = vec![0xABu8; 4096];
        metered.write_all(&payload).await.unwrap();
        metered.flush().await.unwrap();

        let mut sink = vec![0u8; payload.len()];
        b.read_exact(&mut sink).await.unwrap();
        assert_eq!(sink, payload);

        // read path (in)
        b.write_all(&payload).await.unwrap();
        b.flush().await.unwrap();

        let mut buf = vec![0u8; payload.len()];
        metered.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, payload);

        assert_eq!(
            meter_impl.bytes_out.load(Ordering::Relaxed),
            payload.len() as u64
        );
        assert_eq!(
            meter_impl.bytes_in.load(Ordering::Relaxed),
            payload.len() as u64
        );
    }
}
