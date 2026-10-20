#!/usr/bin/env Rscript
# Reference UCell scores for parity tests (crates/xenium-score/tests/r_parity.rs) and
# for checking xenium-web scores on a real section.
#
# Usage: Rscript ucell_reference.R <case_dir>
#   <case_dir>/matrix.mtx       genes x units, MatrixMarket coordinate (real or integer)
#   <case_dir>/genes.txt        one gene name per row of the matrix
#   <case_dir>/units.txt        one unit (cell/bin) id per column
#   <case_dir>/signatures.json  {"name": ["GENE", "GENE+", "GENE-", ...], ...}
#   <case_dir>/params.json      {"max_rank": 1500, "w_neg": 1, "missing_genes": "impute"}
# or, for a 10x cell_feature_matrix directory (matrix.mtx.gz, features.tsv.gz,
# barcodes.tsv.gz; only "Gene Expression" rows are ranked):
#   Rscript ucell_reference.R <case_dir> --tenx <cell_feature_matrix_dir>
# Writes <case_dir>/r_scores.tsv: unit, then one column per signature (%.17g, NaN kept).
# Exits non-zero if UCell refuses the input (e.g. a signature longer than maxRank).

suppressPackageStartupMessages({
  library(Matrix)
  library(jsonlite)
  library(UCell)
})

args <- commandArgs(trailingOnly = TRUE)
dir <- args[1]
tenx <- if (length(args) >= 3 && args[2] == "--tenx") args[3] else NULL

if (is.null(tenx)) {
  m <- as(readMM(file.path(dir, "matrix.mtx")), "CsparseMatrix")
  rownames(m) <- readLines(file.path(dir, "genes.txt"))
  colnames(m) <- readLines(file.path(dir, "units.txt"))
} else {
  m <- as(readMM(gzfile(file.path(tenx, "matrix.mtx.gz"))), "CsparseMatrix")
  feats <- read.delim(gzfile(file.path(tenx, "features.tsv.gz")), header = FALSE, stringsAsFactors = FALSE)
  rownames(m) <- feats[[2]]
  colnames(m) <- readLines(gzfile(file.path(tenx, "barcodes.tsv.gz")))
  m <- m[feats[[3]] == "Gene Expression", , drop = FALSE]
}

sigs <- fromJSON(file.path(dir, "signatures.json"), simplifyVector = FALSE)
sigs <- lapply(sigs, unlist)
p <- fromJSON(file.path(dir, "params.json"))

scores <- ScoreSignatures_UCell(
  m, features = sigs, maxRank = p$max_rank, w_neg = p$w_neg,
  missing_genes = p$missing_genes, chunk.size = 1000, name = ""
)

out <- file(file.path(dir, "r_scores.tsv"), "w")
writeLines(paste(c("unit", colnames(scores)), collapse = "\t"), out)
body <- apply(scores, 2, function(x) sprintf("%.17g", x))
if (is.null(dim(body))) body <- matrix(body, nrow = 1)
writeLines(paste(rownames(scores), apply(body, 1, paste, collapse = "\t"), sep = "\t"), out)
close(out)
cat(sprintf("UCell %s: %d units x %d signatures, maxRank %s (panel %d genes)\n",
            as.character(packageVersion("UCell")), nrow(scores), ncol(scores),
            format(min(p$max_rank, nrow(m))), nrow(m)))
