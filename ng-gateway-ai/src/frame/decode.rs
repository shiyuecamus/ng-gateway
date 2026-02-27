//! Frame decoder pool — decodes encoded video frames to RGB24.
//!
//! Supports JPEG (via `image` crate, in-process) and H.264/H.265 (via FFmpeg,
//! dispatched to a dedicated OS thread pool). NV12 frames are converted
//! in-process using a direct YUV→RGB transform.
//!
//! # Architecture
//!
//! ```text
//! decode(&VideoFrame)
//!   ├── JPEG / RGB24 → fast-path (current thread)
//!   ├── NV12          → in-process colorspace conversion
//!   └── H264 / H265   → dispatch to FFmpeg worker pool
//!         │
//!         ▼
//!     ┌──────────────────────────────────────────┐
//!     │  mpsc::channel<DecodeRequest>             │
//!     │  (bounded = workers × 4)                  │
//!     └───────┬──────────┬──────────┬─────────────┘
//!             ▼          ▼          ▼
//!       ┌──────────┐ ┌──────────┐ ┌──────────┐
//!       │ Worker 0 │ │ Worker 1 │ │ Worker N │
//!       │ (OS thd) │ │ (OS thd) │ │ (OS thd) │
//!       │ FFmpeg   │ │ FFmpeg   │ │ FFmpeg   │
//!       │ ctx      │ │ ctx      │ │ ctx      │
//!       └──────────┘ └──────────┘ └──────────┘
//! ```
//!
//! Each worker thread owns a per-codec FFmpeg decoder context and a mini
//! Tokio current-thread runtime for channel I/O. The decode operation itself
//! is synchronous (CPU-bound) and runs outside the main Tokio executor to
//! avoid starving async tasks.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded::DecodedFrame;
    use bytes::Bytes;
    use ffmpeg_next::{
        codec::{self, Context},
        decoder::Video as DecoderVideo,
        format::Pixel,
        frame::Video as AvFrame,
        software::scaling::{self, Context as ScalerContext},
        Packet,
    };
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::types::{FrameFormat, VideoFrame};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        OnceLock,
    };
    use tokio::sync::{mpsc, oneshot};
    use tracing::info_span;

    /// Frame decoder pool — decodes encoded video frames to RGB24.
    ///
    /// JPEG and RGB24 frames are handled on the calling task (fast path).
    /// H.264/H.265 NAL units are dispatched to a pool of dedicated OS threads,
    /// each owning an independent FFmpeg decoder context.
    pub struct FrameDecoderPool {
        /// Per-worker bounded request queues.
        workers: Vec<mpsc::Sender<DecodeRequest>>,
        /// Round-robin cursor used as fallback routing.
        rr_cursor: AtomicUsize,
    }

    struct DecodeRequest {
        frame: VideoFrame,
        result_tx: oneshot::Sender<Result<DecodedFrame, AiEngineError>>,
    }

    impl FrameDecoderPool {
        /// Create a new decoder pool with `n` FFmpeg worker threads.
        ///
        /// Workers are spawned as named OS threads (`ai-decode-0`, `ai-decode-1`, …).
        /// Each worker owns an independent queue and persistent FFmpeg contexts.
        pub fn new(workers: usize) -> Result<Self, AiEngineError> {
            let workers = workers.max(1);
            ffmpeg_init_once()?;

            let mut worker_senders = Vec::with_capacity(workers);
            let queue_capacity = workers * 4;
            for worker_id in 0..workers {
                let (tx, rx) = tokio::sync::mpsc::channel::<DecodeRequest>(queue_capacity);
                std::thread::Builder::new()
                    .name(format!("ai-decode-{worker_id}"))
                    .spawn(move || {
                        decode_worker_loop(worker_id, rx);
                    })
                    .map_err(|e| {
                        AiEngineError::IoError(format!("failed to spawn decode worker: {e}"))
                    })?;
                worker_senders.push(tx);
            }

            tracing::info!(workers, "frame decoder pool initialized");
            Ok(Self {
                workers: worker_senders,
                rr_cursor: AtomicUsize::new(0),
            })
        }

        /// Decode a video frame to RGB24.
        ///
        /// - **JPEG**: decoded in-process via `image` crate (fast path).
        /// - **RGB24**: zero-cost passthrough.
        /// - **NV12**: in-process YUV→RGB conversion.
        /// - **H264/H265**: dispatched to FFmpeg worker pool.
        pub async fn decode(&self, frame: &VideoFrame) -> Result<DecodedFrame, AiEngineError> {
            match frame.format {
                FrameFormat::Jpeg => decode_jpeg(&frame.data),
                FrameFormat::Rgb24 => Ok(DecodedFrame {
                    data: frame.data.clone(),
                    width: frame.width,
                    height: frame.height,
                }),
                FrameFormat::Nv12 => decode_nv12(&frame.data, frame.width, frame.height),
                FrameFormat::H264Nal | FrameFormat::H265Nal => self.dispatch_ffmpeg(frame).await,
            }
        }

        /// Dispatch a frame to the FFmpeg worker pool and await the result.
        async fn dispatch_ffmpeg(&self, frame: &VideoFrame) -> Result<DecodedFrame, AiEngineError> {
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            let worker_index = self.select_worker(frame);
            self.workers[worker_index]
                .send(DecodeRequest {
                    frame: frame.clone(),
                    result_tx,
                })
                .await
                .map_err(|_| AiEngineError::InternalError("decoder pool channel closed".into()))?;

            let decode_wait_span = info_span!("decode_wait", format = ?frame.format);
            let _decode_wait_guard = decode_wait_span.enter();
            result_rx
                .await
                .map_err(|_| AiEngineError::InternalError("decoder response lost".into()))?
        }

        /// Select one decode worker.
        ///
        /// The default path uses frame sequence sharding to preserve stream
        /// locality. If sequence is unavailable, round-robin fallback is used.
        fn select_worker(&self, frame: &VideoFrame) -> usize {
            let worker_count = self.workers.len();
            if worker_count <= 1 {
                return 0;
            }

            let shard_by_seq = (frame.seq as usize) % worker_count;
            let rr = self.rr_cursor.fetch_add(1, Ordering::Relaxed) % worker_count;
            if frame.seq == 0 {
                rr
            } else {
                shard_by_seq
            }
        }
    }

    // ── FFmpeg initialization ─────────────────────────────────────────

    fn ffmpeg_init_once() -> Result<(), AiEngineError> {
        static INIT: OnceLock<Result<(), String>> = OnceLock::new();
        let result = INIT.get_or_init(|| ffmpeg_next::init().map_err(|e| e.to_string()));
        match result {
            Ok(()) => Ok(()),
            Err(err) => Err(AiEngineError::DecodeError(format!(
                "ffmpeg init failed: {err}"
            ))),
        }
    }

    // ── Worker loop ───────────────────────────────────────────────────

    /// Main loop for each FFmpeg decode worker thread.
    ///
    /// Each worker creates a mini current-thread Tokio runtime purely for
    /// async channel I/O. The actual decode is synchronous (CPU-bound).
    fn decode_worker_loop(worker_id: usize, mut rx: mpsc::Receiver<DecodeRequest>) {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(
                    worker_id,
                    error = %error,
                    "failed to build decode worker runtime"
                );
                return;
            }
        };

        rt.block_on(async {
            let mut decoder_context = DecoderContext::default();
            loop {
                let req = rx.recv().await;
                match req {
                    Some(req) => {
                        let decode_exec_span =
                            info_span!("decode_exec", format = ?req.frame.format, worker_id = worker_id);
                        let _decode_exec_guard = decode_exec_span.enter();
                        let result = decode_frame_ffmpeg(&req.frame, &mut decoder_context);
                        drop(_decode_exec_guard);
                        let _ = req.result_tx.send(result);
                    }
                    None => break,
                }
            }
        });
    }

    // ── FFmpeg decode ─────────────────────────────────────────────────

    /// Reusable FFmpeg decode/scaling context local to one worker.
    #[derive(Default)]
    struct DecoderContext {
        h264: Option<DecoderVideo>,
        h265: Option<DecoderVideo>,
        scaler: Option<ScalerContext>,
        scaler_spec: Option<ScalerSpec>,
    }

    /// Input/output shape and format for scaler cache key.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ScalerSpec {
        src_format: Pixel,
        src_width: u32,
        src_height: u32,
        dst_format: Pixel,
        dst_width: u32,
        dst_height: u32,
    }

    impl DecoderContext {
        /// Get or create decoder for one encoded format.
        fn decoder_for(&mut self, format: FrameFormat) -> Result<&mut DecoderVideo, AiEngineError> {
            let slot = match format {
                FrameFormat::H264Nal => &mut self.h264,
                FrameFormat::H265Nal => &mut self.h265,
                _ => {
                    return Err(AiEngineError::DecodeError(format!(
                        "unsupported format for FFmpeg decode: {format:?}"
                    )));
                }
            };
            if slot.is_none() {
                let codec_id = match format {
                    FrameFormat::H264Nal => codec::Id::H264,
                    FrameFormat::H265Nal => codec::Id::HEVC,
                    _ => unreachable!(),
                };
                *slot = Some(open_decoder(codec_id)?);
            }
            slot.as_mut().ok_or_else(|| {
                AiEngineError::DecodeError("decoder context initialization failed".to_string())
            })
        }

        /// Get or create scaler for one source/destination pair.
        fn scaler_for(&mut self, spec: ScalerSpec) -> Result<&mut ScalerContext, AiEngineError> {
            let needs_rebuild = self
                .scaler_spec
                .map(|current| current != spec)
                .unwrap_or(true);
            if needs_rebuild {
                let scaler = scaling::Context::get(
                    spec.src_format,
                    spec.src_width,
                    spec.src_height,
                    spec.dst_format,
                    spec.dst_width,
                    spec.dst_height,
                    scaling::Flags::BILINEAR,
                )
                .map_err(|e| AiEngineError::DecodeError(format!("scaler init: {e}")))?;
                self.scaler = Some(scaler);
                self.scaler_spec = Some(spec);
            }
            self.scaler.as_mut().ok_or_else(|| {
                AiEngineError::DecodeError("scaler context initialization failed".to_string())
            })
        }
    }

    /// Open one FFmpeg decoder context for the given codec.
    fn open_decoder(codec_id: codec::Id) -> Result<DecoderVideo, AiEngineError> {
        let decoder_codec = codec::decoder::find(codec_id).ok_or_else(|| {
            AiEngineError::DecodeError(format!("FFmpeg codec not found: {codec_id:?}"))
        })?;
        Context::new_with_codec(decoder_codec)
            .decoder()
            .video()
            .map_err(|e| AiEngineError::DecodeError(format!("decoder open: {e}")))
    }

    /// Decode a single H.264 or H.265 NAL unit to RGB24 using FFmpeg.
    ///
    /// Uses worker-local decoder/scaler context to avoid per-frame reinitialization.
    fn decode_frame_ffmpeg(
        frame: &VideoFrame,
        decoder_context: &mut DecoderContext,
    ) -> Result<DecodedFrame, AiEngineError> {
        let mut av_frame = AvFrame::empty();
        {
            let decoder = decoder_context.decoder_for(frame.format)?;

            // Feed NAL data as one packet.
            let packet = Packet::copy(&frame.data);
            decoder
                .send_packet(&packet)
                .map_err(|e| AiEngineError::DecodeError(format!("send_packet: {e}")))?;

            // Flush for packet-at-a-time decode to force output frame availability.
            let _ = decoder.send_eof();

            // Receive one decoded frame.
            decoder
                .receive_frame(&mut av_frame)
                .map_err(|e| AiEngineError::DecodeError(format!("receive_frame: {e}")))?;
        }

        let src_w = av_frame.width();
        let src_h = av_frame.height();
        let scaler_spec = ScalerSpec {
            src_format: av_frame.format(),
            src_width: src_w,
            src_height: src_h,
            dst_format: Pixel::RGB24,
            dst_width: src_w,
            dst_height: src_h,
        };

        // Convert to RGB24 using reusable swscale context.
        let scaler = decoder_context.scaler_for(scaler_spec)?;
        let mut rgb_frame = AvFrame::empty();
        scaler
            .run(&av_frame, &mut rgb_frame)
            .map_err(|e| AiEngineError::DecodeError(format!("colorspace conversion: {e}")))?;

        // Extract RGB24 data from the output frame (may have stride padding).
        let stride = rgb_frame.stride(0);
        let expected_row_bytes = (src_w as usize) * 3;
        let mut rgb_data = Vec::with_capacity(expected_row_bytes * src_h as usize);

        let plane_data = rgb_frame.data(0);
        for row in 0..src_h as usize {
            let start = row * stride;
            let end = start + expected_row_bytes;
            if end <= plane_data.len() {
                rgb_data.extend_from_slice(&plane_data[start..end]);
            }
        }

        Ok(DecodedFrame {
            data: Bytes::from(rgb_data),
            width: src_w,
            height: src_h,
        })
    }

    // ── JPEG decode ───────────────────────────────────────────────────

    /// Decode JPEG bytes to RGB24 using the `image` crate.
    fn decode_jpeg(data: &[u8]) -> Result<DecodedFrame, AiEngineError> {
        use std::io::Cursor;

        let img = image::ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| AiEngineError::DecodeError(format!("format guess: {e}")))?
            .decode()
            .map_err(|e| AiEngineError::DecodeError(format!("JPEG decode: {e}")))?;

        let rgb = img.to_rgb8();
        let (width, height) = (rgb.width(), rgb.height());

        Ok(DecodedFrame {
            data: Bytes::from(rgb.into_raw()),
            width,
            height,
        })
    }

    // ── NV12 → RGB24 conversion ───────────────────────────────────────

    /// Convert NV12 frame to RGB24.
    ///
    /// NV12 layout: `width × height` Y plane followed by `width × height/2`
    /// interleaved UV plane. Uses BT.601 coefficients for YUV→RGB.
    fn decode_nv12(data: &[u8], width: u32, height: u32) -> Result<DecodedFrame, AiEngineError> {
        let w = width as usize;
        let h = height as usize;
        let y_plane_size = w * h;
        let expected_size = y_plane_size + y_plane_size / 2;

        if data.len() < expected_size {
            return Err(AiEngineError::DecodeError(format!(
                "NV12 data too short: expected {expected_size}, got {}",
                data.len()
            )));
        }

        let y_plane = &data[..y_plane_size];
        let uv_plane = &data[y_plane_size..];

        let mut rgb = Vec::with_capacity(w * h * 3);

        for row in 0..h {
            for col in 0..w {
                let y_val = y_plane[row * w + col] as f32;
                let uv_idx = (row / 2) * w + (col & !1);
                let u_val = uv_plane[uv_idx] as f32 - 128.0;
                let v_val = uv_plane[uv_idx + 1] as f32 - 128.0;

                // BT.601 YUV → RGB
                let r = (y_val + 1.402 * v_val).round().clamp(0.0, 255.0) as u8;
                let g = (y_val - 0.344136 * u_val - 0.714136 * v_val)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let b = (y_val + 1.772 * u_val).round().clamp(0.0, 255.0) as u8;

                rgb.push(r);
                rgb.push(g);
                rgb.push(b);
            }
        }

        Ok(DecodedFrame {
            data: Bytes::from(rgb),
            width,
            height,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use bytes::Bytes;

        #[test]
        fn decode_rgb24_passthrough() {
            let pool = FrameDecoderPool::new(1).unwrap();
            let frame = VideoFrame {
                data: Bytes::from(vec![128u8; 320 * 240 * 3]),
                format: FrameFormat::Rgb24,
                width: 320,
                height: 240,
                timestamp: chrono::Utc::now(),
                seq: 0,
            };
            let rt = tokio::runtime::Runtime::new().unwrap();
            let decoded = rt.block_on(pool.decode(&frame)).unwrap();
            assert_eq!(decoded.width, 320);
            assert_eq!(decoded.height, 240);
            assert_eq!(decoded.data.len(), 320 * 240 * 3);
        }

        #[test]
        fn decode_nv12_basic() {
            let w = 4u32;
            let h = 4u32;
            let y_size = (w * h) as usize;
            let uv_size = y_size / 2;
            let mut nv12 = vec![128u8; y_size + uv_size];
            // Set UV to 128 (neutral) so output should be roughly gray
            for v in &mut nv12[y_size..] {
                *v = 128;
            }
            let decoded = decode_nv12(&nv12, w, h).unwrap();
            assert_eq!(decoded.width, w);
            assert_eq!(decoded.height, h);
            assert_eq!(decoded.data.len(), (w * h * 3) as usize);
        }

        #[test]
        fn decode_nv12_rejects_short_data() {
            assert!(decode_nv12(&[0u8; 10], 320, 240).is_err());
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
