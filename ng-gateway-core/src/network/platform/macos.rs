//! macOS read-only network manager fallback.
//!
//! Uses `networksetup` CLI to obtain per-service network details (IP, gateway, DNS, subnet mask).
//! The `network-interface` crate provides raw interface enumeration + MAC addresses.
//! Wi-Fi scanning uses the private `airport` CLI.
//! All write operations return `PlatformNotSupported`.

use crate::network::platform::{scan_wifi_native, PlatformNetworkManager};
use async_trait::async_trait;
use network_interface::{Addr, NetworkInterfaceConfig};
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest, DnsConfig,
    InterfaceKind, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config, LinkState,
    NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary, PlatformSupport,
    StaApCapability, WifiAccessPoint, WifiConnectRequest, WifiMode, WifiStaStatus,
};
use std::{collections::BTreeMap, net::IpAddr};
use tokio::process::Command;

/// macOS network manager (read-only).
pub struct MacosNetworkManager;

impl MacosNetworkManager {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacosNetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlatformNetworkManager for MacosNetworkManager {
    async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        let ni_interfaces = network_interface::NetworkInterface::show()
            .map_err(|e| NetworkError::DBusError(format!("Failed to enumerate interfaces: {e}")))?;

        let mut iface_map: BTreeMap<String, NetworkInterfaceSummary> = BTreeMap::new();

        for ni in &ni_interfaces {
            if ni.name == "lo0" || ni.name.starts_with("utun") || ni.name.starts_with("awdl") {
                continue;
            }

            let kind = classify_macos_interface(&ni.name);
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
                        display_name: display_name_for_macos(&ni.name),
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
                        let prefix = v4.netmask.map(netmask_to_prefix_v4).unwrap_or(24);
                        let info = Ipv4AddressInfo {
                            address: IpAddr::V4(v4.ip),
                            prefix_length: prefix,
                        };
                        entry
                            .ipv4
                            .get_or_insert_with(|| Ipv4Config {
                                addresses: Vec::new(),
                                gateway: None,
                                dns: Vec::new(),
                                method: IpMethod::Dhcp,
                            })
                            .addresses
                            .push(info);
                        entry.link_state = LinkState::Up;
                    }
                    Addr::V6(v6) => {
                        let prefix = v6.netmask.map(netmask_to_prefix_v6).unwrap_or(64);
                        let info = Ipv6AddressInfo {
                            address: IpAddr::V6(v6.ip),
                            prefix_length: prefix,
                        };
                        entry
                            .ipv6
                            .get_or_insert_with(|| Ipv6Config {
                                addresses: Vec::new(),
                                gateway: None,
                                dns: Vec::new(),
                                method: IpMethod::Dhcp,
                            })
                            .addresses
                            .push(info);
                        entry.link_state = LinkState::Up;
                    }
                }
            }

            if mac_address.is_some() && entry.mac_address.is_none() {
                entry.mac_address = mac_address;
            }
        }

        let mut interfaces: Vec<NetworkInterfaceSummary> = iface_map.into_values().collect();

        // Enrich each interface with per-service gateway/DNS via `networksetup`.
        // macOS maps BSD interface names to "network service" names.
        let service_map = build_service_map().await;

        for iface in &mut interfaces {
            if let Some(service_name) = service_map.get(&iface.name) {
                let info = get_networksetup_info(service_name).await;
                if let Some(ref mut ipv4) = iface.ipv4 {
                    if ipv4.gateway.is_none() {
                        ipv4.gateway = info.gateway;
                    }
                    if ipv4.dns.is_empty() {
                        ipv4.dns = info.dns;
                    }
                    if info.is_manual {
                        ipv4.method = IpMethod::Static;
                    }
                }
            }
        }

        // Enrich Wi-Fi with SSID.
        if let Ok(ssid) = get_current_wifi_ssid().await {
            for iface in &mut interfaces {
                if iface.kind == InterfaceKind::Wifi {
                    iface.connected_ssid = Some(ssid.clone());
                    iface.wifi_mode = Some(WifiMode::Station);
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
            .ok_or_else(|| NetworkError::InterfaceNotFound(name.to_string()))?;

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
        Ok(NetworkCapabilities {
            platform: PlatformSupport::ReadOnly,
            os: "macos".to_string(),
            arch: std::env::consts::ARCH.to_string(),
            network_manager_available: false,
            network_manager_version: None,
            can_configure_interfaces: false,
            can_scan_wifi: true,
            can_connect_wifi: false,
            can_manage_ap: false,
            sta_ap_capability: StaApCapability::Unknown,
            wireless_interfaces: Vec::new(),
        })
    }

    async fn configure_interface(
        &self,
        _name: &str,
        _config: &ConfigureInterfaceRequest,
    ) -> NGResult<()> {
        Err(NetworkError::PlatformNotSupported(
            "Interface configuration is not supported on macOS".to_string(),
        )
        .into())
    }

    async fn scan_wifi(&self, _interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        // Use wifi_scan crate (CoreWLAN native API) — works on macOS 13+.
        // Note: SSID visibility requires Location Services permission on macOS.
        scan_wifi_native().await
    }

    async fn connect_wifi(&self, _request: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        Err(NetworkError::PlatformNotSupported(
            "Wi-Fi connection is not supported on macOS from gateway".to_string(),
        )
        .into())
    }

    async fn disconnect_wifi(&self, _interface_name: Option<&str>) -> NGResult<()> {
        Err(NetworkError::PlatformNotSupported(
            "Wi-Fi disconnect is not supported on macOS from gateway".to_string(),
        )
        .into())
    }

    async fn wifi_sta_status(&self, _interface_name: Option<&str>) -> NGResult<WifiStaStatus> {
        let ssid = get_current_wifi_ssid().await.ok();
        let connected = ssid.is_some();

        // Get real Wi-Fi interface data (IP, gateway, DNS) via list_interfaces.
        let (ip_address, gateway, dns, _method, _prefix) = if connected {
            let interfaces = self.list_interfaces().await.unwrap_or_default();
            let wifi = interfaces.iter().find(|i| i.kind == InterfaceKind::Wifi);
            let ip = wifi
                .and_then(|w| w.ipv4.as_ref())
                .and_then(|v4| v4.addresses.first())
                .map(|a| a.address);
            let gw = wifi.and_then(|w| w.ipv4.as_ref()).and_then(|v4| v4.gateway);
            let d = wifi
                .and_then(|w| w.ipv4.as_ref())
                .map(|v4| v4.dns.clone())
                .unwrap_or_default();
            let m = wifi.and_then(|w| w.ipv4.as_ref()).map(|v4| v4.method);
            let p = wifi
                .and_then(|w| w.ipv4.as_ref())
                .and_then(|v4| v4.addresses.first())
                .map(|a| a.prefix_length);
            (ip, gw, d, m, p)
        } else {
            (None, None, Vec::new(), None, None)
        };

        Ok(WifiStaStatus {
            connected,
            interface_name: Some("en0".to_string()),
            ssid,
            bssid: None,
            security: None,
            band: None,
            channel: None,
            frequency: None,
            signal_dbm: None,
            signal_quality: None,
            ip_address,
            gateway,
            dns,
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
        })
    }

    async fn configure_ap(&self, _config: &ConfigureApRequest) -> NGResult<ApStatus> {
        Err(NetworkError::PlatformNotSupported(
            "AP management is not supported on macOS".to_string(),
        )
        .into())
    }

    async fn get_dns(&self) -> NGResult<DnsConfig> {
        // Aggregate DNS from all active interfaces.
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

    async fn configure_dns(&self, _config: &ConfigureDnsRequest) -> NGResult<()> {
        Err(NetworkError::PlatformNotSupported(
            "DNS configuration is not supported on macOS from gateway".to_string(),
        )
        .into())
    }
}

// ─── macOS System Command Helpers ───

/// Result of `networksetup -getinfo <service>`.
struct NetworkSetupInfo {
    gateway: Option<IpAddr>,
    dns: Vec<IpAddr>,
    is_manual: bool,
}

/// Build a mapping from BSD interface name → macOS network service name.
///
/// Uses `networksetup -listallhardwareports` which outputs:
/// ```text
/// Hardware Port: Wi-Fi
/// Device: en0
/// Ethernet Address: ...
/// ```
async fn build_service_map() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let output = match Command::new("networksetup")
        .arg("-listallhardwareports")
        .output()
        .await
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return map,
    };

    let mut current_service: Option<String> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(port) = trimmed.strip_prefix("Hardware Port: ") {
            current_service = Some(port.to_string());
        } else if let Some(dev) = trimmed.strip_prefix("Device: ") {
            if let Some(ref svc) = current_service {
                map.insert(dev.trim().to_string(), svc.clone());
            }
        }
    }
    map
}

/// Get IP config for a macOS network service via `networksetup -getinfo <service>`.
///
/// Output format:
/// ```text
/// DHCP Configuration
/// IP address: 192.168.66.68
/// Subnet mask: 255.255.255.0
/// Router: 192.168.66.1
/// ...
/// ```
///
/// Also reads DNS via `networksetup -getdnsservers <service>`.
async fn get_networksetup_info(service: &str) -> NetworkSetupInfo {
    let mut info = NetworkSetupInfo {
        gateway: None,
        dns: Vec::new(),
        is_manual: false,
    };

    // Get IP info.
    if let Ok(output) = Command::new("networksetup")
        .args(["-getinfo", service])
        .output()
        .await
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("Manual Configuration") {
                info.is_manual = true;
            }
            if let Some(router) = trimmed.strip_prefix("Router: ") {
                info.gateway = router.trim().parse::<IpAddr>().ok();
            }
        }
    }

    // Get DNS servers.
    if let Ok(output) = Command::new("networksetup")
        .args(["-getdnsservers", service])
        .output()
        .await
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains("aren't any") {
            for line in stdout.lines() {
                if let Ok(addr) = line.trim().parse::<IpAddr>() {
                    info.dns.push(addr);
                }
            }
        }
    }

    info
}

/// Classify macOS interface by name convention.
fn classify_macos_interface(name: &str) -> InterfaceKind {
    match name {
        "lo0" => InterfaceKind::Loopback,
        n if n.starts_with("en") => {
            if n == "en0" {
                InterfaceKind::Wifi
            } else {
                InterfaceKind::Ethernet
            }
        }
        n if n.starts_with("bridge") => InterfaceKind::Bridge,
        _ => InterfaceKind::Unknown,
    }
}

/// Generate human-readable display name for macOS interfaces.
fn display_name_for_macos(name: &str) -> Option<String> {
    match name {
        "en0" => Some("Wi-Fi".to_string()),
        "en1" => Some("Ethernet".to_string()),
        n if n.starts_with("bridge") => Some("Bridge".to_string()),
        _ => None,
    }
}

fn netmask_to_prefix_v4(mask: std::net::Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

fn netmask_to_prefix_v6(mask: std::net::Ipv6Addr) -> u8 {
    u128::from(mask).count_ones() as u8
}

/// Get current Wi-Fi SSID via `networksetup -getairportnetwork en0`.
async fn get_current_wifi_ssid() -> Result<String, NetworkError> {
    let output = Command::new("networksetup")
        .args(["-getairportnetwork", "en0"])
        .output()
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: "networksetup -getairportnetwork en0".to_string(),
            reason: e.to_string(),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(ssid) = stdout.strip_prefix("Current Wi-Fi Network: ") {
        let ssid = ssid.trim();
        if ssid.is_empty() || ssid.contains("not associated") {
            return Err(NetworkError::WifiError("Not connected".to_string()));
        }
        Ok(ssid.to_string())
    } else {
        Err(NetworkError::WifiError(
            "Unable to parse networksetup output".to_string(),
        ))
    }
}
