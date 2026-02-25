//! # Camera Southward Driver
//!
//! AI-enabled camera southward driver supporting RTSP/ONVIF/MJPEG protocols.
//!
//! This crate follows the standard ng-gateway southward driver pattern
//! (Connector → Session → SouthwardHandle) and integrates with the
//! `ng-gateway-ai` processing engine for real-time video analysis.
//!
//! ## Architecture
//!
//! ```text
//! CameraConnector
//!   ├── new(ctx) → parse config, extract AI engine handle
//!   └── connect(ctx) → establish RTSP/ONVIF/MJPEG stream
//!         └── CameraSession
//!               ├── init() → start frame loop in background task
//!               └── run()  → monitor for stream errors / cancellation
//!                     └── CameraHandle (data-plane)
//!                           ├── frame_loop → pull frames → AI engine → cache result
//!                           └── collect_data() → read latest cached result
//! ```
//!
//! ## Feature Status
//!
//! | Protocol | Status |
//! |----------|--------|
//! | RTSP     | Phase 1 (retina-based) |
//! | ONVIF    | Phase 2 (planned) |
//! | MJPEG    | Phase 2 (planned) |

mod connector;
mod converter;
mod handle;
mod metadata;
pub mod protocol;
pub mod ptz;
mod session;
pub mod types;

use connector::CameraConnector;
use converter::CameraModelConverter;
use metadata::build_camera_schemas;

ng_gateway_sdk::ng_driver_factory!(
    name = "Camera",
    description = "AI-enabled camera driver supporting RTSP/ONVIF/MJPEG protocols",
    driver_type = "camera",
    component = CameraConnector,
    metadata_fn = build_camera_schemas,
    model_convert = CameraModelConverter
);
