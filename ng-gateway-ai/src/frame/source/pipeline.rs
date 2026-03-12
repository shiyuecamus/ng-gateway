//! Programmatic GStreamer pipeline construction.
//!
//! Replaces the string-concatenation approach with type-safe `ElementFactory`
//! API calls. Uses `decodebin3` for automatic codec detection and hardware
//! decoder selection, with platform-specific post-processing chains.
//!
//! # Pipeline topology
//!
//! ```text
//! source_element → [decodebin3] → queue → [platform CSC/resize] → [videorate] → appsink
//! ```
//!
//! The source element and decodebin3 have dynamic pads that are connected
//! at runtime via `pad-added` signals.

use gstreamer::prelude::*;
use ng_gateway_error::ai::AiEngineError;

use super::gst_source::{GstFrameSourceConfig, RtspTransport};
use crate::frame::{memory::HardwarePlatform, platform::PlatformCapabilities};

/// URI scheme classification for source element selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Rtsp,
    Http,
    V4l2,
    File,
}

impl SourceKind {
    /// Classify a URI into a source kind.
    pub fn from_uri(uri: &str) -> Self {
        let lower = uri.to_lowercase();
        if lower.starts_with("rtsp://") || lower.starts_with("rtsps://") {
            Self::Rtsp
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            Self::Http
        } else if lower.starts_with("v4l2://") || lower.starts_with("/dev/video") {
            Self::V4l2
        } else if lower.starts_with("file://") || std::path::Path::new(uri).exists() {
            Self::File
        } else {
            tracing::warn!(uri, "unknown URI scheme, treating as RTSP");
            Self::Rtsp
        }
    }
}

/// Result of building a GStreamer pipeline.
///
/// Holds strong references to the pipeline and appsink for lifetime
/// management and runtime monitoring.
pub(crate) struct BuiltPipeline {
    /// The constructed GStreamer pipeline.
    pub pipeline: gstreamer::Pipeline,
    /// The appsink element (for setting callbacks and pulling samples).
    pub appsink: gstreamer_app::AppSink,
}

/// Builds a GStreamer pipeline using programmatic `ElementFactory` API.
///
/// The builder selects source elements based on URI scheme, uses `decodebin3`
/// for automatic codec detection and decoder selection, and constructs
/// platform-optimized post-processing chains.
pub(crate) struct PipelineBuilder<'a> {
    config: &'a GstFrameSourceConfig,
    caps: &'a PlatformCapabilities,
}

impl<'a> PipelineBuilder<'a> {
    pub fn new(config: &'a GstFrameSourceConfig, caps: &'a PlatformCapabilities) -> Self {
        Self { config, caps }
    }

    /// Build the complete pipeline.
    ///
    /// The pipeline is constructed but left in `Null` state — the caller
    /// is responsible for setting it to `Playing` after attaching appsink
    /// callbacks.
    pub fn build(&self) -> Result<BuiltPipeline, AiEngineError> {
        let effective_caps = self.effective_capabilities();

        let pipeline = gstreamer::Pipeline::default();
        let appsink = self.create_appsink(&effective_caps)?;
        let source_kind = SourceKind::from_uri(&self.config.uri);

        match source_kind {
            SourceKind::Rtsp => {
                self.build_rtsp_pipeline(&pipeline, &appsink, &effective_caps)?;
            }
            SourceKind::Http => {
                self.build_http_pipeline(&pipeline, &appsink)?;
            }
            SourceKind::V4l2 => {
                self.build_v4l2_pipeline(&pipeline, &appsink, &effective_caps)?;
            }
            SourceKind::File => {
                self.build_file_pipeline(&pipeline, &appsink, &effective_caps)?;
            }
        }

        tracing::info!(
            uri = %self.config.uri,
            platform = ?effective_caps.platform,
            source = ?source_kind,
            "programmatic GStreamer pipeline built"
        );

        Ok(BuiltPipeline { pipeline, appsink })
    }

    /// When the user explicitly disables DMA-buf, fall back to generic
    /// software decode + videoconvert regardless of detected HW capabilities.
    fn effective_capabilities(&self) -> PlatformCapabilities {
        if self.config.request_dma_buf {
            self.caps.clone()
        } else {
            PlatformCapabilities::probe(HardwarePlatform::Generic)
        }
    }

    fn create_appsink(
        &self,
        caps: &PlatformCapabilities,
    ) -> Result<gstreamer_app::AppSink, AiEngineError> {
        let appsink = gstreamer_app::AppSink::builder()
            .name("sink")
            .sync(false)
            .max_buffers(self.config.max_buffers)
            .drop(true)
            .build();

        let sink_caps = build_appsink_caps(caps);
        appsink.set_caps(Some(&sink_caps));

        Ok(appsink)
    }

    // ── RTSP pipeline ──────────────────────────────────────────────

    /// Build RTSP pipeline:
    /// `rtspsrc → decodebin3 → [postprocess chain] → appsink`
    ///
    /// Both `rtspsrc` and `decodebin3` have dynamic pads. We connect:
    /// 1. rtspsrc `pad-added` → link video pads to decodebin3
    /// 2. decodebin3 `pad-added` → build & link postprocess chain
    fn build_rtsp_pipeline(
        &self,
        pipeline: &gstreamer::Pipeline,
        appsink: &gstreamer_app::AppSink,
        caps: &PlatformCapabilities,
    ) -> Result<(), AiEngineError> {
        let rtspsrc = self.create_rtspsrc()?;
        let decodebin = self.create_decodebin(caps)?;

        pipeline.add_many([&rtspsrc, &decodebin]).map_err(|e| {
            AiEngineError::FrameError(format!("failed to add RTSP elements to pipeline: {e}"))
        })?;

        // rtspsrc → decodebin3: connected via pad-added (dynamic pads).
        let decodebin_weak = decodebin.downgrade();
        rtspsrc.connect_pad_added(move |_src, src_pad| {
            let Some(decodebin) = decodebin_weak.upgrade() else {
                return;
            };

            let pad_caps = src_pad
                .current_caps()
                .unwrap_or_else(|| src_pad.query_caps(None));
            let Some(structure) = pad_caps.structure(0) else {
                return;
            };

            // Only link RTP video pads; skip audio and other media types.
            if structure.name().as_str() != "application/x-rtp" {
                return;
            }
            let media = structure.get::<&str>("media").unwrap_or("");
            if media != "video" {
                tracing::debug!(media, "skipping non-video rtspsrc pad");
                return;
            }

            if let Ok(encoding) = structure.get::<&str>("encoding-name") {
                tracing::info!(encoding, "RTSP stream codec detected");
            }

            // Link to decodebin3's request sink pad.
            let sink_pad = match decodebin.compatible_pad(src_pad, None) {
                Some(pad) => pad,
                None => {
                    tracing::error!("no compatible sink pad on decodebin3 for rtspsrc");
                    return;
                }
            };

            if let Err(e) = src_pad.link(&sink_pad) {
                tracing::error!(?e, "failed to link rtspsrc pad to decodebin3");
            }
        });

        // decodebin3 → postprocess chain: connected via pad-added.
        self.connect_decodebin_to_postprocess(pipeline, &decodebin, appsink, caps)?;

        Ok(())
    }

    fn create_rtspsrc(&self) -> Result<gstreamer::Element, AiEngineError> {
        let protocols = match self.config.rtsp_transport {
            RtspTransport::Tcp => "tcp",
            RtspTransport::UdpFallback => "udp+tcp",
            RtspTransport::TcpFallback => "tcp+udp",
        };
        let timeout_us = self.config.connect_timeout.as_micros() as u64;

        gstreamer::ElementFactory::make("rtspsrc")
            .name("source")
            .property("location", &self.config.uri)
            .property("latency", 100u32)
            .property_from_str("protocols", protocols)
            .property("tcp-timeout", timeout_us)
            .property("udp-reconnect", true)
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create rtspsrc: {e}")))
    }

    // ── HTTP pipeline (MJPEG) ──────────────────────────────────────

    /// Build HTTP pipeline for MJPEG streams:
    /// `souphttpsrc → multipartdemux → jpegdec → videoconvert → appsink`
    fn build_http_pipeline(
        &self,
        pipeline: &gstreamer::Pipeline,
        appsink: &gstreamer_app::AppSink,
    ) -> Result<(), AiEngineError> {
        let source = gstreamer::ElementFactory::make("souphttpsrc")
            .name("source")
            .property("location", &self.config.uri)
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create souphttpsrc: {e}")))?;

        let demux = make_element("multipartdemux", "demux")?;
        let decoder = make_element("jpegdec", "jpeg_decoder")?;
        let convert = make_element("videoconvert", "convert")?;

        let capsfilter = gstreamer::ElementFactory::make("capsfilter")
            .name("http_caps")
            .property(
                "caps",
                gstreamer::Caps::builder("video/x-raw")
                    .field("format", "RGB")
                    .build(),
            )
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create capsfilter: {e}")))?;

        let appsink_element = appsink.clone().upcast::<gstreamer::Element>();
        let elements: Vec<&gstreamer::Element> = vec![
            &source,
            &demux,
            &decoder,
            &convert,
            &capsfilter,
            &appsink_element,
        ];

        pipeline
            .add_many(elements.iter().copied())
            .map_err(|e| AiEngineError::FrameError(format!("failed to add HTTP elements: {e}")))?;

        // souphttpsrc → multipartdemux (static link)
        source.link(&demux).map_err(|e| {
            AiEngineError::FrameError(format!("failed to link souphttpsrc → demux: {e}"))
        })?;

        // multipartdemux has dynamic pads; link to jpegdec on pad-added
        let decoder_weak = decoder.downgrade();
        demux.connect_pad_added(move |_demux, src_pad| {
            let Some(decoder) = decoder_weak.upgrade() else {
                return;
            };
            let sink_pad = match decoder.static_pad("sink") {
                Some(pad) => pad,
                None => return,
            };
            if sink_pad.is_linked() {
                return;
            }
            if let Err(e) = src_pad.link(&sink_pad) {
                tracing::error!(?e, "failed to link multipartdemux → jpegdec");
            }
        });

        // jpegdec → convert → capsfilter → appsink
        gstreamer::Element::link_many([&decoder, &convert, &capsfilter, &appsink_element])
            .map_err(|e| {
                AiEngineError::FrameError(format!("failed to link HTTP decode chain: {e}"))
            })?;

        Ok(())
    }

    // ── V4L2 pipeline ──────────────────────────────────────────────

    /// Build V4L2 pipeline:
    /// `v4l2src → [platform CSC] → capsfilter → appsink`
    fn build_v4l2_pipeline(
        &self,
        pipeline: &gstreamer::Pipeline,
        appsink: &gstreamer_app::AppSink,
        caps: &PlatformCapabilities,
    ) -> Result<(), AiEngineError> {
        let device = self
            .config
            .uri
            .strip_prefix("v4l2://")
            .unwrap_or(&self.config.uri);

        let source = gstreamer::ElementFactory::make("v4l2src")
            .name("source")
            .property("device", device)
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create v4l2src: {e}")))?;

        let appsink_element = appsink.clone().upcast::<gstreamer::Element>();
        pipeline
            .add(&source)
            .map_err(|e| AiEngineError::FrameError(format!("failed to add v4l2src: {e}")))?;

        let chain = build_postprocess_elements(caps, self.config)?;
        add_and_link_chain(pipeline, &source, &chain, &appsink_element)?;

        Ok(())
    }

    // ── File pipeline ──────────────────────────────────────────────

    /// Build file pipeline:
    /// `filesrc → decodebin3 → [postprocess chain] → appsink`
    fn build_file_pipeline(
        &self,
        pipeline: &gstreamer::Pipeline,
        appsink: &gstreamer_app::AppSink,
        caps: &PlatformCapabilities,
    ) -> Result<(), AiEngineError> {
        let uri = &self.config.uri;
        let location = if uri.starts_with("file://") {
            uri.strip_prefix("file://").unwrap_or(uri)
        } else {
            uri.as_str()
        };

        let source = gstreamer::ElementFactory::make("filesrc")
            .name("source")
            .property("location", location)
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create filesrc: {e}")))?;

        let decodebin = self.create_decodebin(caps)?;

        pipeline
            .add_many([&source, &decodebin])
            .map_err(|e| AiEngineError::FrameError(format!("failed to add file elements: {e}")))?;

        gstreamer::Element::link_many([&source, &decodebin]).map_err(|e| {
            AiEngineError::FrameError(format!("failed to link filesrc → decodebin3: {e}"))
        })?;

        self.connect_decodebin_to_postprocess(pipeline, &decodebin, appsink, caps)?;

        Ok(())
    }

    // ── Shared helpers ─────────────────────────────────────────────

    /// Create and configure `decodebin3`.
    ///
    /// `decodebin3` automatically detects the media type, selects the optimal
    /// decoder (hardware-preferred via element ranking), and outputs raw video.
    ///
    /// On Rockchip platforms with RGA support, installs a `deep-element-added`
    /// signal handler that configures `mppvideodec`'s built-in RGA engine for
    /// hardware color-space conversion and resize, avoiding the need for
    /// separate postprocess elements.
    fn create_decodebin(
        &self,
        caps: &PlatformCapabilities,
    ) -> Result<gstreamer::Element, AiEngineError> {
        let decodebin = gstreamer::ElementFactory::make("decodebin3")
            .name("decoder")
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create decodebin3: {e}")))?;

        // Limit to video streams only — ignore audio/subtitles.
        let video_caps = gstreamer::Caps::builder("video/x-raw")
            .any_features()
            .build();
        decodebin.set_property("caps", &video_caps);

        // On Rockchip with RGA: configure mppvideodec's built-in RGA engine
        // when decodebin3 instantiates it, so CSC (NV12→RGB) and resize are
        // performed in the decoder's output path by the RGA 2D hardware.
        if caps.platform == HardwarePlatform::Rockchip && caps.supports_hw_csc {
            let target_resolution = self.config.target_resolution;
            decodebin.connect("deep-element-added", false, move |args| {
                let element = args[2].get::<gstreamer::Element>().ok()?;
                let factory = element.factory()?;
                if factory.name() != "mppvideodec" {
                    return None;
                }

                tracing::info!("configuring mppvideodec built-in RGA for hardware CSC + resize");
                element.set_property_from_str("format", "RGB");

                if let Some((w, h)) = target_resolution {
                    element.set_property("width", w);
                    element.set_property("height", h);
                    tracing::info!(width = w, height = h, "mppvideodec RGA resize configured");
                }

                None
            });
        }

        Ok(decodebin)
    }

    /// Connect decodebin3's `pad-added` signal to dynamically build
    /// the platform-specific postprocess chain and link to appsink.
    fn connect_decodebin_to_postprocess(
        &self,
        pipeline: &gstreamer::Pipeline,
        decodebin: &gstreamer::Element,
        appsink: &gstreamer_app::AppSink,
        caps: &PlatformCapabilities,
    ) -> Result<(), AiEngineError> {
        let pipeline_weak = pipeline.downgrade();
        let caps_clone = caps.clone();
        let config_clone = self.config.clone();
        let appsink_element = appsink.clone().upcast::<gstreamer::Element>();

        decodebin.connect_pad_added(move |_dbin, src_pad| {
            let pad_caps = src_pad
                .current_caps()
                .unwrap_or_else(|| src_pad.query_caps(None));
            let Some(structure) = pad_caps.structure(0) else {
                return;
            };

            // Only handle video streams.
            if !structure.name().as_str().starts_with("video/") {
                tracing::debug!(
                    caps = %structure.name(),
                    "skipping non-video decodebin3 pad"
                );
                return;
            }

            let Some(pipeline) = pipeline_weak.upgrade() else {
                return;
            };

            tracing::info!(
                caps = %pad_caps.to_string(),
                "decodebin3 produced video pad, building postprocess chain"
            );

            if let Err(e) = link_postprocess_chain(
                &pipeline,
                src_pad,
                &appsink_element,
                &caps_clone,
                &config_clone,
            ) {
                tracing::error!(%e, "failed to build postprocess chain from decodebin3");
            }
        });

        // Add appsink to pipeline (postprocess elements are added in pad-added).
        let appsink_element = appsink.clone().upcast::<gstreamer::Element>();
        pipeline.add(&appsink_element).map_err(|e| {
            AiEngineError::FrameError(format!("failed to add appsink to pipeline: {e}"))
        })?;

        Ok(())
    }
}

// ── Pipeline element construction helpers ──────────────────────────

/// Convenience wrapper for `ElementFactory::make` with error mapping.
fn make_element(factory: &str, name: &str) -> Result<gstreamer::Element, AiEngineError> {
    gstreamer::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|e| {
            AiEngineError::FrameError(format!(
                "failed to create GStreamer element '{factory}' (name='{name}'): {e}"
            ))
        })
}

/// Build the appsink caps based on platform capabilities.
///
/// Tells GStreamer what output format we want from the postprocess chain.
fn build_appsink_caps(caps: &PlatformCapabilities) -> gstreamer::Caps {
    match caps.platform {
        HardwarePlatform::Rockchip if caps.supports_dma_buf && caps.supports_hw_csc => {
            gstreamer::Caps::builder("video/x-raw")
                .field("format", "RGB")
                .build()
        }
        HardwarePlatform::NvidiaJetson if caps.supports_hw_csc => {
            gstreamer::Caps::builder("video/x-raw")
                .field("format", "RGBA")
                .build()
        }
        HardwarePlatform::Vaapi if caps.supports_dma_buf && caps.supports_hw_csc => {
            gstreamer::Caps::builder("video/x-raw").build()
        }
        _ => gstreamer::Caps::builder("video/x-raw")
            .field("format", "RGB")
            .build(),
    }
}

/// Build the platform-specific postprocess element chain.
///
/// Returns a vector of elements to be inserted between the decoder output
/// and the appsink.
fn build_postprocess_elements(
    caps: &PlatformCapabilities,
    config: &GstFrameSourceConfig,
) -> Result<Vec<gstreamer::Element>, AiEngineError> {
    let mut elements = Vec::new();

    // Thread-decoupling queue between decoder and downstream.
    let queue = gstreamer::ElementFactory::make("queue")
        .name("post_queue")
        .property("max-size-buffers", 2u32)
        .property("max-size-time", 0u64)
        .property("max-size-bytes", 0u32)
        .build()
        .map_err(|e| AiEngineError::FrameError(format!("failed to create queue: {e}")))?;
    elements.push(queue);

    // Platform-specific CSC and optional resize.
    //
    // Rockchip: When mppvideodec has built-in RGA support, CSC and resize
    // are configured directly on the decoder via `deep-element-added`
    // (see `connect_decodebin_to_postprocess`). The postprocess chain only
    // needs a capsfilter to enforce the expected output format.
    match caps.platform {
        HardwarePlatform::Rockchip if caps.supports_hw_csc => {
            let mut caps_builder = gstreamer::Caps::builder("video/x-raw").field("format", "RGB");

            if let Some((w, h)) = config.target_resolution {
                caps_builder = caps_builder
                    .field("width", w as i32)
                    .field("height", h as i32);
            }

            let capsfilter = gstreamer::ElementFactory::make("capsfilter")
                .name("rga_caps")
                .property("caps", caps_builder.build())
                .build()
                .map_err(|e| {
                    AiEngineError::FrameError(format!("failed to create RGA capsfilter: {e}"))
                })?;
            elements.push(capsfilter);
        }

        HardwarePlatform::NvidiaJetson if caps.supports_hw_csc => {
            let nvvidconv = make_element("nvvidconv", "nv_csc")?;
            elements.push(nvvidconv);

            let capsfilter = gstreamer::ElementFactory::make("capsfilter")
                .name("nv_caps")
                .property(
                    "caps",
                    gstreamer::Caps::builder("video/x-raw")
                        .field("format", "RGBA")
                        .build(),
                )
                .build()
                .map_err(|e| {
                    AiEngineError::FrameError(format!("failed to create Jetson capsfilter: {e}"))
                })?;
            elements.push(capsfilter);
        }

        HardwarePlatform::Vaapi if caps.supports_hw_csc => {
            let postproc = gstreamer::ElementFactory::make("vaapipostproc")
                .name("vaapi_csc")
                .property_from_str("format", "rgbx")
                .build()
                .map_err(|e| {
                    AiEngineError::FrameError(format!("failed to create vaapipostproc: {e}"))
                })?;
            elements.push(postproc);
        }

        _ => {
            let convert = make_element("videoconvert", "sw_convert")?;
            elements.push(convert);

            if config.target_resolution.is_some() {
                let scale = make_element("videoscale", "sw_scale")?;
                elements.push(scale);
            }

            let mut caps_builder = gstreamer::Caps::builder("video/x-raw").field("format", "RGB");

            if let Some((w, h)) = config.target_resolution {
                caps_builder = caps_builder
                    .field("width", w as i32)
                    .field("height", h as i32);
            }

            let capsfilter = gstreamer::ElementFactory::make("capsfilter")
                .name("sw_caps")
                .property("caps", caps_builder.build())
                .build()
                .map_err(|e| {
                    AiEngineError::FrameError(format!("failed to create SW capsfilter: {e}"))
                })?;
            elements.push(capsfilter);
        }
    }

    // Level 1 sampling: videorate caps the frame rate post-decode.
    if let Some(fps) = config.target_fps {
        let rate = gstreamer::ElementFactory::make("videorate")
            .name("framerate_limiter")
            .property("drop-only", true)
            .build()
            .map_err(|e| AiEngineError::FrameError(format!("failed to create videorate: {e}")))?;
        elements.push(rate);

        let fps_int = (fps.ceil() as i32).max(1);
        let rate_caps = gstreamer::ElementFactory::make("capsfilter")
            .name("rate_caps")
            .property(
                "caps",
                gstreamer::Caps::builder("video/x-raw")
                    .field("framerate", gstreamer::Fraction::new(fps_int, 1))
                    .build(),
            )
            .build()
            .map_err(|e| {
                AiEngineError::FrameError(format!("failed to create rate capsfilter: {e}"))
            })?;
        elements.push(rate_caps);
    }

    Ok(elements)
}

/// Dynamically build and link the postprocess chain in decodebin3's
/// `pad-added` callback.
///
/// Called from the GStreamer streaming thread when decodebin3 produces a
/// new source pad with decoded video. Builds the platform-specific
/// postprocess chain, adds all elements to the pipeline, links them,
/// and syncs their state with the parent pipeline.
fn link_postprocess_chain(
    pipeline: &gstreamer::Pipeline,
    decode_src_pad: &gstreamer::Pad,
    appsink: &gstreamer::Element,
    caps: &PlatformCapabilities,
    config: &GstFrameSourceConfig,
) -> Result<(), AiEngineError> {
    let chain = build_postprocess_elements(caps, config)?;

    // Add all postprocess elements to the pipeline.
    for element in &chain {
        pipeline.add(element).map_err(|e| {
            AiEngineError::FrameError(format!(
                "failed to add postprocess element '{}': {e}",
                element.name()
            ))
        })?;
    }

    // Link decoder src pad → first element of chain.
    if let Some(first) = chain.first() {
        let sink_pad = first.static_pad("sink").ok_or_else(|| {
            AiEngineError::FrameError(format!("no sink pad on element '{}'", first.name()))
        })?;
        decode_src_pad.link(&sink_pad).map_err(|e| {
            AiEngineError::FrameError(format!("failed to link decoder → {}: {e:?}", first.name()))
        })?;
    }

    // Link chain elements together.
    for window in chain.windows(2) {
        window[0].link(&window[1]).map_err(|e| {
            AiEngineError::FrameError(format!(
                "failed to link {} → {}: {e}",
                window[0].name(),
                window[1].name()
            ))
        })?;
    }

    // Link last chain element → appsink.
    if let Some(last) = chain.last() {
        last.link(appsink).map_err(|e| {
            AiEngineError::FrameError(format!("failed to link {} → appsink: {e}", last.name()))
        })?;
    } else {
        // No postprocess elements — link decoder directly to appsink.
        let sink_pad = appsink
            .static_pad("sink")
            .ok_or_else(|| AiEngineError::FrameError("appsink has no sink pad".into()))?;
        decode_src_pad.link(&sink_pad).map_err(|e| {
            AiEngineError::FrameError(format!("failed to link decoder → appsink: {e:?}"))
        })?;
    }

    // Sync all new elements to the pipeline's current state.
    for element in &chain {
        element.sync_state_with_parent().map_err(|e| {
            AiEngineError::FrameError(format!(
                "failed to sync state for '{}': {e}",
                element.name()
            ))
        })?;
    }

    Ok(())
}

/// Add a chain of elements to the pipeline and link `source → chain → sink`.
fn add_and_link_chain(
    pipeline: &gstreamer::Pipeline,
    source: &gstreamer::Element,
    chain: &[gstreamer::Element],
    sink: &gstreamer::Element,
) -> Result<(), AiEngineError> {
    for element in chain {
        pipeline.add(element).map_err(|e| {
            AiEngineError::FrameError(format!(
                "failed to add '{}' to pipeline: {e}",
                element.name()
            ))
        })?;
    }
    pipeline
        .add(sink)
        .map_err(|e| AiEngineError::FrameError(format!("failed to add sink to pipeline: {e}")))?;

    // Build the full link sequence: source → chain[0] → ... → chain[n] → sink
    let mut all: Vec<&gstreamer::Element> = Vec::with_capacity(chain.len() + 2);
    all.push(source);
    all.extend(chain.iter());
    all.push(sink);

    gstreamer::Element::link_many(all)
        .map_err(|e| AiEngineError::FrameError(format!("failed to link element chain: {e}")))?;

    Ok(())
}
