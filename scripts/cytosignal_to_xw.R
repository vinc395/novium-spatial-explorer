#!/usr/bin/env Rscript
# Convert CytoSignal per-fragment outputs (fragment_<n>_<diff|cont>_scores.rds) of one
# sample into a xenium-web column bundle (docs/DATA_CONTRACT.md § Column bundles):
#   <out>/<sample>_cytosignal.json        column metadata (labels, groups, provenance)
#   <out>/<sample>_cytosignal.cells.txt.gz  cell_id per row
#   <out>/<sample>_cytosignal.f32.gz        column-major little-endian float32 (NaN = missing)
# Per interaction and type (diffusion / contact):
#   score    CytoSignal Raw_smooth LR score; NA where the interaction was not estimable
#            in the cell's fragment (missing is never zero)
#   hotspot  categorical: 1 background, 2 hotspot (spatial FDR), 3 hotspot (HQ); 0 = not estimable
# Interactions not estimable anywhere in the sample are omitted. Values are copied, not recomputed.
# Run settings in <cytosignal_root>/RUN_CONFIG.json (when present) go into the provenance.
# Usage: Rscript cytosignal_to_xw.R <cytosignal_root> <sample> <out_dir>
suppressMessages({ library(Matrix); library(jsonlite) })
a <- commandArgs(TRUE)
stopifnot(length(a) == 3)
root <- a[1]; sample <- a[2]; out <- a[3]
dir.create(out, recursive = TRUE, showWarnings = FALSE)
cov <- read.csv(file.path(root, "panel_LR_coverage.csv"), stringsAsFactors = FALSE)
res <- file.path(root, "results", sample)
files <- list.files(res, pattern = "^fragment_[0-9]+_(diff|cont)_scores\\.rds$", full.names = TRUE)
if (!length(files)) stop("no CytoSignal score files for ", sample)

# Union of analysed cells over fragments (a cell belongs to one fragment).
load_type <- function(type) lapply(files[grepl(paste0("_", type, "_scores\\.rds$"), files)], readRDS)
frags <- list(diff = load_type("diff"), cont = load_type("cont"))
cells <- unique(unlist(lapply(c(frags$diff, frags$cont), function(L) L$cells$cell_id)))
stopifnot(!anyDuplicated(cells))
n <- length(cells)
row_of <- function(ids) match(ids, cells)
# Rows of each fragment's score matrix, matched once per fragment.
for (type in names(frags)) frags[[type]] <- lapply(frags[[type]], function(L) { L$rows <- row_of(rownames(L$score)); L })

columns <- list(); data <- list()
type_name <- c(diff = "diffusion", cont = "contact")
run_cfg <- if (file.exists(file.path(root, "RUN_CONFIG.json"))) fromJSON(file.path(root, "RUN_CONFIG.json")) else list()
prov_base <- list(method = sprintf("CytoSignal %s (b2c1a3c)", if (is.null(run_cfg$cytosignal_version)) "0.5.1" else run_cfg$cytosignal_version),
                  analysis_run_id = basename(normalizePath(root)),
                  inference = "per tissue fragment", radius_um = 200,
                  null_samples = if (is.null(run_cfg$null_samples)) 100000 else run_cfg$null_samples,
                  missing = "not estimable in the cell's fragment (never zero)")
for (k in intersect(c("spatial_FDR", "p_threshold", "reads_threshold", "signal_threshold",
                      "minimum_fragment_cells", "annotation_policy"), names(run_cfg))) prov_base[[k]] <- run_cfg[[k]]
for (type in c("diff", "cont")) {
  universe <- cov[cov$eligible & cov$type == type, ]
  for (k in seq_len(nrow(universe))) {
    id <- universe$id[k]
    sc <- rep(NA_real_, n); hs <- rep(0, n); estimable <- FALSE
    for (L in frags[[type]]) {
      S <- L$score
      if (!(id %in% colnames(S))) next
      estimable <- TRUE
      r <- L$rows
      sc[r] <- as.numeric(S[, id])
      hs[r] <- 1
      hot <- L$hotspots[[id]]; if (length(hot)) hs[row_of(hot)] <- 2
      hq <- L$hotspots_hq[[id]]; if (length(hq)) hs[row_of(hq)] <- 3
    }
    if (!estimable) next
    pair <- sprintf("%s → %s", universe$ligand[k], universe$receptor[k])
    base <- sprintf("cs:%s:%s", type, id)
    prov <- c(prov_base, list(interaction_id = id, ligand = universe$ligand[k], receptor = universe$receptor[k],
                              type = type_name[[type]]))
    columns[[length(columns) + 1]] <- list(id = paste0(base, ":score"), label = sprintf("%s · %s score", pair, type_name[[type]]),
      kind = "numeric", group = "lr_score", provenance = c(prov, list(score = "Raw_smooth")))
    data[[length(data) + 1]] <- sc
    columns[[length(columns) + 1]] <- list(id = paste0(base, ":hotspot"), label = sprintf("%s · %s hotspot", pair, type_name[[type]]),
      kind = "categorical", categories = c("background", "hotspot", "hotspot (HQ)"), group = "lr_hotspot",
      provenance = c(prov, list(hotspot = "spatial FDR 0.05; HQ = CytoSignal high-quality subset")))
    data[[length(data) + 1]] <- hs
  }
}
prefix <- file.path(out, paste0(sample, "_cytosignal"))
con <- gzfile(paste0(prefix, ".cells.txt.gz"), "w"); writeLines(cells, con); close(con)
con <- gzfile(paste0(prefix, ".f32.gz"), "wb")
for (v in data) writeBin(as.numeric(v), con, size = 4, endian = "little")
close(con)
meta <- list(format = "xenium-web-columns", version = 1, sample = sample, n_rows = n,
             cells_file = basename(paste0(prefix, ".cells.txt.gz")), data_file = basename(paste0(prefix, ".f32.gz")),
             source = "CytoSignal fragment_*_{diff,cont}_scores.rds", columns = columns)
write_json(meta, paste0(prefix, ".json"), auto_unbox = TRUE, pretty = TRUE, digits = NA)
cat(sample, ": ", n, " cells, ", length(columns), " columns (", sum(sapply(columns, `[[`, "group") == "lr_score"), " interactions)\n", sep = "")
