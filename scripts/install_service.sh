#!/usr/bin/env bash
# Run NOVIUM Spatial Explorer as an always-on systemd user service (no sudo): it starts at
# boot without anyone logging in ("linger"), restarts if it stops, and waits for the
# index drive and the LAN address by retrying every 15 s.
#   LAN_IP=192.168.1.20 INDEX_ROOT=/data/xenium-web-index scripts/install_service.sh
#   scripts/install_service.sh --link        print the private access link
#   scripts/install_service.sh --uninstall   stop and remove the service
# The server, the web files and serve_lan.sh are copied to ~/.local/share/novium, so
# rebuilding the repository never changes the running server; rerun this script to
# deploy a new build. Log: ~/.local/state/novium/server.log (the private link is not logged).
set -euo pipefail
UNIT=novium-explorer.service
HERE=$(cd "$(dirname "$0")/.." && pwd)
DEST=${NOVIUM_HOME:-$HOME/.local/share/novium}
CONF=${XENIUM_WEB_CONF:-$HOME/.config/xenium-web}
UNIT_FILE=$HOME/.config/systemd/user/$UNIT
LOG=${NOVIUM_LOG:-$HOME/.local/state/novium/server.log}
PORT=${PORT:-8443}

case "${1:-}" in
  --uninstall)
    systemctl --user disable --now "$UNIT" 2>/dev/null || true
    rm -f "$UNIT_FILE"
    systemctl --user daemon-reload
    echo "removed $UNIT (copies in $DEST kept; 'loginctl disable-linger' stops all your user services starting at boot)"
    exit 0 ;;
  --link)
    ip=$(sed -n 's/^Environment="LAN_IP=\(.*\)"$/\1/p' "$UNIT_FILE" 2>/dev/null)
    port=$(sed -n 's/^Environment="PORT=\(.*\)"$/\1/p' "$UNIT_FILE" 2>/dev/null)
    echo "https://${ip:-$LAN_IP}:${port:-$PORT}/?token=$(cat "$CONF/token")"
    exit 0 ;;
  "") ;;
  *) echo "usage: $0 [--link | --uninstall]" >&2; exit 2 ;;
esac

: "${LAN_IP:?LAN address to bind, e.g. 192.168.1.20}" "${INDEX_ROOT:?index root}"
INDEX_ROOT=$(cd "$INDEX_ROOT" && pwd) # absolute: unit files do not expand ~
BIN="$HERE/target/release/xenium-web"
[[ -x "$BIN" && -f "$HERE/web/dist/index.html" ]] ||
  { echo "build first: cargo build --release -p xenium-cli && (cd web && npm run build)" >&2; exit 1; }
if ss -ltn | grep -q ":$PORT\b" && ! systemctl --user -q is-active "$UNIT"; then
  echo "port $PORT is in use: stop the server running in your terminal first (Ctrl-C)" >&2
  exit 1
fi

# Deploy copies; each one is swapped in whole.
mkdir -p "$DEST/bin" "$(dirname "$UNIT_FILE")" "$(dirname "$LOG")"
chmod 700 "$(dirname "$LOG")" # sign-in warnings name client addresses
cp "$BIN" "$DEST/bin/xenium-web.new"
strip --strip-debug "$DEST/bin/xenium-web.new" 2>/dev/null || true
mv -f "$DEST/bin/xenium-web.new" "$DEST/bin/xenium-web"
rm -rf "$DEST/web.new" "$DEST/web.old"
cp -r "$HERE/web/dist" "$DEST/web.new"
[[ -d "$DEST/web" ]] && mv "$DEST/web" "$DEST/web.old"
mv "$DEST/web.new" "$DEST/web"
rm -rf "$DEST/web.old"
cp "$HERE/scripts/serve_lan.sh" "$DEST/serve_lan.sh.new"
chmod 755 "$DEST/serve_lan.sh.new"
mv -f "$DEST/serve_lan.sh.new" "$DEST/serve_lan.sh"

cat > "$UNIT_FILE" <<EOF
[Unit]
Description=NOVIUM Spatial Explorer on https://$LAN_IP:$PORT
# Keep retrying while the index drive or the network is not there yet.
StartLimitIntervalSec=0

[Service]
Environment="LAN_IP=$LAN_IP"
Environment="PORT=$PORT"
Environment="INDEX_ROOT=$INDEX_ROOT"
Environment="XENIUM_WEB_CONF=$CONF"
Environment="XENIUM_WEB_BIN=$DEST/bin/xenium-web"
Environment="XENIUM_WEB_STATIC=$DEST/web"
ExecStartPre=/usr/bin/test -d "$INDEX_ROOT"
ExecStart="$DEST/serve_lan.sh"
# A plain file: the user journal is not readable on every machine.
StandardOutput=append:$LOG
StandardError=append:$LOG
Restart=always
RestartSec=15

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable "$UNIT" >/dev/null
systemctl --user restart "$UNIT"
# Start at boot without a login session (allowed for your own account; no sudo).
loginctl enable-linger "$USER" ||
  echo "warning: could not enable start at boot; an admin can run: sudo loginctl enable-linger $USER" >&2

for _ in $(seq 60); do
  ss -ltn | grep -q ":$PORT\b" && break
  sleep 1
done
if ss -ltn | grep -q ":$PORT\b"; then
  echo "NOVIUM Spatial Explorer is running as $UNIT on https://$LAN_IP:$PORT"
  echo "starts at boot: $(loginctl show-user "$USER" -p Linger --value)  ·  private link: $0 --link"
else
  echo "not listening yet; see $LOG" >&2
  exit 1
fi
