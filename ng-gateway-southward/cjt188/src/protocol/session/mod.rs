use crate::{
    protocol::{
        codec::Cjt188FrameCodec,
        error::ProtocolError,
        frame::body::Cjt188Body,
        frame::defs::{Cjt188Address, DataIdentifier, MeterType},
        frame::{builder, Cjt188TypedFrame},
        sequence::SequenceManager,
    },
    types::Cjt188Version,
};
use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

/// Shared timing and size configuration for CJ/T 188 sessions.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub wakeup_preamble: Vec<u8>,
}

impl SessionConfig {
    pub fn new(wakeup_preamble: Vec<u8>) -> Self {
        Self { wakeup_preamble }
    }
}

#[derive(Debug, Clone)]
pub struct ReadDataParams {
    pub meter_type: MeterType,
    pub address: Cjt188Address,
    pub di: DataIdentifier,
}

#[derive(Debug, Clone)]
pub struct WriteDataParams {
    pub meter_type: MeterType,
    pub address: Cjt188Address,
    pub di: DataIdentifier,
    pub value_bytes: Bytes,
}

#[derive(Debug, Clone)]
pub struct WriteMotorSyncParams {
    pub meter_type: MeterType,
    pub address: Cjt188Address,
    pub di: DataIdentifier,
    pub value_bytes: Bytes,
}

#[derive(Debug)]
pub struct Cjt188LogicalResponse {
    pub meter_type: MeterType,
    pub address: Cjt188Address,
    pub frames: Vec<Cjt188TypedFrame>,
    pub payload: Vec<u8>,
}

#[async_trait]
pub trait Cjt188Session: Send + Sync {
    async fn request(
        &self,
        frame: Cjt188TypedFrame,
        timeout: Duration,
    ) -> Result<Cjt188TypedFrame, ProtocolError>;

    async fn read_next_frame(&self, timeout: Duration) -> Result<Cjt188TypedFrame, ProtocolError>;

    /// Send a frame without waiting for response.
    async fn send_only(
        &self,
        frame: Cjt188TypedFrame,
        timeout: Duration,
    ) -> Result<(), ProtocolError>;

    /// Close the underlying transport.
    async fn close(&self) -> Result<(), ProtocolError>;

    // High-level methods
    async fn read_data(
        &self,
        params: ReadDataParams,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError>;

    async fn write_data(
        &self,
        params: WriteDataParams,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError>;

    async fn write_motor_sync(
        &self,
        params: WriteMotorSyncParams,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError>;

    async fn read_address(&self, timeout: Duration) -> Result<Cjt188Address, ProtocolError>;

    async fn write_address(
        &self,
        meter_type: MeterType,
        current_address: Cjt188Address,
        new_address: Cjt188Address,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError>;
}

/// Generic implementation over any Framed stream.
pub struct Cjt188SessionImpl<T> {
    pub framed: tokio::sync::Mutex<Framed<T, Cjt188FrameCodec>>,
    pub config: SessionConfig,
    pub sequence: SequenceManager,
    pub version: Cjt188Version,
}

impl<T> Cjt188SessionImpl<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    pub fn new(io: T, config: SessionConfig, version: Cjt188Version) -> Self {
        Self {
            framed: tokio::sync::Mutex::new(Framed::new(io, Cjt188FrameCodec::new(version))),
            config,
            sequence: SequenceManager::new(),
            version,
        }
    }

    /// Generate the next frame serial number (SER/SEQ).
    ///
    /// ## Background
    /// Both CJ/T 188-2004 and CJ/T 188-2018 define a 1-byte sequence domain in the data field
    /// (commonly referred to as **SER** or **SEQ**) to correlate request/response frames.
    ///
    /// For robustness, the driver always generates SER and expects devices to echo it back.
    fn next_serial(&self) -> u8 {
        self.sequence.next()
    }
}

#[async_trait]
impl<T> Cjt188Session for Cjt188SessionImpl<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn request(
        &self,
        frame: Cjt188TypedFrame,
        timeout: Duration,
    ) -> Result<Cjt188TypedFrame, ProtocolError> {
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
            Err(_) => Err(ProtocolError::Timeout("Request timeout".into())),
        }
    }

    async fn read_next_frame(&self, timeout: Duration) -> Result<Cjt188TypedFrame, ProtocolError> {
        let mut framed = self.framed.lock().await;
        match tokio::time::timeout(timeout, framed.next()).await {
            Ok(Some(Ok(resp))) => Ok(resp),
            Ok(Some(Err(e))) => Err(ProtocolError::Io(e)),
            Ok(None) => Err(ProtocolError::Transport("Connection closed".into())),
            Err(_) => Err(ProtocolError::Timeout("Read next timeout".into())),
        }
    }

    async fn send_only(
        &self,
        frame: Cjt188TypedFrame,
        timeout: Duration,
    ) -> Result<(), ProtocolError> {
        let mut framed = self.framed.lock().await;

        if !self.config.wakeup_preamble.is_empty() {
            use tokio::io::AsyncWriteExt;
            framed
                .get_mut()
                .write_all(&self.config.wakeup_preamble)
                .await
                .map_err(ProtocolError::Io)?;
        }

        match tokio::time::timeout(timeout, framed.send(frame)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ProtocolError::Io(e)),
            Err(_) => Err(ProtocolError::Timeout("Send timeout".into())),
        }
    }

    async fn close(&self) -> Result<(), ProtocolError> {
        let mut framed = self.framed.lock().await;
        framed.close().await.map_err(ProtocolError::Io)
    }

    async fn read_data(
        &self,
        params: ReadDataParams,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError> {
        let serial = self.next_serial();
        let req =
            builder::build_read_data_frame(params.meter_type, params.address, params.di, serial);
        self.request_full(req, timeout).await
    }

    async fn write_data(
        &self,
        params: WriteDataParams,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError> {
        let serial = self.next_serial();
        let req = builder::build_write_data_frame(
            params.meter_type,
            params.address,
            params.di,
            serial,
            params.value_bytes.to_vec(),
        );
        self.request_full(req, timeout).await
    }

    async fn write_motor_sync(
        &self,
        params: WriteMotorSyncParams,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError> {
        let serial = self.next_serial();
        let req = builder::build_write_motor_sync_frame(
            params.meter_type,
            params.address,
            params.di,
            serial,
            params.value_bytes.to_vec(),
        );
        self.request_full(req, timeout).await
    }

    async fn read_address(&self, timeout: Duration) -> Result<Cjt188Address, ProtocolError> {
        let serial = self.next_serial();
        let req = builder::build_read_address_frame(serial);
        let resp = self.request_full(req, timeout).await?;
        Ok(resp.address)
    }

    async fn write_address(
        &self,
        meter_type: MeterType,
        current_address: Cjt188Address,
        new_address: Cjt188Address,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError> {
        let serial = self.next_serial();
        let req =
            builder::build_write_address_frame(meter_type, current_address, new_address, serial);
        self.request_full(req, timeout).await
    }
}

impl<T> Cjt188SessionImpl<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn request_full(
        &self,
        req: Cjt188TypedFrame,
        timeout: Duration,
    ) -> Result<Cjt188LogicalResponse, ProtocolError> {
        // Validate SER match if present in request
        // This ensures the response corresponds to the request we just sent.
        let req_serial = match &req.body {
            Cjt188Body::ReadData { serial, .. } => Some(*serial),
            Cjt188Body::WriteData { serial, .. } => Some(*serial),
            Cjt188Body::WriteMotorSync { serial, .. } => Some(*serial),
            Cjt188Body::WriteAddr { serial, .. } => Some(*serial),
            _ => None,
        };

        // CJ/T 188 D5 bit indicates follow-up frames.
        let first = self.request(req, timeout).await?;

        // Check for exception response (D6=1)
        if let Cjt188Body::ExceptionResponse { error_status, .. } = &first.body {
            return Err(ProtocolError::Semantic(format!(
                "Device exception response: status={:02X}",
                error_status
            )));
        }

        if let Some(req_s) = req_serial {
            let resp_serial = match &first.body {
                Cjt188Body::ReadDataResponse { serial, .. } => Some(*serial),
                Cjt188Body::WriteDataResponse { serial, .. } => Some(*serial),
                Cjt188Body::WriteMotorSyncResponse { serial, .. } => Some(*serial),
                Cjt188Body::WriteAddrResponse { serial, .. } => Some(*serial),
                _ => None,
            };

            if let Some(resp_s) = resp_serial {
                if req_s != resp_s {
                    return Err(ProtocolError::Semantic(format!(
                        "Serial mismatch: request={:02X}, response={:02X}",
                        req_s, resp_s
                    )));
                }
            }
        }

        let mut frames = vec![first];

        while frames.last().unwrap().control.has_follow_up() {
            let next = self.read_next_frame(timeout).await?;
            // Also check next frames for exception? (Unlikely in follow-up but safe to check if structure allows)
            if let Cjt188Body::ExceptionResponse { error_status, .. } = &next.body {
                return Err(ProtocolError::Semantic(format!(
                    "Device exception in follow-up frame: status={:02X}",
                    error_status
                )));
            }
            frames.push(next);
        }

        let mut payload = Vec::new();
        for f in &frames {
            if let Some(p) = f.body.data_payload() {
                payload.extend_from_slice(&p);
            }
        }

        Ok(Cjt188LogicalResponse {
            meter_type: frames[0].meter_type,
            address: frames[0].address,
            frames,
            payload,
        })
    }
}
