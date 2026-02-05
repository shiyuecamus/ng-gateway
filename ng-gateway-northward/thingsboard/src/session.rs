//! ThingsBoard supervised session implementation.
//!
//! This module contains the per-attempt session lifecycle driven by the SDK supervisor:
//! - `init()`: define readiness (MQTT connack + subscriptions).
//! - `run()`: poll MQTT event loop and route incoming messages.

use super::{config::ThingsBoardPluginConfig, handle::ThingsBoardHandle, topics::Topics};
use ng_gateway_sdk::{
    mqtt::router::MessageRouter,
    supervision::{RunOutcome, Session, SessionContext},
    NorthwardError, NorthwardResult,
};
use rumqttc::{Event, EventLoop, Packet, SubscribeFilter};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// ThingsBoard supervised session for a single attempt.
pub struct ThingsBoardSession {
    pub(crate) handle: Arc<ThingsBoardHandle>,
    pub(crate) config: Arc<ThingsBoardPluginConfig>,
    pub(crate) router: Arc<MessageRouter>,
    pub(crate) event_loop: Option<EventLoop>,
    pub(crate) app_id: i32,
}

#[async_trait::async_trait]
impl Session for ThingsBoardSession {
    type Handle = ThingsBoardHandle;
    type Error = NorthwardError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        // Wait until we observe ConnAck, then subscribe topics.
        let Some(mut ev) = self.event_loop.take() else {
            return Ok(());
        };

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    self.event_loop = Some(ev);
                    return Err(NorthwardError::NotConnected);
                }
                res = ev.poll() => {
                    let event = res.map_err(|e| NorthwardError::MqttError { reason: e.to_string() })?;
                    match event {
                        Event::Incoming(Packet::ConnAck(_)) => {
                            // Subscriptions define Ready.
                            subscribe_required_topics(&self.handle, &self.config).await?;
                            // Allow router to transition to "connected" state if it needs it.
                            let _ = self.router.handle_connected().await;
                            break;
                        }
                        _ => {
                            // Keep polling until ConnAck.
                        }
                    }
                }
            }
        }

        // Hand the event loop back to run().
        self.event_loop = Some(ev);
        Ok(())
    }

    async fn run(mut self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        let mut ev = match self.event_loop.take() {
            Some(ev) => ev,
            None => return Ok(RunOutcome::Disconnected),
        };

        info!(app_id = self.app_id, "ThingsBoard session run loop started");

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                res = ev.poll() => {
                    match res {
                        Ok(event) => {
                            match event {
                                Event::Incoming(Packet::Publish(publish)) => {
                                    let payload = &publish.payload[..];
                                    if let Err(e) = self.router.route_message(&publish.topic, payload).await {
                                        warn!(app_id = self.app_id, error = %e, "Error handling MQTT message");
                                    }
                                }
                                Event::Incoming(Packet::SubAck(_)) => {
                                    debug!(app_id = self.app_id, "Subscription acknowledged");
                                }
                                Event::Incoming(Packet::ConnAck(_)) => {
                                    debug!(app_id = self.app_id, "MQTT connection acknowledged");
                                    let _ = self.router.handle_connected().await;
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            warn!(
                                app_id = self.app_id,
                                error = %e,
                                "ThingsBoard MQTT event loop stopped with error"
                            );
                            return Err(NorthwardError::MqttError { reason: e.to_string() });
                        }
                    }
                }
            }
        }

        info!(app_id = self.app_id, "ThingsBoard session run loop stopped");
        Ok(RunOutcome::Disconnected)
    }
}

async fn subscribe_required_topics(
    handle: &ThingsBoardHandle,
    config: &ThingsBoardPluginConfig,
) -> NorthwardResult<()> {
    let qos = config.communication.qos();
    let topics = vec![
        // Sub-device shared attributes changed (Gateway API)
        Topics::gateway_attributes(),
        // Gateway device attributes changed (Device API) - observability only
        Topics::device_attributes(),
        Topics::device_attributes_response_sub(),
        Topics::device_rpc_request_sub(),
        Topics::device_rpc_response_sub(),
        Topics::gateway_rpc(),
    ];

    handle
        .client()
        .subscribe_many(topics.into_iter().map(|t| SubscribeFilter::new(t, qos)))
        .await
        .map_err(|e| NorthwardError::MqttError {
            reason: e.to_string(),
        })
}
