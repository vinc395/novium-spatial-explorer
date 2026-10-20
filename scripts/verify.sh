#!/usr/bin/env bash
# Full local verification on synthetic data. Fails on the first error.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$HOME/.local/opt/node/bin:$PATH"
OUT=${VERIFY_OUT:-test-output/verify}
PORT=${VERIFY_PORT:-18080}
step() { printf '\n== %s\n' "$*"; }

step "toolchain"; rustc --version; cargo --version; node --version; npm --version
step "cargo fmt --check"; cargo fmt --all -- --check
step "cargo clippy"; cargo clippy --workspace --all-targets -- -D warnings
step "cargo test"
# Live UCell parity against R when the reference env exists (scripts/setup_ucell_reference.sh);
# the golden-file parity test runs either way.
RS=${XENIUM_UCELL_RSCRIPT:-$HOME/miniforge3/envs/ucell-ref/bin/Rscript}
if [[ -x "$RS" ]]; then export XENIUM_UCELL_RSCRIPT="$RS"; echo "UCell R parity: live ($RS)"
else unset XENIUM_UCELL_RSCRIPT; echo "UCell R parity: golden file only (no R UCell found)"; fi
cargo test --workspace
step "web: install/check/test/build"
(cd web && npm ci --no-audit --no-fund && npx tsc -b --noEmit && npx vitest run && npx vite build)
step "release build"; cargo build --release -p xenium-cli
B=target/release/xenium-web
rm -rf "$OUT"; mkdir -p "$OUT"
step "synthetic ingest + validate"
$B synth --out "$OUT/a" --morphology >/dev/null
$B synth --out "$OUT/b" --variant mirrored --legacy-columns --cols 20 --rows 15 --seed 3 >/dev/null
$B ingest --section-id A --xenium "$OUT/a/xenium" --index-root "$OUT/index" --he "$OUT/a/he.tif" \
  --he-registration "$OUT/a/registration.json" --annotations "$OUT/a/cell_annotations.csv" \
  --lr-scores "$OUT/a/lr_cell_scores.csv" --grid niches="$OUT/a/niche_grid.csv" \
  --morphology "$OUT/a/xenium/morphology_focus"
$B ingest --section-id B --xenium "$OUT/b/xenium" --index-root "$OUT/index" --he "$OUT/b/he.tif" \
  --he-alignment-csv "$OUT/b/he_imagealignment.csv"
$B validate --index "$OUT/index/A" >/dev/null
$B validate --index "$OUT/index/B" >/dev/null
step "HTTP smoke + browser e2e on 127.0.0.1:$PORT"
printf 'sample_id,patient,Age,Secret\nA,P-SYN-1,54,do-not-show\n' > "$OUT/samples.csv"
$B serve --index-root "$OUT/index" --static-dir web/dist --port "$PORT" \
  --sample-metadata "$OUT/samples.csv" --sample-metadata-columns patient,Age & PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for _ in $(seq 50); do curl -fsS "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && break; sleep 0.1; done
curl -fsS "http://127.0.0.1:$PORT/healthz"; echo
curl -fsS "http://127.0.0.1:$PORT/api/v1/sections" | grep -q '"section_id":"A"'
curl -fsS "http://127.0.0.1:$PORT/api/v1/sections/A/manifest" >/dev/null
curl -fsS -o /dev/null "http://127.0.0.1:$PORT/api/v1/sections/A/transcripts/tiles/1/1?genes=0,1"
curl -fsS -o /dev/null "http://127.0.0.1:$PORT/api/v1/sections/A/images/he/0/0/0"
curl -fsS "http://127.0.0.1:$PORT/" | grep -q '<div id="root">'
code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/api/v1/sections/..%2Fetc/manifest")
[[ "$code" == 400 ]] || { echo "traversal not rejected ($code)"; exit 1; }
if [[ "${SKIP_E2E:-0}" != 1 ]]; then
  (cd web && npx playwright install chromium >/dev/null && XENIUM_E2E_URL="http://127.0.0.1:$PORT" npx playwright test)
fi
step "all checks passed"
