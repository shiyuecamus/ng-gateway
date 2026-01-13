use crate::NorthwardResult;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::RwLock;

/// Result type for message handlers
pub type HandlerResult = NorthwardResult<()>;

/// Message handler callback type
pub type MessageHandler = Arc<
    dyn Fn(&str, &[u8]) -> Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
        + Send
        + Sync,
>;

/// Connection event handler callback type  
pub type ConnectionHandler =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>> + Send + Sync>;

/// Disconnection event handler callback type
pub type DisconnectionHandler = Arc<
    dyn Fn(Option<String>) -> Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>
        + Send
        + Sync,
>;

/// Route configuration for mapping topics to handlers
#[derive(Debug, Clone)]
pub struct Route {
    /// Topic pattern to match (supports wildcards)
    pub pattern: String,
    /// Priority for route matching (higher = checked first)  
    pub priority: u8,
}

impl Route {
    /// Create a new route
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            priority: 0,
        }
    }

    /// Set route priority
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
}

/// Message router that dispatches messages to callback functions
pub struct MessageRouter {
    /// Registered routes with their handlers
    routes: Arc<RwLock<Vec<(Route, MessageHandler)>>>,
    /// Default handler for unmatched messages
    default_handler: Arc<RwLock<Option<MessageHandler>>>,
    /// Connection event handler
    connection_handler: Arc<RwLock<Option<ConnectionHandler>>>,
    /// Disconnection event handler  
    disconnection_handler: Arc<RwLock<Option<DisconnectionHandler>>>,
}

impl MessageRouter {
    /// Create a new message router
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(Vec::new())),
            default_handler: Arc::new(RwLock::new(None)),
            connection_handler: Arc::new(RwLock::new(None)),
            disconnection_handler: Arc::new(RwLock::new(None)),
        }
    }

    /// Register a message handler for a topic pattern
    pub async fn register(&self, pattern: impl Into<String>, handler: MessageHandler) {
        let route = Route::new(pattern);
        self.register_with_route(route, handler).await;
    }

    /// Register a message handler with custom route configuration
    pub async fn register_with_route(&self, route: Route, handler: MessageHandler) {
        let mut routes = self.routes.write().await;
        routes.push((route, handler));
        // Sort by priority (descending) to check higher priority routes first
        routes.sort_by(|a, b| b.0.priority.cmp(&a.0.priority));
    }

    /// Set default handler for unmatched messages
    pub async fn set_default_handler(&self, handler: MessageHandler) {
        let mut default_handler = self.default_handler.write().await;
        *default_handler = Some(handler);
    }

    /// Set connection event handler
    pub async fn set_connection_handler(&self, handler: ConnectionHandler) {
        let mut connection_handler = self.connection_handler.write().await;
        *connection_handler = Some(handler);
    }

    /// Set disconnection event handler
    pub async fn set_disconnection_handler(&self, handler: DisconnectionHandler) {
        let mut disconnection_handler = self.disconnection_handler.write().await;
        *disconnection_handler = Some(handler);
    }

    /// Route a message to the appropriate handler
    pub async fn route_message(&self, topic: &str, payload: &[u8]) -> HandlerResult {
        let routes = self.routes.read().await;

        // Find matching route
        for (route, handler) in routes.iter() {
            if mqtt_pattern_matches(&route.pattern, topic) {
                return handler(topic, payload).await;
            }
        }

        // Try default handler if no route matched
        let default_handler = self.default_handler.read().await;
        if let Some(handler) = default_handler.as_ref() {
            tracing::debug!("No specific route found for topic '{topic}', using default handler");
            return handler(topic, payload).await;
        }

        tracing::warn!("No handler found for topic: {topic}");
        Ok(())
    }

    /// Handle connection established event
    pub async fn handle_connected(&self) -> HandlerResult {
        let connection_handler = self.connection_handler.read().await;
        if let Some(handler) = connection_handler.as_ref() {
            handler().await
        } else {
            tracing::debug!("Connection established but no connection handler registered");
            Ok(())
        }
    }

    /// Handle connection lost event
    pub async fn handle_disconnected(&self, reason: Option<String>) -> HandlerResult {
        let disconnection_handler = self.disconnection_handler.read().await;
        if let Some(handler) = disconnection_handler.as_ref() {
            handler(reason).await
        } else {
            tracing::debug!("Connection lost but no disconnection handler registered");
            Ok(())
        }
    }

    /// Get number of registered routes
    pub async fn route_count(&self) -> usize {
        self.routes.read().await.len()
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if an MQTT topic matches a pattern with wildcards
///
/// Supports MQTT-style wildcards:
/// - `+` matches exactly one topic level
/// - `#` matches zero or more topic levels (must be at the end)
pub fn mqtt_pattern_matches(pattern: &str, topic: &str) -> bool {
    // Handle exact matches first (optimization)
    if pattern == topic {
        return true;
    }

    // Handle single "#" pattern - matches any topic including empty
    if pattern == "#" {
        return true;
    }

    // Handle multi-level wildcard at the end
    if let Some(prefix) = pattern.strip_suffix("/#") {
        // Check if pattern also contains single-level wildcards
        if prefix.contains('+') {
            return matches_mixed_wildcards(pattern, topic);
        }

        if topic.starts_with(prefix) {
            // Check if the topic has the exact prefix or continues with '/'
            return topic.len() == prefix.len() || topic.chars().nth(prefix.len()) == Some('/');
        }
        return false;
    }

    // Handle single-level wildcards
    if pattern.contains('+') {
        return matches_with_single_level_wildcards(pattern, topic);
    }

    false
}

/// Helper function to match pattern parts with topic parts
/// Returns true if the first `count` parts of pattern match the topic parts
fn matches_pattern_parts(pattern_parts: &[&str], topic_parts: &[&str], count: usize) -> bool {
    if topic_parts.len() < count {
        return false;
    }

    for i in 0..count {
        if pattern_parts[i] != "+" && pattern_parts[i] != topic_parts[i] {
            return false;
        }
    }

    true
}

/// Match pattern with single-level wildcards (+)
fn matches_with_single_level_wildcards(pattern: &str, topic: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    // Must have the same number of levels for single-level wildcards
    if pattern_parts.len() != topic_parts.len() {
        return false;
    }

    matches_pattern_parts(&pattern_parts, &topic_parts, pattern_parts.len())
}

/// Match pattern with mixed wildcards (both + and #)
fn matches_mixed_wildcards(pattern: &str, topic: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    // The last part should be "#"
    if pattern_parts.last() != Some(&"#") {
        return false;
    }

    // Match the prefix parts (before the "#")
    let prefix_len = pattern_parts.len() - 1; // Exclude the "#" part
    matches_pattern_parts(&pattern_parts, &topic_parts, prefix_len)
}

/// Compile a pattern into a more efficient matcher
/// This can be used for frequently used patterns to avoid repeated parsing
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    parts: Vec<PatternPart>,
    has_multi_level_wildcard: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum PatternPart {
    Literal(String),
    SingleWildcard,
    MultiWildcard,
}

impl CompiledPattern {
    /// Compile a pattern string into an optimized matcher
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        if pattern.is_empty() {
            return Err(PatternError::EmptyPattern);
        }

        let mut parts = Vec::new();
        let mut has_multi_level_wildcard = false;

        for (index, part) in pattern.split('/').enumerate() {
            match part {
                "+" => parts.push(PatternPart::SingleWildcard),
                "#" => {
                    if index != pattern.split('/').count() - 1 {
                        return Err(PatternError::MultiLevelWildcardNotAtEnd);
                    }
                    parts.push(PatternPart::MultiWildcard);
                    has_multi_level_wildcard = true;
                }
                literal => {
                    if literal.contains('+') || literal.contains('#') {
                        return Err(PatternError::InvalidWildcardUsage);
                    }
                    parts.push(PatternPart::Literal(literal.to_string()));
                }
            }
        }

        Ok(Self {
            parts,
            has_multi_level_wildcard,
        })
    }

    /// Check if a topic matches this compiled pattern
    pub fn matches(&self, topic: &str) -> bool {
        let topic_parts: Vec<&str> = topic.split('/').collect();

        if self.has_multi_level_wildcard {
            return self.matches_with_multi_level(&topic_parts);
        }

        // For patterns without multi-level wildcards, part counts must match
        if self.parts.len() != topic_parts.len() {
            return false;
        }

        for (pattern_part, topic_part) in self.parts.iter().zip(topic_parts.iter()) {
            match pattern_part {
                PatternPart::Literal(literal) => {
                    if literal != topic_part {
                        return false;
                    }
                }
                PatternPart::SingleWildcard => {
                    // Single wildcard matches any single level
                    continue;
                }
                PatternPart::MultiWildcard => {
                    // This shouldn't happen as we handle multi-level separately
                    return true;
                }
            }
        }

        true
    }

    /// Handle matching with multi-level wildcard
    fn matches_with_multi_level(&self, topic_parts: &[&str]) -> bool {
        let pattern_parts_without_wildcard = &self.parts[..self.parts.len() - 1];

        // Topic must have at least as many parts as the pattern (excluding #)
        if topic_parts.len() < pattern_parts_without_wildcard.len() {
            return false;
        }

        // Check the parts before the multi-level wildcard
        for (pattern_part, topic_part) in pattern_parts_without_wildcard
            .iter()
            .zip(topic_parts.iter())
        {
            match pattern_part {
                PatternPart::Literal(literal) => {
                    if literal != topic_part {
                        return false;
                    }
                }
                PatternPart::SingleWildcard => {
                    // Single wildcard matches any single level
                    continue;
                }
                PatternPart::MultiWildcard => {
                    // This shouldn't happen
                    return true;
                }
            }
        }

        true
    }
}

/// Errors that can occur during pattern compilation
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PatternError {
    #[error("Pattern cannot be empty")]
    EmptyPattern,
    #[error("Multi-level wildcard (#) must be at the end of the pattern")]
    MultiLevelWildcardNotAtEnd,
    #[error("Wildcards (+, #) cannot be mixed with literal text in the same level")]
    InvalidWildcardUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(mqtt_pattern_matches(
            "sensor/temperature",
            "sensor/temperature"
        ));
        assert!(!mqtt_pattern_matches(
            "sensor/temperature",
            "sensor/humidity"
        ));
    }

    #[test]
    fn test_single_level_wildcard() {
        assert!(mqtt_pattern_matches(
            "sensor/+/temperature",
            "sensor/device1/temperature"
        ));
        assert!(mqtt_pattern_matches(
            "sensor/+/temperature",
            "sensor/device2/temperature"
        ));
        assert!(!mqtt_pattern_matches(
            "sensor/+/temperature",
            "sensor/device1/humidity"
        ));
        assert!(!mqtt_pattern_matches(
            "sensor/+/temperature",
            "sensor/device1/data/temperature"
        ));
    }

    #[test]
    fn test_multi_level_wildcard() {
        assert!(mqtt_pattern_matches(
            "sensor/#",
            "sensor/device1/temperature"
        ));
        assert!(mqtt_pattern_matches(
            "sensor/#",
            "sensor/device1/data/temperature"
        ));
        assert!(mqtt_pattern_matches("sensor/#", "sensor"));
        assert!(!mqtt_pattern_matches("sensor/#", "device/sensor"));
    }

    #[test]
    fn test_compiled_pattern() {
        let pattern = CompiledPattern::new("sensor/+/temperature").unwrap();
        assert!(pattern.matches("sensor/device1/temperature"));
        assert!(!pattern.matches("sensor/device1/humidity"));

        let multi_pattern = CompiledPattern::new("sensor/#").unwrap();
        assert!(multi_pattern.matches("sensor/device1/temperature"));
        assert!(multi_pattern.matches("sensor/device1/data/temperature"));
    }

    #[test]
    fn test_pattern_errors() {
        assert!(matches!(
            CompiledPattern::new(""),
            Err(PatternError::EmptyPattern)
        ));
        assert!(matches!(
            CompiledPattern::new("sensor/#/temperature"),
            Err(PatternError::MultiLevelWildcardNotAtEnd)
        ));
        assert!(matches!(
            CompiledPattern::new("sensor/device+/temperature"),
            Err(PatternError::InvalidWildcardUsage)
        ));
    }

    #[test]
    fn test_complex_patterns() {
        // Multiple single-level wildcards
        assert!(mqtt_pattern_matches(
            "building/+/floor/+/temperature",
            "building/A/floor/1/temperature"
        ));
        assert!(!mqtt_pattern_matches(
            "building/+/floor/+/temperature",
            "building/A/floor/1/humidity"
        ));

        // Mixed patterns
        assert!(mqtt_pattern_matches(
            "sensor/+/#",
            "sensor/device1/temperature/current"
        ));
        assert!(!mqtt_pattern_matches(
            "sensor/+/#",
            "device/sensor/temperature"
        ));
    }

    #[test]
    fn test_edge_cases() {
        // Empty topic parts
        assert!(mqtt_pattern_matches("+", "test"));
        assert!(mqtt_pattern_matches("#", ""));
        assert!(mqtt_pattern_matches("#", "any/topic/here"));

        // Root level topics
        assert!(mqtt_pattern_matches("+", "root"));
        assert!(!mqtt_pattern_matches("+", "root/sub"));
    }
}
