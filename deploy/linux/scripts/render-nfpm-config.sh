#!/usr/bin/env bash
set -euo pipefail

# render-nfpm-config.sh
#
# Render a nfpm config from a simple `{{PLACEHOLDER}}` template.
# This keeps CI dependencies minimal (no envsubst/jinja).

tmpl="${TMPL:-}"
out="${OUT:-}"

if [[ -z "${tmpl}" || -z "${out}" ]]; then
  echo "error: missing TMPL/OUT"
  exit 1
fi
if [[ ! -f "${tmpl}" ]]; then
  echo "error: template not found: ${tmpl}"
  exit 1
fi

cp -f "${tmpl}" "${out}"

replace() {
  local key="$1"
  local val="$2"
  if [[ -z "${val}" ]]; then
    echo "error: missing value for ${key}"
    exit 1
  fi
  # Use `|` as separator to avoid path escaping.
  sed -i.bak -e "s|{{${key}}}|${val}|g" "${out}"
  rm -f "${out}.bak"
}

replace "PKG_VERSION" "${PKG_VERSION:-}"
replace "ROOTFS_DIR" "${ROOTFS_DIR:-}"
replace "SYSTEMD_UNIT" "${SYSTEMD_UNIT:-}"
replace "POSTINSTALL" "${POSTINSTALL:-}"
replace "PREREMOVE" "${PREREMOVE:-}"

if grep -q "{{DEB_ARCH}}" "${out}"; then
  replace "DEB_ARCH" "${DEB_ARCH:-}"
fi
if grep -q "{{RPM_ARCH}}" "${out}"; then
  replace "RPM_ARCH" "${RPM_ARCH:-}"
fi

echo "[ok] rendered: ${out}"

