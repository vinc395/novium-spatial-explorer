#!/usr/bin/env bash
# Install the reference R UCell (2.14.0, Bioconductor 3.22) into a conda env so that
# `scripts/verify.sh` also runs the live R parity tests (it looks for
# $HOME/miniforge3/envs/ucell-ref/bin/Rscript, or XENIUM_UCELL_RSCRIPT).
set -euo pipefail
MAMBA=${MAMBA:-$HOME/miniforge3/bin/mamba}
"$MAMBA" create -y -n ucell-ref -c conda-forge -c bioconda \
  r-base=4.5 bioconductor-ucell=2.14.0 r-data.table r-matrix r-jsonlite
"$(dirname "$MAMBA")/../envs/ucell-ref/bin/Rscript" -e 'cat("UCell", as.character(packageVersion("UCell")), "\n")'
