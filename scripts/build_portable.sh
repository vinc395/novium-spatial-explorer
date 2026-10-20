#!/usr/bin/env bash
# Build a xenium-web binary that runs on older Linux clusters (glibc >= 2.17),
# using zig as the C toolchain. Output: dist/xenium-web-x86_64-glibc2.17
set -euo pipefail
cd "$(dirname "$0")/.."
VENV=${ZIG_VENV:-$HOME/.local/share/xenium-web-zig}
if [[ ! -x "$VENV/bin/cargo-zigbuild" ]]; then
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install -q ziglang cargo-zigbuild
fi
export PATH="$VENV/bin:$HOME/.cargo/bin:$PATH"
# Stripped, no debug info: ~10x smaller to copy to a cluster.
CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=true \
  CARGO_TARGET_DIR=target/portable cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17 -p xenium-cli
mkdir -p dist
cp target/portable/x86_64-unknown-linux-gnu/release/xenium-web dist/xenium-web-x86_64-glibc2.17
objdump -T dist/xenium-web-x86_64-glibc2.17 | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1
