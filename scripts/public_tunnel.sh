#!/usr/bin/env bash
# Public access to NOVIUM Spatial Explorer through Cloudflare. Only people on an email
# list can sign in, each with their own password, and the clinical sample columns are
# not served on this address. Two user services, started and stopped together.
#   scripts/public_tunnel.sh add-user EMAIL      add someone (with the master password if set)
#   scripts/public_tunnel.sh remove-user EMAIL   remove someone (their session ends)
#   scripts/public_tunnel.sh list-users
#   scripts/public_tunnel.sh set-master          set the master password for everyone on the list
#   scripts/public_tunnel.sh start               start, then print the public address
#   scripts/public_tunnel.sh stop                public access off
#   scripts/public_tunnel.sh status              running? and the current address
#   scripts/public_tunnel.sh set-token           save the tunnel token (pasted, not shown)
#   scripts/public_tunnel.sh worker URL          fixed workers.dev address (deploy/cloudflare)
#   scripts/public_tunnel.sh named HOSTNAME      fixed address on your Cloudflare domain
#   scripts/public_tunnel.sh boot on|off         start at boot (fixed address only)
#   scripts/public_tunnel.sh quick               back to a random trycloudflare.com address
# Three ways in: a quick tunnel (random https://….trycloudflare.com address that changes
# whenever the tunnel restarts; no account); `worker` (the novium Worker on workers.dev
# reaches the tunnel through Workers VPC; needs the tunnel token saved in
# ~/.config/xenium-web/public/tunnel-token); `named` (your own domain, after
# `cloudflared tunnel login`).
# Uses the build deployed by scripts/install_service.sh (~/.local/share/novium) and the
# LAN service's index. Logs: ~/.local/state/novium/{public,tunnel}.log
set -euo pipefail
DEST=${NOVIUM_HOME:-$HOME/.local/share/novium}
CONF=${XENIUM_WEB_CONF:-$HOME/.config/xenium-web}
USERS=$CONF/public/users.tsv
# Everyone on the list signs in with this password (one line, owner-only); without it,
# add-user generates a password per person and prints it once.
MASTER=$CONF/public/master-password
HOSTFILE=$CONF/public/hostname
# Worker mode: the fixed workers.dev address, and the tunnel token (owner-only; you save it)
WORKERFILE=$CONF/public/worker-url
TOKENFILE=$CONF/public/tunnel-token
STATE=$HOME/.local/state/novium
UNITS=$HOME/.config/systemd/user
PORT=${PUBLIC_PORT:-8444}
TUNNEL=${TUNNEL_NAME:-novium}
CLOUDFLARED=${CLOUDFLARED:-$HOME/.local/bin/cloudflared}
BIN=$DEST/bin/xenium-web

usage() { sed -n '5,16p' "$0" | sed 's/^# //' >&2; exit 2; }
named_host() { [[ -s "$HOSTFILE" ]] && cat "$HOSTFILE"; }
worker_url() { [[ -s "$WORKERFILE" ]] && cat "$WORKERFILE"; }
fixed() { [[ -n "$(worker_url || true)$(named_host || true)" ]]; }
url() {
  if [[ -s "$WORKERFILE" ]]; then cat "$WORKERFILE"; return; fi
  if [[ -s "$HOSTFILE" ]]; then echo "https://$(cat "$HOSTFILE")"; return; fi
  grep -ao 'https://[a-z0-9-]*\.trycloudflare\.com' "$STATE/tunnel.log" 2>/dev/null | tail -1 || true
}
people() { "$BIN" users --file "$USERS" list; }

write_units() {
  local root=${INDEX_ROOT:-} tunnel_cmd install=""
  [[ -n "$root" ]] || root=$(sed -n 's/^Environment="INDEX_ROOT=\(.*\)"$/\1/p' "$UNITS/novium-explorer.service" 2>/dev/null)
  [[ -n "$root" ]] || { echo "set INDEX_ROOT, or install the LAN service first (scripts/install_service.sh)" >&2; exit 1; }
  if [[ -n "$(worker_url || true)" ]]; then
    # No public hostname: only the Worker reaches 127.0.0.1:$PORT through Workers VPC.
    tunnel_cmd="\"$CLOUDFLARED\" tunnel --no-autoupdate run --token-file \"$TOKENFILE\""
    install=$'\n[Install]\nWantedBy=default.target'
  elif [[ -n "$(named_host || true)" ]]; then
    tunnel_cmd="\"$CLOUDFLARED\" tunnel --no-autoupdate run --url http://127.0.0.1:$PORT $TUNNEL"
    install=$'\n[Install]\nWantedBy=default.target'
  else
    tunnel_cmd="\"$CLOUDFLARED\" tunnel --no-autoupdate --url http://127.0.0.1:$PORT"
  fi
  mkdir -p "$UNITS" "$STATE/public-state"
  chmod 700 "$STATE"
  cat > "$UNITS/novium-public.service" <<EOF
[Unit]
Description=NOVIUM Spatial Explorer, public instance (loopback only, behind the Cloudflare tunnel)
PartOf=novium-tunnel.service
# Keep retrying while the index drive is not mounted yet (e.g. just after a boot).
StartLimitIntervalSec=0

[Service]
ExecStartPre=/usr/bin/test -d "$root"
# No --sample-metadata: the clinical columns are not served on the public address.
ExecStart="$BIN" serve --index-root "$root" --static-dir "$DEST/web" --host 127.0.0.1 --port $PORT --access-users-file "$USERS" --behind-cloudflare --state-dir "$STATE/public-state"
StandardOutput=append:$STATE/public.log
StandardError=append:$STATE/public.log
Restart=on-failure
RestartSec=15
TimeoutStopSec=30
EOF
  cat > "$UNITS/novium-tunnel.service" <<EOF
[Unit]
Description=NOVIUM Spatial Explorer, public address (Cloudflare tunnel)
# Wants, not Requires: a slow index drive at boot must not stop the tunnel for good.
Wants=novium-public.service
After=novium-public.service
StartLimitIntervalSec=0

[Service]
ExecStart=$tunnel_cmd
StandardOutput=append:$STATE/tunnel.log
StandardError=append:$STATE/tunnel.log
Restart=on-failure
RestartSec=10
$install
EOF
  systemctl --user daemon-reload
}

start() {
  [[ -x "$CLOUDFLARED" ]] || { echo "cloudflared not found at $CLOUDFLARED" >&2; exit 1; }
  [[ -n "$(people 2>/dev/null)" ]] || { echo "nobody on the list yet: $0 add-user EMAIL" >&2; exit 1; }
  if systemctl --user -q is-active novium-tunnel.service; then
    echo "already running: $(url)"
    return
  fi
  write_units
  : > "$STATE/tunnel.log"
  systemctl --user start novium-tunnel.service
  local u="" code=""
  for _ in $(seq 90); do
    u=$(url)
    if [[ -n "$u" ]]; then
      code=$(curl -s -o /dev/null -m 5 -w '%{http_code}' "$u/healthz" || true)
      [[ "$code" == 200 ]] && break
    fi
    sleep 1
  done
  [[ "$code" == 200 ]] || { echo "not reachable yet (${u:-no address}); see $STATE/tunnel.log" >&2; exit 1; }
  echo "public address: $u"
  echo "sign-in: email + password ($(people | wc -l) people); public access off: $0 stop"
}

case "${1:-}" in
  add-user)
    [[ -n "${2:-}" ]] || usage
    mkdir -p "$(dirname "$USERS")"
    chmod 700 "$(dirname "$USERS")"
    if [[ -s "$MASTER" ]]; then
      "$BIN" users --file "$USERS" add "$2" --password-file "$MASTER"
    else
      "$BIN" users --file "$USERS" add "$2"
    fi ;;
  set-master)
    read -rsp "new master password: " a; echo
    read -rsp "again: " b; echo
    [[ "$a" == "$b" ]] || { echo "the two entries differ" >&2; exit 1; }
    mkdir -p "$(dirname "$MASTER")"
    (umask 077 && printf '%s\n' "$a" > "$MASTER.new")
    mv -f "$MASTER.new" "$MASTER"
    "$BIN" users --file "$USERS" reset-all --password-file "$MASTER" ;;
  remove-user)
    [[ -n "${2:-}" ]] || usage
    "$BIN" users --file "$USERS" remove "$2" ;;
  list-users)
    people ;;
  start)
    start ;;
  stop)
    systemctl --user stop novium-tunnel.service novium-public.service
    echo "public access is off" ;;
  status)
    for s in novium-public novium-tunnel; do
      printf '%s: %s\n' "$s" "$(systemctl --user is-active "$s" || true)"
    done
    printf 'starts at boot: %s\n' "$(systemctl --user -q is-enabled novium-tunnel.service 2>/dev/null && echo yes || echo no)"
    if systemctl --user -q is-active novium-tunnel.service; then echo "public address: $(url)"; fi ;;
  named)
    host=$(printf '%s' "${2:-}" | tr '[:upper:]' '[:lower:]')
    [[ "$host" =~ ^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$ ]] || { echo "usage: $0 named novium.yourdomain.com" >&2; exit 2; }
    [[ -f "$HOME/.cloudflared/cert.pem" ]] || { echo "authorise this computer first: $CLOUDFLARED tunnel login" >&2; exit 1; }
    if ! "$CLOUDFLARED" tunnel info "$TUNNEL" >/dev/null 2>&1; then
      "$CLOUDFLARED" tunnel create "$TUNNEL"
    fi
    # Creates a CNAME to the tunnel; refuses to replace an existing record of that name.
    "$CLOUDFLARED" tunnel route dns "$TUNNEL" "$host"
    mkdir -p "$(dirname "$HOSTFILE")"
    printf '%s\n' "$host" > "$HOSTFILE"
    systemctl --user stop novium-tunnel.service 2>/dev/null || true
    start ;;
  set-token)
    read -rsp "Paste the tunnel token (or the whole install command), then press Enter: " t; echo
    t=${t##* }
    [[ "$t" =~ ^eyJ[A-Za-z0-9+/=_-]{40,}$ ]] || { echo "that does not look like a Cloudflare tunnel token (it starts with eyJ); nothing saved" >&2; exit 1; }
    mkdir -p "$(dirname "$TOKENFILE")"
    (umask 077 && printf '%s\n' "$t" > "$TOKENFILE.new")
    mv -f "$TOKENFILE.new" "$TOKENFILE"
    echo "saved to $TOKENFILE (readable only by you)" ;;
  worker)
    u=$(printf '%s' "${2:-}" | tr '[:upper:]' '[:lower:]')
    [[ "$u" =~ ^https://[a-z0-9-]+\.[a-z0-9-]+\.workers\.dev$ ]] || { echo "usage: $0 worker https://novium.<account>.workers.dev" >&2; exit 2; }
    [[ -s "$TOKENFILE" ]] || { echo "save the tunnel token first in $TOKENFILE" >&2; exit 1; }
    chmod 600 "$TOKENFILE"
    printf '%s\n' "$u" > "$WORKERFILE"
    rm -f "$HOSTFILE"
    systemctl --user stop novium-tunnel.service 2>/dev/null || true
    write_units
    : > "$STATE/tunnel.log"
    systemctl --user start novium-tunnel.service
    for _ in $(seq 60); do grep -q 'Registered tunnel connection' "$STATE/tunnel.log" 2>/dev/null && break; sleep 1; done
    grep -q 'Registered tunnel connection' "$STATE/tunnel.log" || { echo "the tunnel did not connect; see $STATE/tunnel.log" >&2; exit 1; }
    echo "tunnel connected (id $(grep -aoE 'tunnelID=[0-9a-f-]{36}' "$STATE/tunnel.log" | head -1 | cut -d= -f2)); address: $u"
    curl -s -o /dev/null -m 10 -w 'through the Worker: HTTP %{http_code}\n' "$u/healthz" || true ;;
  boot)
    fixed || { echo "a quick tunnel's address changes on every start; use '$0 worker URL' or '$0 named HOSTNAME' first" >&2; exit 1; }
    case "${2:-}" in
      on) write_units; systemctl --user enable novium-tunnel.service >/dev/null; echo "the public address starts at boot" ;;
      off) systemctl --user disable novium-tunnel.service >/dev/null 2>&1 || true; echo "the public address no longer starts at boot" ;;
      *) usage ;;
    esac ;;
  quick)
    systemctl --user disable --now novium-tunnel.service >/dev/null 2>&1 || true
    rm -f "$HOSTFILE" "$WORKERFILE"
    start ;;
  *) usage ;;
esac
