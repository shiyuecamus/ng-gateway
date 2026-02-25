//! ONVIF PTZ (Pan-Tilt-Zoom) controller.
//!
//! Provides camera movement control through ONVIF PTZ SOAP commands.
//! The controller is created during ONVIF session establishment and
//! shared with the [`CameraHandle`] for action execution.
//!
//! # Supported Commands
//!
//! | Command | ONVIF Operation | Description |
//! |---------|----------------|-------------|
//! | `ptz_move` | ContinuousMove | Start continuous PTZ movement |
//! | `ptz_stop` | Stop | Stop all PTZ movement |
//! | `ptz_preset` | GotoPreset | Move to a saved preset position |
//! | `ptz_absolute` | AbsoluteMove | Move to absolute coordinates |

use crate::protocol::onvif::OnvifClient;
use ng_gateway_sdk::DriverError;
use std::sync::Arc;

/// PTZ controller backed by ONVIF PTZ service.
///
/// Wraps an [`OnvifClient`] and provides high-level PTZ command methods.
/// Thread-safe via `Arc` sharing between handle and session.
#[derive(Clone)]
pub struct PtzController {
    /// ONVIF client (shared, connection-pooled).
    client: Arc<OnvifClient>,
    /// Active media profile token for PTZ operations.
    profile_token: String,
}

/// PTZ movement velocity parameters.
///
/// All values are normalized to `[-1.0, 1.0]`.
#[derive(Debug, Clone, Copy)]
pub struct PtzVelocity {
    /// Pan velocity (-1.0 = full left, 1.0 = full right).
    pub pan: f32,
    /// Tilt velocity (-1.0 = full down, 1.0 = full up).
    pub tilt: f32,
    /// Zoom velocity (-1.0 = zoom out, 1.0 = zoom in).
    pub zoom: f32,
}

/// PTZ absolute position parameters.
///
/// Pan and tilt are normalized to `[-1.0, 1.0]`, zoom to `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy)]
pub struct PtzPosition {
    /// Pan position (-1.0 = full left, 1.0 = full right).
    pub pan: f32,
    /// Tilt position (-1.0 = full down, 1.0 = full up).
    pub tilt: f32,
    /// Zoom level (0.0 = wide, 1.0 = tele).
    pub zoom: f32,
}

impl PtzController {
    /// Create a new PTZ controller from an established ONVIF connection.
    pub fn new(client: Arc<OnvifClient>, profile_token: String) -> Self {
        Self {
            client,
            profile_token,
        }
    }

    /// Start continuous PTZ movement.
    ///
    /// The camera will continue moving at the specified velocity until
    /// [`stop`] is called or the movement times out (camera-dependent).
    pub async fn continuous_move(
        &self,
        velocity: PtzVelocity,
        timeout_secs: Option<f32>,
    ) -> Result<(), DriverError> {
        let timeout_attr = timeout_secs
            .map(|t| format!(r#" Timeout="PT{t:.1}S""#))
            .unwrap_or_default();

        let body = format!(
            r#"<ContinuousMove xmlns="http://www.onvif.org/ver20/ptz/wsdl">
                <ProfileToken>{}</ProfileToken>
                <Velocity>
                    <PanTilt xmlns="http://www.onvif.org/ver10/schema"
                             x="{:.4}" y="{:.4}"/>
                    <Zoom xmlns="http://www.onvif.org/ver10/schema"
                          x="{:.4}"/>
                </Velocity>{timeout_attr}
            </ContinuousMove>"#,
            self.profile_token, velocity.pan, velocity.tilt, velocity.zoom,
        );

        self.client.send_ptz_command(&body).await?;
        tracing::debug!(
            pan = velocity.pan,
            tilt = velocity.tilt,
            zoom = velocity.zoom,
            "PTZ continuous move started"
        );
        Ok(())
    }

    /// Stop all PTZ movement.
    pub async fn stop(&self, stop_pan_tilt: bool, stop_zoom: bool) -> Result<(), DriverError> {
        let body = format!(
            r#"<Stop xmlns="http://www.onvif.org/ver20/ptz/wsdl">
                <ProfileToken>{}</ProfileToken>
                <PanTilt>{stop_pan_tilt}</PanTilt>
                <Zoom>{stop_zoom}</Zoom>
            </Stop>"#,
            self.profile_token,
        );

        self.client.send_ptz_command(&body).await?;
        tracing::debug!("PTZ stopped");
        Ok(())
    }

    /// Move to a saved preset position.
    pub async fn goto_preset(
        &self,
        preset_token: &str,
        speed: Option<PtzVelocity>,
    ) -> Result<(), DriverError> {
        let speed_xml = match speed {
            Some(v) => format!(
                r#"<Speed>
                    <PanTilt xmlns="http://www.onvif.org/ver10/schema"
                             x="{:.4}" y="{:.4}"/>
                    <Zoom xmlns="http://www.onvif.org/ver10/schema"
                          x="{:.4}"/>
                </Speed>"#,
                v.pan.abs(),
                v.tilt.abs(),
                v.zoom.abs(),
            ),
            None => String::new(),
        };

        let body = format!(
            r#"<GotoPreset xmlns="http://www.onvif.org/ver20/ptz/wsdl">
                <ProfileToken>{}</ProfileToken>
                <PresetToken>{preset_token}</PresetToken>
                {speed_xml}
            </GotoPreset>"#,
            self.profile_token,
        );

        self.client.send_ptz_command(&body).await?;
        tracing::debug!(preset = preset_token, "PTZ goto preset");
        Ok(())
    }

    /// Move to an absolute position.
    pub async fn absolute_move(
        &self,
        position: PtzPosition,
        speed: Option<PtzVelocity>,
    ) -> Result<(), DriverError> {
        let speed_xml = match speed {
            Some(v) => format!(
                r#"<Speed>
                    <PanTilt xmlns="http://www.onvif.org/ver10/schema"
                             x="{:.4}" y="{:.4}"/>
                    <Zoom xmlns="http://www.onvif.org/ver10/schema"
                          x="{:.4}"/>
                </Speed>"#,
                v.pan.abs(),
                v.tilt.abs(),
                v.zoom.abs(),
            ),
            None => String::new(),
        };

        let body = format!(
            r#"<AbsoluteMove xmlns="http://www.onvif.org/ver20/ptz/wsdl">
                <ProfileToken>{}</ProfileToken>
                <Position>
                    <PanTilt xmlns="http://www.onvif.org/ver10/schema"
                             x="{:.4}" y="{:.4}"/>
                    <Zoom xmlns="http://www.onvif.org/ver10/schema"
                          x="{:.4}"/>
                </Position>
                {speed_xml}
            </AbsoluteMove>"#,
            self.profile_token, position.pan, position.tilt, position.zoom,
        );

        self.client.send_ptz_command(&body).await?;
        tracing::debug!(
            pan = position.pan,
            tilt = position.tilt,
            zoom = position.zoom,
            "PTZ absolute move"
        );
        Ok(())
    }
}

/// Parse PTZ velocity parameters from action input values.
///
/// Expected parameter keys: `pan`, `tilt`, `zoom` (all f64, [-1.0, 1.0]).
pub fn parse_ptz_velocity(
    params: &[(String, serde_json::Value)],
) -> Result<PtzVelocity, DriverError> {
    let pan = extract_f32_param(params, "pan").unwrap_or(0.0);
    let tilt = extract_f32_param(params, "tilt").unwrap_or(0.0);
    let zoom = extract_f32_param(params, "zoom").unwrap_or(0.0);

    Ok(PtzVelocity {
        pan: pan.clamp(-1.0, 1.0),
        tilt: tilt.clamp(-1.0, 1.0),
        zoom: zoom.clamp(-1.0, 1.0),
    })
}

/// Parse preset token from action parameters.
pub fn parse_preset_token(params: &[(String, serde_json::Value)]) -> Result<String, DriverError> {
    params
        .iter()
        .find(|(k, _)| k == "preset_token" || k == "presetToken")
        .and_then(|(_, v)| v.as_str().map(|s| s.to_string()))
        .ok_or(DriverError::ExecutionError(
            "PTZ preset requires 'preset_token' parameter".into(),
        ))
}

/// Extract an f32 parameter from a key-value list.
fn extract_f32_param(params: &[(String, serde_json::Value)], key: &str) -> Option<f32> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_f64())
        .map(|v| v as f32)
}
