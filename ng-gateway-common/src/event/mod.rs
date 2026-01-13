mod builtin;

use async_trait::async_trait;
use dashmap::DashMap;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{
    event::{EventBusConfig, EventBusMetrics, EventStats, NGEvent},
    settings::Settings,
    EventBus,
};
use opentelemetry::KeyValue;
use std::{
    any::{type_name, TypeId},
    marker::PhantomData,
    sync::Arc,
};
use tokio::sync::{
    broadcast::{channel, Receiver, Sender},
    Mutex, RwLock,
};
use tracing::{debug, instrument, warn};

/// The main event bus implementation
#[derive(Debug, Clone)]
pub struct NGEventBus {
    config: EventBusConfig,
    channels: Arc<DashMap<TypeId, Sender<Arc<dyn NGEvent>>>>,
    dispatchers: Arc<DashMap<TypeId, Box<dyn std::any::Any + Send + Sync>>>,
    stats: Arc<RwLock<EventStats>>,
    metrics: Option<EventBusMetrics>,
}

impl NGEventBus {
    /// Creates a new event bus with the given configuration
    pub fn new(config: EventBusConfig) -> Self {
        Self {
            config,
            channels: Arc::new(DashMap::new()),
            dispatchers: Arc::new(DashMap::new()),
            stats: Arc::new(RwLock::new(EventStats::default())),
            metrics: None,
        }
    }

    /// Initializes OpenTelemetry metrics
    pub fn init_metrics(&mut self) {
        if self.config.enable_metrics {
            self.metrics = Some(EventBusMetrics::default());
        }
    }

    /// Gets or creates a channel for a specific event type
    async fn get_channel<E: NGEvent>(&self) -> Sender<Arc<dyn NGEvent>> {
        let event_type = TypeId::of::<E>();

        if let Some(sender) = self.channels.get(&event_type) {
            return sender.value().clone();
        }

        let (sender, _) = channel(self.config.channel_capacity);
        let sender_clone = sender.clone();
        self.channels.insert(event_type, sender);
        sender_clone
    }

    /// Subscribes to events of a specific type
    pub async fn subscribe<E: NGEvent>(&self) -> Receiver<Arc<dyn NGEvent>> {
        self.get_channel::<E>().await.subscribe()
    }

    /// Returns the current event statistics
    pub async fn stats(&self) -> EventStats {
        self.stats.read().await.clone()
    }

    /// Gets or creates a dispatcher for a specific event type
    async fn get_dispatcher<E: NGEvent>(&self) -> Arc<RwLock<EventDispatcher<E>>> {
        let event_type = TypeId::of::<E>();

        if let Some(dispatcher) = self.dispatchers.get(&event_type) {
            dispatcher
                .downcast_ref::<Arc<RwLock<EventDispatcher<E>>>>()
                .map(Arc::clone)
                .unwrap()
        } else {
            let dispatcher = EventDispatcher::<E>::new(Arc::new(self.clone())).await;
            let dispatcher = Arc::new(RwLock::new(dispatcher));
            self.dispatchers
                .insert(event_type, Box::new(Arc::clone(&dispatcher)));
            dispatcher
        }
    }
}

#[async_trait]
impl EventBus for NGEventBus {
    async fn init(settings: &Settings) -> Arc<Self> {
        let mut config = EventBusConfig::default();
        config.set_enable_metrics(settings.metrics.enabled);
        let mut event_bus = Self::new(config);
        event_bus.init_metrics();
        // TODO: register all events
        builtin::register_builtin_events(&event_bus).await;
        Arc::new(event_bus)
    }

    #[inline]
    /// Registers an event handler for a specific event type
    async fn register_handler<E, F>(&self, handler: F)
    where
        E: NGEvent + 'static,
        F: FnMut(&E) -> NGResult<()> + Send + Sync + 'static,
    {
        let dispatcher = self.get_dispatcher::<E>().await;
        {
            let mut lock = dispatcher.write().await;
            lock.add_handler(handler);
        }

        let dispatcher_clone = Arc::clone(&dispatcher);
        tokio::spawn(async move {
            let mut lock = dispatcher_clone.write().await;
            lock.start().await;
        });

        // Update active handlers count
        if let Some(metrics) = &self.metrics {
            metrics.active_handlers.add(1, &[]);
        }
    }

    #[inline]
    #[instrument(skip_all)]
    /// Publishes an event to all subscribers
    async fn publish<E>(&self, event: E) -> NGResult<usize>
    where
        E: NGEvent + 'static,
    {
        let event_type = type_name::<E>();
        let event = Arc::new(event);
        // Update statistics
        let mut stats = self.stats.write().await;
        stats.total_events += 1;

        // Update metrics if enabled
        if let Some(metrics) = &self.metrics {
            metrics
                .total_events
                .add(1, &[KeyValue::new("event_type", event_type)]);
        }

        // Send event to all subscribers
        let sender = self.get_channel::<E>().await;
        let subscriber_count = sender.receiver_count();

        if subscriber_count == 0 {
            warn!(event_type=%event_type, "No subscribers for event type");
            return Ok(0);
        }

        match sender.send(event.clone()) {
            Ok(_) => {
                if self.config.enable_tracing {
                    debug!("Event published: type={event_type}, subscribers={subscriber_count}");
                }
                Ok(subscriber_count)
            }
            Err(e) => {
                warn!(error=%e, "Failed to send event");
                Err(NGError::from(format!("Failed to send event: {e}")))
            }
        }
    }
}

type EventHandler<E> = Arc<Mutex<dyn FnMut(&E) -> NGResult<()> + Send + Sync>>;

/// Event dispatcher for handling specific event types
pub struct EventDispatcher<E: NGEvent> {
    event_bus: Arc<NGEventBus>,
    receiver: Receiver<Arc<dyn NGEvent>>,
    handlers: Vec<EventHandler<E>>,
    _phantom: PhantomData<E>,
}

impl<E: NGEvent> EventDispatcher<E> {
    /// Creates a new event dispatcher for a specific event type
    pub async fn new(event_bus: Arc<NGEventBus>) -> Self {
        let receiver = event_bus.subscribe::<E>().await;

        Self {
            event_bus,
            receiver,
            handlers: Vec::new(),
            _phantom: PhantomData,
        }
    }

    #[inline]
    /// Adds a new handler to the dispatcher
    pub fn add_handler<F>(&mut self, handler: F)
    where
        F: FnMut(&E) -> NGResult<()> + Send + Sync + 'static,
    {
        self.handlers.push(Arc::new(Mutex::new(handler)));
    }

    #[inline]
    /// Returns the number of active handlers
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    #[inline]
    /// Starts processing events
    pub async fn start(&mut self) {
        let handlers = &self.handlers;
        let event_bus = &self.event_bus;

        while let Ok(event) = self.receiver.recv().await {
            if let Ok(event) = event.downcast_arc::<E>() {
                let mut successful = 0;
                let mut failed = 0;

                for handler in handlers {
                    let mut handler = handler.lock().await;
                    match handler(&event) {
                        Ok(_) => successful += 1,
                        Err(e) => {
                            warn!(error=%e, "Handler failed");
                            failed += 1;
                        }
                    }
                }

                // Update statistics
                let mut stats = event_bus.stats.write().await;
                stats.successful_handlers += successful;
                stats.failed_handlers += failed;

                // Update metrics if enabled
                if let Some(metrics) = &event_bus.metrics {
                    metrics.successful_handlers.add(successful, &[]);
                    metrics.failed_handlers.add(failed, &[]);
                }
            }
        }
    }

    #[inline]
    /// Dispatches an event of the specific type
    pub async fn dispatch(&self, event: E) -> NGResult<usize> {
        self.event_bus.publish(event).await
    }
}
