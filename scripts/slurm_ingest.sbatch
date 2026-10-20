#!/bin/bash
# Template: build one xenium-web index on a Slurm cluster. Edit the paths, then
#   sbatch scripts/slurm_ingest.sbatch SECTION_ID XENIUM_DIR HE_TIFF ALIGNMENT_CSV
#SBATCH --job-name=xenium-ingest
#SBATCH --cpus-per-task=8
#SBATCH --mem=24G
#SBATCH --time=02:00:00
set -euo pipefail
SECTION_ID=${1:?section id}
XENIUM_DIR=${2:?xenium output dir}
HE_TIFF=${3:-}
ALIGN_CSV=${4:-}
BIN=${XENIUM_WEB_BIN:-$HOME/xenium-web/target/release/xenium-web}
INDEX_ROOT=${XENIUM_INDEX_ROOT:?set XENIUM_INDEX_ROOT to a writable directory (not inside the raw data)}

args=(ingest --section-id "$SECTION_ID" --xenium "$XENIUM_DIR" --index-root "$INDEX_ROOT" --threads "${SLURM_CPUS_PER_TASK:-4}")
if [[ -n "$HE_TIFF" ]]; then
  [[ -n "$ALIGN_CSV" ]] || { echo "an H&E needs an explicit registration (alignment CSV)" >&2; exit 2; }
  args+=(--he "$HE_TIFF" --he-alignment-csv "$ALIGN_CSV")
fi
/usr/bin/time -v "$BIN" "${args[@]}"
"$BIN" validate --index "$INDEX_ROOT/$SECTION_ID" > "$INDEX_ROOT/$SECTION_ID.validate.json"
