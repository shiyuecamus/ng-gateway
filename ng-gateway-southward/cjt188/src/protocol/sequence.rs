use std::sync::atomic::{AtomicU8, Ordering};

/// Manages CJ/T 188 frame sequence numbers (SER).
///
/// According to CJ/T 188-2018 6.3.3.4, the SER domain is 1 byte, used to mark the frame sequence.
/// The response frame SER must match the request frame SER.
///
/// Best Practice:
/// - Maintain a rolling counter (0-255) per communication channel/session.
/// - Increment for each new request.
/// - Retransmissions should ideally use the same SER (though often treated as new requests in simple drivers).
#[derive(Debug, Default)]
pub struct SequenceManager {
    next: AtomicU8,
}

impl SequenceManager {
    pub fn new() -> Self {
        Self {
            next: AtomicU8::new(0),
        }
    }

    /// Get the next sequence number.
    pub fn next(&self) -> u8 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}
