//! AP configuration file renderer.
//!
//! Generates three configuration files consumed by the systemd-managed AP services:
//! - `ap-env` — shell variables sourced by `ng-gateway-ap-setup.service`
//! - `hostapd.conf` — hostapd main configuration
//! - `dnsmasq-ap.conf` — dnsmasq DHCP/DNS for AP clients
//!
//! All files are written to `/etc/ng-gateway/` (or a configurable base directory).

use ng_gateway_error::{network::NetworkError, NGResult};
use ng_gateway_models::domain::prelude::WifiBand;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, info};

/// Default configuration directory for AP files.
pub const AP_CONFIG_DIR: &str = "/etc/ng-gateway";

/// Configuration file names.
pub const AP_ENV_FILE: &str = "ap-env";
pub const HOSTAPD_CONF_FILE: &str = "hostapd.conf";
pub const DNSMASQ_AP_CONF_FILE: &str = "dnsmasq-ap.conf";

/// Default Wi-Fi regulatory country code.
///
/// Required by hostapd (`country_code` + `ieee80211d=1`) to set the correct
/// regulatory domain. Without it the kernel stays in "world" mode where all
/// frequencies are marked `PASSIVE-SCAN` and the AP may not broadcast beacons,
/// making the hotspot invisible to nearby devices.
pub const DEFAULT_COUNTRY_CODE: &str = "CN";

/// Resolved AP configuration ready for rendering.
pub struct ApRenderContext {
    pub interface: String,
    pub ssid: String,
    pub password: String,
    pub channel: u32,
    pub ip: String,
    pub prefix_length: u8,
    pub dhcp_range_start: String,
    pub dhcp_range_end: String,
    pub dhcp_lease_time: String,
    /// The primary Wi-Fi STA interface (used for NAT uplink in concurrent mode).
    pub sta_iface: String,
    /// The detected uplink interface (interface with default route).
    pub uplink_iface: String,
    /// Whether AP and STA are mutually exclusive (single card, no concurrency).
    pub exclusive: bool,
    /// ISO 3166-1 alpha-2 country code for Wi-Fi regulatory domain (e.g. "CN", "US").
    pub country_code: String,
    /// Frequency bands supported by the hardware. Used to select the best default
    /// channel when `channel == 0` and to validate user-specified channels.
    pub supported_bands: Vec<WifiBand>,
}

/// Determine the 802.11 hw_mode for hostapd based on the channel number.
///
/// - Channels 1–14 → `g` (2.4 GHz, 802.11b/g/n)
/// - Channels 32–177 → `a` (5 GHz, 802.11a/n/ac)
/// - Channel 0 (auto) is resolved to a concrete channel before this is called.
pub fn hw_mode_for_channel(channel: u32) -> &'static str {
    if channel <= 14 {
        "g"
    } else {
        "a"
    }
}

/// Pick a sensible default channel when the user specifies `0` (auto).
///
/// Prefers 5 GHz (channel 36) if the hardware supports it — less interference and
/// higher throughput. Falls back to 2.4 GHz channel 6 (widely compatible, minimal
/// overlap with channels 1 and 11).
pub fn default_channel_for_bands(bands: &[WifiBand]) -> u32 {
    if bands.contains(&WifiBand::Band5Ghz) {
        36
    } else {
        6
    }
}

/// Check whether a channel number falls in the 5 GHz range.
pub fn is_5ghz_channel(channel: u32) -> bool {
    channel >= 32
}

/// Render and write all three AP configuration files atomically.
///
/// Returns the list of written file paths.
pub async fn render_and_write_ap_config(
    ctx: &ApRenderContext,
    config_dir: &str,
) -> NGResult<Vec<PathBuf>> {
    let dir = Path::new(config_dir);
    fs::create_dir_all(dir).await.map_err(|e| {
        NetworkError::ApError(format!("Failed to create config dir {config_dir}: {e}"))
    })?;

    let ap_env = render_ap_env(ctx);
    let hostapd = render_hostapd_conf(ctx);
    let dnsmasq = render_dnsmasq_conf(ctx);

    let mut written = Vec::with_capacity(3);

    for (name, content) in [
        (AP_ENV_FILE, &ap_env),
        (HOSTAPD_CONF_FILE, &hostapd),
        (DNSMASQ_AP_CONF_FILE, &dnsmasq),
    ] {
        let path = dir.join(name);
        // Write to a temp file first, then rename for atomicity.
        let tmp_path = dir.join(format!(".{name}.tmp"));
        fs::write(&tmp_path, content).await.map_err(|e| {
            NetworkError::ApError(format!("Failed to write {}: {e}", tmp_path.display()))
        })?;
        fs::rename(&tmp_path, &path).await.map_err(|e| {
            NetworkError::ApError(format!("Failed to rename to {}: {e}", path.display()))
        })?;
        written.push(path);
    }

    info!(dir = config_dir, "AP configuration files rendered");
    debug!(ssid = %ctx.ssid, channel = ctx.channel, iface = %ctx.interface);

    Ok(written)
}

/// Backup existing AP configuration files for rollback.
pub async fn backup_ap_config(config_dir: &str) -> NGResult<()> {
    let dir = Path::new(config_dir);
    for name in [AP_ENV_FILE, HOSTAPD_CONF_FILE, DNSMASQ_AP_CONF_FILE] {
        let src = dir.join(name);
        let dst = dir.join(format!("{name}.bak"));
        if src.exists() {
            fs::copy(&src, &dst).await.map_err(|e| {
                NetworkError::ApError(format!("Failed to backup {}: {e}", src.display()))
            })?;
        }
    }
    Ok(())
}

/// Restore AP configuration files from backup.
pub async fn restore_ap_config(config_dir: &str) -> NGResult<()> {
    let dir = Path::new(config_dir);
    for name in [AP_ENV_FILE, HOSTAPD_CONF_FILE, DNSMASQ_AP_CONF_FILE] {
        let bak = dir.join(format!("{name}.bak"));
        let dst = dir.join(name);
        if bak.exists() {
            fs::rename(&bak, &dst).await.map_err(|e| {
                NetworkError::ApError(format!("Failed to restore {}: {e}", bak.display()))
            })?;
        }
    }
    info!(dir = config_dir, "AP configuration restored from backup");
    Ok(())
}

/// Render `ap-env` — shell variables sourced by the setup service.
fn render_ap_env(ctx: &ApRenderContext) -> String {
    format!(
        r#"# Auto-generated by ng-gateway — do not edit manually.
# Sourced by ng-gateway-ap-setup.service.

AP_IFACE="{iface}"
STA_IFACE="{sta_iface}"
UPLINK_IFACE="{uplink_iface}"
AP_EXCLUSIVE="{exclusive}"
AP_IP="{ip}"
AP_PREFIX="{prefix}"
AP_DHCP_START="{dhcp_start}"
AP_DHCP_END="{dhcp_end}"
"#,
        iface = ctx.interface,
        sta_iface = ctx.sta_iface,
        uplink_iface = ctx.uplink_iface,
        exclusive = ctx.exclusive,
        ip = ctx.ip,
        prefix = ctx.prefix_length,
        dhcp_start = ctx.dhcp_range_start,
        dhcp_end = ctx.dhcp_range_end,
    )
}

/// Render `hostapd.conf`.
///
/// Automatically selects `hw_mode` based on the channel:
/// - 2.4 GHz (ch 1–14): `hw_mode=g` with 802.11n (HT)
/// - 5 GHz (ch 36–177): `hw_mode=a` with 802.11n (HT) + 802.11ac (VHT)
///
/// When channel is 0 (auto), picks the best default based on `supported_bands`.
fn render_hostapd_conf(ctx: &ApRenderContext) -> String {
    let channel = if ctx.channel == 0 {
        default_channel_for_bands(&ctx.supported_bands)
    } else {
        ctx.channel
    };
    let hw_mode = hw_mode_for_channel(channel);

    // Build band-specific 802.11 capability lines.
    let band_capabilities = if is_5ghz_channel(channel) {
        // 5 GHz: enable 802.11n (HT) and 802.11ac (VHT) for better throughput.
        // WMM is required for 802.11n/ac operation.
        "ieee80211n=1\nieee80211ac=1\nwmm_enabled=1"
    } else {
        // 2.4 GHz: enable 802.11n (HT) for better throughput.
        // WMM disabled to maximise compatibility with low-end IoT clients.
        "ieee80211n=1\nwmm_enabled=0"
    };

    format!(
        r#"# Auto-generated by ng-gateway — do not edit manually.
interface={iface}
driver=nl80211
ssid={ssid}
country_code={country}
ieee80211d=1
hw_mode={hw_mode}
channel={channel}
{band_capabilities}
macaddr_acl=0
auth_algs=1
ignore_broadcast_ssid=0
wpa=2
wpa_passphrase={password}
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP

# Control interface for status queries
ctrl_interface=/var/run/hostapd
ctrl_interface_group=0
"#,
        iface = ctx.interface,
        ssid = ctx.ssid,
        country = ctx.country_code,
        hw_mode = hw_mode,
        channel = channel,
        band_capabilities = band_capabilities,
        password = ctx.password,
    )
}

/// Render `dnsmasq-ap.conf`.
///
/// Uses `bind-dynamic` + `listen-address` to ensure dnsmasq only listens on
/// the AP interface address. `bind-dynamic` (vs `bind-interfaces`) avoids
/// conflicts with `systemd-resolved` (127.0.0.53:53) and allows the AP
/// interface to appear after dnsmasq starts.
fn render_dnsmasq_conf(ctx: &ApRenderContext) -> String {
    format!(
        r#"# Auto-generated by ng-gateway — do not edit manually.
# DHCP + DNS for AP clients only.

interface={iface}
bind-dynamic
listen-address={ip}
dhcp-range={start},{end},{lease}
dhcp-option=6,{ip}

# Prevent dnsmasq from reading the host's /etc/resolv.conf
no-resolv
# Forward DNS queries to well-known public resolvers
server=8.8.8.8
server=1.1.1.1
"#,
        iface = ctx.interface,
        ip = ctx.ip,
        start = ctx.dhcp_range_start,
        end = ctx.dhcp_range_end,
        lease = ctx.dhcp_lease_time,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ApRenderContext {
        ApRenderContext {
            interface: "wlan0_ap".to_string(),
            ssid: "NG-Gateway-ABCD".to_string(),
            password: "ng-gateway".to_string(),
            channel: 6,
            ip: "10.47.0.1".to_string(),
            prefix_length: 24,
            dhcp_range_start: "10.47.0.10".to_string(),
            dhcp_range_end: "10.47.0.200".to_string(),
            dhcp_lease_time: "12h".to_string(),
            sta_iface: "wlan0".to_string(),
            uplink_iface: "wlan0".to_string(),
            exclusive: false,
            country_code: DEFAULT_COUNTRY_CODE.to_string(),
            supported_bands: vec![WifiBand::Band2_4Ghz, WifiBand::Band5Ghz],
        }
    }

    #[test]
    fn test_render_hostapd_2g_channel() {
        let conf = render_hostapd_conf(&test_ctx());
        assert!(conf.contains("interface=wlan0_ap"));
        assert!(conf.contains("ssid=NG-Gateway-ABCD"));
        assert!(conf.contains("hw_mode=g"));
        assert!(conf.contains("channel=6"));
        assert!(conf.contains("ieee80211n=1"));
        assert!(!conf.contains("ieee80211ac=1"));
        assert!(conf.contains("wmm_enabled=0"));
        assert!(conf.contains("wpa_passphrase=ng-gateway"));
        assert!(conf.contains("country_code=CN"));
    }

    #[test]
    fn test_render_hostapd_5g_channel() {
        let mut ctx = test_ctx();
        ctx.channel = 36;
        let conf = render_hostapd_conf(&ctx);
        assert!(conf.contains("hw_mode=a"));
        assert!(conf.contains("channel=36"));
        assert!(conf.contains("ieee80211n=1"));
        assert!(conf.contains("ieee80211ac=1"));
        assert!(conf.contains("wmm_enabled=1"));
    }

    #[test]
    fn test_render_hostapd_auto_channel_prefers_5g() {
        let mut ctx = test_ctx();
        ctx.channel = 0;
        ctx.supported_bands = vec![WifiBand::Band2_4Ghz, WifiBand::Band5Ghz];
        let conf = render_hostapd_conf(&ctx);
        assert!(conf.contains("hw_mode=a"));
        assert!(conf.contains("channel=36"));
    }

    #[test]
    fn test_render_hostapd_auto_channel_2g_only() {
        let mut ctx = test_ctx();
        ctx.channel = 0;
        ctx.supported_bands = vec![WifiBand::Band2_4Ghz];
        let conf = render_hostapd_conf(&ctx);
        assert!(conf.contains("hw_mode=g"));
        assert!(conf.contains("channel=6"));
    }

    #[test]
    fn test_render_dnsmasq_conf() {
        let conf = render_dnsmasq_conf(&test_ctx());
        assert!(conf.contains("interface=wlan0_ap"));
        assert!(conf.contains("dhcp-range=10.47.0.10,10.47.0.200,12h"));
        assert!(conf.contains("bind-dynamic"));
        assert!(conf.contains("listen-address=10.47.0.1"));
    }

    #[test]
    fn test_render_ap_env() {
        let env = render_ap_env(&test_ctx());
        assert!(env.contains("AP_IFACE=\"wlan0_ap\""));
        assert!(env.contains("AP_IP=\"10.47.0.1\""));
        assert!(env.contains("STA_IFACE=\"wlan0\""));
        assert!(env.contains("UPLINK_IFACE=\"wlan0\""));
        assert!(env.contains("AP_EXCLUSIVE=\"false\""));
    }

    #[test]
    fn test_render_ap_env_exclusive() {
        let mut ctx = test_ctx();
        ctx.exclusive = true;
        ctx.interface = "wlP2p33s0".to_string();
        ctx.sta_iface = "wlP2p33s0".to_string();
        let env = render_ap_env(&ctx);
        assert!(env.contains("AP_EXCLUSIVE=\"true\""));
        assert!(env.contains("AP_IFACE=\"wlP2p33s0\""));
    }

    #[test]
    fn test_hw_mode_for_channel() {
        assert_eq!(hw_mode_for_channel(1), "g");
        assert_eq!(hw_mode_for_channel(6), "g");
        assert_eq!(hw_mode_for_channel(13), "g");
        assert_eq!(hw_mode_for_channel(14), "g");
        assert_eq!(hw_mode_for_channel(36), "a");
        assert_eq!(hw_mode_for_channel(149), "a");
        assert_eq!(hw_mode_for_channel(165), "a");
    }

    #[test]
    fn test_default_channel_for_bands() {
        assert_eq!(default_channel_for_bands(&[WifiBand::Band2_4Ghz]), 6);
        assert_eq!(default_channel_for_bands(&[WifiBand::Band5Ghz]), 36);
        assert_eq!(
            default_channel_for_bands(&[WifiBand::Band2_4Ghz, WifiBand::Band5Ghz]),
            36
        );
        assert_eq!(default_channel_for_bands(&[]), 6);
    }
}
