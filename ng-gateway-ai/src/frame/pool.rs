//! Pre-allocated CPU buffer pool for fallback preprocessing paths.
//!
//! On the CPU fallback path (generic x86, DMA-buf → mmap → resize → normalize),
//! each frame requires a temporary RGB buffer for resize output. Without pooling,
//! this causes per-frame `Vec<u8>` allocations of 6+ MB (1080p RGB24), putting
//! pressure on the system allocator and fragmenting the heap.
//!
//! [`CpuBufferPool`] maintains a fixed set of pre-allocated buffers in a
//! lock-free `ArrayQueue`. Buffers are checked out, used, and returned
//! automatically via [`PooledBuffer`]'s `Drop` implementation.
//!
//! # Sizing
//!
//! The pool is sized for the largest expected frame. For 4K RGB24 (3840×2160×3)
//! this is ~25 MB per buffer. With a typical pool depth of 4-8, total overhead
//! is 100-200 MB — acceptable given the memory savings from avoiding per-frame
//! allocations.

use crossbeam_queue::ArrayQueue;
use std::sync::Arc;

/// Pre-allocated buffer pool for CPU preprocessing.
///
/// Avoids per-frame `Vec<u8>` allocation on the hot path. Each buffer is
/// sized for the largest expected frame. Buffers are checked out, used,
/// and returned — no allocator pressure.
///
/// # Thread Safety
///
/// `ArrayQueue` is lock-free and safe for concurrent producers and consumers.
/// The pool itself is `Send + Sync` and can be shared across async tasks.
pub struct CpuBufferPool {
    pool: Arc<ArrayQueue<Vec<u8>>>,
    buffer_capacity: usize,
}

impl CpuBufferPool {
    /// Create a new buffer pool.
    ///
    /// - `pool_size`: number of pre-allocated buffers (recommend 4-8).
    /// - `buffer_capacity`: byte capacity of each buffer (e.g. `3840 * 2160 * 3`
    ///   for 4K RGB24). Buffers are `Vec::with_capacity` — no physical memory
    ///   is committed until written.
    pub fn new(pool_size: usize, buffer_capacity: usize) -> Self {
        let pool = ArrayQueue::new(pool_size.max(1));
        for _ in 0..pool_size {
            let buf = Vec::with_capacity(buffer_capacity);
            let _ = pool.push(buf);
        }
        Self {
            pool: Arc::new(pool),
            buffer_capacity,
        }
    }

    /// Create a pool sized for a given resolution and pixel format.
    ///
    /// Convenience constructor that computes `buffer_capacity` from dimensions
    /// and bytes-per-pixel.
    pub fn for_resolution(pool_size: usize, max_width: u32, max_height: u32, bpp: usize) -> Self {
        let capacity = max_width as usize * max_height as usize * bpp;
        Self::new(pool_size, capacity)
    }

    /// Check out a buffer from the pool.
    ///
    /// If the pool is empty, a fresh buffer is allocated (graceful degradation
    /// rather than blocking). The returned [`PooledBuffer`] automatically
    /// returns the buffer to the pool when dropped.
    pub fn checkout(&self) -> PooledBuffer {
        let buf = self.pool.pop().unwrap_or_else(|| {
            tracing::trace!(
                "CpuBufferPool exhausted, allocating temporary buffer ({} bytes)",
                self.buffer_capacity
            );
            Vec::with_capacity(self.buffer_capacity)
        });
        PooledBuffer {
            buf,
            pool: Some(Arc::clone(&self.pool)),
        }
    }

    /// Return an externally owned buffer back to the pool.
    ///
    /// This is useful when a buffer originates from the pool, is temporarily
    /// moved into other structures, and later recovered via `into_raw()`.
    /// The buffer is cleared before being pushed back. If the pool is full,
    /// the buffer is dropped.
    pub fn put_back(&self, mut buf: Vec<u8>) {
        buf.clear();
        let _ = self.pool.push(buf);
    }

    /// Number of buffers currently available in the pool.
    #[inline]
    pub fn available(&self) -> usize {
        self.pool.len()
    }
}

/// RAII guard for a pooled buffer.
///
/// When dropped, the buffer is cleared (length set to 0, capacity preserved)
/// and returned to the pool. If the pool is full (more returns than checkouts
/// due to temporary overflow allocations), the buffer is simply freed.
pub struct PooledBuffer {
    buf: Vec<u8>,
    pool: Option<Arc<ArrayQueue<Vec<u8>>>>,
}

impl PooledBuffer {
    /// Get a mutable reference to the inner buffer.
    #[inline]
    pub fn as_mut_vec(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    /// Consume the guard and take ownership of the buffer.
    ///
    /// The buffer will NOT be returned to the pool. Use when you need
    /// to hand off the buffer to a different owner (e.g. `Bytes::from()`).
    #[inline]
    pub fn take(mut self) -> Vec<u8> {
        self.pool = None;
        std::mem::take(&mut self.buf)
    }
}

impl std::ops::Deref for PooledBuffer {
    type Target = Vec<u8>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.buf
    }
}

impl std::ops::DerefMut for PooledBuffer {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buf
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            self.buf.clear();
            let mut recycled = Vec::new();
            std::mem::swap(&mut recycled, &mut self.buf);
            // Return to pool if there's room; otherwise just drop.
            let _ = pool.push(recycled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_and_return() {
        let pool = CpuBufferPool::new(2, 1024);
        assert_eq!(pool.available(), 2);

        let mut buf = pool.checkout();
        assert_eq!(pool.available(), 1);

        buf.extend_from_slice(&[1u8; 512]);
        assert_eq!(buf.len(), 512);

        drop(buf);
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn overflow_allocation() {
        let pool = CpuBufferPool::new(1, 256);
        let _b1 = pool.checkout();
        assert_eq!(pool.available(), 0);

        // This should allocate a fresh buffer rather than blocking.
        let _b2 = pool.checkout();
        assert_eq!(pool.available(), 0);

        drop(_b1);
        assert_eq!(pool.available(), 1);

        // Pool is full (capacity=1), second return just drops.
        drop(_b2);
        assert_eq!(pool.available(), 1);
    }

    #[test]
    fn take_prevents_return() {
        let pool = CpuBufferPool::new(1, 128);
        let buf = pool.checkout();
        assert_eq!(pool.available(), 0);

        let _owned = buf.take();
        assert_eq!(pool.available(), 0);
    }

    #[test]
    fn for_resolution_correct_capacity() {
        let pool = CpuBufferPool::for_resolution(4, 1920, 1080, 3);
        assert_eq!(pool.available(), 4);
        let buf = pool.checkout();
        assert!(buf.capacity() >= 1920 * 1080 * 3);
    }

    #[test]
    fn concurrent_checkout_return_is_safe() {
        use std::sync::Arc;
        let pool = Arc::new(CpuBufferPool::new(8, 1024));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let pool = Arc::clone(&pool);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        let mut buf = pool.checkout();
                        buf.extend_from_slice(&[42u8; 128]);
                        drop(buf);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread panicked");
        }
        // Pool should still be in a consistent state.
        assert!(pool.available() <= 8);
    }

    #[test]
    fn pooled_buffer_deref_and_deref_mut() {
        let pool = CpuBufferPool::new(1, 256);
        let mut buf = pool.checkout();
        // DerefMut: write via Vec methods
        buf.extend_from_slice(&[1, 2, 3]);
        // Deref: read via Vec methods
        assert_eq!(buf.len(), 3);
        assert_eq!(&buf[..], &[1, 2, 3]);
    }
}
