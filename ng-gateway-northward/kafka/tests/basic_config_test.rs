use ng_plugin_kafka::config::{KafkaPluginConfig, KafkaSecurityProtocol};

#[test]
fn test_config_deserialize_minimal_plaintext() {
    let raw = serde_json::json!({
        "connection": {
            "bootstrapServers": "127.0.0.1:9092",
            "security": { "protocol": "plaintext" }
        },
        "uplink": { "enabled": true },
        "downlink": { "enabled": false }
    });

    let cfg: KafkaPluginConfig = serde_json::from_value(raw).expect("deserialize config");
    assert_eq!(cfg.connection.bootstrap_servers, "127.0.0.1:9092");
    assert_eq!(
        cfg.connection.security.protocol,
        KafkaSecurityProtocol::Plaintext
    );
}
