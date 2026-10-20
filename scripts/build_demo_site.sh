#!/usr/bin/env bash
# Build the online demo into site/: the viewer in demo mode (no server) plus every reply
# it needs, saved as files, for two small synthetic sections (not biological data).
# Serve site/ with any static web server, e.g. GitHub Pages. Needs Rust, Node >= 22.6.
# Synthetic data only: every reply in site/ is a public file once published.
#   scripts/build_demo_site.sh                 build site/
#   DEMO_REPO=https://github.com/you/repo scripts/build_demo_site.sh   link the badge
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=${OUT:-site}
WORK=${WORK:-target/demo}
PORT=${PORT:-18765}
B=target/release/xenium-web
cargo build --release -p xenium-cli
rm -rf "$WORK" "$OUT"
mkdir -p "$WORK"
$B synth --out "$WORK/a" --morphology >/dev/null
$B synth --out "$WORK/b" --variant mirrored --cols 24 --rows 16 --seed 3 >/dev/null
$B ingest --section-id DEMO1 --name "Demo 1 (synthetic)" --xenium "$WORK/a/xenium" --index-root "$WORK/index" \
  --he "$WORK/a/he.tif" --he-registration "$WORK/a/registration.json" --morphology "$WORK/a/xenium/morphology_focus" \
  --annotations "$WORK/a/cell_annotations.csv" --lr-scores "$WORK/a/lr_cell_scores.csv" \
  --grid niches="$WORK/a/niche_grid.csv" >/dev/null
$B ingest --section-id DEMO2 --name "Demo 2 (synthetic, mirrored H&E)" --xenium "$WORK/b/xenium" --index-root "$WORK/index" \
  --he "$WORK/b/he.tif" --he-alignment-csv "$WORK/b/he_imagealignment.csv" \
  --annotations "$WORK/b/cell_annotations.csv" >/dev/null
$B serve --index-root "$WORK/index" --port "$PORT" --state-dir "$WORK/state" >/dev/null 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for _ in $(seq 60); do curl -fsS -o /dev/null "http://127.0.0.1:$PORT/healthz" 2>/dev/null && break; sleep 0.5; done
(cd web && VITE_DEMO=1 VITE_DEMO_REPO="${DEMO_REPO:-}" npx vite build --base=./ --sourcemap false --outDir "../$OUT" --emptyOutDir >/dev/null)
node --experimental-strip-types --no-warnings scripts/export_demo.ts "http://127.0.0.1:$PORT" "$OUT/api"
touch "$OUT/.nojekyll"
du -sh "$OUT"
