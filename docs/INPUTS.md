# Preparing your data: the input guide

This guide shows how to bring each kind of data into NOVIUM Spatial Explorer: the Xenium
output (cells, boundaries, transcripts), an H&E image and its registration, the morphology
stains, per-cell annotations and scores, niche grids and per-sample information. The exact
file formats and every rule are in [DATA_CONTRACT.md](DATA_CONTRACT.md).

Each **section** (one Xenium region, usually one tissue sample) is imported once with
`xenium-web ingest` into an **index**: a folder of compact files built for fast streaming.
The server then streams only what the screen shows. Your source files are only read,
never changed.

```
Xenium output folder ─┐
H&E image + registration ─┤
morphology_focus/ (stains) ─┼─ xenium-web ingest ──> <index-root>/<section-id>/ ──> xenium-web serve ──> browser
annotations, scores, grids ─┘
```

## 0. Try it first with synthetic data

`xenium-web synth` writes a small artificial section with every kind of input, so you can
see working examples of all the files below (they are not biological data):

```bash
B=target/release/xenium-web
$B synth --out demo --morphology
# demo/xenium/                  cells.parquet, cell_boundaries.parquet, nucleus_boundaries.parquet,
#                               transcripts.parquet, experiment.xenium, morphology_focus/*.ome.tif
# demo/he.tif                   an H&E-like image
# demo/registration.json        its registration (image pixels -> Xenium µm)
# demo/he_imagealignment.csv    the same registration as a Xenium Explorer alignment CSV
# demo/cell_annotations.csv     cell types and a numeric column per cell
# demo/lr_cell_scores.csv       ligand–receptor scores in long format
# demo/niche_grid.csv           a segmentation-free niche grid
$B ingest --section-id DEMO --xenium demo/xenium --index-root index \
  --he demo/he.tif --he-registration demo/registration.json \
  --morphology demo/xenium/morphology_focus \
  --annotations demo/cell_annotations.csv --lr-scores demo/lr_cell_scores.csv \
  --grid niches=demo/niche_grid.csv
$B serve --index-root index --static-dir web/dist        # open http://127.0.0.1:8080
```

## 1. Cells, boundaries and transcripts (the Xenium output folder)

Point `--xenium` at the Xenium Onboard Analysis or Xenium Ranger output folder
(`outs/`). Check what it contains first; this prints file names, column schemas and counts,
never data values:

```bash
xenium-web inspect --xenium /path/to/outs
```

| File | Needed | What it gives you |
|---|---|---|
| `cells.parquet` | yes | one dot per cell (`cell_id`, `x_centroid`, `y_centroid` in µm); its other columns (counts, areas, segmentation method…) become colourable fields |
| `cell_boundaries.parquet` | optional | cell outlines (`cell_id`, `vertex_x`, `vertex_y`) |
| `nucleus_boundaries.parquet` | optional | nucleus outlines |
| `transcripts.parquet` | optional | every transcript (`feature_name`, `x_location`, `y_location`, plus `qv`, `cell_id`, `z_location`, `overlaps_nucleus` when present): gene search, transcript dots and density, per-cell expression |
| `experiment.xenium` | recommended | pixel size and panel name |

- The original string `cell_id` is kept everywhere (inspector, exports, joins).
- Resegmented outputs (e.g. Xenium Ranger `resegment`) work the same way.
- No transcripts needed? Add `--skip-transcripts` for a much faster import.
- Controls and blank codewords are recognised and kept apart from genes; expression uses
  QV ≥ 20 and assigned transcripts.

## 2. The H&E image and its registration

`--he` takes a TIFF or OME-TIFF: tiled or stripped; uncompressed, LZW, Deflate, PackBits
or JPEG; 8- or 16-bit RGB, RGBA or grayscale; BigTIFF. JPEG 2000 and palette images are
not supported (convert them first, e.g. with `vips`). The image is shown at full
resolution and is **never warped**: it is placed with one exact transform that you supply.
Give exactly one of these:

**a) A Xenium Explorer alignment file** (Xenium Explorer → Image alignment → export the
`*_imagealignment.csv`, a 3×3 matrix):
```bash
--he he.ome.tif --he-alignment-csv sample_he_imagealignment.csv
```
By 10x convention the matrix maps H&E pixels to Xenium *morphology pixels*; it is scaled by
the pixel size in `experiment.xenium`. If your matrix maps to µm instead, add
`--he-alignment-units um`.

**b) An explicit affine matrix** in `registration.json`:
```json
{"mode": "affine", "direction": "xu_from_px",
 "matrix": [[0.4988, -0.0349, 109.07], [0.0349, 0.4988, 26.37], [0, 0, 1]]}
```
`xu_from_px` maps image pixels to Xenium µm; use `"direction": "px_from_xu"` for the
inverse (it is inverted once, exactly). Rotations and mirroring are fine; a mirrored image
is flagged in the viewer.
```bash
--he he.tif --he-registration registration.json
```

**c) An image already warped onto the Xenium grid** (e.g. registered with palom or VALIS
onto the DAPI image; same origin, no rotation):
```bash
--he he_registered.ome.tif --he-already-aligned-pixel-size 0.2125
```

Multi-image TIFFs: pick the full-resolution image with `--he-ifds 0`, or three grayscale
images as R,G,B with `--he-ifds 2,1,0`.

**Check the alignment.** After ingest, `xenium-web qc-registration --index index/SECTION`
measures how dark the H&E is at the Xenium nucleus centres versus shifted positions,
globally and per region, and reports the offset (numbers only): **OK**, **CHECK** (a region
is off: misregistration or local tissue deformation) or **FAIL**. Then confirm by eye at
cell scale: H&E nuclei should sit inside the Xenium nucleus outlines across the section.

## 3. Morphology stains (DAPI and the multimodal cell-segmentation stains)

```bash
--morphology /path/to/outs/morphology_focus
```
Every `*.ome.tif` channel (DAPI, ATP1A1/CD45/E-Cadherin, 18S, alphaSMA/Vimentin…)
becomes a fluorescence layer on the Xenium pixel grid. In the viewer each channel gets its
own colour, brightness and opacity, mixed additively. `xenium-web qc-registration --index
index/SECTION --image morph-dapi` checks that DAPI lines up with the nuclei.

## 4. Per-cell annotations (cell types, clusters, niches, QC flags)

Any table with one row per cell: `.csv`, `.tsv`, `.txt` (optionally `.gz`) or `.parquet`,
with a `cell_id` column (or name yours with `--annotation-id-column`):

```
cell_id,cell_type,cluster,confidence
aaabbbcc-1,T cell,7,0.92
aaabbccd-1,Fibroblast,2,0.81
```
```bash
--annotations cell_types.csv \
--annotation-columns 'cell_type,major_type,cluster:cat,confidence,niche_*'
```
- Every column becomes a field you can colour by: numeric when all values are numbers,
  otherwise categories (up to 2,000 distinct values; any number of colours).
- `--annotation-columns` picks and orders columns: `*` globs, `:cat` treats numbers as
  categories (e.g. cluster IDs), `:num` forces numbers. The viewer first colours by
  `major_cell_type` if there is one, otherwise `fine_cell_type`, otherwise the first
  categorical column.
- Repeat `--annotations` for several tables; each column comes from the first table that
  has it. Missing values (`""`, `NA`, `NaN`, `null`, `None`) are shown grey (or hidden).
- Cells absent from `cells.parquet` are ignored and counted in the import report.

## 5. Per-cell scores

### a) A few scores: add them as annotation columns
Numeric annotation columns (pathway scores, module scores, probabilities) work like any
other field, with colour maps, ranges and log scaling.

### b) Ligand–receptor (or any pairwise) scores in long format: `--lr-scores`
```
cell_id,pair_id,role,score,method,analysis_run_id
aaabbbcc-1,CXCL12_CXCR4,sender,0.82,my_method,run_2026_01
aaabbbcc-1,CXCL12_CXCR4,receiver,0.10,my_method,run_2026_01
```
One field `lr:<pair>:<role>` per pair and role (`role` is optional). The extra columns
are kept as provenance and shown in the legend. Sections with LR fields are marked "· LR".

### c) Hundreds of columns: a column bundle (`--columns name.json`)
For large outputs (e.g. 880 CytoSignal columns per sample) a column bundle is far faster
than a wide CSV. It is three files side by side:
```
name.json           which columns, their labels, groups and provenance
name.cells.txt.gz   one cell_id per line
name.f32.gz         float32 values, column after column (NaN = missing;
                    categorical columns: 0 = missing, 1..k = the k categories)
```
Writing one from Python:
```python
import gzip, json
import numpy as np, pandas as pd

df = pd.read_csv("scores.csv")                       # cell_id + one column per score
cols = [c for c in df.columns if c != "cell_id"]
with gzip.open("myscores.cells.txt.gz", "wt") as f:
    f.write("\n".join(df["cell_id"].astype(str)) + "\n")
with gzip.open("myscores.f32.gz", "wb") as f:
    for c in cols:                                   # column-major
        f.write(df[c].to_numpy(dtype="<f4").tobytes())
meta = {
    "format": "xenium-web-columns", "version": 1, "n_rows": len(df),
    "cells_file": "myscores.cells.txt.gz", "data_file": "myscores.f32.gz",
    "columns": [{"id": f"my:{c}", "label": c, "kind": "numeric", "group": "my_scores",
                 "provenance": {"method": "describe how these were computed"}} for c in cols],
}
json.dump(meta, open("myscores.json", "w"), indent=1)
```
Column IDs may use letters, digits and `_ - . :` (up to 128 characters). Columns in the
groups `lr_score` and `lr_hotspot` are treated as LR results: the inspector loads them only
when "LR at this cell" is opened, and exports include them only when ticked.
`scripts/cytosignal_to_xw.R` converts CytoSignal per-fragment results into a bundle (a
score and a hotspot column per interaction); adjust its provenance fields to your run.

### d) Gene-signature scores (UCell): nothing to prepare
Create signatures in the viewer ("Gene signatures") and score them at cell or bin level;
results match R UCell 2.14. Headless: `xenium-web ucell --index index/SECTION
--signatures sigs.json|.gmt --out scores.parquet`.

## 6. Segmentation-free grids (niches, programmes, any regular map)

A regular square grid of points with values, e.g. niche labels or programme scores
computed on bins rather than cells:
```
x_um,y_um,niche_label,program_1,program_2
110.00,70.00,Niche_1,0.7827,-0.1020
130.00,70.00,Niche_1,0.8147,-0.1020
```
```bash
--grid niches=niche_grid.csv --grid-columns 'niches=niche_label,program_*'
```
`x_um`/`y_um` are the centres of the grid squares in Xenium µm (other names with
`--grid-x-col` / `--grid-y-col`); the spacing is inferred or set with `--grid-spacing-um`.
Grids are independent of the cells. The viewer draws them smoothed (display only: each
label is interpolated with a cubic B-spline onto pixels of about 2 µm, as
`scipy.ndimage.zoom(order=3)` does, and each pixel takes the label with the largest value;
numeric values are interpolated the same way), or as one square per point with "smooth"
off. Their legends have the same colour controls as the cell legends. Repeat `--grid` for
several grids.

## 7. Per-sample information (shown in the Index panel)

Not part of the index: given to the server, so you can change it without re-importing.
```
sample_id,age,sex,timepoint
SAMPLE01,54,F,baseline
```
```bash
xenium-web serve ... --sample-metadata samples.csv --sample-metadata-columns age,sex,timepoint
```
Only the listed columns are read and served, after sign-in; values are never logged.

## 8. Putting it together

```bash
xenium-web ingest --section-id SAMPLE01 --name "Sample 01" \
  --xenium /data/SAMPLE01/outs --index-root /data/xenium-web-index \
  --he /data/SAMPLE01/he.ome.tif --he-alignment-csv /data/SAMPLE01/he_imagealignment.csv \
  --morphology /data/SAMPLE01/outs/morphology_focus \
  --annotations /data/annotations/SAMPLE01.csv --annotation-columns 'cell_type,cluster:cat' \
  --columns /data/scores/SAMPLE01_scores.json \
  --grid niches=/data/niches/SAMPLE01.csv
xenium-web validate --index /data/xenium-web-index/SAMPLE01
xenium-web qc-registration --index /data/xenium-web-index/SAMPLE01
xenium-web serve --index-root /data/xenium-web-index --static-dir web/dist
```
For a cohort, loop over your samples in a script (one `ingest` per section);
`scripts/slurm_ingest.sbatch` is a template for a Slurm cluster. Re-running `ingest`
publishes a new version atomically: viewers switch to it on their next request, and
`--keep-versions` (default 2) sets how many versions are kept.

## 9. Data protection

The index contains cell IDs, coordinates, expression and image tiles, so keep it, and any
image exports, where your ethics and data approvals allow. Run, region and slide
identifiers from `experiment.xenium` are not copied. Serving on a network needs an access
token or sign-in and HTTPS; see [DEPLOYMENT.md](DEPLOYMENT.md).
