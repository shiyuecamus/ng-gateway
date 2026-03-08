//! Windows network platform implementation.
//!
//! Primary API sources:
//! - **Native Wifi API** (`windows` crate, `Win32_NetworkManagement_WiFi`):
//!   Wi-Fi interface enumeration, scan, connect, disconnect, profile management,
//!   connection status query — all via WLAN handle operations.
//! - **IP Helper API** (`windows` crate, `Win32_NetworkManagement_IpHelper`):
//!   `GetAdaptersAddresses` for interface/IP/DNS/gateway/MTU enumeration — replaces
//!   `netsh interface ipv4 show config` text parsing.
//! - **`network-interface`** crate: supplementary address enumeration.
//!
//! CLI tools (`netsh`) are retained **only** for IP configuration writes
//! (DHCP/Static/DNS set), because the Win32 IP Helper write APIs require
//! elevated privileges and more complex setup.

use crate::network::platform::PlatformNetworkManager;
use async_trait::async_trait;
use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::{
    ApMode, ApStatus, ConfigureApRequest, ConfigureInterfaceRequest, ForgetWifiRequest,
    InterfaceKind, IpConfig, IpMethod, Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config,
    LinkState, NetworkCapabilities, NetworkInterfaceDetail, NetworkInterfaceSummary,
    PlatformSupport, SavedWifiConnection, StaApCapability, WifiAccessPoint, WifiBand,
    WifiConnectPreflight, WifiConnectRequest, WifiDisconnectRequest, WifiMode, WifiSecurity,
    WifiStaStatus, WirelessInterfaceCapability,
};
use std::{
    collections::HashSet,
    ffi::c_void,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::{Duration, Instant},
};
use tokio::process::Command;
use tracing::{debug, info, warn};
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE},
    NetworkManagement::{
        IpHelper::{GetAdaptersAddresses, GET_ADAPTERS_ADDRESSES_FLAGS, IP_ADAPTER_ADDRESSES_LH},
        Ndis::IF_OPER_STATUS,
        WiFi::{
            WlanCloseHandle, WlanConnect, WlanDeleteProfile, WlanDisconnect, WlanEnumInterfaces,
            WlanFreeMemory, WlanGetAvailableNetworkList, WlanGetProfileList, WlanOpenHandle,
            WlanQueryInterface, DOT11_AUTH_ALGORITHM, DOT11_AUTH_ALGO_80211_OPEN,
            DOT11_AUTH_ALGO_RSNA, DOT11_AUTH_ALGO_RSNA_PSK, DOT11_AUTH_ALGO_WPA,
            DOT11_AUTH_ALGO_WPA3, DOT11_AUTH_ALGO_WPA3_SAE, DOT11_AUTH_ALGO_WPA_PSK,
            DOT11_BSS_TYPE, DOT11_SSID, WLAN_AVAILABLE_NETWORK, WLAN_AVAILABLE_NETWORK_LIST,
            WLAN_CONNECTION_ATTRIBUTES, WLAN_CONNECTION_MODE, WLAN_CONNECTION_PARAMETERS,
            WLAN_INTERFACE_INFO_LIST, WLAN_INTERFACE_STATE, WLAN_PROFILE_INFO_LIST,
        },
    },
    Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6},
};

/// Windows network manager backed by Native Wifi API + IP Helper API.
pub struct WindowsNetworkManager;

impl Default for WindowsNetworkManager {
    fn default() -> Self {
        Self
    }
}

impl WindowsNetworkManager {
    pub fn new() -> Self {
        Self
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// WLAN Handle RAII wrapper
// ═══════════════════════════════════════════════════════════════════════════

/// RAII wrapper around a WLAN client handle, ensuring `WlanCloseHandle` on drop.
struct WlanHandle(HANDLE);

impl WlanHandle {
    /// Open a WLAN client handle (negotiating API version 2).
    fn open() -> Result<Self, NetworkError> {
        let mut negotiated_version: u32 = 0;
        let mut handle = HANDLE::default();

        let result = unsafe { WlanOpenHandle(2, None, &mut negotiated_version, &mut handle) };

        if result != ERROR_SUCCESS.0 {
            return Err(NetworkError::CommandFailed {
                command: "WlanOpenHandle".to_string(),
                reason: format!("Failed with error code {result}"),
            });
        }

        Ok(WlanHandle(handle))
    }

    fn as_raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for WlanHandle {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.0, None) };
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Native Wifi API helper types
// ═══════════════════════════════════════════════════════════════════════════

/// Parsed Wi-Fi interface info from Native Wifi API.
#[derive(Debug, Clone)]
struct NativeWlanInterface {
    guid: windows::core::GUID,
    description: String,
    #[allow(dead_code)]
    state: WLAN_INTERFACE_STATE,
}

/// Parsed WLAN connection attributes from `WlanQueryInterface`.
#[derive(Debug)]
struct NativeConnectionInfo {
    ssid: Option<String>,
    bssid: Option<String>,
    security: WifiSecurity,
    signal_quality: u32,
    channel: Option<u32>,
    speed_mbps: Option<u32>,
    #[allow(dead_code)]
    state: WLAN_INTERFACE_STATE,
}

// ═══════════════════════════════════════════════════════════════════════════
// PlatformNetworkManager implementation
// ═══════════════════════════════════════════════════════════════════════════

#[async_trait]
impl PlatformNetworkManager for WindowsNetworkManager {
    // ─── Discovery ───

    async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        let adapter_info = tokio::task::spawn_blocking(get_adapters_addresses)
            .await
            .map_err(|e| NetworkError::CommandFailed {
                command: "GetAdaptersAddresses".to_string(),
                reason: format!("task join failed: {e}"),
            })?
            .map_err(|e| NetworkError::CommandFailed {
                command: "GetAdaptersAddresses".to_string(),
                reason: e,
            })?;

        let wlan_interfaces = tokio::task::spawn_blocking(enumerate_wlan_interfaces)
            .await
            .unwrap_or_else(|_| Ok(Vec::new()))
            .unwrap_or_default();

        let wlan_guid_set: HashSet<windows::core::GUID> =
            wlan_interfaces.iter().map(|w| w.guid).collect();

        let wlan_conn = tokio::task::spawn_blocking(|| -> Option<NativeConnectionInfo> {
            let handle = WlanHandle::open().ok()?;
            let ifaces = enumerate_wlan_interfaces_with_handle(&handle).ok()?;
            let first = ifaces.first()?;
            query_wlan_connection(&handle, &first.guid).ok()?
        })
        .await
        .unwrap_or(None);

        let mut interfaces: Vec<NetworkInterfaceSummary> = adapter_info
            .into_iter()
            .filter(|a| !should_skip_interface(&a.name, &a.description))
            .map(|a| {
                let is_wifi = a.if_type == 71 || wlan_guid_set.contains(&a.guid);
                let kind = if is_wifi {
                    InterfaceKind::Wifi
                } else if a.if_type == 6 {
                    InterfaceKind::Ethernet
                } else {
                    InterfaceKind::Unknown
                };

                let link_state = if a.oper_status_up {
                    LinkState::Up
                } else {
                    LinkState::Down
                };

                let (connected_ssid, signal_dbm, signal_quality, speed_mbps) = if is_wifi {
                    if let Some(ref conn) = wlan_conn {
                        (
                            conn.ssid.clone(),
                            Some(quality_to_approx_rssi(conn.signal_quality as u8)),
                            Some(conn.signal_quality.min(100) as u8),
                            conn.speed_mbps,
                        )
                    } else {
                        (None, None, None, None)
                    }
                } else {
                    (None, None, None, None)
                };

                NetworkInterfaceSummary {
                    name: a.name.clone(),
                    display_name: if a.description.is_empty() {
                        None
                    } else {
                        Some(a.description)
                    },
                    kind,
                    link_state,
                    mac_address: a.mac_address.clone(),
                    ipv4: a.ipv4,
                    ipv6: a.ipv6,
                    wifi_mode: if is_wifi {
                        Some(WifiMode::Station)
                    } else {
                        None
                    },
                    connected_ssid,
                    ap_ssid: None,
                    signal_dbm,
                    signal_quality,
                    speed_mbps,
                    rx_bytes: None,
                    tx_bytes: None,
                }
            })
            .collect();

        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(interfaces)
    }

    async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        let name_owned = name.to_string();

        let mtu = tokio::task::spawn_blocking(move || -> Option<u32> {
            let adapters = get_adapters_addresses().ok()?;
            adapters
                .into_iter()
                .find(|a| a.name == name_owned)
                .and_then(|a| a.mtu)
        })
        .await
        .unwrap_or(None);

        let interfaces = self.list_interfaces().await?;
        let summary = interfaces
            .into_iter()
            .find(|i| i.name == name)
            .ok_or_else(|| NetworkError::InterfaceNotFound(name.to_string()))?;

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
        let wlan_interfaces = tokio::task::spawn_blocking(enumerate_wlan_interfaces)
            .await
            .unwrap_or_else(|_| Ok(Vec::new()))
            .unwrap_or_default();

        let has_wifi = !wlan_interfaces.is_empty();

        let wireless_caps: Vec<WirelessInterfaceCapability> = wlan_interfaces
            .iter()
            .map(|w| WirelessInterfaceCapability {
                name: w.description.clone(),
                phy: String::new(),
                supported_modes: vec!["managed".to_string()],
                supports_sta_ap_concurrent: false,
                supported_bands: vec![WifiBand::Band2_4Ghz, WifiBand::Band5Ghz],
                current_mode: Some(WifiMode::Station),
            })
            .collect();

        Ok(NetworkCapabilities {
            platform: PlatformSupport::Partial,
            os: "windows".to_string(),
            arch: std::env::consts::ARCH.to_string(),
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
        match &config.ip_config {
            IpConfig::Dhcp {
                dns,
                search_domains: _,
            } => {
                run_netsh(&[
                    "interface",
                    "ipv4",
                    "set",
                    "address",
                    &format!("name={name}"),
                    "source=dhcp",
                ])
                .await?;

                if let Some(ref servers) = dns {
                    if servers.is_empty() {
                        run_netsh(&[
                            "interface",
                            "ipv4",
                            "set",
                            "dnsservers",
                            &format!("name={name}"),
                            "source=dhcp",
                        ])
                        .await?;
                    } else {
                        run_netsh(&[
                            "interface",
                            "ipv4",
                            "set",
                            "dnsservers",
                            &format!("name={name}"),
                            &format!("static={}", servers[0]),
                            "primary",
                        ])
                        .await?;

                        for dns_addr in servers.iter().skip(1) {
                            run_netsh(&[
                                "interface",
                                "ipv4",
                                "add",
                                "dnsservers",
                                &format!("name={name}"),
                                &format!("address={dns_addr}"),
                            ])
                            .await?;
                        }
                    }
                } else {
                    run_netsh(&[
                        "interface",
                        "ipv4",
                        "set",
                        "dnsservers",
                        &format!("name={name}"),
                        "source=dhcp",
                    ])
                    .await?;
                }
            }
            IpConfig::Static { config: static_cfg } => {
                let mask = prefix_to_netmask_v4(static_cfg.prefix_length);
                let mut args = vec![
                    "interface".to_string(),
                    "ipv4".to_string(),
                    "set".to_string(),
                    "address".to_string(),
                    format!("name={name}"),
                    "source=static".to_string(),
                    format!("address={}", static_cfg.ip_address),
                    format!("mask={mask}"),
                ];

                if let Some(gw) = static_cfg.gateway {
                    args.push(format!("gateway={gw}"));
                }

                let str_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                run_netsh(&str_refs).await?;

                if let Some(ref servers) = static_cfg.dns {
                    if !servers.is_empty() {
                        run_netsh(&[
                            "interface",
                            "ipv4",
                            "set",
                            "dnsservers",
                            &format!("name={name}"),
                            &format!("static={}", servers[0]),
                            "primary",
                        ])
                        .await?;

                        for dns_addr in servers.iter().skip(1) {
                            run_netsh(&[
                                "interface",
                                "ipv4",
                                "add",
                                "dnsservers",
                                &format!("name={name}"),
                                &format!("address={dns_addr}"),
                            ])
                            .await?;
                        }
                    }
                }
            }
            IpConfig::Disabled => {
                // Disable the interface via netsh.
                // Note: This disables the entire adapter, not just IPv4.
                run_netsh(&[
                    "interface",
                    "set",
                    "interface",
                    &format!("name={name}"),
                    "admin=disabled",
                ])
                .await?;
            }
        }

        Ok(())
    }

    // ─── Wi-Fi ───

    async fn scan_wifi(&self, _interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        let aps = tokio::task::spawn_blocking(|| -> Result<Vec<WifiAccessPoint>, NetworkError> {
            let handle = WlanHandle::open()?;
            let interfaces = enumerate_wlan_interfaces_with_handle(&handle)?;

            let iface = interfaces.first().ok_or_else(|| {
                NetworkError::InterfaceNotFound("No Wi-Fi interface available".to_string())
            })?;

            // Trigger a fresh scan.
            let scan_result = unsafe {
                windows::Win32::NetworkManagement::WiFi::WlanScan(
                    handle.as_raw(),
                    &iface.guid,
                    None,
                    None,
                    None,
                )
            };
            if scan_result != ERROR_SUCCESS.0 {
                warn!(
                    error_code = scan_result,
                    "WlanScan returned non-zero, proceeding with cached results"
                );
            }

            // Small delay to allow scan to populate.
            std::thread::sleep(Duration::from_millis(500));

            // Get available network list.
            let mut network_list_ptr: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
            let result = unsafe {
                WlanGetAvailableNetworkList(
                    handle.as_raw(),
                    &iface.guid,
                    0,
                    None,
                    &mut network_list_ptr,
                )
            };

            if result != ERROR_SUCCESS.0 {
                return Err(NetworkError::CommandFailed {
                    command: "WlanGetAvailableNetworkList".to_string(),
                    reason: format!("Failed with error code {result}"),
                });
            }

            let _guard = WlanMemoryGuard(network_list_ptr as *mut c_void);

            let network_list = unsafe { &*network_list_ptr };
            let count = network_list.dwNumberOfItems as usize;
            let networks_ptr = network_list.Network.as_ptr();

            let current_conn = query_wlan_connection(&handle, &iface.guid).ok().flatten();

            let mut aps = Vec::with_capacity(count);
            for i in 0..count {
                let net = unsafe { &*networks_ptr.add(i) };
                if let Some(ap) = convert_wlan_network(net, current_conn.as_ref()) {
                    aps.push(ap);
                }
            }

            aps.sort_unstable_by(|a, b| b.signal_quality.cmp(&a.signal_quality));
            aps.dedup_by(|a, b| a.ssid == b.ssid && a.bssid == b.bssid);

            Ok(aps)
        })
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: "scan_wifi spawn_blocking".to_string(),
            reason: format!("task join failed: {e}"),
        })??;

        Ok(aps)
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
        let hidden = request.hidden.unwrap_or(false);

        info!(ssid = %ssid, "Connecting to Wi-Fi via Native Wifi API");

        let iface_name = tokio::task::spawn_blocking(move || -> Result<String, NetworkError> {
            let handle = WlanHandle::open()?;
            let interfaces = enumerate_wlan_interfaces_with_handle(&handle)?;
            let iface = interfaces.first().ok_or_else(|| {
                NetworkError::InterfaceNotFound("No Wi-Fi interface available".to_string())
            })?;

            // Use adapter FriendlyName (not WLAN description) for netsh/configure_interface.
            let adapters = get_adapters_addresses().map_err(|e| {
                NetworkError::CommandFailed {
                    command: "GetAdaptersAddresses".to_string(),
                    reason: e,
                }
            })?;
            let netsh_name = adapters
                .iter()
                .find(|a| a.guid == iface.guid)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| iface.description.clone());

            // Build and set a WLAN profile XML.
            let profile_xml = build_wifi_profile_xml(&ssid, password.as_deref(), hidden);
            let profile_wide: Vec<u16> = profile_xml
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let set_result = unsafe {
                windows::Win32::NetworkManagement::WiFi::WlanSetProfile(
                    handle.as_raw(),
                    &iface.guid,
                    0, // all-user profile
                    PCWSTR(profile_wide.as_ptr()),
                    None,
                    true, // overwrite existing
                    None,
                    std::ptr::null_mut(),
                )
            };

            if set_result != ERROR_SUCCESS.0 {
                return Err(NetworkError::WifiError(format!(
                    "WlanSetProfile failed with error code {set_result}"
                )));
            }

            // Build connection parameters.
            let profile_name_wide: Vec<u16> =
                ssid.encode_utf16().chain(std::iter::once(0)).collect();
            let mut dot11_ssid = DOT11_SSID {
                uSSIDLength: ssid.len().min(32) as u32,
                ucSSID: [0u8; 32],
            };
            dot11_ssid.ucSSID[..ssid.len().min(32)]
                .copy_from_slice(&ssid.as_bytes()[..ssid.len().min(32)]);

            let params = WLAN_CONNECTION_PARAMETERS {
                wlanConnectionMode: WLAN_CONNECTION_MODE(0), // wlan_connection_mode_profile
                strProfile: PCWSTR(profile_name_wide.as_ptr()),
                pDot11Ssid: &mut dot11_ssid,
                pDesiredBssidList: std::ptr::null_mut(),
                dot11BssType: DOT11_BSS_TYPE(1), // dot11_BSS_type_infrastructure
                dwFlags: 0,
            };

            let connect_result =
                unsafe { WlanConnect(handle.as_raw(), &iface.guid, &params, None) };

            if connect_result != ERROR_SUCCESS.0 {
                return Err(NetworkError::WifiError(format!(
                    "WlanConnect failed with error code {connect_result}"
                )));
            }

            Ok(netsh_name)
        })
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: "connect_wifi spawn_blocking".to_string(),
            reason: format!("task join failed: {e}"),
        })??;

        // Poll until connected.
        let connect_deadline = Instant::now() + Duration::from_secs(15);
        let mut status = loop {
            let current = self.wifi_sta_status(None).await?;
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

        // Apply static IP if requested.
        if let Some(ref ip_config) = request.ip_config {
            let config_iface = iface_name.clone();
            self.configure_interface(
                &config_iface,
                &ConfigureInterfaceRequest {
                    ip_config: ip_config.clone(),
                },
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            status = self.wifi_sta_status(None).await?;
        }

        info!(ssid = %request.ssid, "Wi-Fi connection established via Native Wifi API");
        Ok(status)
    }

    async fn disconnect_wifi(&self, _request: &WifiDisconnectRequest) -> NGResult<()> {
        info!("Disconnecting Wi-Fi via Native Wifi API");

        tokio::task::spawn_blocking(move || -> Result<(), NetworkError> {
            let handle = WlanHandle::open()?;
            let interfaces = enumerate_wlan_interfaces_with_handle(&handle)?;
            let iface = interfaces.first().ok_or_else(|| {
                NetworkError::InterfaceNotFound("No Wi-Fi interface available".to_string())
            })?;

            let result = unsafe { WlanDisconnect(handle.as_raw(), &iface.guid, None) };

            if result != ERROR_SUCCESS.0 {
                return Err(NetworkError::WifiError(format!(
                    "WlanDisconnect failed with error code {result}"
                )));
            }

            Ok(())
        })
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: "disconnect_wifi spawn_blocking".to_string(),
            reason: format!("task join failed: {e}"),
        })??;

        info!("Wi-Fi disconnected via Native Wifi API");
        Ok(())
    }

    async fn wifi_sta_status(&self, _interface_name: Option<&str>) -> NGResult<WifiStaStatus> {
        let adapters = tokio::task::spawn_blocking(get_adapters_addresses)
            .await
            .map_err(|e| NetworkError::CommandFailed {
                command: "GetAdaptersAddresses".to_string(),
                reason: format!("task join failed: {e}"),
            })?
            .unwrap_or_default();

        let conn = tokio::task::spawn_blocking(|| -> Option<NativeConnectionInfo> {
            let handle = WlanHandle::open().ok()?;
            let ifaces = enumerate_wlan_interfaces_with_handle(&handle).ok()?;
            let first = ifaces.first()?;
            query_wlan_connection(&handle, &first.guid).ok()?
        })
        .await
        .unwrap_or(None);

        let wifi_adapter = adapters.iter().find(|a| a.if_type == 71);

        let connected = conn.as_ref().map(|c| c.ssid.is_some()).unwrap_or(false);

        let (ip_address, gateway, dns) = if connected {
            if let Some(adapter) = wifi_adapter {
                let ip = adapter
                    .ipv4
                    .as_ref()
                    .and_then(|v4| v4.addresses.first().map(|a| a.address));
                let gw = adapter.ipv4.as_ref().and_then(|v4| v4.gateway);
                let d = adapter
                    .ipv4
                    .as_ref()
                    .map(|v4| v4.dns.clone())
                    .unwrap_or_default();
                (ip, gw, d)
            } else {
                (None, None, Vec::new())
            }
        } else {
            (None, None, Vec::new())
        };

        let iface_name = wifi_adapter.map(|a| a.name.clone());

        if let Some(ref c) = conn {
            Ok(WifiStaStatus {
                connected,
                interface_name: iface_name,
                ssid: c.ssid.clone(),
                bssid: c.bssid.clone(),
                security: Some(c.security),
                band: c.channel.map(|ch| channel_to_band(ch)),
                channel: c.channel,
                frequency: c
                    .channel
                    .map(|ch| channel_to_frequency(ch, channel_to_band(ch))),
                signal_dbm: Some(quality_to_approx_rssi(c.signal_quality as u8)),
                signal_quality: Some(c.signal_quality as u8),
                ip_address,
                gateway,
                dns,
                speed_mbps: c.speed_mbps,
                connected_secs: None,
            })
        } else {
            Ok(WifiStaStatus {
                connected: false,
                interface_name: iface_name,
                ..Default::default()
            })
        }
    }

    async fn list_saved_wifi_connections(&self) -> NGResult<Vec<SavedWifiConnection>> {
        let saved =
            tokio::task::spawn_blocking(|| -> Result<Vec<SavedWifiConnection>, NetworkError> {
                let handle = WlanHandle::open()?;
                let interfaces = enumerate_wlan_interfaces_with_handle(&handle)?;
                let iface = interfaces.first().ok_or_else(|| {
                    NetworkError::InterfaceNotFound("No Wi-Fi interface available".to_string())
                })?;

                let mut profile_list_ptr: *mut WLAN_PROFILE_INFO_LIST = std::ptr::null_mut();
                let result = unsafe {
                    WlanGetProfileList(handle.as_raw(), &iface.guid, None, &mut profile_list_ptr)
                };

                if result != ERROR_SUCCESS.0 {
                    return Err(NetworkError::CommandFailed {
                        command: "WlanGetProfileList".to_string(),
                        reason: format!("Failed with error code {result}"),
                    });
                }

                let _guard = WlanMemoryGuard(profile_list_ptr as *mut c_void);
                let profile_list = unsafe { &*profile_list_ptr };
                let count = profile_list.dwNumberOfItems as usize;
                let profiles_ptr = profile_list.ProfileInfo.as_ptr();

                let current_conn = query_wlan_connection(&handle, &iface.guid).ok().flatten();
                let current_ssid = current_conn.as_ref().and_then(|c| c.ssid.clone());

                let mut connections = Vec::with_capacity(count);
                for i in 0..count {
                    let profile = unsafe { &*profiles_ptr.add(i) };
                    let ssid = wchar_to_string(&profile.strProfileName);
                    if ssid.is_empty() {
                        continue;
                    }

                    let is_active = current_ssid.as_deref() == Some(&ssid);
                    let security = if is_active {
                        current_conn
                            .as_ref()
                            .map(|c| c.security)
                            .unwrap_or(WifiSecurity::Unknown)
                    } else {
                        WifiSecurity::Unknown
                    };

                    connections.push(SavedWifiConnection {
                        uuid: ssid.clone(),
                        ssid,
                        is_active,
                        autoconnect: true,
                        security,
                        ip_config: IpConfig::Dhcp {
                            dns: None,
                            search_domains: None,
                        },
                        last_connected: None,
                    });
                }

                Ok(connections)
            })
            .await
            .map_err(|e| NetworkError::CommandFailed {
                command: "list_saved_wifi_connections spawn_blocking".to_string(),
                reason: format!("task join failed: {e}"),
            })??;

        Ok(saved)
    }

    async fn forget_wifi(&self, request: &ForgetWifiRequest) -> NGResult<()> {
        let profile_name = request.uuid.clone();

        info!(profile = %profile_name, "Deleting Wi-Fi profile via Native Wifi API");

        tokio::task::spawn_blocking(move || -> Result<(), NetworkError> {
            let handle = WlanHandle::open()?;
            let interfaces = enumerate_wlan_interfaces_with_handle(&handle)?;
            let iface = interfaces.first().ok_or_else(|| {
                NetworkError::InterfaceNotFound("No Wi-Fi interface available".to_string())
            })?;

            let profile_wide: Vec<u16> = profile_name
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let result = unsafe {
                WlanDeleteProfile(
                    handle.as_raw(),
                    &iface.guid,
                    PCWSTR(profile_wide.as_ptr()),
                    None,
                )
            };

            if result != ERROR_SUCCESS.0 {
                return Err(NetworkError::WifiError(format!(
                    "WlanDeleteProfile failed with error code {result}"
                )));
            }

            Ok(())
        })
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: "forget_wifi spawn_blocking".to_string(),
            reason: format!("task join failed: {e}"),
        })??;

        info!(profile = %request.uuid, "Wi-Fi profile deleted via Native Wifi API");
        Ok(())
    }

    // ─── AP Hotspot (not supported on Windows) ───

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
        Err(NetworkError::PlatformNotSupported(
            "AP mode is not available on Windows (hostednetwork deprecated since Win10)"
                .to_string(),
        )
        .into())
    }

    async fn stop_ap(&self) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP mode is not available on Windows".to_string())
                .into(),
        )
    }

    async fn configure_ap(&self, _config: &ConfigureApRequest) -> NGResult<ApStatus> {
        Err(
            NetworkError::PlatformNotSupported("AP mode is not available on Windows".to_string())
                .into(),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Native Wifi API operations
// ═══════════════════════════════════════════════════════════════════════════

/// RAII guard for WLAN memory allocated by the API.
struct WlanMemoryGuard(*mut c_void);

impl Drop for WlanMemoryGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WlanFreeMemory(self.0) };
        }
    }
}

/// Enumerate all WLAN interfaces (opens and closes its own handle).
fn enumerate_wlan_interfaces() -> Result<Vec<NativeWlanInterface>, NetworkError> {
    let handle = WlanHandle::open()?;
    enumerate_wlan_interfaces_with_handle(&handle)
}

/// Enumerate WLAN interfaces using an existing handle.
fn enumerate_wlan_interfaces_with_handle(
    handle: &WlanHandle,
) -> Result<Vec<NativeWlanInterface>, NetworkError> {
    let mut iface_list_ptr: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    let result = unsafe { WlanEnumInterfaces(handle.as_raw(), None, &mut iface_list_ptr) };

    if result != ERROR_SUCCESS.0 {
        return Err(NetworkError::CommandFailed {
            command: "WlanEnumInterfaces".to_string(),
            reason: format!("Failed with error code {result}"),
        });
    }

    let _guard = WlanMemoryGuard(iface_list_ptr as *mut c_void);
    let iface_list = unsafe { &*iface_list_ptr };
    let count = iface_list.dwNumberOfItems as usize;
    let entries_ptr = iface_list.InterfaceInfo.as_ptr();

    let mut interfaces = Vec::with_capacity(count);
    for i in 0..count {
        let info = unsafe { &*entries_ptr.add(i) };
        interfaces.push(NativeWlanInterface {
            guid: info.InterfaceGuid,
            description: wchar_to_string(&info.strInterfaceDescription),
            state: info.isState,
        });
    }

    Ok(interfaces)
}

/// Query connection attributes for a WLAN interface via `WlanQueryInterface`.
fn query_wlan_connection(
    handle: &WlanHandle,
    guid: &windows::core::GUID,
) -> Result<Option<NativeConnectionInfo>, NetworkError> {
    let mut data_size: u32 = 0;
    let mut data_ptr: *mut c_void = std::ptr::null_mut();

    // wlan_intf_opcode_current_connection = 7
    let opcode = windows::Win32::NetworkManagement::WiFi::WLAN_INTF_OPCODE(7);

    let result = unsafe {
        WlanQueryInterface(
            handle.as_raw(),
            guid,
            opcode,
            None,
            &mut data_size,
            &mut data_ptr,
            None,
        )
    };

    if result != ERROR_SUCCESS.0 {
        // Not connected — this is normal.
        return Ok(None);
    }

    let _guard = WlanMemoryGuard(data_ptr);
    let attrs = unsafe { &*(data_ptr as *const WLAN_CONNECTION_ATTRIBUTES) };

    let assoc = &attrs.wlanAssociationAttributes;
    let sec = &attrs.wlanSecurityAttributes;

    let ssid = dot11_ssid_to_string(&assoc.dot11Ssid);
    let bssid = mac_to_string(&assoc.dot11Bssid);

    let security = auth_algo_to_security(sec.dot11AuthAlgorithm);

    // Channel: WLAN_ASSOCIATION_ATTRIBUTES does not have ulChCenterFrequency
    // (that field is in WLAN_BSS_ENTRY). Use 0 when unavailable.
    let freq_khz = 0u32;
    let freq_mhz = if freq_khz > 0 { freq_khz / 1000 } else { 0 };
    let channel = if freq_mhz > 0 {
        Some(frequency_to_channel(freq_mhz))
    } else {
        None
    };

    let speed_mbps = {
        let rx = assoc.ulRxRate / 1000;
        let tx = assoc.ulTxRate / 1000;
        let max = rx.max(tx);
        if max > 0 {
            Some(max)
        } else {
            None
        }
    };

    let signal = assoc.wlanSignalQuality;

    Ok(Some(NativeConnectionInfo {
        ssid: if ssid.is_empty() { None } else { Some(ssid) },
        bssid: if bssid.is_empty() || bssid == "00:00:00:00:00:00" {
            None
        } else {
            Some(bssid)
        },
        security,
        signal_quality: signal,
        channel,
        speed_mbps,
        state: attrs.isState,
    }))
}

/// Convert a `WLAN_AVAILABLE_NETWORK` to our domain `WifiAccessPoint`.
fn convert_wlan_network(
    net: &WLAN_AVAILABLE_NETWORK,
    current_conn: Option<&NativeConnectionInfo>,
) -> Option<WifiAccessPoint> {
    let ssid = dot11_ssid_to_string(&net.dot11Ssid);
    if ssid.is_empty() {
        return None;
    }

    let signal_quality = net.wlanSignalQuality.min(100) as u8;
    let signal_dbm = quality_to_approx_rssi(signal_quality);
    let security = auth_algo_to_security(net.dot11DefaultAuthAlgorithm);
    let is_connected = current_conn
        .and_then(|c| c.ssid.as_deref())
        .map(|s| s == ssid)
        .unwrap_or(false);

    Some(WifiAccessPoint {
        ssid,
        bssid: String::new(), // Available network list doesn't include per-BSS info.
        security,
        band: WifiBand::Unknown,
        channel: 0,
        frequency: 0,
        signal_dbm,
        signal_quality,
        max_bitrate_kbps: None,
        is_connected,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// IP Helper API: GetAdaptersAddresses
// ═══════════════════════════════════════════════════════════════════════════

/// Parsed adapter info from `GetAdaptersAddresses`.
#[derive(Debug, Clone)]
struct AdapterInfo {
    name: String,
    description: String,
    mac_address: Option<String>,
    if_type: u32,
    guid: windows::core::GUID,
    oper_status_up: bool,
    ipv4: Option<Ipv4Config>,
    ipv6: Option<Ipv6Config>,
    mtu: Option<u32>,
}

/// Call `GetAdaptersAddresses` and parse the linked list into structured data.
fn get_adapters_addresses() -> Result<Vec<AdapterInfo>, String> {
    let flags = GET_ADAPTERS_ADDRESSES_FLAGS(0x0010 | 0x0080); // GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_INCLUDE_ALL_INTERFACES

    let mut buf_size: u32 = 15000;
    let mut buffer: Vec<u8> = vec![0u8; buf_size as usize];

    let mut result = unsafe {
        GetAdaptersAddresses(
            AF_UNSPEC.0 as u32,
            flags,
            None,
            Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
            &mut buf_size,
        )
    };

    // ERROR_BUFFER_OVERFLOW = 111
    if result == 111 {
        buffer.resize(buf_size as usize, 0);
        result = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.0 as u32,
                flags,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut buf_size,
            )
        };
    }

    if result != ERROR_SUCCESS.0 {
        return Err(format!("GetAdaptersAddresses failed with error {result}"));
    }

    let mut adapters = Vec::new();
    let mut current = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;

    while !current.is_null() {
        let adapter = unsafe { &*current };

        let name = unsafe { adapter.FriendlyName.to_string() }.unwrap_or_default();
        let description = unsafe { adapter.Description.to_string() }.unwrap_or_default();

        let mac = if adapter.PhysicalAddressLength > 0 {
            let bytes = &adapter.PhysicalAddress[..adapter.PhysicalAddressLength as usize];
            Some(
                bytes
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(":"),
            )
        } else {
            None
        };

        let oper_up = adapter.OperStatus == IF_OPER_STATUS(1); // IfOperStatusUp

        let mut ipv4_addresses = Vec::new();
        let mut ipv6_addresses = Vec::new();

        // Walk unicast address linked list.
        let mut unicast = adapter.FirstUnicastAddress;
        while !unicast.is_null() {
            let addr = unsafe { &*unicast };
            let sa = addr.Address;
            if !sa.lpSockaddr.is_null() {
                let family = unsafe { (*sa.lpSockaddr).sa_family };
                if family == AF_INET {
                    let sin = unsafe { &*(sa.lpSockaddr as *const SOCKADDR_IN) };
                    let raw = unsafe { sin.sin_addr.S_un.S_addr };
                    let ip = Ipv4Addr::from(raw.to_be_bytes());
                    ipv4_addresses.push(Ipv4AddressInfo {
                        address: IpAddr::V4(ip),
                        prefix_length: addr.OnLinkPrefixLength,
                    });
                } else if family == AF_INET6 {
                    let sin6 = unsafe { &*(sa.lpSockaddr as *const SOCKADDR_IN6) };
                    let ip = Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte });
                    ipv6_addresses.push(Ipv6AddressInfo {
                        address: IpAddr::V6(ip),
                        prefix_length: addr.OnLinkPrefixLength,
                    });
                }
            }
            unicast = addr.Next;
        }

        // Walk gateway address linked list.
        let mut gateway_v4: Option<IpAddr> = None;
        let mut gateway_v6: Option<IpAddr> = None;
        let mut gw = adapter.FirstGatewayAddress;
        while !gw.is_null() {
            let gw_addr = unsafe { &*gw };
            let sa = gw_addr.Address;
            if !sa.lpSockaddr.is_null() {
                let family = unsafe { (*sa.lpSockaddr).sa_family };
                if family == AF_INET && gateway_v4.is_none() {
                    let sin = unsafe { &*(sa.lpSockaddr as *const SOCKADDR_IN) };
                    let raw = unsafe { sin.sin_addr.S_un.S_addr };
                    let ip = Ipv4Addr::from(raw.to_be_bytes());
                    gateway_v4 = Some(IpAddr::V4(ip));
                } else if family == AF_INET6 && gateway_v6.is_none() {
                    let sin6 = unsafe { &*(sa.lpSockaddr as *const SOCKADDR_IN6) };
                    let ip = Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte });
                    gateway_v6 = Some(IpAddr::V6(ip));
                }
            }
            gw = gw_addr.Next;
        }

        // Walk DNS server address linked list.
        let mut dns_v4: Vec<IpAddr> = Vec::new();
        let mut dns_v6: Vec<IpAddr> = Vec::new();
        let mut dns = adapter.FirstDnsServerAddress;
        while !dns.is_null() {
            let dns_addr = unsafe { &*dns };
            let sa = dns_addr.Address;
            if !sa.lpSockaddr.is_null() {
                let family = unsafe { (*sa.lpSockaddr).sa_family };
                if family == AF_INET {
                    let sin = unsafe { &*(sa.lpSockaddr as *const SOCKADDR_IN) };
                    let raw = unsafe { sin.sin_addr.S_un.S_addr };
                    let ip = Ipv4Addr::from(raw.to_be_bytes());
                    dns_v4.push(IpAddr::V4(ip));
                } else if family == AF_INET6 {
                    let sin6 = unsafe { &*(sa.lpSockaddr as *const SOCKADDR_IN6) };
                    let ip = Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte });
                    dns_v6.push(IpAddr::V6(ip));
                }
            }
            dns = dns_addr.Next;
        }

        // Determine IP method from DHCP flag (union access requires unsafe).
        let flags = unsafe { adapter.Anonymous2.Flags };
        let is_dhcp = flags & 0x0004 != 0; // IP_ADAPTER_DHCP_ENABLED
        let method = if is_dhcp {
            IpMethod::Dhcp
        } else {
            IpMethod::Static
        };

        let ipv4 = if !ipv4_addresses.is_empty() {
            Some(Ipv4Config {
                addresses: ipv4_addresses,
                gateway: gateway_v4,
                dns: dns_v4,
                search_domains: Vec::new(),
                method,
            })
        } else {
            None
        };

        let ipv6 = if !ipv6_addresses.is_empty() {
            Some(Ipv6Config {
                addresses: ipv6_addresses,
                gateway: gateway_v6,
                dns: dns_v6,
                method: IpMethod::Dhcp,
            })
        } else {
            None
        };

        let mtu = if adapter.Mtu > 0 {
            Some(adapter.Mtu)
        } else {
            None
        };

        adapters.push(AdapterInfo {
            name,
            description,
            mac_address: mac,
            if_type: adapter.IfType,
            guid: adapter.NetworkGuid,
            oper_status_up: oper_up,
            ipv4,
            ipv6,
            mtu,
        });

        current = adapter.Next;
    }

    Ok(adapters)
}

// ═══════════════════════════════════════════════════════════════════════════
// Conversion helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a `DOT11_SSID` to a UTF-8 string.
fn dot11_ssid_to_string(ssid: &DOT11_SSID) -> String {
    let len = ssid.uSSIDLength as usize;
    if len == 0 || len > 32 {
        return String::new();
    }
    String::from_utf8_lossy(&ssid.ucSSID[..len]).to_string()
}

/// Convert a 6-byte MAC address to a colon-separated hex string.
fn mac_to_string(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Convert a wide-char (u16) null-terminated array to a String.
fn wchar_to_string(wchar: &[u16]) -> String {
    let len = wchar.iter().position(|&c| c == 0).unwrap_or(wchar.len());
    String::from_utf16_lossy(&wchar[..len])
}

/// Convert `DOT11_AUTH_ALGORITHM` to our domain `WifiSecurity`.
fn auth_algo_to_security(auth: DOT11_AUTH_ALGORITHM) -> WifiSecurity {
    if auth == DOT11_AUTH_ALGO_WPA3_SAE || auth == DOT11_AUTH_ALGO_WPA3 {
        WifiSecurity::Wpa3Sae
    } else if auth == DOT11_AUTH_ALGO_RSNA_PSK {
        WifiSecurity::Wpa2Psk
    } else if auth == DOT11_AUTH_ALGO_RSNA {
        WifiSecurity::Wpa2Enterprise
    } else if auth == DOT11_AUTH_ALGO_WPA_PSK {
        WifiSecurity::WpaPsk
    } else if auth == DOT11_AUTH_ALGO_WPA {
        WifiSecurity::WpaEnterprise
    } else if auth == DOT11_AUTH_ALGO_80211_OPEN {
        WifiSecurity::Open
    } else {
        WifiSecurity::Unknown
    }
}

/// Approximate RSSI (dBm) from Windows signal quality percentage (0-100).
///
/// Windows reports signal as a quality percentage; the inverse mapping uses
/// Microsoft's documented formula: `quality = 2 * (dBm + 100)` for dBm in [-100, -50].
fn quality_to_approx_rssi(quality: u8) -> i32 {
    (quality as i32 / 2) - 100
}

/// Convert channel number to Wi-Fi band.
fn channel_to_band(channel: u32) -> WifiBand {
    match channel {
        1..=14 => WifiBand::Band2_4Ghz,
        32..=177 => WifiBand::Band5Ghz,
        _ => WifiBand::Unknown,
    }
}

/// Convert channel number and band to frequency in MHz.
fn channel_to_frequency(channel: u32, band: WifiBand) -> u32 {
    match band {
        WifiBand::Band2_4Ghz => {
            if channel == 14 {
                2484
            } else if channel >= 1 && channel <= 13 {
                2407 + channel * 5
            } else {
                0
            }
        }
        WifiBand::Band5Ghz => 5000 + channel * 5,
        _ => 0,
    }
}

/// Convert frequency in MHz to channel number.
fn frequency_to_channel(freq_mhz: u32) -> u32 {
    if freq_mhz == 2484 {
        14
    } else if (2412..=2472).contains(&freq_mhz) {
        (freq_mhz - 2407) / 5
    } else if (5170..=5885).contains(&freq_mhz) {
        (freq_mhz - 5000) / 5
    } else if (5925..=7125).contains(&freq_mhz) {
        (freq_mhz - 5950) / 5
    } else {
        0
    }
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

/// Build a WLAN profile XML for WPA2-PSK or Open network.
fn build_wifi_profile_xml(ssid: &str, password: Option<&str>, hidden: bool) -> String {
    let connection_mode = if hidden { "manual" } else { "auto" };
    let non_broadcast = if hidden { "true" } else { "false" };

    if let Some(pw) = password {
        format!(
            r#"<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1">
  <name>{ssid}</name>
  <SSIDConfig>
    <SSID><name>{ssid}</name></SSID>
    <nonBroadcast>{non_broadcast}</nonBroadcast>
  </SSIDConfig>
  <connectionType>ESS</connectionType>
  <connectionMode>{connection_mode}</connectionMode>
  <MSM>
    <security>
      <authEncryption>
        <authentication>WPA2PSK</authentication>
        <encryption>AES</encryption>
        <useOneX>false</useOneX>
      </authEncryption>
      <sharedKey>
        <keyType>passPhrase</keyType>
        <protected>false</protected>
        <keyMaterial>{pw}</keyMaterial>
      </sharedKey>
    </security>
  </MSM>
</WLANProfile>"#
        )
    } else {
        format!(
            r#"<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1">
  <name>{ssid}</name>
  <SSIDConfig>
    <SSID><name>{ssid}</name></SSID>
    <nonBroadcast>{non_broadcast}</nonBroadcast>
  </SSIDConfig>
  <connectionType>ESS</connectionType>
  <connectionMode>{connection_mode}</connectionMode>
  <MSM>
    <security>
      <authEncryption>
        <authentication>open</authentication>
        <encryption>none</encryption>
        <useOneX>false</useOneX>
      </authEncryption>
    </security>
  </MSM>
</WLANProfile>"#
        )
    }
}

/// Interfaces to skip during enumeration.
fn should_skip_interface(name: &str, description: &str) -> bool {
    let lower = name.to_lowercase();
    let desc_lower = description.to_lowercase();

    lower == "loopback pseudo-interface 1"
        || lower.starts_with("isatap")
        || lower.starts_with("teredo")
        || desc_lower.contains("virtual")
        || desc_lower.contains("vmware")
        || desc_lower.contains("virtualbox")
        || desc_lower.contains("hyper-v")
        || desc_lower.contains("loopback")
        || desc_lower.contains("pseudo")
}

// ═══════════════════════════════════════════════════════════════════════════
// CLI fallback — minimal use for IP configuration writes only
// ═══════════════════════════════════════════════════════════════════════════

/// Run a `netsh` command and return stdout on success.
async fn run_netsh(args: &[&str]) -> Result<String, NetworkError> {
    debug!(command = %format!("netsh {}", args.join(" ")), "Running netsh");

    let output = Command::new("netsh")
        .args(args)
        .output()
        .await
        .map_err(|e| NetworkError::CommandFailed {
            command: format!("netsh {}", args.join(" ")),
            reason: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(NetworkError::CommandFailed {
            command: format!("netsh {}", args.join(" ")),
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
