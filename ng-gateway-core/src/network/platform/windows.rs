//! Windows read-only network manager fallback.
//!
//! Uses the `network-interface` crate for interface/address enumeration,
//! and `netsh` CLI for gateway, DNS, and Wi-Fi status.
//! All write operations return `PlatformNotSupported`.

use crate::network::platform::{scan_wifi_native, PlatformNetworkManager};
use async_trait::async_trait;
use network_interface::{Addr, NetworkInterfaceConfig};
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApMode, ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest,
    DnsConfig, InterfaceKind, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config,
    LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    PlatformSupport, StaApCapability, WifiAccessPoint, WifiConnectRequest, WifiStaStatus,
};
use std::{collections::BTreeMap, net::IpAddr};
use tokio::process::Command;
use tracing::debug;

/// Windows network manager (read-only).
pub struct WindowsNetworkManager;

impl WindowsNetworkManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsNetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlatformNetworkManager for WindowsNetworkManager {
    async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        let ni_interfaces = network_interface::NetworkInterface::show()
            .map_err(|e| NetworkError::DBusError(format!("Failed to enumerate interfaces: {e}")))?;

        let mut iface_map: BTreeMap<String, NetworkInterfaceSummary> = BTreeMap::new();

        for ni in &ni_interfaces {
            let kind = classify_windows_interface(&ni.name);
            if kind == InterfaceKind::Loopback {
                continue;
            }

            let mac_address = ni.mac_addr.as_ref().and_then(|m| {
                if m == "00:00:00:00:00:00" {
                    None
                } else {
                    Some(m.clone())
                }
            });

            let entry =
                iface_map
                    .entry(ni.name.clone())
                    .or_insert_with(|| NetworkInterfaceSummary {
                        name: ni.name.clone(),
                        display_name: None,
                        kind,
                        link_state: if ni.addr.is_empty() {
                            LinkState::Down
                        } else {
                            LinkState::Up
                        },
                        mac_address: mac_address.clone(),
                        ipv4: None,
                        ipv6: None,
                        wifi_mode: None,
                        connected_ssid: None,
                        ap_ssid: None,
                        signal_dbm: None,
                        signal_quality: None,
                        speed_mbps: None,
                        rx_bytes: None,
                        tx_bytes: None,
                    });

            for addr in &ni.addr {
                match addr {
                    Addr::V4(v4) => {
                        let prefix = v4
                            .netmask
                            .map(|m| u32::from(m).count_ones() as u8)
                            .unwrap_or(24);
                        entry
                            .ipv4
                            .get_or_insert_with(|| Ipv4Config {
                                addresses: Vec::new(),
                                gateway: None,
                                dns: Vec::new(),
                                method: IpMethod::Dhcp,
                            })
                            .addresses
                            .push(Ipv4AddressInfo {
                                address: IpAddr::V4(v4.ip),
                                prefix_length: prefix,
                            });
                        entry.link_state = LinkState::Up;
                    }
                    Addr::V6(v6) => {
                        let prefix = v6
                            .netmask
                            .map(|m| u128::from(m).count_ones() as u8)
                            .unwrap_or(64);
                        entry
                            .ipv6
                            .get_or_insert_with(|| Ipv6Config {
                                addresses: Vec::new(),
                                gateway: None,
                                dns: Vec::new(),
                                method: IpMethod::Dhcp,
                            })
                            .addresses
                            .push(Ipv6AddressInfo {
                                address: IpAddr::V6(v6.ip),
                                prefix_length: prefix,
                            });
                        entry.link_state = LinkState::Up;
                    }
                }
            }

            if mac_address.is_some() && entry.mac_address.is_none() {
                entry.mac_address = mac_address;
            }
        }

        let mut interfaces: Vec<NetworkInterfaceSummary> = iface_map.into_values().collect();

        // Enrich with gateway/DNS from `netsh`.
        for iface in &mut interfaces {
            if iface.link_state != LinkState::Up {
                continue;
            }
            let info = get_netsh_info(&iface.name).await;
            if let Some(ref mut ipv4) = iface.ipv4 {
                if ipv4.gateway.is_none() {
                    ipv4.gateway = info.gateway;
                }
                if ipv4.dns.is_empty() {
                    ipv4.dns = info.dns;
                }
                if info.is_static {
                    ipv4.method = IpMethod::Static;
                }
            }
        }

        // Enrich Wi-Fi with SSID.
        if let Some(ssid) = get_wifi_ssid().await {
            for iface in &mut interfaces {
                if iface.kind == InterfaceKind::Wifi {
                    iface.connected_ssid = Some(ssid.clone());
                    iface.wifi_mode = Some(ng_gateway_models::domain::prelude::WifiMode::Station);
                    break;
                }
            }
        }

        Ok(interfaces)
    }

    async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        let interfaces = self.list_interfaces().await?;
        let summary = interfaces
            .into_iter()
            .find(|i| i.name == name)
            .ok_or(NetworkError::InterfaceNotFound(name.to_string()))?;
        Ok(NetworkInterfaceDetail {
            summary,
            nm_connection_uuid: None,
            mtu: None,
            driver: None,
            firmware_version: None,
            rx_packets: None,
            tx_packets: None,
            rx_errors: None,
            tx_errors: None,
        })
    }

    async fn detect_capabilities(&self) -> NGResult<NetworkCapabilities> {
        let interfaces = self.list_interfaces().await.unwrap_or_default();
        let has_wifi = interfaces.iter().any(|i| i.kind == InterfaceKind::Wifi);

        Ok(NetworkCapabilities {
            platform: PlatformSupport::ReadOnly,
            os: "windows".to_string(),
            arch: std::env::consts::ARCH.to_string(),
            network_manager_available: false,
            network_manager_version: None,
            can_configure_interfaces: false,
            can_scan_wifi: has_wifi,
            can_connect_wifi: false,
            can_manage_ap: false,
            ap_mode: ApMode::Unavailable,
            sta_ap_capability: StaApCapability::Unknown,
            wireless_interfaces: Vec::new(),
        })
    }

    async fn configure_interface(&self, _: &str, _: &ConfigureInterfaceRequest) -> NGResult<()> {
        Err(NetworkError::PlatformNotSupported(
            "Interface configuration is not supported on Windows".into(),
        )
        .into())
    }

    async fn scan_wifi(&self, _: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        scan_wifi_native().await
    }

    async fn connect_wifi(&self, _: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        Err(NetworkError::PlatformNotSupported(
            "Wi-Fi connection is not supported on Windows from gateway".into(),
        )
        .into())
    }

    async fn disconnect_wifi(&self, _: Option<&str>) -> NGResult<()> {
        Err(NetworkError::PlatformNotSupported(
            "Wi-Fi disconnect is not supported on Windows from gateway".into(),
        )
        .into())
    }

    async fn wifi_sta_status(&self, _: Option<&str>) -> NGResult<WifiStaStatus> {
        let interfaces = self.list_interfaces().await.unwrap_or_default();
        let wifi = interfaces.iter().find(|i| i.kind == InterfaceKind::Wifi);
        let connected = wifi.is_some_and(|w| w.connected_ssid.is_some());

        Ok(WifiStaStatus {
            connected,
            interface_name: wifi.map(|w| w.name.clone()),
            ssid: wifi.and_then(|w| w.connected_ssid.clone()),
            bssid: None,
            security: None,
            band: None,
            channel: None,
            frequency: None,
            signal_dbm: None,
            signal_quality: None,
            ip_address: wifi
                .and_then(|w| w.ipv4.as_ref())
                .and_then(|v4| v4.addresses.first())
                .map(|a| a.address),
            gateway: wifi.and_then(|w| w.ipv4.as_ref()).and_then(|v4| v4.gateway),
            dns: wifi
                .and_then(|w| w.ipv4.as_ref())
                .map(|v4| v4.dns.clone())
                .unwrap_or_default(),
            speed_mbps: None,
            connected_secs: None,
        })
    }

    async fn ap_status(&self) -> NGResult<ApStatus> {
        Ok(ApStatus {
            active: false,
            interface_name: None,
            ssid: None,
            band: None,
            channel: None,
            frequency: None,
            security: None,
            connected_clients: None,
            ip_address: None,
            prefix_length: None,
            ap_mode: ApMode::Unavailable,
            sta_will_disconnect: false,
        })
    }

    async fn start_ap(&self) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP management is not supported on Windows".into())
                .into(),
        )
    }

    async fn stop_ap(&self) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP management is not supported on Windows".into())
                .into(),
        )
    }

    async fn configure_ap(&self, _: &ConfigureApRequest) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP management is not supported on Windows".into())
                .into(),
        )
    }

    async fn get_dns(&self) -> NGResult<DnsConfig> {
        let interfaces = self.list_interfaces().await.unwrap_or_default();
        let mut servers = Vec::new();
        for iface in &interfaces {
            if let Some(ref ipv4) = iface.ipv4 {
                for d in &ipv4.dns {
                    if !servers.contains(d) {
                        servers.push(*d);
                    }
                }
            }
        }
        Ok(DnsConfig {
            servers,
            search_domains: Vec::new(),
            mode: IpMethod::Dhcp,
        })
    }

    async fn configure_dns(&self, _: &ConfigureDnsRequest) -> NGResult<()> {
        Err(NetworkError::PlatformNotSupported(
            "DNS configuration is not supported on Windows from gateway".into(),
        )
        .into())
    }
}

// ─── Windows System Command Helpers ───

struct NetshInfo {
    gateway: Option<IpAddr>,
    dns: Vec<IpAddr>,
    is_static: bool,
}

/// Parse `netsh interface ipv4 show config name="<iface>"` output.
async fn get_netsh_info(iface_name: &str) -> NetshInfo {
    let mut info = NetshInfo {
        gateway: None,
        dns: Vec::new(),
        is_static: false,
    };

    let output = match Command::new("netsh")
        .args([
            "interface",
            "ipv4",
            "show",
            "config",
            &format!("name=\"{iface_name}\""),
        ])
        .output()
        .await
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return info,
    };

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("DHCP") && trimmed.contains("No") {
            info.is_static = true;
        }
        if trimmed.starts_with("Default Gateway") || trimmed.starts_with("默认网关") {
            if let Some(ip_str) = trimmed.split_whitespace().last() {
                info.gateway = ip_str.parse().ok();
            }
        }
        if (trimmed.starts_with("DNS") || trimmed.starts_with("Statically Configured DNS"))
            && !trimmed.contains("Search")
        {
            if let Some(ip_str) = trimmed.split_whitespace().last() {
                if let Ok(ip) = ip_str.parse::<IpAddr>() {
                    info.dns.push(ip);
                }
            }
        }
    }
    info
}

/// Get currently connected Wi-Fi SSID via `netsh wlan show interfaces`.
async fn get_wifi_ssid() -> Option<String> {
    let output = Command::new("netsh")
        .args(["wlan", "show", "interfaces"])
        .output()
        .await
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(ssid) = trimmed
            .strip_prefix("SSID")
            .or(trimmed.strip_prefix("SSID"))
        {
            let ssid = ssid
                .trim_start_matches(|c: char| c == ':' || c == ' ')
                .trim();
            if !ssid.is_empty() && !ssid.contains("BSSID") {
                return Some(ssid.to_string());
            }
        }
    }
    None
}

fn classify_windows_interface(name: &str) -> InterfaceKind {
    let lower = name.to_lowercase();
    if lower.contains("loopback") {
        InterfaceKind::Loopback
    } else if lower.contains("wi-fi") || lower.contains("wlan") || lower.contains("wireless") {
        InterfaceKind::Wifi
    } else if lower.contains("ethernet") || lower.contains("eth") || lower.contains("local area") {
        InterfaceKind::Ethernet
    } else {
        InterfaceKind::Unknown
    }
}
