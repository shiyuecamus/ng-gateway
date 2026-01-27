//! Streaming ZIP utilities.
//!
//! This module provides a **true streaming** ZIP builder intended for HTTP downloads:
//! - build ZIP bytes incrementally
//! - push chunks into a bounded channel for backpressure
//! - avoid buffering the entire archive in memory
//!
//! # Compression strategy
//! Default is **fast**: `CompressionMethod::Stored` (no compression).

use bytes::Bytes;
use std::{
    io::{self, Read, Write},
    path::PathBuf,
};
use tokio::sync::mpsc;
use zip::{
    write::{SimpleFileOptions, ZipWriter},
    CompressionMethod,
};

/// A writer that writes to a channel.
pub struct ChannelWriter {
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    const CHUNK: usize = 64 * 1024;

    fn new(tx: mpsc::Sender<Result<Bytes, io::Error>>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(Self::CHUNK),
        }
    }

    fn flush_chunk(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buf);
        self.buf = Vec::with_capacity(Self::CHUNK);
        self.tx
            .blocking_send(Ok(Bytes::from(chunk)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client gone"))?;
        Ok(())
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        if self.buf.len() >= Self::CHUNK {
            self.flush_chunk()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_chunk()
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.flush_chunk();
    }
}

/// Stream a ZIP archive (stored/no-compression) to a chunk channel.
///
/// # Arguments
/// - `entries`: `(zip_entry_name, file_path)` pairs. The caller must ensure `zip_entry_name` is safe.
/// - `tx`: bounded channel sender. This function will `blocking_send` chunks.
///
/// # Notes
/// - Intended to run inside `spawn_blocking`, since ZIP writing is synchronous and CPU-bound.
/// - If the receiver is dropped (client gone), sending will return `BrokenPipe`.
pub fn stream_zip_stored(
    entries: Vec<(String, PathBuf)>,
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
) -> io::Result<()> {
    let writer = ChannelWriter::new(tx);
    let mut zip = ZipWriter::new_stream(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    let mut buf = vec![0u8; 128 * 1024];
    for (name, path) in entries {
        // Basic ZipSlip defense-in-depth (caller should also validate).
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid zip entry name",
            ));
        }

        zip.start_file(name, options)?;
        let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])?;
        }
    }

    let mut w = zip.finish()?;
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a unique temporary directory path for tests.
    ///
    /// # Note
    /// We intentionally avoid adding extra dev-dependencies (e.g. `tempfile`)
    /// to keep changes minimal.
    fn temp_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("ng-gateway-zip-stream-test-{}", nanos))
    }

    #[test]
    fn stream_zip_stored_writes_non_empty_zip_with_entries() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let host = dir.join("host.log.2026-01-27");
        let modbus = dir.join("modbus.log.2026-01-27");
        std::fs::write(&host, b"hello host\n").expect("write host file");
        std::fs::write(&modbus, b"hello modbus\n").expect("write modbus file");

        let entries = vec![
            ("host.log.2026-01-27".to_string(), host.clone()),
            ("modbus.log.2026-01-27".to_string(), modbus.clone()),
        ];

        let (tx, mut rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
        let t = std::thread::spawn(move || stream_zip_stored(entries, tx));

        // Collect all zip bytes from the receiver.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");
        let zip_bytes: Vec<u8> = rt.block_on(async move {
            let mut out: Vec<u8> = Vec::new();
            while let Some(item) = rx.recv().await {
                let chunk = item.expect("zip stream chunk");
                out.extend_from_slice(&chunk);
            }
            out
        });

        let res = t.join().expect("join zip thread");
        res.expect("zip stream should succeed");

        assert!(
            !zip_bytes.is_empty(),
            "zip should not be empty when entries exist"
        );

        // Verify ZIP archive entries are readable.
        let reader = Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(reader).expect("open zip archive");
        assert_eq!(archive.len(), 2, "zip should contain 2 entries");

        let mut names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "host.log.2026-01-27".to_string(),
                "modbus.log.2026-01-27".to_string()
            ]
        );

        // Cleanup best-effort.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
