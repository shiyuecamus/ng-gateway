//! ThingsBoard MQTT topic constants
//!
//! This module contains all the MQTT topic constants used for communication
//! with ThingsBoard platform, including device and gateway topics.

/// Base topic components
#[allow(unused)]
pub const REQUEST: &str = "/request";
#[allow(unused)]
pub const RESPONSE: &str = "/response";
#[allow(unused)]
pub const RPC: &str = "/rpc";
#[allow(unused)]
pub const CONNECT: &str = "/connect";
#[allow(unused)]
pub const DISCONNECT: &str = "/disconnect";
#[allow(unused)]
pub const TELEMETRY: &str = "/telemetry";
#[allow(unused)]
pub const ATTRIBUTES: &str = "/attributes";
#[allow(unused)]
pub const CLAIM: &str = "/claim";
#[allow(unused)]
pub const SUB_TOPIC: &str = "+";
#[allow(unused)]
pub const PROVISION: &str = "/provision";
#[allow(unused)]
pub const FIRMWARE: &str = "/fw";
#[allow(unused)]
pub const SOFTWARE: &str = "/sw";
#[allow(unused)]
pub const CHUNK: &str = "/chunk/";
#[allow(unused)]
pub const ERROR: &str = "/error";

/// Short topic components
#[allow(unused)]
pub const TELEMETRY_SHORT: &str = "/t";
#[allow(unused)]
pub const ATTRIBUTES_SHORT: &str = "/a";
#[allow(unused)]
pub const RPC_SHORT: &str = "/r";
#[allow(unused)]
pub const REQUEST_SHORT: &str = "/req";
#[allow(unused)]
pub const RESPONSE_SHORT: &str = "/res";
#[allow(unused)]
pub const JSON_SHORT: &str = "j";
#[allow(unused)]
pub const PROTO_SHORT: &str = "p";

/// Base API topics - modify these to change all related topics
#[allow(unused)]
pub const BASE_DEVICE_API_TOPIC: &str = "v1/devices/me";
#[allow(unused)]
pub const BASE_GATEWAY_API_TOPIC: &str = "v1/gateway";

/// Composite topic components - manually concatenated for clarity
#[allow(unused)]
pub const ATTRIBUTES_RESPONSE: &str = "/attributes/response";
#[allow(unused)]
pub const ATTRIBUTES_REQUEST: &str = "/attributes/request";
#[allow(unused)]
pub const ATTRIBUTES_RESPONSE_SHORT: &str = "/a/res/";
#[allow(unused)]
pub const ATTRIBUTES_REQUEST_SHORT: &str = "/a/req/";
#[allow(unused)]
pub const DEVICE_RPC_RESPONSE: &str = "/rpc/response/";
#[allow(unused)]
pub const DEVICE_RPC_REQUEST: &str = "/rpc/request/";
#[allow(unused)]
pub const DEVICE_RPC_RESPONSE_SHORT: &str = "/r/res/";
#[allow(unused)]
pub const DEVICE_RPC_REQUEST_SHORT: &str = "/r/req/";
#[allow(unused)]
pub const DEVICE_ATTRIBUTES_RESPONSE: &str = "/attributes/response/";
#[allow(unused)]
pub const DEVICE_ATTRIBUTES_REQUEST: &str = "/attributes/request/";

/// Topic builder for dynamic topic construction
pub struct Topics;

#[allow(unused)]
impl Topics {
    // Device API topic builders
    pub fn device_telemetry() -> String {
        format!("{}{}", BASE_DEVICE_API_TOPIC, TELEMETRY)
    }

    pub fn device_attributes() -> String {
        format!("{}{}", BASE_DEVICE_API_TOPIC, ATTRIBUTES)
    }

    pub fn device_claim() -> String {
        format!("{}{}", BASE_DEVICE_API_TOPIC, CLAIM)
    }

    pub fn gateway_rpc_response_topic() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, RPC)
    }

    pub fn gateway_rpc_response_sub() -> String {
        format!(
            "{}{}{}",
            BASE_GATEWAY_API_TOPIC, DEVICE_RPC_RESPONSE, SUB_TOPIC
        )
    }

    pub fn gateway_rpc_request_topic(request_id: &str) -> String {
        format!(
            "{}{}{}",
            BASE_GATEWAY_API_TOPIC, DEVICE_RPC_REQUEST, request_id
        )
    }

    pub fn gateway_rpc_request_sub() -> String {
        format!(
            "{}{}{}",
            BASE_GATEWAY_API_TOPIC, DEVICE_RPC_REQUEST, SUB_TOPIC
        )
    }

    pub fn device_rpc_response_topic(request_id: &str) -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_RPC_RESPONSE, request_id
        )
    }

    pub fn device_rpc_response_sub() -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_RPC_RESPONSE, SUB_TOPIC
        )
    }

    pub fn device_rpc_request_topic(request_id: &str) -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_RPC_REQUEST, request_id
        )
    }

    pub fn device_rpc_request_sub() -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_RPC_REQUEST, SUB_TOPIC
        )
    }

    pub fn device_attributes_response_topic(request_id: &str) -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_ATTRIBUTES_RESPONSE, request_id
        )
    }

    pub fn device_attributes_request_topic(request_id: &str) -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_ATTRIBUTES_REQUEST, request_id
        )
    }

    pub fn device_attributes_request_sub() -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_ATTRIBUTES_REQUEST, SUB_TOPIC
        )
    }

    pub fn device_attributes_response_sub() -> String {
        format!(
            "{}{}{}",
            BASE_DEVICE_API_TOPIC, DEVICE_ATTRIBUTES_RESPONSE, SUB_TOPIC
        )
    }

    // Gateway API topic builders
    pub fn gateway_connect() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, CONNECT)
    }

    pub fn gateway_disconnect() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, DISCONNECT)
    }

    pub fn gateway_telemetry() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, TELEMETRY)
    }

    pub fn gateway_attributes() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, ATTRIBUTES)
    }

    pub fn gateway_claim() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, CLAIM)
    }

    pub fn gateway_rpc() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, RPC)
    }

    pub fn gateway_attributes_request() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, ATTRIBUTES_REQUEST)
    }

    pub fn gateway_attributes_response() -> String {
        format!("{}{}", BASE_GATEWAY_API_TOPIC, ATTRIBUTES_RESPONSE)
    }

    // Provision topics
    pub fn device_provision_request() -> String {
        format!("{}{}", PROVISION, REQUEST)
    }

    pub fn device_provision_response() -> String {
        format!("{}{}", PROVISION, RESPONSE)
    }

    // Custom base topic builders
    pub fn with_custom_device_base(base: &str, suffix: &str) -> String {
        format!("{}{}", base, suffix)
    }

    pub fn with_custom_gateway_base(base: &str, suffix: &str) -> String {
        format!("{}{}", base, suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_topic_constants() {
        assert_eq!(BASE_DEVICE_API_TOPIC, "v1/devices/me");
        assert_eq!(BASE_GATEWAY_API_TOPIC, "v1/gateway");
    }

    #[test]
    fn test_component_constants() {
        assert_eq!(TELEMETRY, "/telemetry");
        assert_eq!(ATTRIBUTES, "/attributes");
        assert_eq!(RPC, "/rpc");
        assert_eq!(REQUEST, "/request");
        assert_eq!(RESPONSE, "/response");
    }

    #[test]
    fn test_device_topic_builders() {
        assert_eq!(Topics::device_telemetry(), "v1/devices/me/telemetry");
        assert_eq!(Topics::device_attributes(), "v1/devices/me/attributes");
        assert_eq!(Topics::device_claim(), "v1/devices/me/claim");

        let request_id = "123";
        assert_eq!(
            Topics::device_rpc_response_topic(request_id),
            "v1/devices/me/rpc/response/123"
        );
        assert_eq!(
            Topics::device_rpc_response_sub(),
            "v1/devices/me/rpc/response/+"
        );
        assert_eq!(
            Topics::device_attributes_response_topic(request_id),
            "v1/devices/me/attributes/response/123"
        );
        assert_eq!(
            Topics::device_attributes_request_sub(),
            "v1/devices/me/attributes/request/+"
        );
        assert_eq!(
            Topics::device_attributes_response_sub(),
            "v1/devices/me/attributes/response/+"
        );
    }

    #[test]
    fn test_gateway_topic_builders() {
        assert_eq!(Topics::gateway_telemetry(), "v1/gateway/telemetry");
        assert_eq!(Topics::gateway_attributes(), "v1/gateway/attributes");
        assert_eq!(Topics::gateway_connect(), "v1/gateway/connect");
        assert_eq!(Topics::gateway_disconnect(), "v1/gateway/disconnect");
        assert_eq!(Topics::gateway_rpc(), "v1/gateway/rpc");
        assert_eq!(
            Topics::gateway_attributes_request(),
            "v1/gateway/attributes/request"
        );
        assert_eq!(
            Topics::gateway_attributes_response(),
            "v1/gateway/attributes/response"
        );
    }

    #[test]
    fn test_provision_topics() {
        assert_eq!(Topics::device_provision_request(), "/provision/request");
        assert_eq!(Topics::device_provision_response(), "/provision/response");
    }

    #[test]
    fn test_custom_base_builders() {
        let custom_base = "v2/devices/custom";
        assert_eq!(
            Topics::with_custom_device_base(custom_base, TELEMETRY),
            "v2/devices/custom/telemetry"
        );

        let custom_gateway = "v2/gateway/custom";
        assert_eq!(
            Topics::with_custom_gateway_base(custom_gateway, RPC),
            "v2/gateway/custom/rpc"
        );
    }

    #[test]
    fn test_base_topic_change_propagation() {
        // This test demonstrates that if you change BASE_DEVICE_API_TOPIC,
        // all related topics will automatically update
        let original_base = BASE_DEVICE_API_TOPIC;
        let new_base = "v2/devices/me";

        // Simulate what would happen if we changed the base
        let telemetry_with_new_base = format!("{new_base}{}", TELEMETRY);
        let rpc_response_with_new_base = format!("{new_base}{}", DEVICE_RPC_RESPONSE);

        assert_eq!(telemetry_with_new_base, "v2/devices/me/telemetry");
        assert_eq!(rpc_response_with_new_base, "v2/devices/me/rpc/response/");

        // Verify original still works
        assert_eq!(
            Topics::device_telemetry(),
            format!("{original_base}{}", TELEMETRY)
        );
    }
}
