//! Camera protocol implementations.
//!
//! ONVIF client is retained for service discovery (profile enumeration,
//! RTSP URL resolution) and PTZ control. The actual video stream transport
//! is handled by the AI engine's internal GStreamer pipeline.

pub mod onvif;
