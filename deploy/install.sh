#!/bin/sh
# Installs meowtify-jam-relay as a systemd service.
# Usage: sudo ./deploy/install.sh [path/to/meowtify-jam-relay]
set -eu

BIN="${1:-./meowtify-jam-relay}"
HERE="$(cd "$(dirname "$0")" && pwd)"

[ "$(id -u)" = 0 ] || { echo "Run this as root (use sudo)." >&2; exit 1; }
[ -f "$BIN" ] || { echo "Binary not found: $BIN" >&2; exit 1; }
command -v systemctl >/dev/null || { echo "This installer needs systemd." >&2; exit 1; }

id meowtify-jam-relay >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin meowtify-jam-relay
install -m 755 "$BIN" /usr/local/bin/meowtify-jam-relay
install -m 644 "$HERE/meowtify-jam-relay.service" /etc/systemd/system/meowtify-jam-relay.service

systemctl daemon-reload
systemctl enable --now meowtify-jam-relay
sleep 2
systemctl is-active --quiet meowtify-jam-relay || { echo "The service did not start. See: journalctl -u meowtify-jam-relay" >&2; exit 1; }

echo
journalctl -u meowtify-jam-relay -n 8 --no-pager -o cat
