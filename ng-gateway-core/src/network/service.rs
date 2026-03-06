//! High-level network service façade.
//!
//! [`NetworkService`] is the entry point consumed by REST API handlers.
//! It delegates to the platform-specific [`PlatformNetworkManager`] and manages
//! caching, capability state, and startup AP initialization.

use crate::network::platform::{self, PlatformNetworkManager};
use ng_gateway_error::NGResult;
use ng_gateway_models::{
    domain::prelude::{
        ApStatus, ConfigureApRequest, ConfigureDnsRequest, ConfigureInterfaceRequest, DnsConfig,
        InterfaceKind, LinkState, NetworkCapabilities, NetworkInterfaceDetail,
        NetworkInterfaceSummary, WifiAccessPoint, WifiConnectRequest, WifiStaStatus, WiredStatus,
    },
    settings::Network as NetworkSettings,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, instrument, warn};

/// High-level network management service.
///
/// Thread-safe (`Arc`-wrapped internally) — clone freely across handlers.
pub struct NetworkService {
    /// Platform-specific network manager.
    manager: Box<dyn PlatformNetworkManager>,
    /// Cached platform capabilities (refreshed on startup and on demand).
    capabilities: Arc<RwLock<Option<NetworkCapabilities>>>,
    /// Network configuration from `gateway.toml`.
    settings: NetworkSettings,
}

impl NetworkService {
    /// Create and initialize the network service.
    #[instrument(name = "network-service-init", skip(settings))]
    pub async fn new(settings: NetworkSettings) -> NGResult<Self> {
        info!("Initializing network service...");

        let manager = platform::create_platform_manager().await?;

        let service = Self {
            manager,
            capabilities: Arc::new(RwLock::new(None)),
            settings,
        };

        // Pre-warm capabilities cache.
        match service.refresh_capabilities().await {
            Ok(caps) => {
                info!(
                    platform = ?caps.platform,
                    nm_available = caps.network_manager_available,
                    can_wifi = caps.can_scan_wifi,
                    can_ap = caps.can_manage_ap,
                    "Platform capabilities detected"
                );
            }
            Err(e) => {
                warn!("Failed to detect platform capabilities on startup: {e}");
            }
        }

        // Verify AP service status on Linux.
        #[cfg(target_os = "linux")]
        if settings.ap.enabled {
            service.verify_ap_services().await;
        }

        info!("Network service initialized");
        Ok(service)
    }

    /// Whether the network module is enabled in configuration.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Return a reference to the network settings.
    #[inline]
    pub fn settings(&self) -> &NetworkSettings {
        &self.settings
    }

    // ─── Interface Discovery ───

    /// List all network interfaces.
    #[inline]
    pub async fn list_interfaces(&self) -> NGResult<Vec<NetworkInterfaceSummary>> {
        self.manager.list_interfaces().await
    }

    /// Get detailed info for a specific interface.
    #[inline]
    pub async fn get_interface(&self, name: &str) -> NGResult<NetworkInterfaceDetail> {
        self.manager.get_interface(name).await
    }

    // ─── Capabilities ───

    /// Get cached capabilities or refresh if not available.
    pub async fn capabilities(&self) -> NGResult<NetworkCapabilities> {
        {
            let guard = self.capabilities.read().await;
            if let Some(caps) = guard.as_ref() {
                return Ok(caps.clone());
            }
        }
        self.refresh_capabilities().await
    }

    /// Force-refresh platform capabilities.
    pub async fn refresh_capabilities(&self) -> NGResult<NetworkCapabilities> {
        let caps = self.manager.detect_capabilities().await?;
        let mut guard = self.capabilities.write().await;
        *guard = Some(caps.clone());
        Ok(caps)
    }

    // ─── Aggregated Status ───

    /// Get the best wired interface with enriched status (gateway, DNS filled in).
    ///
    /// Selection priority: connected (up) ethernet > first ethernet > none.
    /// This is the single source of truth for the "Wired Network" tab —
    /// the frontend does not need to do interface selection.
    pub async fn wired_status(&self) -> NGResult<WiredStatus> {
        let all = self.manager.list_interfaces().await?;
        let ethernet: Vec<NetworkInterfaceSummary> = all
            .into_iter()
            .filter(|i| i.kind == InterfaceKind::Ethernet)
            .collect();

        if ethernet.is_empty() {
            return Ok(WiredStatus {
                available: false,
                interface: None,
                all_interfaces: Vec::new(),
            });
        }

        let best = ethernet
            .iter()
            .find(|i| i.link_state == LinkState::Up)
            .or_else(|| ethernet.first())
            .cloned();

        Ok(WiredStatus {
            available: true,
            interface: best,
            all_interfaces: ethernet,
        })
    }

    // ─── Interface Configuration ───

    /// Configure IP settings for an interface.
    #[inline]
    pub async fn configure_interface(
        &self,
        name: &str,
        config: &ConfigureInterfaceRequest,
    ) -> NGResult<()> {
        self.manager.configure_interface(name, config).await
    }

    // ─── Wi-Fi ───

    /// Scan for Wi-Fi access points.
    #[inline]
    pub async fn scan_wifi(&self, interface_name: Option<&str>) -> NGResult<Vec<WifiAccessPoint>> {
        self.manager.scan_wifi(interface_name).await
    }

    /// Connect to a Wi-Fi network.
    #[inline]
    pub async fn connect_wifi(&self, request: &WifiConnectRequest) -> NGResult<WifiStaStatus> {
        self.manager.connect_wifi(request).await
    }

    /// Disconnect Wi-Fi STA.
    #[inline]
    pub async fn disconnect_wifi(&self, interface_name: Option<&str>) -> NGResult<()> {
        self.manager.disconnect_wifi(interface_name).await
    }

    /// Get Wi-Fi STA connection status.
    #[inline]
    pub async fn wifi_sta_status(&self, interface_name: Option<&str>) -> NGResult<WifiStaStatus> {
        self.manager.wifi_sta_status(interface_name).await
    }

    // ─── AP Hotspot ───

    /// Get AP hotspot status.
    #[inline]
    pub async fn ap_status(&self) -> NGResult<ApStatus> {
        self.manager.ap_status().await
    }

    /// Configure AP hotspot.
    #[inline]
    pub async fn configure_ap(&self, config: &ConfigureApRequest) -> NGResult<ApStatus> {
        self.manager.configure_ap(config).await
    }

    // ─── DNS ───

    /// Get current DNS configuration.
    #[inline]
    pub async fn get_dns(&self) -> NGResult<DnsConfig> {
        self.manager.get_dns().await
    }

    /// Set DNS configuration.
    #[inline]
    pub async fn configure_dns(&self, config: &ConfigureDnsRequest) -> NGResult<()> {
        self.manager.configure_dns(config).await
    }

    /// Verify AP systemd service status on startup (Linux only).
    ///
    /// This is a best-effort check — if AP services are not running but should be,
    /// we log a warning. The gateway does NOT attempt to start them (that's systemd's job).
    #[cfg(target_os = "linux")]
    async fn verify_ap_services(&self) {
        use crate::network::ap_manager::ApServiceManager;

        match ApServiceManager::new().await {
            Ok(mgr) => match mgr.status().await {
                Ok(status) => {
                    if status.ap_broadcasting() {
                        info!("AP hotspot is active (hostapd running)");
                    } else {
                        warn!(
                            "AP hotspot is not broadcasting. \
                             Check: systemctl status ng-gateway-hostapd"
                        );
                    }
                }
                Err(e) => {
                    warn!("Failed to query AP service status: {e}");
                }
            },
            Err(e) => {
                info!("AP service manager not available (expected on dev machines): {e}");
            }
        }
    }
}
