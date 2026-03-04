//! ONVIF protocol client for camera device discovery, media profile
//! management, and RTSP stream URI retrieval.
//!
//! Implements a lightweight ONVIF SOAP client using `reqwest` for HTTP
//! transport and `quick-xml` for namespace-aware XML parsing, with
//! WS-Security UsernameToken authentication (SHA-1 password digest
//! per the ONVIF Core Specification).
//!
//! # Design Rationale
//!
//! Full ONVIF crates like `onvif-rs` pull in heavy WSDL code generators
//! and `yaserde`; for a gateway that only needs GetCapabilities,
//! GetProfiles, GetStreamUri, and PTZ, a lean SOAP client with proper
//! `quick-xml` event-based parsing is both lighter and easier to audit.
//!
//! # ONVIF Services Used
//!
//! - **Device Service**: GetCapabilities, GetDeviceInformation
//! - **Media Service**: GetProfiles, GetStreamUri
//! - **PTZ Service**: ContinuousMove, Stop, GotoPreset, AbsoluteMove
//!   (exposed via [`crate::ptz::PtzController`])
//!
//! # Authentication
//!
//! ONVIF uses WS-Security UsernameToken with password digest:
//! `PasswordDigest = Base64(SHA-1(nonce + created + password))`

use base64::Engine as _;
use chrono::Utc;
use ng_gateway_sdk::DriverError;
use quick_xml::events::Event;
use quick_xml::Reader;
use rand::RngExt;
use sha1::{Digest, Sha1};
use std::time::Duration;

/// Base64 standard engine (RFC 4648 §4, with padding).
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// ONVIF client for a single camera device.
///
/// Holds the resolved service endpoint URLs and authentication credentials.
/// Created once during connection establishment and shared for PTZ control.
#[derive(Debug, Clone)]
pub struct OnvifClient {
    /// HTTP client (connection pooled, reusable).
    http: reqwest::Client,
    /// Device service endpoint URL.
    device_url: url::Url,
    /// Media service endpoint URL (resolved from GetCapabilities).
    media_url: Option<url::Url>,
    /// PTZ service endpoint URL (resolved from GetCapabilities).
    ptz_url: Option<url::Url>,
    /// Authentication credentials.
    credentials: Option<OnvifCredentials>,
}

#[derive(Debug, Clone)]
pub(crate) struct OnvifCredentials {
    username: String,
    password: String,
}

/// A resolved ONVIF media profile.
#[derive(Debug, Clone)]
pub struct OnvifProfile {
    /// Profile token (used to reference this profile in API calls).
    pub token: String,
    /// Human-readable profile name.
    pub name: String,
}

/// Result of ONVIF stream URI resolution.
#[derive(Debug, Clone)]
pub struct OnvifStreamUri {
    /// RTSP stream URL.
    pub uri: String,
}

impl OnvifClient {
    /// Create a new ONVIF client and resolve service endpoints.
    ///
    /// # Steps
    /// 1. Build HTTP client with appropriate timeouts
    /// 2. Call GetCapabilities to discover Media and PTZ service URLs
    pub async fn connect(
        endpoint: &url::Url,
        username: Option<&str>,
        password: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, DriverError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| DriverError::SessionError(format!("Failed to create HTTP client: {e}")))?;

        let credentials = match (username, password) {
            (Some(u), Some(p)) if !u.is_empty() => Some(OnvifCredentials {
                username: u.to_string(),
                password: p.to_string(),
            }),
            _ => None,
        };

        let mut client = Self {
            http,
            device_url: endpoint.clone(),
            media_url: None,
            ptz_url: None,
            credentials,
        };

        client.discover_services().await?;

        tracing::info!(
            endpoint = %endpoint,
            media = ?client.media_url.as_ref().map(|u| u.as_str()),
            ptz = ?client.ptz_url.as_ref().map(|u| u.as_str()),
            "ONVIF services discovered"
        );

        Ok(client)
    }

    /// Discover Media and PTZ service endpoints via GetCapabilities.
    async fn discover_services(&mut self) -> Result<(), DriverError> {
        let body = soap_envelope(
            &self.credentials,
            r#"<GetCapabilities xmlns="http://www.onvif.org/ver10/device/wsdl">
                <Category>All</Category>
            </GetCapabilities>"#,
        );

        let response = self.soap_request(&self.device_url.clone(), &body).await?;

        if let Some(media_url) = find_element_text(&response, b"XAddr", b"Media") {
            self.media_url = url::Url::parse(&media_url).ok();
        }
        if let Some(ptz_url) = find_element_text(&response, b"XAddr", b"PTZ") {
            self.ptz_url = url::Url::parse(&ptz_url).ok();
        }

        if self.media_url.is_none() {
            self.media_url = Some(self.device_url.clone());
        }

        Ok(())
    }

    /// Get available media profiles from the ONVIF device.
    pub async fn get_profiles(&self) -> Result<Vec<OnvifProfile>, DriverError> {
        let media_url = self.media_url.as_ref().ok_or(DriverError::SessionError(
            "ONVIF Media service URL not available".into(),
        ))?;

        let body = soap_envelope(
            &self.credentials,
            r#"<GetProfiles xmlns="http://www.onvif.org/ver10/media/wsdl"/>"#,
        );

        let response = self.soap_request(media_url, &body).await?;
        let profiles = parse_profiles(&response);

        if profiles.is_empty() {
            return Err(DriverError::SessionError(
                "No ONVIF media profiles found on device".into(),
            ));
        }

        tracing::debug!(count = profiles.len(), "ONVIF profiles discovered");
        Ok(profiles)
    }

    /// Get the RTSP stream URI for a specific media profile.
    pub async fn get_stream_uri(&self, profile_token: &str) -> Result<OnvifStreamUri, DriverError> {
        let media_url = self.media_url.as_ref().ok_or(DriverError::SessionError(
            "ONVIF Media service URL not available".into(),
        ))?;

        let body = soap_envelope(
            &self.credentials,
            &format!(
                r#"<GetStreamUri xmlns="http://www.onvif.org/ver10/media/wsdl">
                    <StreamSetup>
                        <Stream xmlns="http://www.onvif.org/ver10/schema">RTP-Unicast</Stream>
                        <Transport xmlns="http://www.onvif.org/ver10/schema">
                            <Protocol>RTSP</Protocol>
                        </Transport>
                    </StreamSetup>
                    <ProfileToken>{profile_token}</ProfileToken>
                </GetStreamUri>"#
            ),
        );

        let response = self.soap_request(media_url, &body).await?;

        let uri = find_element_text(&response, b"Uri", b"").ok_or(DriverError::SessionError(
            "ONVIF GetStreamUri did not return a URI".into(),
        ))?;

        Ok(OnvifStreamUri { uri })
    }

    /// Get the PTZ service URL (if available).
    pub fn ptz_url(&self) -> Option<&url::Url> {
        self.ptz_url.as_ref()
    }

    /// Get a reference to the HTTP client for PTZ requests.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    /// Get credentials for PTZ SOAP requests.
    #[allow(dead_code)]
    pub(crate) fn credentials(&self) -> &Option<OnvifCredentials> {
        &self.credentials
    }

    /// Send a raw PTZ SOAP command.
    ///
    /// This is used by [`crate::ptz::PtzController`] to execute PTZ commands
    /// against the resolved PTZ service endpoint.
    pub async fn send_ptz_command(&self, soap_body: &str) -> Result<String, DriverError> {
        let ptz_url = self.ptz_url.as_ref().ok_or(DriverError::ExecutionError(
            "ONVIF PTZ service not available on this device".into(),
        ))?;

        let body = soap_envelope(&self.credentials, soap_body);
        self.soap_request(ptz_url, &body).await
    }

    /// Execute a SOAP HTTP request and return the response body.
    async fn soap_request(&self, url: &url::Url, body: &str) -> Result<String, DriverError> {
        let response = self
            .http
            .post(url.as_str())
            .header("Content-Type", "application/soap+xml; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| {
                DriverError::SessionError(format!("ONVIF SOAP request to {url} failed: {e}"))
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|e| {
            DriverError::SessionError(format!("Failed to read ONVIF response: {e}"))
        })?;

        if !status.is_success() {
            let fault = parse_soap_fault(&text).unwrap_or(text.clone());
            return Err(DriverError::SessionError(format!(
                "ONVIF SOAP error (HTTP {status}): {fault}"
            )));
        }

        Ok(text)
    }
}

// ─── SOAP XML construction ─────────────────────────────────────────

/// Build a SOAP envelope with optional WS-Security UsernameToken.
///
/// ONVIF uses WS-Security with password digest authentication:
/// `PasswordDigest = Base64(SHA-1(nonce + created + password))`
pub(crate) fn soap_envelope(credentials: &Option<OnvifCredentials>, body: &str) -> String {
    let security_header = match credentials {
        Some(creds) => build_ws_security_header(&creds.username, &creds.password),
        None => String::new(),
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tds="http://www.onvif.org/ver10/device/wsdl"
            xmlns:tt="http://www.onvif.org/ver10/schema"
            xmlns:trt="http://www.onvif.org/ver10/media/wsdl"
            xmlns:tptz="http://www.onvif.org/ver20/ptz/wsdl">
    <s:Header>{security_header}</s:Header>
    <s:Body>{body}</s:Body>
</s:Envelope>"#
    )
}

/// Build WS-Security UsernameToken header with SHA-1 password digest.
///
/// Per the ONVIF Core Specification and OASIS WS-Security UsernameToken
/// Profile 1.1:
///
/// ```text
/// PasswordDigest = Base64(SHA-1(Nonce_raw + Created + Password))
/// ```
///
/// Where `Nonce_raw` is 16 random bytes (sent base64-encoded in the header),
/// `Created` is a UTC ISO-8601 timestamp, and `Password` is the raw
/// credential string.
fn build_ws_security_header(username: &str, password: &str) -> String {
    let mut nonce_bytes = [0u8; 16];
    rand::rng().fill(&mut nonce_bytes);
    let nonce_b64 = B64.encode(nonce_bytes);

    let created = Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();

    let mut hasher = Sha1::new();
    hasher.update(nonce_bytes);
    hasher.update(created.as_bytes());
    hasher.update(password.as_bytes());
    let digest_b64 = B64.encode(hasher.finalize());

    format!(
        r#"<Security xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd" s:mustUnderstand="true">
        <UsernameToken>
            <Username>{username}</Username>
            <Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{digest_b64}</Password>
            <Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{nonce_b64}</Nonce>
            <Created xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">{created}</Created>
        </UsernameToken>
    </Security>"#
    )
}

// ─── XML response parsing (quick-xml) ──────────────────────────────
//
// ONVIF SOAP responses use varying namespace prefixes across camera
// vendors (e.g. `tt:`, `trt:`, `tds:`, or none at all). All matching
// is done exclusively on *local names* via `quick_xml::Reader`, which
// is both more correct and more robust than raw string scanning.

/// Find the text content of the first element whose local name matches
/// `target`, optionally scoped within a parent element whose local name
/// matches `scope`.
///
/// When `scope` is non-empty, only the first occurrence of the scope
/// element is searched. Returns `None` if not found or empty.
fn find_element_text(xml: &str, target: &[u8], scope: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let scoped = !scope.is_empty();
    let mut in_scope = !scoped;
    let mut depth: u32 = 0;
    let mut hit = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                let ln = local.as_ref();

                if scoped && !in_scope {
                    if ln == scope {
                        in_scope = true;
                        depth = 1;
                    }
                    continue;
                }

                if scoped {
                    depth += 1;
                }
                hit = ln == target;
            }
            Ok(Event::Text(ref e)) if hit => {
                hit = false;
                if let Ok(cow) = e.decode() {
                    let trimmed = cow.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
            Ok(Event::End(_)) => {
                hit = false;
                if in_scope && scoped {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return None;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {
                hit = false;
            }
        }
    }

    None
}

/// Extract ONVIF media profiles from a GetProfiles response.
///
/// Parses `<Profiles token="…">` elements and extracts each profile's
/// `token` attribute and `<Name>` child text. Uses only local name
/// matching, so it works regardless of the namespace prefix used by
/// the camera vendor.
fn parse_profiles(xml: &str) -> Vec<OnvifProfile> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut profiles = Vec::new();
    // (token, optional name) for the profile currently being parsed.
    let mut current: Option<(String, Option<String>)> = None;
    let mut depth: u32 = 0;
    let mut want_name = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                let ln = local.as_ref();

                if ln == b"Profiles" && current.is_none() {
                    let token = e
                        .attributes()
                        .filter_map(Result::ok)
                        .find(|a| a.key.local_name().as_ref() == b"token")
                        .and_then(|a| String::from_utf8(a.value.to_vec()).ok());

                    if let Some(t) = token {
                        current = Some((t, None));
                        depth = 1;
                    }
                    continue;
                }

                if current.is_some() {
                    depth += 1;
                    want_name = ln == b"Name" && current.as_ref().is_some_and(|(_, n)| n.is_none());
                }
            }
            Ok(Event::Text(ref e)) if want_name => {
                want_name = false;
                if let Ok(cow) = e.decode() {
                    let trimmed = cow.trim();
                    if !trimmed.is_empty() {
                        if let Some((_, ref mut name)) = current {
                            *name = Some(trimmed.to_string());
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                want_name = false;
                if current.is_some() {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        if let Some((token, name)) = current.take() {
                            profiles.push(OnvifProfile {
                                name: name.unwrap_or(token.clone()),
                                token,
                            });
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {
                want_name = false;
            }
        }
    }

    profiles
}

/// Extract a SOAP fault message from an error response.
///
/// Tries `<faultstring>` first (SOAP 1.1), then `<Text>` within
/// `<Reason>` (SOAP 1.2).
fn parse_soap_fault(xml: &str) -> Option<String> {
    find_element_text(xml, b"faultstring", b"").or(find_element_text(xml, b"Text", b"Reason"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_element_text_global() {
        let xml = r#"<root><tt:Uri>rtsp://cam/stream</tt:Uri></root>"#;
        assert_eq!(
            find_element_text(xml, b"Uri", b""),
            Some("rtsp://cam/stream".into())
        );
    }

    #[test]
    fn test_find_element_text_scoped() {
        let xml = r#"
            <Capabilities>
                <tt:Media><tt:XAddr>http://cam/media</tt:XAddr></tt:Media>
                <tt:PTZ><tt:XAddr>http://cam/ptz</tt:XAddr></tt:PTZ>
            </Capabilities>"#;
        assert_eq!(
            find_element_text(xml, b"XAddr", b"Media"),
            Some("http://cam/media".into())
        );
        assert_eq!(
            find_element_text(xml, b"XAddr", b"PTZ"),
            Some("http://cam/ptz".into())
        );
    }

    #[test]
    fn test_find_element_text_missing_returns_none() {
        let xml = r#"<root><Foo>bar</Foo></root>"#;
        assert_eq!(find_element_text(xml, b"Missing", b""), None);
    }

    #[test]
    fn test_find_element_text_scope_without_target() {
        let xml = r#"<Media><Foo>bar</Foo></Media>"#;
        assert_eq!(find_element_text(xml, b"XAddr", b"Media"), None);
    }

    #[test]
    fn test_find_element_text_no_prefix() {
        let xml = r#"<Media><XAddr>http://no-prefix</XAddr></Media>"#;
        assert_eq!(
            find_element_text(xml, b"XAddr", b"Media"),
            Some("http://no-prefix".into())
        );
    }

    #[test]
    fn test_parse_profiles_basic() {
        let xml = r#"
            <GetProfilesResponse>
                <trt:Profiles token="tok1" fixed="true">
                    <tt:Name>MainStream</tt:Name>
                </trt:Profiles>
                <trt:Profiles token="tok2">
                    <tt:Name>SubStream</tt:Name>
                </trt:Profiles>
            </GetProfilesResponse>"#;
        let profiles = parse_profiles(xml);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].token, "tok1");
        assert_eq!(profiles[0].name, "MainStream");
        assert_eq!(profiles[1].token, "tok2");
        assert_eq!(profiles[1].name, "SubStream");
    }

    #[test]
    fn test_parse_profiles_no_name_uses_token() {
        let xml = r#"<Profiles token="only_token"><VideoEncoderConfiguration/></Profiles>"#;
        let profiles = parse_profiles(xml);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].token, "only_token");
        assert_eq!(profiles[0].name, "only_token");
    }

    #[test]
    fn test_parse_profiles_no_prefix() {
        let xml = r#"<Profiles token="t1"><Name>Profile 1</Name></Profiles>"#;
        let profiles = parse_profiles(xml);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Profile 1");
    }

    #[test]
    fn test_parse_soap_fault_soap11() {
        let xml = r#"<Fault><faultstring>Not Authorized</faultstring></Fault>"#;
        assert_eq!(parse_soap_fault(xml), Some("Not Authorized".into()));
    }

    #[test]
    fn test_parse_soap_fault_soap12() {
        let xml = r#"<Fault><Reason><Text>Action not supported</Text></Reason></Fault>"#;
        assert_eq!(parse_soap_fault(xml), Some("Action not supported".into()));
    }

    #[test]
    fn test_ws_security_header_format() {
        let header = build_ws_security_header("admin", "password123");
        assert!(header.contains("<Username>admin</Username>"));
        assert!(header.contains("PasswordDigest"));
        assert!(header.contains("Base64Binary"));
        assert!(header.contains("<Created"));
    }
}
