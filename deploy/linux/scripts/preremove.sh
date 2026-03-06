#!/usr/bin/env bash
set -euo pipefail

# preremove.sh
#
# Best-effort stop of all NG Gateway systemd services on package removal.
# We do NOT remove runtime data/config (/var/lib/ng-gateway, /etc/ng-gateway)
# to avoid accidental data loss.

if command -v systemctl >/dev/null 2>&1; then
  # Stop and disable AP services first (reverse dependency order).
  for unit in ng-gateway-dnsmasq.service ng-gateway-hostapd.service ng-gateway-ap-setup.service; do
    systemctl stop "$unit" >/dev/null 2>&1 || true
    systemctl disable "$unit" >/dev/null 2>&1 || true
  done

  # Stop and disable the main gateway service.
  systemctl stop ng-gateway.service >/dev/null 2>&1 || true
  systemctl disable ng-gateway.service >/dev/null 2>&1 || true

  # Clean up AP systemd unit files.
  rm -f /lib/systemd/system/ng-gateway-ap-setup.service 2>/dev/null || true
  rm -f /lib/systemd/system/ng-gateway-hostapd.service 2>/dev/null || true
  rm -f /lib/systemd/system/ng-gateway-dnsmasq.service 2>/dev/null || true

  systemctl daemon-reload >/dev/null 2>&1 || true
fi

echo "[preremove] NG Gateway services stopped and disabled."
echo "[preremove] Configuration and data preserved at /etc/ng-gateway and /var/lib/ng-gateway."
echo "[preremove] To fully purge, remove these directories manually."

exit 0
