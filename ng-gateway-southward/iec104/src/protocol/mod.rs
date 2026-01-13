#![allow(unused)]
pub mod client;
mod codec;
mod error;
pub mod frame;
pub mod session;

// Public re-exports for external use
pub use self::{
    client::{Client, ClientBuilder},
    error::Error,
    frame::{
        asdu::{Asdu, Cause, CauseOfTransmission, TypeID},
        cproc::{
            BitsString32CommandInfo, DoubleCommandInfo, SetPointCommandFloatInfo,
            SetPointCommandNormalInfo, SetPointCommandScaledInfo, SingleCommandInfo,
            StepCommandInfo,
        },
        csys::{ObjectQCC, ObjectQOI},
    },
    session::{
        create, Session, SessionConfig, SessionEvent, SessionEventLoop, SessionLifecycleState,
    },
};

#[cfg(test)]
mod tests {
    use super::*;
    use super::{
        frame::{
            apci::{new_iframe, new_uframe},
            asdu::{Cause, CauseOfTransmission, IDENTIFIER_SIZE},
            cproc::SingleCommandInfo,
            csys::{ObjectQCC, ObjectQOI},
            mproc::{single, SinglePointInfo},
        },
        ClientBuilder,
    };
    use bytes::Bytes;
    use std::{any::Any, sync::Once};
    use tokio::{
        net::TcpListener,
        time::{sleep, sleep_until, timeout, Duration, Instant},
    };
    use tracing::Level;

    #[tokio::test]
    #[cfg_attr(not(debug_assertions), ignore)]
    /// Integration test that:
    /// 1) Waits for session to become Active
    /// 2) Spawns a background task to continuously read and print incoming ASDUs for 60 seconds
    /// 3) Immediately sends Counter Interrogation Activation, then ActivationTerm after 10s
    /// 4) Immediately sends Interrogation Activation, then ActivationTerm after 10s
    /// 5) After the reading task completes, sends a Single Command before exit
    async fn it_connects_and_subscribes_asdu_and_sends_single_cmd() -> anyhow::Result<()> {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG) // 设置测试中希望看到的最高日志级别
            .with_target(false) // 可选：关闭目标模块路径显示，使输出更简洁
            .without_time() // 可选：关闭时间戳，测试输出更清晰
            .try_init();

        let socket_addr: std::net::SocketAddr = "127.0.0.1:2404".parse()?;
        let client = ClientBuilder::new().socket_addr(socket_addr).build();
        let (session, event_loop) = client.connect().await?;
        let mut lifecycle = session.lifecycle();
        let mut asdu_rx = session
            .take_asdu_receiver()
            .await
            .ok_or(anyhow::anyhow!("ASDU receiver not found"))?;

        // Drive client I/O
        let _io = event_loop.spawn();

        // Wait until Active
        timeout(
            Duration::from_secs(2),
            session.wait_for_state(SessionLifecycleState::Active),
        )
        .await
        .map_err(|_e| anyhow::anyhow!("timeout waiting for Active state"))?;

        // Start background task to read and print ASDUs for 60 seconds
        let reader_handle = tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                tokio::select! {
                    _ = sleep_until(deadline) => {
                        tracing::info!("ASDU reader completed after 60s");
                        break;
                    }
                    maybe_asdu = asdu_rx.recv() => {
                        match maybe_asdu {
                            Some(mut asdu) => {;
                                tracing::info!(
                                    type_id = (asdu.identifier.type_id as u8),
                                    cause = ?asdu.identifier.cot.cause().get(),
                                    len = asdu.raw.len(),
                                    "Received ASDU"
                                );
                            }
                            None => {
                                tracing::warn!("ASDU channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        });

        session
            .counter_interrogation_cmd(
                CauseOfTransmission::new(false, false, Cause::Activation),
                0x0001,
                ObjectQCC::new(0x05),
            )
            .await?;

        tracing::info!("IEC104 TRIGGER: Counter Interrogation CUM BEGIN");

        sleep(Duration::from_secs(10)).await;

        session
            .counter_interrogation_cmd(
                CauseOfTransmission::new(false, false, Cause::ActivationTerm),
                0x0001,
                ObjectQCC::new(0x05),
            )
            .await?;

        tracing::info!("IEC104 TRIGGER: Interrogation CUM END");

        session
            .interrogation_cmd(
                CauseOfTransmission::new(false, false, Cause::Activation),
                0x0001,
                ObjectQOI::new(20),
            )
            .await?;

        tracing::info!("IEC104 TRIGGER: Interrogation ALL BEGIN");

        sleep(Duration::from_secs(30)).await;

        match session
            .interrogation_cmd(
                CauseOfTransmission::new(false, false, Cause::Deactivation),
                0x0001,
                ObjectQOI::new(20),
            )
            .await
        {
            Ok(_) => {
                tracing::info!("IEC104 TRIGGER: Interrogation ALL END");
            }
            Err(e) => {
                tracing::error!(error=%e, "Failed to send interrogation command");
            }
        }

        // Wait for the reader task to finish before exit
        let _ = reader_handle.await;

        // Send a single command (C_SC_NA_1) with Activation right before exit
        let cmd = SingleCommandInfo::new(1, true, false);
        match session
            .single_cmd(
                TypeID::C_SC_NA_1,
                CauseOfTransmission::new(false, false, Cause::Activation),
                1,
                cmd,
            )
            .await
        {
            Ok(_) => {
                tracing::info!("IEC104 TRIGGER: Single Command SENT");
            }
            Err(e) => {
                tracing::error!(error=%e, "Failed to send single command");
            }
        }

        sleep(Duration::from_secs(2)).await;

        Ok(())
    }
}
