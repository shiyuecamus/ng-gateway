use crate::{
    protocol::{
        codec::Dl645FrameCodec,
        error::{Dl645ExceptionCode, ProtocolError},
        frame::{
            build_broadcast_time_sync_frame, build_clear_events_frame,
            build_clear_max_demand_frame, build_clear_meter_frame, build_freeze_frame,
            build_modify_password_frame, build_read_data_frame, build_update_baud_rate_frame,
            build_write_address_frame, build_write_data_frame, Dl645Address, Dl645Body,
            Dl645CodecContext, Dl645Direction, Dl645TypedFrame,
        },
    },
    types::Dl645Version,
};
use async_trait::async_trait;
use bytes::BytesMut;
use chrono::{Datelike, TimeZone, Timelike, Utc};
use futures::{SinkExt, StreamExt};
use ng_gateway_sdk::WireEncode;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

/// Shared timing and size configuration for DL/T 645 sessions.
///
/// This configuration is derived from `Dl645Channel` at runtime and passed to
/// concrete transport implementations (serial or TCP).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Preamble bytes sent before each DL/T645 frame to wake devices.
    ///
    /// An empty vector disables preamble sending.
    pub wakeup_preamble: Vec<u8>,
    /// Protocol version for encoding context.
    pub version: Dl645Version,
}

impl SessionConfig {
    /// Build a new configuration from primitive settings.
    pub fn new(wakeup_preamble: Vec<u8>, version: Dl645Version) -> Self {
        Self {
            wakeup_preamble,
            version,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteDataParams {
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub di: u32,
    pub value_bytes: Vec<u8>,
    pub password: u32,
    pub operator_code: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ReadDataParams {
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub di: u32,
}

#[derive(Debug, Clone)]
pub struct WriteAddressParams {
    pub version: Dl645Version,
    pub current_address: Dl645Address,
    pub new_address: Dl645Address,
}

#[derive(Debug, Clone)]
pub struct BroadcastTimeSyncParams {
    pub version: Dl645Version,
    pub timestamp_bcd: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct FreezeParams {
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub pattern_bcd: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct UpdateBaudRateParams {
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub code: u8,
}

#[derive(Debug, Clone)]
pub struct ModifyPasswordParams {
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub di: Option<u32>,
    pub old_password: u32,
    pub new_password: u32,
}

#[derive(Debug, Clone)]
pub struct ClearMaxDemandParams {
    /// DL/T 645 protocol version (1997/2007/2021).
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub password: u32,
    pub operator_code: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ClearMeterParams {
    /// DL/T 645 protocol version (1997/2007/2021).
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub password: u32,
    pub operator_code: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ClearEventsParams {
    /// DL/T 645 protocol version (1997/2007/2021).
    pub version: Dl645Version,
    pub address: Dl645Address,
    pub di: u32,
    pub password: u32,
    pub operator_code: Option<u32>,
}

impl WriteAddressParams {
    /// Build write‑address parameters from the current BCD address and a new
    /// human‑readable address string.
    ///
    /// This helper keeps the DL/T 645 address layout and BCD encoding logic
    /// inside the protocol layer so that higher‑level callers do not need to
    /// re‑implement address parsing.
    pub fn from_new_address(
        version: Dl645Version,
        current_address: Dl645Address,
        new_address: Dl645Address,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            version,
            current_address,
            new_address,
        })
    }
}

impl BroadcastTimeSyncParams {
    /// Build broadcast time‑synchronization parameters from a Unix timestamp
    /// expressed in whole seconds.
    ///
    /// The DL/T 645 standard expects a BCD‑encoded tuple of
    /// `[second, minute, hour, day, month, year]` where year is stored as the
    /// lower two digits. This helper encapsulates that layout and the BCD
    /// encoding rules so that callers can work with plain timestamps.
    pub fn from_unix_secs(version: Dl645Version, ts_secs: i64) -> Result<Self, ProtocolError> {
        if ts_secs < 0 {
            return Err(ProtocolError::InvalidFrame(
                "DL/T 645 time sync value must be non‑negative Unix timestamp".to_string(),
            ));
        }

        let dt = Utc
            .timestamp_opt(ts_secs, 0)
            .single()
            .ok_or(ProtocolError::InvalidFrame(
                "DL/T 645 time sync timestamp is invalid or out of range".to_string(),
            ))?;

        let year = (dt.year() % 100) as u32;
        let month = dt.month();
        let day = dt.day();
        let hour = dt.hour();
        let minute = dt.minute();
        let second = dt.second();

        fn to_bcd(v: u32) -> u8 {
            let tens = v / 10;
            let units = v % 10;
            ((tens as u8) << 4) | (units as u8)
        }

        let timestamp_bcd = vec![
            to_bcd(second),
            to_bcd(minute),
            to_bcd(hour),
            to_bcd(day),
            to_bcd(month),
            to_bcd(year),
        ];

        Ok(Self {
            version,
            timestamp_bcd,
        })
    }
}

impl FreezeParams {
    /// Build freeze‑command parameters from a human‑readable pattern string
    /// using the `"MMDDhhmm"` format.
    ///
    /// The pattern represents month, day, hour and minute. DL/T 645 encodes
    /// this into four BCD bytes laid out as `[mm, hh, DD, MM]`. This helper
    /// validates the textual representation and performs the BCD conversion so
    /// that higher‑level code can avoid dealing with byte‑level details.
    pub fn from_pattern_str(
        version: Dl645Version,
        address: Dl645Address,
        pattern: &str,
    ) -> Result<Self, ProtocolError> {
        if pattern.len() != 8 || !pattern.chars().all(|c| c.is_ascii_digit()) {
            return Err(ProtocolError::InvalidFrame(
                "DL/T 645 freeze pattern must be 8 decimal digits (MMDDhhmm)".to_string(),
            ));
        }

        let mm = &pattern[0..2];
        let dd = &pattern[2..4];
        let hh = &pattern[4..6];
        let min = &pattern[6..8];

        fn to_bcd_from_dec_str(s: &str) -> Result<u8, ProtocolError> {
            let v: u32 = s.parse().map_err(|_| {
                ProtocolError::InvalidFrame(
                    "DL/T 645 freeze pattern numeric component out of range".to_string(),
                )
            })?;
            if v > 99 {
                return Err(ProtocolError::InvalidFrame(
                    "DL/T 645 freeze pattern component must be between 0 and 99".to_string(),
                ));
            }
            let tens = v / 10;
            let units = v % 10;
            Ok(((tens as u8) << 4) | (units as u8))
        }

        let pattern_bcd = vec![
            to_bcd_from_dec_str(min)?,
            to_bcd_from_dec_str(hh)?,
            to_bcd_from_dec_str(dd)?,
            to_bcd_from_dec_str(mm)?,
        ];

        Ok(Self {
            version,
            address,
            pattern_bcd,
        })
    }
}

/// High-level DL/T 645 session trait for request-response exchange.
///
/// Concrete transports such as `SerialSession` and `TcpSession` implement this
/// trait so that the driver can remain agnostic to the underlying link.
///
/// The protocol layer intentionally uses `ProtocolError` instead of the
/// gateway's `DriverError` so that it stays decoupled from higher-level
/// concerns. Upper layers are expected to provide their own mapping.
#[async_trait]
pub trait Dl645Session: Send + Sync {
    /// Send a DL/T 645 frame without waiting for any response.
    ///
    /// This is primarily used for broadcast-style commands such as time
    /// synchronization, where meters are not expected to respond and any
    /// timeout on the read path would be misleading. Implementations must
    /// still honor `min_idle` and optional wakeup preamble semantics but
    /// are free to ignore the `timeout` argument or only apply it to the
    /// write operation. **Importantly, this method must not return
    /// `ProtocolError::Timeout`** so that upper layers can safely treat it
    /// as fire-and-forget without affecting reconnect logic.
    async fn send_only(
        &self,
        frame: Dl645TypedFrame,
        timeout: Duration,
    ) -> Result<(), ProtocolError>;

    /// Send a DL/T 645 frame and wait for its response as a decoded frame.
    async fn request(
        &self,
        frame: Dl645TypedFrame,
        timeout: Duration,
    ) -> Result<Dl645TypedFrame, ProtocolError>;

    /// Read the next complete DL/T645 frame from the underlying transport.
    ///
    /// This method must not send any additional bytes on the wire. It is used
    /// to retrieve subsequent frames that belong to the same logical response
    /// when the control word indicates that more frames follow (D5 = 1).
    async fn read_next_frame(&self, timeout: Duration) -> Result<Dl645TypedFrame, ProtocolError>;

    /// Close the underlying transport gracefully.
    async fn close(&self) -> Result<(), ProtocolError>;

    /// Send a DL/T645 request and aggregate all subsequent frames into a single
    /// logical response using D5/D6 semantics.
    ///
    /// This default implementation builds on top of `request` and
    /// `read_next_frame` so that concrete transports only need to focus on
    /// frame-level I/O while the multi-frame aggregation logic is shared.
    async fn request_full(
        &self,
        request_frame: Dl645TypedFrame,
        timeout: Duration,
        max_frames: usize,
        version: Dl645Version,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        if max_frames == 0 {
            return Err(ProtocolError::Semantic(
                "DL/T645: max_frames must be greater than zero".to_string(),
            ));
        }

        // First exchange: send the command frame and receive the initial response.
        let first_resp = self.request(request_frame, timeout).await?;

        // Inspect the control word using a temporary view and copy out the fields
        // we need so that the owned frame can subsequently be moved into the
        // logical response container.
        let (first_function, first_has_following_frame) = {
            let view = first_resp.as_response_view();

            // Sanity checks on direction and exception flag for the first frame.
            if !matches!(view.ctrl.direction(), Dl645Direction::SlaveToMaster) {
                return Err(ProtocolError::Semantic(
                    "unexpected direction in first response".to_string(),
                ));
            }

            if view.is_exception {
                let code = view
                    .exception_code
                    .unwrap_or(Dl645ExceptionCode::Unknown(0));
                return Err(ProtocolError::Exception(code));
            }

            (view.function, view.has_following_frame)
        };

        let mut frames = Vec::with_capacity(1);
        let mut payload = Vec::new();

        match &first_resp.body {
            Dl645Body::Raw(data) => {
                payload.extend_from_slice(data);
            }
            other => {
                // If the body was parsed into a structured type (e.g. ReadDataBody),
                // we need to re-encode it into raw bytes so that the upper layers
                // (which expect a raw payload stream) can consume it.
                // We use the provided version to ensure consistent encoding.
                let ctx = Dl645CodecContext::new(version);
                let mut buf = BytesMut::new();
                match other.encode_to(&mut buf, &ctx) {
                    Ok(_) => payload.extend_from_slice(&buf),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to re-encode parsed body in request_full");
                        // If re-encoding fails, we can try to at least preserve the raw data
                        // payload if available, though this might lose protocol headers (like DI).
                        if let Some(data) = other.data_payload() {
                            payload.extend_from_slice(data);
                        }
                    }
                }
            }
        }
        frames.push(first_resp);

        if !first_has_following_frame {
            // No multi-frame response, fast path.
            return Ok(Dl645LogicalResponse {
                address: frames[0].address,
                frames,
                payload,
            });
        }

        // Read subsequent frames until D5 is cleared or the frame limit is hit.
        for _ in 1..max_frames {
            let next = self.read_next_frame(timeout).await?;
            let next_view = next.as_response_view();

            // All frames of a logical response must share the same address and
            // direction. Function codes are also expected to remain stable.
            if next.address != frames[0].address {
                return Err(ProtocolError::Semantic(
                    "address mismatch in subsequent frame".to_string(),
                ));
            }

            if !matches!(next_view.ctrl.direction(), Dl645Direction::SlaveToMaster) {
                return Err(ProtocolError::Semantic(
                    "unexpected direction in subsequent frame".to_string(),
                ));
            }

            if next_view.function != first_function {
                return Err(ProtocolError::Semantic(
                    "function code mismatch in subsequent frame".to_string(),
                ));
            }

            if next_view.is_exception {
                let code = next_view
                    .exception_code
                    .unwrap_or(Dl645ExceptionCode::Unknown(0));
                return Err(ProtocolError::Exception(code));
            }

            match &next.body {
                Dl645Body::Raw(data) => {
                    payload.extend_from_slice(data);
                }
                other => {
                    let ctx = Dl645CodecContext::new(version);
                    let mut buf = BytesMut::new();
                    match other.encode_to(&mut buf, &ctx) {
                        Ok(_) => payload.extend_from_slice(&buf),
                        Err(_) => {
                            if let Some(data) = other.data_payload() {
                                payload.extend_from_slice(data);
                            }
                        }
                    }
                }
            }
            let has_more = next_view.has_following_frame;
            frames.push(next);

            if !has_more {
                return Ok(Dl645LogicalResponse {
                    address: frames[0].address,
                    frames,
                    payload,
                });
            }
        }

        Err(ProtocolError::Semantic(
            "too many subsequent frames, possible protocol loop".to_string(),
        ))
    }

    /// High-level semantic helper for a DL/T 645 "read data" operation.
    ///
    /// This method builds a read-data frame using the provided protocol version,
    /// meter address and data identifier (DI), then aggregates all response
    /// frames into a single logical response.
    async fn read_data(
        &self,
        params: ReadDataParams,
        timeout: Duration,
        max_frames: usize,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame = build_read_data_frame(params.version, params.address, params.di).to_owned();
        self.request_full(frame, timeout, max_frames, params.version)
            .await
    }

    /// High-level semantic helper for a DL/T 645 "write data" operation.
    ///
    /// The caller is responsible for encoding the `value_bytes` according to
    /// parameter metadata (for example via `Dl645Codec::encode_parameter_value`)
    /// while this helper focuses on framing and multi-frame aggregation.
    async fn write_data(
        &self,
        params: WriteDataParams,
        timeout: Duration,
        max_frames: usize,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame = build_write_data_frame(
            params.version,
            params.address,
            params.di,
            params.value_bytes,
            params.password,
            params.operator_code,
        )
        .to_owned();
        self.request_full(frame, timeout, max_frames, params.version)
            .await
    }

    /// High-level semantic helper for a DL/T 645 "write communication address" operation.
    async fn write_address(
        &self,
        params: WriteAddressParams,
        timeout: Duration,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        // Address write is expected to be a single-frame response; 1 is a safe default.
        let frame =
            build_write_address_frame(params.version, params.current_address, params.new_address)
                .to_owned();
        self.request_full(frame, timeout, 1, params.version).await
    }

    /// High-level semantic helper for a DL/T 645 broadcast time-synchronization operation.
    async fn broadcast_time_sync(
        &self,
        params: BroadcastTimeSyncParams,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        let frame =
            build_broadcast_time_sync_frame(params.version, params.timestamp_bcd).to_owned();
        // Fire-and-forget: only send the frame, do not wait for any response.
        self.send_only(frame, timeout).await
    }

    /// High-level semantic helper for a DL/T 645 freeze command.
    async fn freeze(
        &self,
        params: FreezeParams,
        timeout: Duration,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame =
            build_freeze_frame(params.version, params.address, params.pattern_bcd).to_owned();
        self.request_full(frame, timeout, 1, params.version).await
    }

    /// High-level semantic helper for a DL/T 645 "update baud rate" operation.
    async fn update_baud_rate(
        &self,
        params: UpdateBaudRateParams,
        timeout: Duration,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame =
            build_update_baud_rate_frame(params.version, params.address, params.code).to_owned();
        self.request_full(frame, timeout, 1, params.version).await
    }

    /// High-level semantic helper for a DL/T 645 "modify password" operation.
    async fn modify_password(
        &self,
        params: ModifyPasswordParams,
        timeout: Duration,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame = build_modify_password_frame(
            params.version,
            params.address,
            params.di,
            params.old_password,
            params.new_password,
        )
        .to_owned();
        self.request_full(frame, timeout, 1, params.version).await
    }

    /// High-level semantic helper for a DL/T 645 "clear maximum demand" operation.
    async fn clear_max_demand(
        &self,
        params: ClearMaxDemandParams,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        let frame = build_clear_max_demand_frame(
            params.version,
            params.address,
            params.password,
            params.operator_code,
        )
        .to_owned();
        self.send_only(frame, timeout).await
    }

    /// High-level semantic helper for a DL/T 645 "clear meter" operation.
    async fn clear_meter(
        &self,
        params: ClearMeterParams,
        timeout: Duration,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame = build_clear_meter_frame(
            params.version,
            params.address,
            params.password,
            params.operator_code,
        )
        .to_owned();
        self.request_full(frame, timeout, 1, params.version).await
    }

    /// High-level semantic helper for a DL/T 645 "clear events" operation.
    async fn clear_events(
        &self,
        params: ClearEventsParams,
        timeout: Duration,
    ) -> Result<Dl645LogicalResponse, ProtocolError> {
        let frame = build_clear_events_frame(
            params.version,
            params.address,
            params.di,
            params.password,
            params.operator_code,
        )
        .to_owned();
        self.request_full(frame, timeout, 1, params.version).await
    }
}

/// Aggregated logical response for a DL/T645 request.
///
/// A single logical request can span multiple physical frames when the
/// "following frame" flag (D5) is set. This structure groups all frames
/// together and concatenates their payloads. The final control word view can
/// be reconstructed from the last frame when needed.
#[derive(Debug)]
pub struct Dl645LogicalResponse {
    /// Meter address from which the response was received.
    pub address: Dl645Address,
    /// All physical frames that were part of the logical response.
    pub frames: Vec<Dl645TypedFrame>,
    /// Concatenated payload bytes from all frames.
    pub payload: Vec<u8>,
}

/// Generic implementation of DL/T 645 session over any framed transport.
#[derive(Debug)]
pub struct Dl645SessionImpl<T> {
    pub framed: tokio::sync::Mutex<Framed<T, Dl645FrameCodec>>,
    pub config: SessionConfig,
}

impl<T> Dl645SessionImpl<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    pub fn new(io: T, config: SessionConfig) -> Self {
        Self {
            framed: tokio::sync::Mutex::new(Framed::new(io, Dl645FrameCodec::new(config.version))),
            config,
        }
    }
}

#[async_trait]
impl<T> Dl645Session for Dl645SessionImpl<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn send_only(
        &self,
        frame: Dl645TypedFrame,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        let mut framed = self.framed.lock().await;

        // Write preamble if configured
        if !self.config.wakeup_preamble.is_empty() {
            use tokio::io::AsyncWriteExt;
            // Ensure previous writes are flushed?
            // Framed::send flushes.
            framed
                .get_mut()
                .write_all(&self.config.wakeup_preamble)
                .await
                .map_err(ProtocolError::Io)?;
        }

        match tokio::time::timeout(timeout, framed.send(frame)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ProtocolError::Io(e)),
            Err(_) => Err(ProtocolError::Timeout(timeout)),
        }
    }

    async fn request(
        &self,
        frame: Dl645TypedFrame,
        timeout: Duration,
    ) -> Result<Dl645TypedFrame, ProtocolError> {
        let mut framed = self.framed.lock().await;

        if !self.config.wakeup_preamble.is_empty() {
            use tokio::io::AsyncWriteExt;
            framed
                .get_mut()
                .write_all(&self.config.wakeup_preamble)
                .await
                .map_err(ProtocolError::Io)?;
        }

        framed.send(frame).await.map_err(ProtocolError::Io)?;

        match tokio::time::timeout(timeout, framed.next()).await {
            Ok(Some(Ok(resp))) => Ok(resp),
            Ok(Some(Err(e))) => Err(ProtocolError::Io(e)),
            Ok(None) => Err(ProtocolError::Transport("Connection closed".into())),
            Err(_) => Err(ProtocolError::Timeout(timeout)),
        }
    }

    async fn read_next_frame(&self, timeout: Duration) -> Result<Dl645TypedFrame, ProtocolError> {
        let mut framed = self.framed.lock().await;
        match tokio::time::timeout(timeout, framed.next()).await {
            Ok(Some(Ok(resp))) => Ok(resp),
            Ok(Some(Err(e))) => Err(ProtocolError::Io(e)),
            Ok(None) => Err(ProtocolError::Transport("Connection closed".into())),
            Err(_) => Err(ProtocolError::Timeout(timeout)),
        }
    }

    async fn close(&self) -> Result<(), ProtocolError> {
        let mut framed = self.framed.lock().await;
        framed.close().await.map_err(ProtocolError::Io)
    }
}
