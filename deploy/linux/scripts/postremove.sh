#!/usr/bin/env bash
set -euo pipefail

# postremove.sh
#
# Post-removal cleanup for NG Gateway.
#
# dpkg invokes this script (postrm) with $1 set to one of:
#   "remove"    — after normal uninstall (`dpkg -r`); user data preserved.
#   "purge"     — after purge (`dpkg -P` / `apt purge`); remove everything.
#   "upgrade"   — old package's postrm after files replaced by new version.
#   "disappear" — package overwritten by another; treat like remove.
#
# NOTE: The `prerm` script (preremove.sh) always receives $1="remove"
# regardless of whether the user ran `dpkg -r` or `dpkg -P`.  The purge
# signal is ONLY delivered here via `postrm purge`, which dpkg calls in
# a second pass after the package metadata has been removed.

action="${1:-remove}"
config_dir="/etc/ng-gateway"
runtime_dir="/var/lib/ng-gateway"
log_dir="/var/log/ng-gateway"

case "$action" in
  purge)
    rm -rf "${config_dir}" 2>/dev/null || true
    rm -rf "${runtime_dir}" 2>/dev/null || true
    rm -rf "${log_dir}" 2>/dev/null || true

    echo "[postremove] NG Gateway fully purged (config + runtime data removed)."
    ;;

  remove|upgrade|disappear)
    # Nothing extra to do — prerm already stopped services and cleaned up
    # AP resources.  User data is preserved.
    ;;

  *)
    # Unknown action (e.g. "failed-upgrade"); ignore gracefully.
    ;;
esac

exit 0
