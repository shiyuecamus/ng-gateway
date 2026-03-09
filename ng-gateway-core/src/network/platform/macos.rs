//! macOS network platform implementation.
//!
//! Primary API sources:
//! - **CoreWLAN** (`objc2-core-wlan`): Wi-Fi STA status, connect, disconnect,
//!   scan, saved network profiles, power management.
//! - **SystemConfiguration** (`system-configuration` + `core-foundation`):
//!   BSD-interface → network-service mapping, IPv4/IPv6/DNS/Gateway reading,
//!   link-state monitoring.
//! - **`network-interface`** crate: Cross-platform interface/address enumeration
//!   (supplements SC for address details).
//!
//! CLI tools (`networksetup`, `ifconfig`) are retained **only** as a thin
//! fallback layer for IP configuration writes (DHCP/Static/DNS), because
//! SystemConfiguration's `SCNetworkConfigurationOverride` API requires
//! entitlements that a non-bundled binary typically does not have.

use crate::network::platform::{self, PlatformNetworkManager};
use async_trait::async_trait;
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApMode, ApStatus, ConfigureApRequest, ConfigureInterfaceRequest, ForgetWifiRequest,
    InterfaceKind, IpConfig, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config,
    LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    PlatformSupport, SavedWifiConnection, StaApCapability, WifiAccessPoint, WifiBand,
    WifiConnectPreflight, WifiConnectRequest, WifiDisconnectRequest, WifiMode, WifiSecurity,
    WifiStaStatus, WirelessInterfaceCapability,
};
use objc2::rc::Retained;
use objc2_core_wlan::{
    CWChannelBand, CWConfiguration, CWInterface, CWNetwork, CWNetworkProfile, CWSecurity,
    CWWiFiClient,
};
use objc2_foundation::{NSData, NSOrderedSet, NSSet, NSString};
use std::{
    collections::{BTreeMap, HashSet},
    env,
    ffi::c_void,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    process::Command as StdCommand,
    time::{Duration, Instant},
};
use system_configuration::core_foundation::{
    array::CFArray,
    base::TCFType,
    dictionary::CFDictionary,
    string::{CFString, CFStringRef},
};
use system_configuration::{
    dynamic_store::SCDynamicStoreBuilder, network_configuration::SCNetworkService,
    preferences::SCPreferences,
};
use tokio::process::Command;
use tracing::{debug, info};

/// macOS network manager backed by CoreWLAN + SystemConfiguration.
///
/// All Wi-Fi control-plane operations go through CoreWLAN natively.
/// Interface/IP discovery uses SystemConfiguration + `network-interface` crate.
/// IP configuration writes still delegate to `networksetup` CLI as a pragmatic
/// fallback (the SC write APIs require root + entitlements on modern macOS).
pub struct MacosNetworkManager;

impl Default for MacosNetworkManager {
    fn default() -> Self {
        Self
    }
}

impl MacosNetworkManager {
    pub fn new() -> Self {
        Self
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CoreWLAN helpers — run blocking ObjC calls on a dedicated thread.
//
// CoreWLAN methods are synchronous and must not block the Tokio runtime.
// We use `spawn_blocking` consistently to isolate them.
// ═══════════════════════════════════════════════════════════════════════════

/// Obtain the shared `CWWiFiClient` singleton and execute `f` with the
/// default `CWInterface`.
///
/// Returns `Err` if no Wi-Fi interface is available.
async fn with_default_interface<F, T>(f: F) -> Result<T, NetworkError>
where
    F: FnOnce(&CWInterface) -> Result<T, NetworkError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let client = unsafe { CWWiFiClient::sharedWiFiClient() };
        let iface = unsafe { client.interface() }.ok_or(NetworkError::InterfaceNotFound(
            "No Wi-Fi interface".to_string(),
        ))?;
        f(&iface)
    })
    .await
    .map_err(|e| NetworkError::CommandFailed {
        command: "CoreWLAN spawn_blocking".to_string(),
        reason: format!("task join failed: {e}"),
    })?
}

/// Obtain the shared `CWWiFiClient` singleton and resolve a specific
/// interface by name, falling back to the default if `name` is `None`.
async fn with_named_interface<F, T>(name: Option<&str>, f: F) -> Result<T, NetworkError>
where
    F: FnOnce(&CWInterface) -> Result<T, NetworkError> + Send + 'static,
    T: Send + 'static,
{
    let name_owned = name.map(|s| s.to_string());
    tokio::task::spawn_blocking(move || {
        let client = unsafe { CWWiFiClient::sharedWiFiClient() };
        let iface = if let Some(ref n) = name_owned {
            let ns = NSString::from_str(n);
            unsafe { client.interfaceWithName(Some(&ns)) }.ok_or(
                NetworkError::InterfaceNotFound(format!("Wi-Fi interface '{n}' not found")),
            )?
        } else {
            unsafe { client.interface() }.ok_or(NetworkError::InterfaceNotFound(
                "No Wi-Fi interface".to_string(),
            ))?
        };
        f(&iface)
    })
    .await
    .map_err(|e| NetworkError::CommandFailed {
        command: "CoreWLAN spawn_blocking".to_string(),
        reason: format!("task join failed: {e}"),
    })?
}

// ═══════════════════════════════════════════════════════════════════════════
// PlatformNetworkManager implementation
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl PlatformNetworkManager for MacosNetworkManager {
    // ─── Discovery ───

    async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        let ni_interfaces = NetworkInterface::show().map_err(|e| NetworkError::CommandFailed {
            command: "NetworkInterface::show".to_string(),
            reason: format!("Failed to enumerate interfaces: {e}"),
        })?;

        let mut service_map = build_service_map_native();
        // Merge networksetup-derived map for interfaces missing from native (e.g. when
        // SCPreferences lacks access). Native entries take precedence.
        for (bsd, info) in build_service_map_from_networksetup() {
            service_map.entry(bsd).or_insert(info);
        }
        let sc_ip_map = read_sc_ip_state();

        // CoreWLAN is the authoritative source for Wi-Fi interface names on macOS.
        let wifi_names: HashSet<String> = tokio::task::spawn_blocking(|| {
            let client = unsafe { CWWiFiClient::sharedWiFiClient() };
            let names = unsafe { client.interfaceNames() };
            names
                .map(|arr| {
                    arr.to_vec()
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();

        // Merge multi-address entries from network-interface crate.
        let mut merged: BTreeMap<String, NetworkInterfaceSummary> = BTreeMap::new();
        for ni in &ni_interfaces {
            if should_skip_interface(&ni.name) {
                continue;
            }
            let kind = classify_interface(&ni.name, &service_map, &wifi_names);
            let entry = merged.entry(ni.name.clone()).or_insert_with(|| {
                let display = service_map
                    .get(&ni.name)
                    .map(|s| s.service_name.clone())
                    .or(Some(ni.name.clone()));
                NetworkInterfaceSummary {
                    name: ni.name.clone(),
                    display_name: display,
                    kind,
                    link_state: LinkState::Down,
                    mac_address: ni.mac_addr.clone(),
                    ipv4: None,
                    ipv6: None,
                    wifi_mode: if kind == InterfaceKind::Wifi {
                        Some(WifiMode::Station)
                    } else {
                        None
                    },
                    connected_ssid: None,
                    ap_ssid: None,
                    signal_dbm: None,
                    signal_quality: None,
                    speed_mbps: None,
                    rx_bytes: None,
                    tx_bytes: None,
                }
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
                                search_domains: Vec::new(),
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
        }

        // Enrich with SystemConfiguration dynamic store data (gateway, DNS, method).
        for (iface_name, summary) in &mut merged {
            if let Some(sc) = sc_ip_map.get(iface_name.as_str()) {
                if let Some(ref mut v4) = summary.ipv4 {
                    if v4.gateway.is_none() {
                        v4.gateway = sc.gateway;
                    }
                    if v4.dns.is_empty() {
                        v4.dns.clone_from(&sc.dns);
                    }
                    if !sc.search_domains.is_empty() {
                        v4.search_domains.clone_from(&sc.search_domains);
                    }
                    v4.method = sc.method;
                }
            }

            // For Wi-Fi interfaces: enrich with CoreWLAN status (SSID, signal).
            if summary.kind == InterfaceKind::Wifi {
                if let Ok(wifi_info) = get_corewlan_iface_status(Some(iface_name)).await {
                    summary.connected_ssid = wifi_info.ssid.clone();
                    summary.signal_dbm = wifi_info.signal_dbm;
                    summary.signal_quality = wifi_info.signal_quality;
                    summary.speed_mbps = wifi_info.speed_mbps;
                }
            }

            // Link state fallback via ifconfig when no IP address is present.
            if summary.link_state == LinkState::Down {
                if let Some(up) = check_interface_up(iface_name).await {
                    if up {
                        summary.link_state = LinkState::Up;
                    }
                }
            }
        }

        let mut interfaces: Vec<NetworkInterfaceSummary> = merged.into_values().collect();
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(interfaces)
    }

    async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        let interfaces = self.list_interfaces().await?;
        let summary = interfaces
            .into_iter()
            .find(|i| i.name == name)
            .ok_or(NetworkError::InterfaceNotFound(name.to_string()))?;

        let mtu = get_interface_mtu(name).await;

        Ok(NetworkInterfaceDetail {
            summary,
            nm_connection_uuid: None,
            mtu,
            driver: None,
            firmware_version: None,
            rx_packets: None,
            tx_packets: None,
            rx_errors: None,
            tx_errors: None,
        })
    }

    async fn detect_capabilities(&self) -> NGResult<NetworkCapabilities> {
        let wifi_interfaces = tokio::task::spawn_blocking(|| {
            let client = unsafe { CWWiFiClient::sharedWiFiClient() };
            let names = unsafe { client.interfaceNames() };
            names
                .map(|arr| {
                    arr.to_vec()
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();

        let has_wifi = !wifi_interfaces.is_empty();

        let wireless_caps: Vec<WirelessInterfaceCapability> = wifi_interfaces
            .iter()
            .map(|name| WirelessInterfaceCapability {
                name: name.clone(),
                phy: String::new(),
                supported_modes: vec!["managed".to_string()],
                supports_sta_ap_concurrent: false,
                supported_bands: vec![WifiBand::Band2_4Ghz, WifiBand::Band5Ghz],
                current_mode: Some(WifiMode::Station),
            })
            .collect();

        Ok(NetworkCapabilities {
            platform: PlatformSupport::Partial,
            os: "macos".to_string(),
            arch: env::consts::ARCH.to_string(),
            network_manager_available: false,
            network_manager_version: None,
            can_configure_interfaces: true,
            can_scan_wifi: has_wifi,
            can_connect_wifi: has_wifi,
            can_manage_ap: false,
            ap_mode: ApMode::Unavailable,
            sta_ap_capability: StaApCapability::Unknown,
            wireless_interfaces: wireless_caps,
        })
    }

    // ─── Interface Configuration ───

    async fn configure_interface(
        &self,
        name: &str,
        config: &ConfigureInterfaceRequest,
    ) -> NGResult<()> {
        let service_name = resolve_service_name(name)?;

        match &config.ip_config {
            IpConfig::Dhcp {
                dns,
                search_domains,
            } => {
                run_networksetup(&["-setdhcp", &service_name]).await?;

                if let Some(ref servers) = dns {
                    if servers.is_empty() {
                        run_networksetup(&["-setdnsservers", &service_name, "Empty"]).await?;
                    } else {
                        let mut args = vec!["-setdnsservers".to_string(), service_name.clone()];
                        args.extend(servers.iter().map(|d| d.to_string()));
                        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        run_networksetup(&refs).await?;
                    }
                } else {
                    run_networksetup(&["-setdnsservers", &service_name, "Empty"]).await?;
                }

                if let Some(ref domains) = search_domains {
                    if domains.is_empty() {
                        run_networksetup(&["-setsearchdomains", &service_name, "Empty"]).await?;
                    } else {
                        let mut args = vec!["-setsearchdomains".to_string(), service_name.clone()];
                        args.extend(domains.clone());
                        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        run_networksetup(&refs).await?;
                    }
                }
            }
            IpConfig::Static { config: static_cfg } => {
                let mask = prefix_to_netmask_v4(static_cfg.prefix_length);
                let gateway_str = static_cfg
                    .gateway
                    .map(|g| g.to_string())
                    .unwrap_or_default();

                if gateway_str.is_empty() {
                    run_networksetup(&[
                        "-setmanual",
                        &service_name,
                        &static_cfg.ip_address.to_string(),
                        &mask,
                        "0.0.0.0",
                    ])
                    .await?;
                } else {
                    run_networksetup(&[
                        "-setmanual",
                        &service_name,
                        &static_cfg.ip_address.to_string(),
                        &mask,
                        &gateway_str,
                    ])
                    .await?;
                }

                if let Some(ref servers) = static_cfg.dns {
                    if servers.is_empty() {
                        run_networksetup(&["-setdnsservers", &service_name, "Empty"]).await?;
                    } else {
                        let mut args = vec!["-setdnsservers".to_string(), service_name.clone()];
                        args.extend(servers.iter().map(|d| d.to_string()));
                        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        run_networksetup(&refs).await?;
                    }
                }

                if let Some(ref domains) = static_cfg.search_domains {
                    if domains.is_empty() {
                        run_networksetup(&["-setsearchdomains", &service_name, "Empty"]).await?;
                    } else {
                        let mut args = vec!["-setsearchdomains".to_string(), service_name.clone()];
                        args.extend(domains.clone());
                        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        run_networksetup(&refs).await?;
                    }
                }
            }
            IpConfig::Disabled => {
                run_networksetup(&["-setv4off", &service_name]).await?;
            }
        }

        Ok(())
    }

    // ─── Wi-Fi ───

    async fn scan_wifi(&self, interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        let iface_name = interface_name.map(|s| s.to_string());

        let networks =
            tokio::task::spawn_blocking(move || -> Result<Vec<WifiAccessPoint>, NetworkError> {
                let client = unsafe { CWWiFiClient::sharedWiFiClient() };
                let iface = if let Some(ref name) = iface_name {
                    let ns = NSString::from_str(name);
                    unsafe { client.interfaceWithName(Some(&ns)) }.ok_or(
                        NetworkError::InterfaceNotFound(format!(
                            "Wi-Fi interface '{name}' not found"
                        )),
                    )?
                } else {
                    unsafe { client.interface() }.ok_or(NetworkError::InterfaceNotFound(
                        "No Wi-Fi interface".to_string(),
                    ))?
                };

                // Trigger a fresh scan. Pass None for SSID to scan all networks.
                let scan_set: Retained<NSSet<CWNetwork>> = unsafe {
                    iface.scanForNetworksWithSSID_error(None)
                }
                .map_err(|e| NetworkError::CommandFailed {
                    command: "CWInterface::scanForNetworksWithSSID".to_string(),
                    reason: format!("{e}"),
                })?;

                let current_ssid = unsafe { iface.ssid() }.map(|s| s.to_string());

                let networks: Vec<Retained<CWNetwork>> = scan_set.to_vec();

                let mut aps: Vec<WifiAccessPoint> = networks
                    .iter()
                    .filter_map(|net| convert_cw_network(net, current_ssid.as_deref()))
                    .collect();

                aps.sort_unstable_by(|a, b| b.signal_quality.cmp(&a.signal_quality));
                aps.dedup_by(|a, b| {
                    a.ssid == b.ssid && a.bssid == b.bssid && a.channel == b.channel
                });

                Ok(aps)
            })
            .await
            .map_err(|e| NetworkError::CommandFailed {
                command: "scan_wifi spawn_blocking".to_string(),
                reason: format!("task join failed: {e}"),
            })??;

        Ok(networks)
    }

    async fn wifi_connect_preflight(
        &self,
        request: &WifiConnectRequest,
    ) -> NGResult<WifiConnectPreflight> {
        Ok(WifiConnectPreflight {
            ssid: request.ssid.clone(),
            ap_will_stop: false,
            connection_will_be_lost: false,
            ap_can_restore: false,
            warnings: Vec::new(),
        })
    }

    async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        let ssid = request.ssid.clone();
        let password = request.password.clone();
        let bssid = request.bssid.clone();
        let iface_name = request.interface_name.clone();

        info!(ssid = %ssid, bssid = ?bssid, "Connecting to Wi-Fi via CoreWLAN");

        // Phase 1: Scan for the target network and connect via CoreWLAN.
        let connect_iface_name =
            tokio::task::spawn_blocking(move || -> Result<String, NetworkError> {
                let client = unsafe { CWWiFiClient::sharedWiFiClient() };
                let iface = if let Some(ref name) = iface_name {
                    let ns = NSString::from_str(name);
                    unsafe { client.interfaceWithName(Some(&ns)) }.ok_or(
                        NetworkError::InterfaceNotFound(format!(
                            "Wi-Fi interface '{name}' not found"
                        )),
                    )?
                } else {
                    unsafe { client.interface() }.ok_or(NetworkError::InterfaceNotFound(
                        "No Wi-Fi interface".to_string(),
                    ))?
                };

                let resolved_name = unsafe { iface.interfaceName() }
                    .map(|s| s.to_string())
                    .unwrap_or("en0".to_string());

                // Ensure Wi-Fi power is on.
                let power_on = unsafe { iface.powerOn() };
                if !power_on {
                    unsafe { iface.setPower_error(true) }.map_err(|e| {
                        NetworkError::WifiError(format!("Failed to enable Wi-Fi power: {e}"))
                    })?;
                }

                // Scan to find the target network.
                let ssid_data = NSData::with_bytes(ssid.as_bytes());
                let scan_set: Retained<NSSet<CWNetwork>> =
                    unsafe { iface.scanForNetworksWithSSID_error(Some(&ssid_data)) }.map_err(
                        |e| NetworkError::WifiError(format!("Scan for target SSID failed: {e}")),
                    )?;

                let networks: Vec<Retained<CWNetwork>> = scan_set.to_vec();

                if networks.is_empty() {
                    return Err(NetworkError::WifiError(format!(
                        "Target SSID '{ssid}' not found in scan results"
                    )));
                }

                // Select the best matching network (optionally by BSSID).
                let target = if let Some(ref target_bssid) = bssid {
                    networks
                        .iter()
                        .find(|n| {
                            unsafe { n.bssid() }
                                .map(|b| b.to_string().eq_ignore_ascii_case(target_bssid))
                                .unwrap_or(false)
                        })
                        .or(networks.first())
                } else {
                    networks.first()
                }
                .ok_or(NetworkError::WifiError(format!(
                    "No matching network for SSID '{ssid}'"
                )))?;

                // Connect using CoreWLAN native API.
                let ns_password = password.as_deref().map(NSString::from_str);
                unsafe { iface.associateToNetwork_password_error(target, ns_password.as_deref()) }
                    .map_err(|e| {
                        NetworkError::WifiError(format!("CoreWLAN association failed: {e}"))
                    })?;

                Ok(resolved_name)
            })
            .await
            .map_err(|e| NetworkError::CommandFailed {
                command: "connect_wifi spawn_blocking".to_string(),
                reason: format!("task join failed: {e}"),
            })??;

        // Phase 2: Poll until the association is confirmed and IP is ready.
        let connect_deadline = Instant::now() + Duration::from_secs(15);
        let mut status = loop {
            let current = self.wifi_sta_status(Some(&connect_iface_name)).await?;
            if current.connected && current.ssid.as_deref() == Some(request.ssid.as_str()) {
                break current;
            }
            if Instant::now() >= connect_deadline {
                return Err(NetworkError::WifiError(format!(
                    "Wi-Fi association timed out (target SSID: {})",
                    request.ssid
                ))
                .into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };

        // Phase 3: Apply static IP if requested.
        if let Some(ref ip_config) = request.ip_config {
            self.configure_interface(
                &connect_iface_name,
                &ConfigureInterfaceRequest {
                    ip_config: ip_config.clone(),
                },
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            status = self.wifi_sta_status(Some(&connect_iface_name)).await?;
        }

        info!(ssid = %request.ssid, "Wi-Fi connection established via CoreWLAN");
        Ok(status)
    }

    async fn disconnect_wifi(&self, request: &WifiDisconnectRequest) -> NGResult<()> {
        let iface_name = request.interface_name.clone();

        info!(interface = ?iface_name, "Disconnecting Wi-Fi via CoreWLAN");

        with_named_interface(iface_name.as_deref(), |iface: &CWInterface| {
            unsafe { iface.disassociate() };
            Ok(())
        })
        .await?;

        info!("Wi-Fi disconnected via CoreWLAN");
        Ok(())
    }

    async fn wifi_sta_status(&self, interface_name: Option<&str>) -> NGResult<WifiStaStatus> {
        let iface_name_owned = interface_name.map(|s| s.to_string());

        // Read CoreWLAN state on a blocking thread.
        let cw_status = get_corewlan_iface_status(interface_name).await?;

        // Read IP/DNS/Gateway from interface list for the Wi-Fi interface.
        let interfaces = self.list_interfaces().await.unwrap_or_default();
        let iface_name_ref = cw_status
            .interface_name
            .as_deref()
            .or(iface_name_owned.as_deref());

        let wifi_iface = iface_name_ref.and_then(|n| interfaces.iter().find(|i| i.name == n));

        let (ip_address, gateway, dns) = if cw_status.connected {
            let ip = wifi_iface
                .and_then(|w| w.ipv4.as_ref())
                .and_then(|v4| v4.addresses.first())
                .map(|a| a.address);
            let gw = wifi_iface
                .and_then(|w| w.ipv4.as_ref())
                .and_then(|v4| v4.gateway);
            let d = wifi_iface
                .and_then(|w| w.ipv4.as_ref())
                .map(|v4| v4.dns.clone())
                .unwrap_or_default();
            (ip, gw, d)
        } else {
            (None, None, Vec::new())
        };

        Ok(WifiStaStatus {
            connected: cw_status.connected,
            interface_name: cw_status.interface_name,
            ssid: cw_status.ssid,
            bssid: cw_status.bssid,
            security: cw_status.security,
            band: cw_status.band,
            channel: cw_status.channel,
            frequency: cw_status.frequency,
            signal_dbm: cw_status.signal_dbm,
            signal_quality: cw_status.signal_quality,
            ip_address,
            gateway,
            dns,
            speed_mbps: cw_status.speed_mbps,
            connected_secs: None,
        })
    }

    async fn list_saved_wifi_connections(&self) -> NGResult<Vec<SavedWifiConnection>> {
        let current_status = self.wifi_sta_status(None).await.ok();
        let current_iface = self
            .list_interfaces()
            .await
            .ok()
            .and_then(|ifaces| ifaces.into_iter().find(|i| i.kind == InterfaceKind::Wifi));

        let saved = tokio::task::spawn_blocking(
            move || -> Result<Vec<SavedWifiConnection>, NetworkError> {
                let client = unsafe { CWWiFiClient::sharedWiFiClient() };
                let iface = unsafe { client.interface() }.ok_or(
                    NetworkError::InterfaceNotFound("No Wi-Fi interface".to_string()),
                )?;

                let config: Retained<CWConfiguration> =
                    unsafe { iface.configuration() }.ok_or(NetworkError::CommandFailed {
                        command: "CWInterface::configuration".to_string(),
                        reason: "Failed to get Wi-Fi configuration".to_string(),
                    })?;

                let profiles: Retained<NSOrderedSet<CWNetworkProfile>> =
                    unsafe { config.networkProfiles() };

                let current_ssid = current_status.as_ref().and_then(|s| s.ssid.clone());

                // Iterate via objectEnumerator since NSOrderedSet doesn't
                // expose to_vec() in objc2-foundation.
                let enumerator = unsafe { profiles.objectEnumerator() };
                let mut connections: Vec<SavedWifiConnection> = Vec::new();
                loop {
                    let profile: Option<Retained<CWNetworkProfile>> =
                        unsafe { objc2::msg_send![&enumerator, nextObject] };
                    let profile = match profile {
                        Some(p) => p,
                        None => break,
                    };

                    let ssid = match unsafe { profile.ssid() } {
                        Some(s) => s.to_string(),
                        None => continue,
                    };
                    if ssid.is_empty() {
                        continue;
                    }

                    let is_active = current_ssid.as_deref() == Some(&ssid);
                    let security = cw_security_to_wifi_security(unsafe { profile.security() });

                    let ip_config = if is_active {
                        current_iface
                            .as_ref()
                            .and_then(|iface| iface.ipv4.as_ref())
                            .map(ip_config_from_ipv4)
                            .unwrap_or(IpConfig::Dhcp {
                                dns: None,
                                search_domains: None,
                            })
                    } else {
                        IpConfig::Dhcp {
                            dns: None,
                            search_domains: None,
                        }
                    };

                    connections.push(SavedWifiConnection {
                        uuid: ssid.clone(),
                        ssid,
                        is_active,
                        autoconnect: true,
                        security,
                        ip_config,
                        last_connected: None,
                    });
                }

                Ok(connections)
            },
        )
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: "list_saved_wifi_connections spawn_blocking".to_string(),
            reason: format!("task join failed: {e}"),
        })??;

        Ok(saved)
    }

    async fn forget_wifi(&self, request: &ForgetWifiRequest) -> NGResult<()> {
        let target_ssid = request.uuid.clone();

        info!(ssid = %target_ssid, "Forgetting Wi-Fi network via CoreWLAN");

        // Resolve the Wi-Fi interface name for networksetup.
        let wifi_iface =
            with_default_interface(|iface: &CWInterface| -> Result<String, NetworkError> {
                unsafe { iface.interfaceName() }
                    .map(|s| s.to_string())
                    .ok_or_else(|| {
                        NetworkError::InterfaceNotFound("Wi-Fi interface name unknown".to_string())
                    })
            })
            .await?;

        // Use `networksetup -removepreferredwirelessnetwork` as it's the most
        // reliable way to remove a saved network without complex NSOrderedSet
        // manipulation and authorization dialogs.
        run_networksetup(&["-removepreferredwirelessnetwork", &wifi_iface, &target_ssid]).await?;

        info!(ssid = %request.uuid, "Wi-Fi network forgotten via CoreWLAN");
        Ok(())
    }

    // ─── AP Hotspot (not supported on macOS) ───

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
            sta_restore_failed: false,
        })
    }

    async fn start_ap(&self) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP mode is not available on macOS".to_string())
                .into(),
        )
    }

    async fn stop_ap(&self) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP mode is not available on macOS".to_string())
                .into(),
        )
    }

    async fn configure_ap(&self, _config: &ConfigureApRequest) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP mode is not available on macOS".to_string())
                .into(),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CoreWLAN data extraction helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Intermediate CoreWLAN interface status.
struct CoreWlanIfaceStatus {
    connected: bool,
    interface_name: Option<String>,
    ssid: Option<String>,
    bssid: Option<String>,
    security: Option<WifiSecurity>,
    band: Option<WifiBand>,
    channel: Option<u32>,
    frequency: Option<u32>,
    signal_dbm: Option<i32>,
    signal_quality: Option<u8>,
    speed_mbps: Option<u32>,
}

/// Read Wi-Fi status from CoreWLAN for the named (or default) interface.
async fn get_corewlan_iface_status(
    interface_name: Option<&str>,
) -> Result<CoreWlanIfaceStatus, NetworkError> {
    let name_owned = interface_name.map(|s| s.to_string());

    tokio::task::spawn_blocking(move || {
        let client = unsafe { CWWiFiClient::sharedWiFiClient() };
        let iface = if let Some(ref name) = name_owned {
            let ns = NSString::from_str(name);
            unsafe { client.interfaceWithName(Some(&ns)) }
        } else {
            unsafe { client.interface() }
        };

        let iface = match iface {
            Some(i) => i,
            None => {
                return Ok(CoreWlanIfaceStatus {
                    connected: false,
                    interface_name: name_owned,
                    ssid: None,
                    bssid: None,
                    security: None,
                    band: None,
                    channel: None,
                    frequency: None,
                    signal_dbm: None,
                    signal_quality: None,
                    speed_mbps: None,
                });
            }
        };

        let resolved_name = unsafe { iface.interfaceName() }
            .map(|s| s.to_string())
            .or(name_owned);

        let ssid = unsafe { iface.ssid() }.map(|s| s.to_string());
        let bssid = unsafe { iface.bssid() }.map(|s| s.to_string());
        let connected = ssid.is_some();
        let security = if connected {
            Some(cw_security_to_wifi_security(unsafe { iface.security() }))
        } else {
            None
        };

        let rssi = if connected {
            Some(unsafe { iface.rssiValue() } as i32)
        } else {
            None
        };
        let signal_quality = rssi.map(platform::rssi_to_quality);

        let tx_rate = if connected {
            let rate = unsafe { iface.transmitRate() };
            if rate > 0.0 {
                Some(rate as u32)
            } else {
                None
            }
        } else {
            None
        };

        let (channel_num, band, frequency) = if let Some(ch) = unsafe { iface.wlanChannel() } {
            let num = unsafe { ch.channelNumber() } as u32;
            let band_enum = unsafe { ch.channelBand() };
            let band = match band_enum {
                CWChannelBand::Band2GHz => Some(WifiBand::Band2_4Ghz),
                CWChannelBand::Band5GHz => Some(WifiBand::Band5Ghz),
                CWChannelBand::Band6GHz => Some(WifiBand::Band6Ghz),
                _ => Some(WifiBand::Unknown),
            };
            let freq = channel_to_frequency(num, band);
            (Some(num), band, freq)
        } else {
            (None, None, None)
        };

        Ok(CoreWlanIfaceStatus {
            connected,
            interface_name: resolved_name,
            ssid,
            bssid,
            security,
            band,
            channel: channel_num,
            frequency,
            signal_dbm: rssi,
            signal_quality,
            speed_mbps: tx_rate,
        })
    })
    .await
    .map_err(|e| NetworkError::CommandFailed {
        command: "get_corewlan_iface_status".to_string(),
        reason: format!("task join failed: {e}"),
    })?
}

/// Convert a `CWNetwork` scan result to our domain `WifiAccessPoint`.
fn convert_cw_network(net: &CWNetwork, current_ssid: Option<&str>) -> Option<WifiAccessPoint> {
    let ssid = unsafe { net.ssid() }?.to_string();
    if ssid.is_empty() {
        return None;
    }

    let bssid = unsafe { net.bssid() }
        .map(|s| s.to_string())
        .unwrap_or_default();
    let rssi = unsafe { net.rssiValue() } as i32;
    let signal_quality = crate::network::platform::rssi_to_quality(rssi);

    let (channel, band, frequency) = if let Some(ch) = unsafe { net.wlanChannel() } {
        let num = unsafe { ch.channelNumber() } as u32;
        let band_enum = unsafe { ch.channelBand() };
        let band = match band_enum {
            CWChannelBand::Band2GHz => WifiBand::Band2_4Ghz,
            CWChannelBand::Band5GHz => WifiBand::Band5Ghz,
            CWChannelBand::Band6GHz => WifiBand::Band6Ghz,
            _ => WifiBand::Unknown,
        };
        let freq = channel_to_frequency(num, Some(band)).unwrap_or(0);
        (num, band, freq)
    } else {
        (0, WifiBand::Unknown, 0)
    };

    let security = detect_network_security(net);
    let is_connected = current_ssid == Some(ssid.as_str());

    Some(WifiAccessPoint {
        ssid,
        bssid,
        security,
        band,
        channel,
        frequency,
        signal_dbm: rssi,
        signal_quality,
        max_bitrate_kbps: None,
        is_connected,
    })
}

/// Detect security type of a `CWNetwork` by probing supported security modes.
fn detect_network_security(net: &CWNetwork) -> WifiSecurity {
    // Probe in order of preference (strongest first).
    if unsafe { net.supportsSecurity(CWSecurity::WPA3Personal) } {
        WifiSecurity::Wpa3Sae
    } else if unsafe { net.supportsSecurity(CWSecurity::WPA2Enterprise) } {
        WifiSecurity::Wpa2Enterprise
    } else if unsafe { net.supportsSecurity(CWSecurity::WPA2Personal) } {
        WifiSecurity::Wpa2Psk
    } else if unsafe { net.supportsSecurity(CWSecurity::WPAEnterprise) }
        || unsafe { net.supportsSecurity(CWSecurity::WPAEnterpriseMixed) }
    {
        WifiSecurity::WpaEnterprise
    } else if unsafe { net.supportsSecurity(CWSecurity::WPAPersonal) }
        || unsafe { net.supportsSecurity(CWSecurity::WPAPersonalMixed) }
    {
        WifiSecurity::WpaPsk
    } else if unsafe { net.supportsSecurity(CWSecurity::WEP) } {
        WifiSecurity::Wep
    } else if unsafe { net.supportsSecurity(CWSecurity::None) } {
        WifiSecurity::Open
    } else {
        WifiSecurity::Unknown
    }
}

/// Convert `CWSecurity` enum to our domain `WifiSecurity`.
///
/// macOS may return `CWSecurity::Personal` (generic) for saved profiles when the
/// exact type (WPA2/WPA3) is not stored; we map it to Wpa2Psk as the common case.
fn cw_security_to_wifi_security(sec: CWSecurity) -> WifiSecurity {
    match sec {
        CWSecurity::None => WifiSecurity::Open,
        CWSecurity::WEP => WifiSecurity::Wep,
        CWSecurity::WPAPersonal | CWSecurity::WPAPersonalMixed => WifiSecurity::WpaPsk,
        CWSecurity::WPA2Personal | CWSecurity::Personal => WifiSecurity::Wpa2Psk,
        CWSecurity::WPA3Personal => WifiSecurity::Wpa3Sae,
        CWSecurity::WPA3Transition => WifiSecurity::Wpa2Psk, // WPA3/WPA2 mixed
        CWSecurity::WPAEnterprise | CWSecurity::WPAEnterpriseMixed => WifiSecurity::WpaEnterprise,
        CWSecurity::WPA2Enterprise | CWSecurity::Enterprise => WifiSecurity::Wpa2Enterprise,
        CWSecurity::WPA3Enterprise => WifiSecurity::Wpa2Enterprise,
        CWSecurity::DynamicWEP => WifiSecurity::Wep,
        CWSecurity::OWE | CWSecurity::OWETransition => WifiSecurity::Open, // Enhanced Open
        CWSecurity::Unknown => WifiSecurity::Unknown,
        _ => WifiSecurity::Unknown,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SystemConfiguration: interface → service mapping, IP/DNS/Gateway state
// ═══════════════════════════════════════════════════════════════════════════

/// Metadata about a macOS network service (obtained from SystemConfiguration).
#[derive(Debug, Clone)]
struct MacosServiceInfo {
    /// The macOS network service name (e.g. "Wi-Fi", "Thunderbolt Ethernet").
    service_name: String,
    /// The hardware port type if known.
    hardware_port: Option<String>,
    /// The SC service ID (UUID) for dynamic store lookups.
    #[allow(dead_code)]
    service_id: Option<String>,
}

/// IP state read from the SystemConfiguration dynamic store.
#[derive(Debug, Clone)]
struct ScIpState {
    gateway: Option<IpAddr>,
    dns: Vec<IpAddr>,
    search_domains: Vec<String>,
    method: IpMethod,
}

// Raw FFI binding for SCNetworkServiceGetName (not exposed by system-configuration crate).
extern "C" {
    fn SCNetworkServiceGetName(service: *const c_void) -> CFStringRef;
}

/// Get the user-visible name of an SCNetworkService (e.g. "Wi-Fi", "Ethernet").
fn sc_service_name(service: &SCNetworkService) -> Option<String> {
    let raw_ref = service.as_CFTypeRef();
    let name_ref = unsafe { SCNetworkServiceGetName(raw_ref) };
    if name_ref.is_null() {
        return None;
    }
    let cf_str = unsafe { CFString::wrap_under_get_rule(name_ref) };
    Some(cf_str.to_string())
}

/// Build a BSD interface → network service mapping using SystemConfiguration's
/// `SCNetworkService` API. This is the authoritative source on macOS, replacing
/// the old `networksetup -listnetworkserviceorder` CLI parsing.
fn build_service_map_native() -> BTreeMap<String, MacosServiceInfo> {
    let mut map = BTreeMap::new();

    let prefs = SCPreferences::default(&CFString::new("ng-gateway"));
    let services = SCNetworkService::get_services(&prefs);

    for service in services.iter() {
        if !service.enabled() {
            continue;
        }
        let service_name = match sc_service_name(&service) {
            Some(name) => name,
            None => continue,
        };
        let service_id = service.id().map(|id| id.to_string());
        if let Some(iface) = service.network_interface() {
            if let Some(bsd_name) = iface.bsd_name() {
                let bsd = bsd_name.to_string();
                let hw_type = iface.interface_type_string().map(|t| t.to_string());
                map.insert(
                    bsd,
                    MacosServiceInfo {
                        service_name,
                        hardware_port: hw_type,
                        service_id,
                    },
                );
            }
        }
    }

    map
}

/// Read IP state for all interfaces from the SystemConfiguration dynamic store.
///
/// Queries `State:/Network/Service/<id>/IPv4` and `State:/Network/Service/<id>/DNS`
/// for each known service, building a `bsd_name → ScIpState` map.
fn read_sc_ip_state() -> BTreeMap<String, ScIpState> {
    let mut result = BTreeMap::new();

    let store = match SCDynamicStoreBuilder::new("ng-gateway").build() {
        Some(s) => s,
        None => return result,
    };
    let prefs = SCPreferences::default(&CFString::new("ng-gateway"));
    let services = SCNetworkService::get_services(&prefs);

    for service in services.iter() {
        let service_id = match service.id() {
            Some(id) => id.to_string(),
            None => continue,
        };
        let bsd_name = service
            .network_interface()
            .and_then(|i| i.bsd_name())
            .map(|n| n.to_string());
        let bsd_name = match bsd_name {
            Some(n) => n,
            None => continue,
        };

        let ipv4_key = format!("State:/Network/Service/{service_id}/IPv4");
        let dns_key = format!("State:/Network/Service/{service_id}/DNS");
        let setup_ipv4_key = format!("Setup:/Network/Service/{service_id}/IPv4");

        let mut state = ScIpState {
            gateway: None,
            dns: Vec::new(),
            search_domains: Vec::new(),
            method: IpMethod::Dhcp,
        };

        // Read IPv4 state (router/gateway, method).
        if let Some(val) = store.get(ipv4_key.as_str()) {
            if let Some(dict) = val.downcast_into::<CFDictionary>() {
                state.gateway =
                    get_cf_dict_string(&dict, "Router").and_then(|s| s.parse::<IpAddr>().ok());

                if let Some(method) = get_cf_dict_string(&dict, "ConfigMethod") {
                    if method.contains("Manual") {
                        state.method = IpMethod::Static;
                    }
                }
            }
        }

        // Check setup (configured) method — distinguishes DHCP-with-overrides from Static.
        if state.method == IpMethod::Dhcp {
            if let Some(val) = store.get(setup_ipv4_key.as_str()) {
                if let Some(dict) = val.downcast_into::<CFDictionary>() {
                    if let Some(method) = get_cf_dict_string(&dict, "ConfigMethod") {
                        if method.contains("Manual") {
                            state.method = IpMethod::Static;
                        }
                    }
                }
            }
        }

        // Read DNS state.
        if let Some(val) = store.get(dns_key.as_str()) {
            if let Some(dict) = val.downcast_into::<CFDictionary>() {
                state.dns = get_cf_dict_string_array(&dict, "ServerAddresses")
                    .into_iter()
                    .filter_map(|s| s.parse::<IpAddr>().ok())
                    .collect();
                state.search_domains = get_cf_dict_string_array(&dict, "SearchDomains");
            }
        }

        result.insert(bsd_name, state);
    }

    result
}

/// Build BSD interface → service mapping by parsing `networksetup -listnetworkserviceorder`.
///
/// Used as fallback when SystemConfiguration API returns empty or lacks an interface
/// (e.g. when the process runs without full preferences access).
fn build_service_map_from_networksetup() -> BTreeMap<String, MacosServiceInfo> {
    let mut map = BTreeMap::new();
    let output = match StdCommand::new("networksetup")
        .args(["-listnetworkserviceorder"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return map,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_service: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim();
        // Match "(N) Service Name" — e.g. "(3) Wi-Fi".
        if line.starts_with('(') {
            if let Some(close) = line.find(')') {
                let after = line[close + 1..].trim();
                if !after.is_empty() && !after.to_lowercase().starts_with("hardware") {
                    current_service = Some(after.to_string());
                }
            }
        }
        // Match "Device: en0" — e.g. "(Hardware Port: Wi-Fi, Device: en0)".
        if let Some(idx) = line.find("Device:") {
            let device_str = line[idx + 7..].trim().trim_end_matches(')').trim();
            if !device_str.is_empty() {
                let hw_start = line.find("Hardware Port:").map(|i| i + 13).unwrap_or(0);
                let hw_end = line.find("Device:").unwrap_or(line.len());
                let hw_port = line[hw_start..hw_end].trim().trim_end_matches(',').trim();
                if let Some(ref svc) = current_service {
                    map.insert(
                        device_str.to_string(),
                        MacosServiceInfo {
                            service_name: svc.clone(),
                            hardware_port: if hw_port.is_empty() {
                                None
                            } else {
                                Some(hw_port.to_string())
                            },
                            service_id: None,
                        },
                    );
                }
            }
        }
    }
    map
}

/// Resolve BSD interface name → macOS network service name for `networksetup` CLI.
///
/// Tries native SystemConfiguration first; falls back to parsing
/// `networksetup -listnetworkserviceorder` when native returns empty or lacks the interface.
fn resolve_service_name(bsd_name: &str) -> Result<String, NetworkError> {
    let native = build_service_map_native();
    if let Some(info) = native.get(bsd_name) {
        return Ok(info.service_name.clone());
    }
    let cli_map = build_service_map_from_networksetup();
    cli_map
        .get(bsd_name)
        .map(|info| info.service_name.clone())
        .ok_or(NetworkError::InterfaceNotFound(format!(
            "No macOS network service found for interface '{bsd_name}'"
        )))
}

// ═══════════════════════════════════════════════════════════════════════════
// CoreFoundation dictionary extraction helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Extract a string value from a CFDictionary by key.
fn get_cf_dict_string(dict: &CFDictionary, key: &str) -> Option<String> {
    let cf_key = CFString::new(key);
    let value_ptr = dict.find(cf_key.as_CFTypeRef())?;
    let cf_str = unsafe { CFString::wrap_under_get_rule(*value_ptr as CFStringRef) };
    Some(cf_str.to_string())
}

/// Extract a string array from a CFDictionary by key.
fn get_cf_dict_string_array(dict: &CFDictionary, key: &str) -> Vec<String> {
    type CfArray = CFArray<CFString>;

    let cf_key = CFString::new(key);
    let value_ptr = match dict.find(cf_key.as_CFTypeRef()) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let arr: CfArray = unsafe { TCFType::wrap_under_get_rule(*value_ptr as _) };
    arr.iter().map(|s| (*s).to_string()).collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// CLI fallback helpers — minimal use for IP configuration writes
// ═══════════════════════════════════════════════════════════════════════════

/// Run a `networksetup` command and return its stdout on success.
async fn run_networksetup(args: &[&str]) -> Result<String, NetworkError> {
    debug!(command = %format!("networksetup {}", args.join(" ")), "Running networksetup");

    let output = Command::new("networksetup")
        .args(args)
        .output()
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: format!("networksetup {}", args.join(" ")),
            reason: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(NetworkError::CommandFailed {
            command: format!("networksetup {}", args.join(" ")),
            reason: format!(
                "exit code {:?}: {}{}",
                output.status.code(),
                stderr.trim(),
                if stdout.trim().is_empty() {
                    String::new()
                } else {
                    format!(" | stdout: {}", stdout.trim())
                }
            ),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ═══════════════════════════════════════════════════════════════════════════
// Interface classification and filtering — string-based heuristics
// ═══════════════════════════════════════════════════════════════════════════

/// BSD interface name prefixes to exclude from enumeration.
/// Order does not matter; use `starts_with` for matching.
const SKIP_INTERFACE_PREFIXES: &[&str] = &[
    "lo",     // loopback (lo0, lo1, etc.)
    "utun",   // User-mode tunnel (VPN, etc.)
    "awdl",   // Apple Wireless Direct Link
    "llw",    // Low-latency Wi-Fi
    "anpi",   // Apple Neural Processing
    "bridge", // Bridge (bridge0, bridge100, etc.)
    "gif",    // Generic tunnel
    "stf",    // 6to4 tunnel
    "XHC",    // USB controller
    "ap",     // AP mode interface (ap1)
    "pktap",  // Packet tap
    "ipsec",  // IPsec tunnel
    "tun",    // Tunnel (tun0, etc.)
    "tap",    // TAP device
];

/// Virtual/hosted network interface prefixes (VMware, Docker, etc.).
const VIRTUAL_INTERFACE_PREFIXES: &[&str] = &[
    "vmenet", // VMware
    "vmnet",  // VMware (alternative)
    "veth",   // Docker/Linux virtual eth
    "vnic",   // Virtual NIC
    "virbr",  // libvirt bridge
];

/// Keywords indicating Wi-Fi hardware (hardware_port or service_name).
const WIFI_KEYWORDS: &[&str] = &["wi-fi", "wifi", "airport", "ieee80211"];

/// Keywords indicating Ethernet hardware (Thunderbolt, USB LAN, etc.).
const ETHERNET_KEYWORDS: &[&str] = &["ethernet", "thunderbolt", "usb"];

fn matches_any_prefix(name: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| name.starts_with(p))
}

fn str_contains_any(s: &str, keywords: &[&str]) -> bool {
    let lower = s.to_lowercase();
    keywords.iter().any(|k| lower.contains(k))
}

/// Interfaces to exclude from enumeration.
fn should_skip_interface(name: &str) -> bool {
    matches_any_prefix(name, SKIP_INTERFACE_PREFIXES)
}

/// Classify interface kind using CoreWLAN (authoritative for Wi-Fi) and
/// SystemConfiguration hardware-port metadata (for Ethernet, Thunderbolt, etc.).
fn classify_interface(
    name: &str,
    service_map: &BTreeMap<String, MacosServiceInfo>,
    wifi_names: &HashSet<String>,
) -> InterfaceKind {
    // CoreWLAN is the authoritative source for Wi-Fi on macOS.
    if wifi_names.contains(name) {
        return InterfaceKind::Wifi;
    }

    // Virtual interfaces (VMware, Docker, libvirt, etc.).
    if matches_any_prefix(name, VIRTUAL_INTERFACE_PREFIXES) {
        return InterfaceKind::Virtual;
    }

    if let Some(info) = service_map.get(name) {
        if let Some(ref hw) = info.hardware_port {
            if str_contains_any(hw, WIFI_KEYWORDS) {
                return InterfaceKind::Wifi;
            }
            if str_contains_any(hw, ETHERNET_KEYWORDS) {
                return InterfaceKind::Ethernet;
            }
        }
        // Fallback: check service name.
        if str_contains_any(&info.service_name, WIFI_KEYWORDS) {
            return InterfaceKind::Wifi;
        }
        if str_contains_any(&info.service_name, ETHERNET_KEYWORDS) {
            return InterfaceKind::Ethernet;
        }
    }

    // No definitive classification: avoid guessing.
    InterfaceKind::Unknown
}

// ═══════════════════════════════════════════════════════════════════════════
// Network math and conversion utilities
// ═══════════════════════════════════════════════════════════════════════════

/// Convert an IPv4 subnet mask to CIDR prefix length.
fn netmask_to_prefix_v4(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

/// Convert an IPv6 subnet mask to CIDR prefix length.
fn netmask_to_prefix_v6(mask: Ipv6Addr) -> u8 {
    u128::from(mask).count_ones() as u8
}

/// Convert CIDR prefix to dotted-decimal subnet mask string.
fn prefix_to_netmask_v4(prefix: u8) -> String {
    let mask = if prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    };
    let octets = mask.to_be_bytes();
    format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])
}

/// Convert a channel number and band to frequency in MHz.
fn channel_to_frequency(channel: u32, band: Option<WifiBand>) -> Option<u32> {
    match band {
        Some(WifiBand::Band2_4Ghz) | None if channel <= 14 => {
            if channel == 14 {
                Some(2484)
            } else if channel >= 1 {
                Some(2407 + channel * 5)
            } else {
                None
            }
        }
        Some(WifiBand::Band5Ghz) if (32..=177).contains(&channel) => Some(5000 + channel * 5),
        Some(WifiBand::Band6Ghz) if channel >= 1 => Some(5950 + channel * 5),
        _ => None,
    }
}

/// Derive an `IpConfig` from a read-only `Ipv4Config` (for saved connection display).
fn ip_config_from_ipv4(v4: &Ipv4Config) -> IpConfig {
    match v4.method {
        IpMethod::Static => {
            if let Some(first) = v4.addresses.first() {
                IpConfig::Static {
                    config: ng_gateway_models::domain::prelude::StaticIpConfig {
                        ip_address: first.address,
                        prefix_length: first.prefix_length,
                        gateway: v4.gateway,
                        dns: if v4.dns.is_empty() {
                            None
                        } else {
                            Some(v4.dns.clone())
                        },
                        search_domains: if v4.search_domains.is_empty() {
                            None
                        } else {
                            Some(v4.search_domains.clone())
                        },
                    },
                }
            } else {
                IpConfig::Dhcp {
                    dns: None,
                    search_domains: None,
                }
            }
        }
        IpMethod::Disabled => IpConfig::Disabled,
        IpMethod::Dhcp => {
            let dns = if v4.dns.is_empty() {
                None
            } else {
                Some(v4.dns.clone())
            };
            let search = if v4.search_domains.is_empty() {
                None
            } else {
                Some(v4.search_domains.clone())
            };
            IpConfig::Dhcp {
                dns,
                search_domains: search,
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Thin CLI helpers for link state and MTU
// ═══════════════════════════════════════════════════════════════════════════

/// Check if an interface is UP via `ifconfig`.
async fn check_interface_up(name: &str) -> Option<bool> {
    let output = Command::new("ifconfig").arg(name).output().await.ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next()?;
    Some(first_line.contains("UP"))
}

/// Read the MTU of an interface via `ifconfig`.
async fn get_interface_mtu(name: &str) -> Option<u32> {
    let output = Command::new("ifconfig").arg(name).output().await.ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(pos) = line.find("mtu ") {
            let mtu_str = &line[pos + 4..];
            if let Some(end) = mtu_str.find(|c: char| !c.is_ascii_digit()) {
                return mtu_str[..end].parse().ok();
            }
            return mtu_str.trim().parse().ok();
        }
    }
    None
}
