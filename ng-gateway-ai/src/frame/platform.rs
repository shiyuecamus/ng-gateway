//! Hardware platform detection and capability probing.
//!
//! Auto-detects the hardware platform by probing available GStreamer elements,
//! system device nodes, and SoC identifiers. Used to select optimal pipeline
//! topology and buffer extraction strategy at runtime.

use super::memory::HardwarePlatform;

/// Auto-detect the hardware platform by probing available GStreamer elements
/// and system devices.
///
/// Detection order (first match wins):
/// 1. **Rockchip MPP**: probe `mppvideodec` element + `/dev/mpp_service`
/// 2. **NVIDIA Jetson**: probe `nvv4l2decoder` + Jetson SoC identifiers
/// 3. **NVIDIA Desktop**: probe `nvdec` element + no Jetson markers
/// 4. **VA-API**: probe `vaapidecodebin` or `vah264dec` + DRI render node
/// 5. **Generic**: fallback (CPU-only software pipeline)
pub fn detect_hardware_platform() -> HardwarePlatform {
    if is_rockchip_available() {
        return HardwarePlatform::Rockchip;
    }

    if is_nvidia_jetson_available() {
        return HardwarePlatform::NvidiaJetson;
    }

    if is_nvidia_desktop_available() {
        return HardwarePlatform::NvidiaDesktop;
    }

    if is_vaapi_available() {
        return HardwarePlatform::Vaapi;
    }

    HardwarePlatform::Generic
}

/// Check for Rockchip MPP hardware decoder availability.
///
/// Requires both the GStreamer `mppvideodec` element (from gst-mpp plugin)
/// and the `/dev/mpp_service` device node (created by the MPP kernel driver).
fn is_rockchip_available() -> bool {
    let has_element = gstreamer::ElementFactory::find("mppvideodec").is_some();
    let has_device = std::path::Path::new("/dev/mpp_service").exists();
    if has_element && has_device {
        tracing::info!("detected Rockchip MPP platform (mppvideodec + /dev/mpp_service)");
        return true;
    }
    false
}

/// Check for NVIDIA Jetson NVDEC availability.
///
/// Differentiates Jetson from desktop NVIDIA GPUs by checking for
/// Jetson-specific device nodes and SoC identifiers. Desktop GPUs
/// may also have `nvv4l2decoder` when L4T libraries are installed,
/// but lack the Tegra/Orin SoC markers.
fn is_nvidia_jetson_available() -> bool {
    let has_element = gstreamer::ElementFactory::find("nvv4l2decoder").is_some();
    if !has_element {
        return false;
    }

    let has_nvdec_device = std::path::Path::new("/dev/nvhost-nvdec").exists();

    let is_jetson_soc = std::fs::read_to_string("/proc/device-tree/compatible")
        .map(|s| s.contains("nvidia,tegra") || s.contains("nvidia,orin"))
        .unwrap_or(false);

    if has_nvdec_device || is_jetson_soc {
        let generation = JetsonGeneration::detect()
            .map(|g| format!("{g:?}"))
            .unwrap_or_else(|| "Unknown".to_string());
        tracing::info!(
            generation,
            "detected NVIDIA Jetson platform (nvv4l2decoder + SoC markers)"
        );
        return true;
    }

    false
}

/// Check for NVIDIA desktop GPU (non-Jetson) NVCODEC availability.
///
/// Desktop NVIDIA GPUs use `nvdec` / `nvh264dec` elements (from
/// gst-plugins-bad NVCODEC plugin) instead of Jetson's `nvv4l2decoder`.
fn is_nvidia_desktop_available() -> bool {
    let has_nvdec = gstreamer::ElementFactory::find("nvh264dec").is_some()
        || gstreamer::ElementFactory::find("nvdec").is_some();

    if has_nvdec {
        tracing::info!("detected NVIDIA desktop GPU platform (nvdec/nvh264dec)");
        return true;
    }
    false
}

/// Check for VA-API (Intel/AMD) hardware decoder availability.
///
/// Probes renderD128–renderD135 to support multi-GPU or non-standard DRI
/// device numbering (e.g. discrete GPU on renderD129).
fn is_vaapi_available() -> bool {
    let has_vaapi_dec = gstreamer::ElementFactory::find("vaapidecodebin").is_some();
    let has_va_dec = gstreamer::ElementFactory::find("vah264dec").is_some();
    let has_render_node =
        (128..=135).any(|n| std::path::Path::new(&format!("/dev/dri/renderD{n}")).exists());

    if (has_vaapi_dec || has_va_dec) && has_render_node {
        tracing::info!(
            "detected VA-API platform (vaapidecodebin={has_vaapi_dec}, \
             vah264dec={has_va_dec}, renderD=true)"
        );
        return true;
    }
    false
}

// ── Jetson generation detection ────────────────────────────────────

/// NVIDIA Jetson SoC generation.
///
/// Different generations have different NVMM buffer handling strategies
/// and memory architecture capabilities:
/// - **Orin**: Full unified memory — NVMM buffers can be accessed by both
///   CPU and GPU without explicit copies.
/// - **Xavier**: Improved NVMM + CUDA interop via NvBufSurface API.
/// - **Legacy** (Nano/TX2): Limited unified memory, may require explicit
///   copies for certain buffer access patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JetsonGeneration {
    /// Jetson Nano / TX2 — limited unified memory support.
    Legacy,
    /// Jetson Xavier (NX/AGX) — improved NVMM + CUDA interop.
    Xavier,
    /// Jetson Orin (Nano/NX/AGX) — full unified memory, best zero-copy.
    Orin,
}

impl JetsonGeneration {
    /// Detect Jetson generation from device tree model string.
    pub fn detect() -> Option<Self> {
        let model = std::fs::read_to_string("/proc/device-tree/model").ok()?;
        let model_lower = model.to_lowercase();
        if model_lower.contains("orin") {
            Some(Self::Orin)
        } else if model_lower.contains("xavier") {
            Some(Self::Xavier)
        } else if model_lower.contains("nano")
            || model_lower.contains("tx1")
            || model_lower.contains("tx2")
        {
            Some(Self::Legacy)
        } else {
            None
        }
    }

    /// Whether this generation supports true unified memory (CPU and GPU
    /// see the same physical memory without explicit copies).
    pub fn supports_unified_memory(&self) -> bool {
        matches!(self, Self::Orin)
    }
}

// ── Platform capabilities ──────────────────────────────────────────

/// Detailed capabilities for the detected platform.
#[derive(Debug, Clone)]
pub struct PlatformCapabilities {
    /// Detected hardware platform.
    pub platform: HardwarePlatform,
    /// Whether output frames may be DMA-buf backed.
    pub supports_dma_buf: bool,
    /// Whether hardware decoding is available.
    pub supports_hw_decode: bool,
    /// Whether hardware color space conversion is available.
    pub supports_hw_csc: bool,
    /// Whether hardware resize is available.
    pub supports_hw_resize: bool,
    /// Available hardware H.264 encoder for WebRTC (if any).
    pub hw_encoder: Option<String>,
    /// Available hardware JPEG encoder for annotation snapshots (if any).
    ///
    /// When present, the annotator can offload JPEG encoding to hardware,
    /// significantly reducing CPU overhead on embedded platforms.
    /// Typical elements: `vaapijpegenc` (Intel), `nvjpegenc` (Jetson),
    /// `mppjpegenc` (Rockchip).
    pub hw_jpeg_encoder: Option<String>,
    /// Jetson SoC generation (only set for `NvidiaJetson` platform).
    pub jetson_generation: Option<JetsonGeneration>,
}

impl PlatformCapabilities {
    /// Probe and build capabilities for the given platform.
    ///
    /// Each hardware element is verified by attempting `ElementFactory::make`
    /// rather than just `find`, ensuring the element can actually be instantiated
    /// (driver loaded, device accessible).
    pub fn probe(platform: HardwarePlatform) -> Self {
        match platform {
            HardwarePlatform::Rockchip => {
                let has_rga = can_instantiate("mppvideodec");
                Self {
                    platform,
                    supports_dma_buf: true,
                    supports_hw_decode: true,
                    supports_hw_csc: has_rga,
                    supports_hw_resize: has_rga,
                    hw_encoder: probe_encoder("mpph264enc"),
                    hw_jpeg_encoder: probe_encoder("mppjpegenc"),
                    jetson_generation: None,
                }
            }

            HardwarePlatform::NvidiaJetson => {
                let has_nvvidconv = can_instantiate("nvvidconv");
                Self {
                    platform,
                    supports_dma_buf: true,
                    supports_hw_decode: true,
                    supports_hw_csc: has_nvvidconv,
                    supports_hw_resize: has_nvvidconv,
                    hw_encoder: probe_encoder("nvv4l2h264enc"),
                    hw_jpeg_encoder: probe_encoder("nvjpegenc"),
                    jetson_generation: JetsonGeneration::detect(),
                }
            }

            HardwarePlatform::NvidiaDesktop => {
                let has_glcolorconvert = can_instantiate("glcolorconvert");
                Self {
                    platform,
                    supports_dma_buf: false,
                    supports_hw_decode: true,
                    supports_hw_csc: has_glcolorconvert,
                    supports_hw_resize: has_glcolorconvert,
                    hw_encoder: probe_encoder("nvh264enc"),
                    hw_jpeg_encoder: probe_encoder("nvjpegenc"),
                    jetson_generation: None,
                }
            }

            HardwarePlatform::Vaapi => {
                let has_postproc = can_instantiate("vaapipostproc");
                Self {
                    platform,
                    supports_dma_buf: true,
                    supports_hw_decode: true,
                    supports_hw_csc: has_postproc,
                    supports_hw_resize: has_postproc,
                    hw_encoder: probe_encoder("vaapih264enc"),
                    hw_jpeg_encoder: probe_encoder("vaapijpegenc"),
                    jetson_generation: None,
                }
            }

            HardwarePlatform::Generic => Self {
                platform,
                supports_dma_buf: false,
                supports_hw_decode: false,
                supports_hw_csc: false,
                supports_hw_resize: false,
                hw_encoder: None,
                hw_jpeg_encoder: None,
                jetson_generation: None,
            },
        }
    }
}

/// Try to instantiate a GStreamer element to confirm the plugin/driver works.
fn can_instantiate(name: &str) -> bool {
    gstreamer::ElementFactory::make(name).build().is_ok()
}

/// Verify an encoder element can be instantiated, returning its name if so.
fn probe_encoder(name: &str) -> Option<String> {
    if can_instantiate(name) {
        Some(name.to_string())
    } else {
        None
    }
}
