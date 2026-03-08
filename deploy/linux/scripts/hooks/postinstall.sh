#!/usr/bin/env bash
set -euo pipefail

# postinstall.sh
#
# This script prepares runtime directories and default config/data for NG Gateway.
# It is used by both `.deb` and `.rpm` packages via nfpm.

runtime_dir="/var/lib/ng-gateway"
config_dir="/etc/ng-gateway"
opt_dir="/opt/ng-gateway"
systemd_dir="/lib/systemd/system"

if [[ -d /usr/lib/systemd/system ]]; then
  systemd_dir="/usr/lib/systemd/system"
fi

mkdir -p "${runtime_dir}" "${config_dir}"
mkdir -p "${runtime_dir}/data" "${runtime_dir}/certs" "${runtime_dir}/pki/own" "${runtime_dir}/pki/private"
mkdir -p "${runtime_dir}/drivers/builtin" "${runtime_dir}/drivers/custom"
mkdir -p "${runtime_dir}/plugins/builtin" "${runtime_dir}/plugins/custom"

# Copy default config on first install (do not overwrite users' config).
if [[ ! -f "${config_dir}/gateway.toml" && -f "${opt_dir}/gateway.toml" ]]; then
  cp -f "${opt_dir}/gateway.toml" "${config_dir}/gateway.toml"
fi

# Create an optional environment override file.
if [[ ! -f "${config_dir}/env" ]]; then
  {
    printf "%s\n" "# NG Gateway environment overrides (optional)"
    printf "%s\n" "# Example: NG__GENERAL__RUNTIME_DIR=/var/lib/ng-gateway"
    printf "%s\n" ""
  } > "${config_dir}/env"
fi

# Ensure builtin drivers/plugins are available under runtime working directory.
if [[ -d "${opt_dir}/drivers/builtin" ]]; then
  cp -af "${opt_dir}/drivers/builtin/." "${runtime_dir}/drivers/builtin/" 2>/dev/null || true
fi
if [[ -d "${opt_dir}/plugins/builtin" ]]; then
  cp -af "${opt_dir}/plugins/builtin/." "${runtime_dir}/plugins/builtin/" 2>/dev/null || true
fi

# Reload systemd unit files if systemd is present.
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
fi

# ─── AP Hotspot Network Initialization ───
#
# Run init-network.sh which handles:
# - Service sanitization (disable/mask conflicting dnsmasq/hostapd)
# - Wireless hardware detection
# - AP configuration generation
# - AP systemd unit deployment and enablement
#
# NOTE: Service sanitization was previously duplicated here AND inside
# init-network.sh. It is now consolidated inside init-network.sh only.
init_script="${opt_dir}/scripts/init-network.sh"
if [[ -f "${init_script}" ]]; then
  echo "[postinstall] Running network initialization..."
  bash "${init_script}" || {
    echo "[postinstall] WARN: Network initialization had issues (non-fatal)."
    echo "[postinstall] AP hotspot may need manual setup. See: ${init_script}"
  }
else
  echo "[postinstall] WARN: init-network.sh not found, skipping AP setup."
fi

# ─── First-Boot Service (factory image support) ───
#
# Deploy and enable the first-boot service for factory-flashed devices.
# On normal DEB installs (not from golden image), the service detects the
# marker file already exists and exits immediately.
if command -v systemctl >/dev/null 2>&1; then
  fb_unit="${opt_dir}/systemd/ng-gateway-first-boot.service"
  fb_dst="${systemd_dir}/ng-gateway-first-boot.service"
  if [[ -f "$fb_unit" ]]; then
    cp -f "$fb_unit" "$fb_dst"
    systemctl daemon-reload || true
    systemctl enable ng-gateway-first-boot.service 2>/dev/null || true
    echo "[postinstall] First-boot service deployed and enabled"
  fi
fi

# On normal package installs we mark first-boot as already satisfied.
# Factory images explicitly remove this marker during golden-sample sanitization,
# so cloned devices still execute first-boot on their actual first power-on.
mkdir -p "${runtime_dir}"
if [[ ! -f "${runtime_dir}/.first-boot-done" ]]; then
  date -Iseconds > "${runtime_dir}/.first-boot-done"
  echo "[postinstall] First-boot marker initialized for non-factory installs"
fi

# ─── Enable and start the gateway service ───
if command -v systemctl >/dev/null 2>&1; then
  systemctl enable ng-gateway.service 2>/dev/null || true
  systemctl start ng-gateway.service 2>/dev/null || true
fi

cat <<EOF

============================================================
NG Gateway installed successfully
============================================================

Runtime directory (WorkingDirectory):
  ${runtime_dir}

Edit configuration:
  ${config_dir}/gateway.toml

Optional environment overrides:
  ${config_dir}/env

Runtime sub-directories (default layout):
  - data:    ${runtime_dir}/data
  - drivers: ${runtime_dir}/drivers (builtin/custom)
  - plugins: ${runtime_dir}/plugins (builtin/custom)
  - certs:   ${runtime_dir}/certs
  - pki:     ${runtime_dir}/pki

Logs (recommended):
  - systemd journal: journalctl -u ng-gateway -f

Service management (systemd):
  - status:  systemctl status ng-gateway --no-pager
  - start:   systemctl start ng-gateway
  - stop:    systemctl stop ng-gateway
  - restart: systemctl restart ng-gateway
  - enable:  systemctl enable --now ng-gateway

Health checks (defaults from ${config_dir}/gateway.toml):
  - HTTP:  curl -fsS http://127.0.0.1:8978/health   # -> OK
  - HTTPS: curl -kfsS https://127.0.0.1:8979/health # if enabled (self-signed: -k)

Web access (depends on [web.ui] settings):
  - UI:  http://127.0.0.1:8978/ (if UI enabled)
  - API: http://127.0.0.1:8978/api

Default UI credentials (initial):
  - username: system_admin
  - password: system_admin

Manual run (without systemd):
  cd ${runtime_dir} && ${opt_dir}/bin/ng-gateway-bin --config ${config_dir}/gateway.toml

============================================================

EOF

exit 0
