//! Video codec detection and GStreamer element mapping.
//!
//! Maps incoming video stream codec identifiers (from GStreamer caps or RTP
//! `encoding-name`) to the corresponding RTP depayloader and parser elements.
//! Used by [`PipelineBuilder`](super::pipeline::PipelineBuilder) for automatic
//! multi-codec support.

/// Detected video codec of an incoming stream.
///
/// Derived from GStreamer caps structure names (e.g. `video/x-h264`) or
/// RTP encoding names (e.g. `H264`). Used to select the correct depayloader
/// and parser elements in the programmatic pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    /// ITU-T H.264 / AVC.
    H264,
    /// ITU-T H.265 / HEVC.
    H265,
    /// Google VP8.
    Vp8,
    /// Google VP9.
    Vp9,
    /// AV1 (AOMedia Video 1).
    Av1,
    /// Motion JPEG.
    Mjpeg,
    /// Unrecognized or unsupported codec.
    Unknown,
}

impl VideoCodec {
    /// Detect codec from a GStreamer caps structure name.
    ///
    /// GStreamer uses names like `video/x-h264`, `video/x-h265`, `image/jpeg`.
    pub fn from_caps_name(name: &str) -> Self {
        match name {
            "video/x-h264" => Self::H264,
            "video/x-h265" => Self::H265,
            "video/x-vp8" => Self::Vp8,
            "video/x-vp9" => Self::Vp9,
            "video/x-av1" => Self::Av1,
            "image/jpeg" => Self::Mjpeg,
            _ => Self::Unknown,
        }
    }

    /// Detect codec from an RTP `encoding-name` field.
    ///
    /// The `encoding-name` in `application/x-rtp` caps identifies the payload
    /// codec (e.g. `H264`, `H265`, `VP8`).
    pub fn from_rtp_encoding_name(name: &str) -> Self {
        match name.to_uppercase().as_str() {
            "H264" => Self::H264,
            "H265" => Self::H265,
            "VP8" => Self::Vp8,
            "VP9" => Self::Vp9,
            "AV1" => Self::Av1,
            "JPEG" => Self::Mjpeg,
            _ => Self::Unknown,
        }
    }

    /// GStreamer RTP depayloader element name for this codec.
    ///
    /// Returns `None` for `Unknown` — the caller should fall back to
    /// `decodebin3` which handles depayloading internally.
    pub fn rtp_depay_element(&self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("rtph264depay"),
            Self::H265 => Some("rtph265depay"),
            Self::Vp8 => Some("rtpvp8depay"),
            Self::Vp9 => Some("rtpvp9depay"),
            Self::Av1 => Some("rtpav1depay"),
            Self::Mjpeg => Some("rtpjpegdepay"),
            Self::Unknown => None,
        }
    }

    /// GStreamer parser element name for this codec.
    ///
    /// Not all codecs require an explicit parser (VP8/MJPEG do not).
    pub fn parser_element(&self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("h264parse"),
            Self::H265 => Some("h265parse"),
            Self::Vp9 => Some("vp9parse"),
            Self::Av1 => Some("av1parse"),
            _ => None,
        }
    }

    /// GStreamer caps structure name for this codec.
    pub fn caps_name(&self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("video/x-h264"),
            Self::H265 => Some("video/x-h265"),
            Self::Vp8 => Some("video/x-vp8"),
            Self::Vp9 => Some("video/x-vp9"),
            Self::Av1 => Some("video/x-av1"),
            Self::Mjpeg => Some("image/jpeg"),
            Self::Unknown => None,
        }
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::H264 => "H.264/AVC",
            Self::H265 => "H.265/HEVC",
            Self::Vp8 => "VP8",
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
            Self::Mjpeg => "MJPEG",
            Self::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_caps_name_known_codecs() {
        assert_eq!(VideoCodec::from_caps_name("video/x-h264"), VideoCodec::H264);
        assert_eq!(VideoCodec::from_caps_name("video/x-h265"), VideoCodec::H265);
        assert_eq!(VideoCodec::from_caps_name("video/x-vp8"), VideoCodec::Vp8);
        assert_eq!(VideoCodec::from_caps_name("video/x-vp9"), VideoCodec::Vp9);
        assert_eq!(VideoCodec::from_caps_name("video/x-av1"), VideoCodec::Av1);
        assert_eq!(VideoCodec::from_caps_name("image/jpeg"), VideoCodec::Mjpeg);
    }

    #[test]
    fn from_caps_name_unknown() {
        assert_eq!(
            VideoCodec::from_caps_name("audio/x-raw"),
            VideoCodec::Unknown
        );
    }

    #[test]
    fn from_rtp_encoding_case_insensitive() {
        assert_eq!(VideoCodec::from_rtp_encoding_name("h264"), VideoCodec::H264);
        assert_eq!(VideoCodec::from_rtp_encoding_name("H265"), VideoCodec::H265);
        assert_eq!(VideoCodec::from_rtp_encoding_name("vp9"), VideoCodec::Vp9);
    }

    #[test]
    fn rtp_depay_elements_exist_for_known_codecs() {
        assert_eq!(VideoCodec::H264.rtp_depay_element(), Some("rtph264depay"));
        assert_eq!(VideoCodec::H265.rtp_depay_element(), Some("rtph265depay"));
        assert_eq!(VideoCodec::Av1.rtp_depay_element(), Some("rtpav1depay"));
        assert_eq!(VideoCodec::Unknown.rtp_depay_element(), None);
    }

    #[test]
    fn parser_elements_only_for_applicable_codecs() {
        assert_eq!(VideoCodec::H264.parser_element(), Some("h264parse"));
        assert_eq!(VideoCodec::H265.parser_element(), Some("h265parse"));
        assert_eq!(VideoCodec::Vp8.parser_element(), None);
        assert_eq!(VideoCodec::Mjpeg.parser_element(), None);
    }

    #[test]
    fn display_names() {
        assert_eq!(format!("{}", VideoCodec::H264), "H.264/AVC");
        assert_eq!(format!("{}", VideoCodec::H265), "H.265/HEVC");
    }
}
