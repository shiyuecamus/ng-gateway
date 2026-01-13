use std::sync::Arc;

use super::super::{
    codec::Codec,
    error::{Error, Result},
    frame::{
        build_cotp_cr_message, build_cotp_data_message, build_setup_comm, parse_param_ref, Cotp,
        CotpCrParams, S7AppBody, S7Message, S7ParamAckDataRef, S7ParamRef, S7PduType, SetupParam,
    },
};
use super::state::SessionConfig;
use futures_util::{SinkExt, StreamExt};
use tokio::{net::TcpStream, time::timeout};
use tokio_util::codec::Framed;

/// Perform COTP CR/CC handshake on an already connected framed transport.
///
/// Returns the TPDU size negotiated in CC.
pub(super) async fn iso_connect(
    framed: &mut Framed<TcpStream, Codec>,
    config: Arc<SessionConfig>,
) -> Result<u8> {
    // Build COTP CR (fixed default TPDU size 0x0A = 1024 bytes)
    let cr = CotpCrParams {
        src_tsap: config.tsap_src,
        dst_tsap: config.tsap_dst,
        ..Default::default()
    };

    // Send COTP CR (as raw COTP)
    timeout(config.write_timeout, framed.send(build_cotp_cr_message(cr)))
        .await
        .map_err(|_| Error::ErrRequestTimeout)??;
    // Receive COTP CC
    let maybe_pkt = timeout(config.read_timeout, framed.next())
        .await
        .map_err(|_| Error::ErrRequestTimeout)?;
    let S7Message { cotp, .. } = maybe_pkt.ok_or(Error::ErrUnexpectedPdu)??;
    let cc_tpdu_size = match cotp {
        Cotp::Cc(params) => params.tpdu_size,
        _ => return Err(Error::ErrUnexpectedPdu),
    };

    Ok(cc_tpdu_size)
}

/// Send S7 SetupCommunication and return negotiated values.
pub(super) async fn negotiation(
    framed: &mut Framed<TcpStream, Codec>,
    config: Arc<SessionConfig>,
    pdu_ref: u16,
) -> Result<SetupParam> {
    // Send S7 SetupCommunication Job
    let msg = build_cotp_data_message(build_setup_comm(
        pdu_ref,
        config.preferred_pdu_size,
        config.preferred_amq_caller,
        config.preferred_amq_callee,
    ));
    timeout(config.write_timeout, framed.send(msg))
        .await
        .map_err(|_| Error::ErrRequestTimeout)??;

    // Wait for AckData of SetupCommunication
    let pkt = timeout(config.read_timeout, framed.next())
        .await
        .map_err(|_| Error::ErrRequestTimeout)?
        .ok_or(Error::ErrUnexpectedPdu)??;
    let pdu = match pkt.app {
        Some(S7AppBody::Parsed(p)) => p,
        _ => return Err(Error::ErrUnexpectedPdu),
    };

    pdu.validate_response()?;

    // Parse AckData param and extract negotiated values
    let (_r, pref) = parse_param_ref(S7PduType::AckData, pdu.param.as_ref())
        .map_err(|_| Error::ErrInvalidFrame)?;
    match pref {
        S7ParamRef::AckData(S7ParamAckDataRef::SetupCommunication(sp)) => Ok(sp),
        _ => Err(Error::ErrUnexpectedPdu),
    }
}
