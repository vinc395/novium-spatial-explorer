#!/usr/bin/env bash
# Serve xenium-web to the local network over HTTPS with an access token.
#   LAN_IP=192.168.1.20 INDEX_ROOT=... scripts/serve_lan.sh
# One-time setup (as an admin):  sudo ufw allow from <subnet>/<bits> to any port 8443 proto tcp
# Always on (starts at boot, restarts if it stops): scripts/install_service.sh
set -euo pipefail
: "${LAN_IP:?LAN address to bind, e.g. 192.168.1.20}" "${INDEX_ROOT:?index root}"
PORT=${PORT:-8443}
CONF=${XENIUM_WEB_CONF:-$HOME/.config/xenium-web}
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${XENIUM_WEB_BIN:-$HERE/target/release/xenium-web}
STATIC=${XENIUM_WEB_STATIC:-$HERE/web/dist}
HOSTS="$LAN_IP,$(hostname),localhost,127.0.0.1"
mkdir -p "$CONF/tls"
# Self-signed HTTPS certificate (keeps the password and data encrypted on the network;
# browsers show a one-time "not private" warning: Advanced -> Proceed). Reissued only
# when missing, expiring within 30 days, or not covering the hosts.
"$BIN" gen-cert --hosts "$HOSTS" --out-dir "$CONF/tls" --if-needed
# Optional sign-in password (one line, kept private): $CONF/password
PW_ARGS=()
[[ -f "$CONF/password" ]] && PW_ARGS=(--access-password-file "$CONF/password")
# Optional per-sample information for the Index panel: $CONF/sample_metadata.csv, with
# the columns to show (comma-separated, one line) in $CONF/sample_metadata_columns.
if [[ -f "$CONF/sample_metadata.csv" && -f "$CONF/sample_metadata_columns" ]]; then
  PW_ARGS+=(--sample-metadata "$CONF/sample_metadata.csv"
            --sample-metadata-id "${SAMPLE_ID_COLUMN:-sample_id}"
            --sample-metadata-columns "$(tr -d '[:space:]' < "$CONF/sample_metadata_columns")")
fi
exec "$BIN" serve --index-root "$INDEX_ROOT" --static-dir "$STATIC" \
  --host "$LAN_IP" --port "$PORT" --allow-remote \
  --access-token-file "$CONF/token" --tls-cert "$CONF/tls/cert.pem" --tls-key "$CONF/tls/key.pem" \
  "${PW_ARGS[@]}"
