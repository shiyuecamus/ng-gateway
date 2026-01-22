use opcua::{
    client::Session,
    types::{NodeId, ReadValueId, TimestampsToReturn, Variant},
};
use std::sync::Arc;

/// Unified, best-effort view of OPC UA server capabilities.
///
/// # Design goals
/// - **Single place** to probe and parse `Server_ServerCapabilities_*` limits.
/// - **Best-effort**: capability probing must never fail the driver.
/// - **Low overhead**: perform a **single** `Read` round-trip per (re)connection when possible.
/// - **Extensible**: new limits can be added without touching driver hot paths.
///
/// # Notes
/// - In OPC UA, a value of `0` typically means "no limit". We normalize those to `None`.
/// - All fields are optional; an `Err` or unexpected type is treated as "unknown".
#[derive(Debug, Clone, Default)]
pub(super) struct ServerCapacity {
    /// Read/transport related limits.
    pub read: ReadCapacity,
    /// Subscription/monitored-item related limits.
    pub subscription: SubscriptionCapacity,
}

/// Read-related limits reported by the OPC UA server.
#[derive(Debug, Clone, Default)]
pub(super) struct ReadCapacity {
    /// `Server_ServerCapabilities_OperationLimits_MaxNodesPerRead` (ns=0;i=11705)
    pub max_nodes_per_read: Option<u32>,
    /// `Server_ServerCapabilities_MaxArrayLength` (ns=0;i=11702)
    pub max_array_length: Option<u32>,
    /// `Server_ServerCapabilities_MaxStringLength` (ns=0;i=11703)
    pub max_string_length: Option<u32>,
    /// `Server_ServerCapabilities_MaxByteStringLength` (ns=0;i=12911)
    pub max_byte_string_length: Option<u32>,
}

impl ReadCapacity {
    #[inline]
    fn normalize(v: Option<u32>) -> Option<u32> {
        match v {
            Some(0) | None => None,
            other => other,
        }
    }
}

/// Subscription-related capacity limits reported by the OPC UA server.
#[derive(Debug, Clone, Default)]
pub(super) struct SubscriptionCapacity {
    /// `Server_ServerCapabilities_MaxSessions` (ns=0;i=24095)
    pub max_sessions: Option<u32>,
    /// `Server_ServerCapabilities_MaxSubscriptions` (ns=0;i=24096)
    pub max_subscriptions: Option<u32>,
    /// `Server_ServerCapabilities_MaxMonitoredItems` (ns=0;i=24097)
    pub max_monitored_items: Option<u32>,
    /// `Server_ServerCapabilities_MaxSubscriptionsPerSession` (ns=0;i=24098)
    pub max_subscriptions_per_session: Option<u32>,
    /// `Server_ServerCapabilities_MaxMonitoredItemsPerSubscription` (ns=0;i=24104)
    pub max_monitored_items_per_subscription: Option<u32>,
}

impl SubscriptionCapacity {
    #[inline]
    fn normalize(v: Option<u32>) -> Option<u32> {
        match v {
            Some(0) | None => None,
            other => other,
        }
    }
}

/// Parse common integer-like variants into u32.
///
/// Returns `None` if the variant cannot be represented as a non-negative u32.
#[inline]
fn variant_to_u32(v: &Variant) -> Option<u32> {
    match v {
        Variant::UInt32(x) => Some(*x),
        Variant::Int32(x) if *x >= 0 => Some(*x as u32),
        Variant::UInt64(x) if *x <= u32::MAX as u64 => Some(*x as u32),
        Variant::Int64(x) if *x >= 0 && *x <= u32::MAX as i64 => Some(*x as u32),
        _ => None,
    }
}

/// Probe a unified set of server capability limits in a **single** `Read` call.
///
/// Any error is logged and results in an empty/default capacity.
pub(super) async fn probe_capacity(session: &Arc<Session>) -> ServerCapacity {
    // Read-related:
    // - Server_ServerCapabilities_MaxArrayLength (11702)
    // - Server_ServerCapabilities_MaxStringLength (11703)
    // - Server_ServerCapabilities_OperationLimits_MaxNodesPerRead (11705)
    // - Server_ServerCapabilities_MaxByteStringLength (12911)
    //
    // Subscription-related:
    // - Server_ServerCapabilities_MaxSessions (24095)
    // - Server_ServerCapabilities_MaxSubscriptions (24096)
    // - Server_ServerCapabilities_MaxMonitoredItems (24097)
    // - Server_ServerCapabilities_MaxSubscriptionsPerSession (24098)
    // - Server_ServerCapabilities_MaxMonitoredItemsPerSubscription (24104)
    let nodes = [
        ReadValueId::new_value(NodeId::new(0, 11702u32)),
        ReadValueId::new_value(NodeId::new(0, 11703u32)),
        ReadValueId::new_value(NodeId::new(0, 11705u32)),
        ReadValueId::new_value(NodeId::new(0, 12911u32)),
        ReadValueId::new_value(NodeId::new(0, 24095u32)),
        ReadValueId::new_value(NodeId::new(0, 24096u32)),
        ReadValueId::new_value(NodeId::new(0, 24097u32)),
        ReadValueId::new_value(NodeId::new(0, 24098u32)),
        ReadValueId::new_value(NodeId::new(0, 24104u32)),
    ];

    let mut out = ServerCapacity::default();

    match session.read(&nodes, TimestampsToReturn::Neither, 0.0).await {
        Ok(values) => {
            if values.len() != nodes.len() {
                tracing::warn!(
                    expected = nodes.len(),
                    actual = values.len(),
                    "OPC UA server capabilities read returned mismatched value count"
                );
            }

            let get_u32 = |idx: usize, label: &str| -> Option<u32> {
                match values.get(idx).and_then(|dv| dv.value.as_ref()) {
                    Some(variant) => {
                        let parsed = variant_to_u32(variant);
                        if parsed.is_none() {
                            tracing::warn!(
                                label,
                                variant = ?variant,
                                "OPC UA server capabilities value has unexpected type"
                            );
                        }
                        parsed
                    }
                    None => None,
                }
            };

            // Read capacities
            out.read.max_array_length = ReadCapacity::normalize(get_u32(0, "MaxArrayLength"));
            out.read.max_string_length = ReadCapacity::normalize(get_u32(1, "MaxStringLength"));
            out.read.max_nodes_per_read =
                ReadCapacity::normalize(get_u32(2, "OperationLimits.MaxNodesPerRead"));
            out.read.max_byte_string_length =
                ReadCapacity::normalize(get_u32(3, "MaxByteStringLength"));

            // Subscription capacities
            out.subscription.max_sessions =
                SubscriptionCapacity::normalize(get_u32(4, "MaxSessions"));
            out.subscription.max_subscriptions =
                SubscriptionCapacity::normalize(get_u32(5, "MaxSubscriptions"));
            out.subscription.max_monitored_items =
                SubscriptionCapacity::normalize(get_u32(6, "MaxMonitoredItems"));
            out.subscription.max_subscriptions_per_session =
                SubscriptionCapacity::normalize(get_u32(7, "MaxSubscriptionsPerSession"));
            out.subscription.max_monitored_items_per_subscription =
                SubscriptionCapacity::normalize(get_u32(8, "MaxMonitoredItemsPerSubscription"));

            tracing::info!(
                // Read
                max_nodes_per_read = ?out.read.max_nodes_per_read,
                max_array_length = ?out.read.max_array_length,
                max_string_length = ?out.read.max_string_length,
                max_byte_string_length = ?out.read.max_byte_string_length,
                // Subscribe
                max_sessions = ?out.subscription.max_sessions,
                max_subscriptions = ?out.subscription.max_subscriptions,
                max_monitored_items = ?out.subscription.max_monitored_items,
                max_subscriptions_per_session = ?out.subscription.max_subscriptions_per_session,
                max_monitored_items_per_subscription = ?out.subscription.max_monitored_items_per_subscription,
                "OPC UA server capacity read from ServerCapabilities"
            );
        }
        Err(status) => {
            tracing::warn!(
                status = %status,
                "Failed to read OPC UA server capabilities; continuing with default capacity"
            );
        }
    }

    out
}
