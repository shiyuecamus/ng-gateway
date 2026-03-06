//! Internal types used across the network module.
//!
//! Re-exports domain types from `ng-gateway-models` for convenience,
//! and defines any module-internal helpers that don't belong in the public API.

pub use ng_gateway_models::domain::prelude::{
    ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest, DnsConfig,
    InterfaceKind, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config, LinkState,
    NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary, PlatformSupport,
    StaApCapability, WifiAccessPoint, WifiBand, WifiConnectRequest, WifiMode, WifiSecurity,
    WifiStaStatus, WirelessInterfaceCapability,
};
