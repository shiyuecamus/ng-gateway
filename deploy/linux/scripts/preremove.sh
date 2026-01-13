#!/usr/bin/env bash
set -euo pipefail

# preremove.sh
#
# Best-effort stop of the systemd service on package removal.
# We do NOT remove runtime data/config to avoid accidental data loss.

if command -v systemctl >/dev/null 2>&1; then
  systemctl stop ng-gateway.service >/dev/null 2>&1 || true
  systemctl disable ng-gateway.service >/dev/null 2>&1 || true
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

exit 0

