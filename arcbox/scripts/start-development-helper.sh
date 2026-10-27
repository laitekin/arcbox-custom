#!/bin/sh
# Start a separate, signature-pinned development helper in the foreground.
# Does not install/replace the production helper or grant future builds.
set -eu
if [ "$#" -ne 2 ] || [ "$(/usr/bin/id -u)" -ne 0 ]; then
  echo 'usage: sudo sh start-development-helper.sh /absolute/ArcBox.app /absolute/private/helper.sock' >&2
  exit 2
fi
app_bundle=$1
helper_socket=$2
grant_uid=${SUDO_UID:-}
case "$grant_uid" in ''|0|*[!0-9]*) echo 'Run via sudo from your normal account.' >&2; exit 2;; esac
case "$app_bundle:$helper_socket" in /*:/*) ;; *) echo 'Both paths must be absolute.' >&2; exit 2;; esac
helper="$app_bundle/Contents/MacOS/bin/arcbox-helper"
daemon="$app_bundle/Contents/Frameworks/com.arcboxlabs.desktop.dev.daemon.app/Contents/MacOS/com.arcboxlabs.desktop.dev.daemon"
cli="$app_bundle/Contents/MacOS/bin/abctl"
for binary in "$helper" "$daemon" "$cli"; do
  /usr/bin/codesign --verify --strict "$binary"
done
code_hash() {
  /usr/bin/codesign -d --verbose=4 "$1" 2>&1 | /usr/bin/sed -n 's/^CDHash=//p'
}
daemon_hash=$(code_hash "$daemon")
cli_hash=$(code_hash "$cli")
if [ -e "$helper_socket" ] || [ -L "$helper_socket" ]; then
  echo 'Private socket already exists. Stop its helper before removing that socket as your normal user.' >&2
  exit 1
fi
echo "Starting private helper for UID $grant_uid; only these builds are granted:"
echo "  daemon CDHash: $daemon_hash"
echo "  CLI CDHash:    $cli_hash"
echo "Socket: $helper_socket"
echo 'Keep this terminal running. No packet capture is started by this script.'
exec /usr/bin/env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin \
  ARCBOX_HELPER_SOCKET="$helper_socket" "$helper" \
  --development-peer "$grant_uid:$daemon_hash" \
  --development-peer "$grant_uid:$cli_hash"
