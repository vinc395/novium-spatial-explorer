# Data contract (index schema_version 2; readers also accept 1)

## Inputs

### Xenium output folder (read-only)
| File | Required | Columns used |
|---|---|---|
| `cells.parquet` | yes | `cell_id` (string or integer), `x_centroid`, `y_centroid` (µm); every other numeric column becomes a field; text columns with ≤ 2000 distinct values become categorical fields |
| `cell_boundaries.parquet` | optional | `cell_id`, `vertex_x`, `vertex_y` (µm), optional `label_id` |
| `nucleus_boundaries.parquet` | optional | same |
| `transcripts.parquet` | optional | `feature_name` (string or binary), `x_location`, `y_location`; optional `z_location`, `qv`, `cell_id`, `overlaps_nucleus`, `codeword_category`, `is_gene` |
| `experiment.xenium` | recommended | `pixel_size`, `analysis_sw_version`, `panel_name`, panel target counts, `num_cells` (run/region/slide identifiers are **not** copied) |

Reported but not used: CSV variants, `*.zarr.zip`, and the `morphology.ome.tif`
z-stack. The `morphology_focus/` channels are imported with `--morphology` (below).

Rules:
- Boundary rows with the same consecutive `(cell_id, label_id)` form one ring.
  The repeated closing vertex is removed, and consecutive duplicates are collapsed.
  Rings with fewer than 3 vertices, or non-finite vertices, are skipped and counted.
- Unassigned transcripts are `cell_id` ∈ {`UNASSIGNED`, empty, null}, or `-1` for
  integer IDs. Transcripts with a `cell_id` absent from `cells.parquet` are stored
  as unassigned with the flag `unknown cell` and counted in the warnings.
- Feature category, in order of preference: `is_gene` → `codeword_category` →
  name prefix (`NegControlProbe_`, `NegControlCodeword_`, `BLANK_`,
  `UnassignedCodeword_`, `DeprecatedCodeword_`, `GenomicControl_`, `Intergenic_`,
  `antisense_`).
- QV is stored as `floor(qv × 4)` (u8), so `qv ≥ t` is exact for any `t` that is
  a multiple of 0.25. Expression counts use QV ≥ 20 and assigned transcripts only.

### Registration (exactly one per image; never guessed)
| CLI | Meaning |
|---|---|
| `--he-alignment-csv F [--he-alignment-units morphology-px\|um]` | 10x Xenium Explorer 3×3 matrix. Default convention (10x, spatialdata-io): image px → morphology px; then `XU = pixel_size × XM` |
| `--he-registration registration.json` | `{"mode":"affine","direction":"xu_from_px"\|"px_from_xu","matrix":[[a,b,c],[d,e,f],[0,0,1]]}` or `{"mode":"already_aligned","pixel_size_um":0.2125}`. `px_from_xu` is inverted exactly once. `displacement_field` is recognised and rejected |
| `--he-already-aligned-pixel-size P` | image already on the Xenium grid (same origin, no rotation) |

Non-affine (projective) matrices, singular matrices and NaN are rejected.
Reflections are allowed, flagged in the manifest and shown in the UI. If the OME
`PhysicalSizeX` differs from the registration's implied scale by more than 5%, an
import warning is recorded.

**QC is visual and mandatory for real data:** at cell scale, H&E nuclei must sit
inside the Xenium nucleus outlines across the whole section. A good global fit
does not prove cell-level alignment, because tissue deforms.

### Morphology channels (`--morphology <xenium>/morphology_focus`)
Every `*.ome.tif` in the folder (sorted by name: `ch0000_dapi.ome.tif` …
`ch0003_alphasma_vimentin.ome.tif`) becomes one grayscale image with the id
`morph-<channel>` (e.g. `morph-dapi`, `morph-atp1a1-cd45-e-cadherin`).
- Accepted: tiled BigTIFF/TIFF, one sample per pixel, unsigned 8- or 16-bit, JPEG 2000
  tiles (compression 34712, decoded with OpenJPEG). Pyramid levels are SubIFDs. Any
  other layout is an error, never a silent skip.
- Name: the OME `Channel Name` that the `TiffData` for this file points to (FileName →
  FirstC), otherwise the file name.
- Placement: already on the Xenium pixel grid (same origin, no rotation), with the OME
  `PhysicalSizeX`, or `pixel_size` from `experiment.xenium` when the OME value is
  missing. A difference of more than 1% between the two is noted on the image.
- Intensity: one linear window per channel. The 0.5th–99.9th percentile of the non-zero
  pixels of a reduced level (the smallest level at least 8,000 px wide) is mapped to
  0–255 and stored in the manifest (`channel.window`). Nothing else is changed: no
  background subtraction, gamma or per-tile scaling.
- Stored as 512-px grayscale JPEG tiles, like the H&E. The viewer colours and mixes the
  channels in the browser.

### Analysis tables (`.csv`, `.tsv`, `.txt`, optional `.gz`, or `.parquet`)
- `--annotations`: one row per cell, ID column `cell_id` (or `--annotation-id-column`).
  Each other column becomes a field: numeric if every non-missing value parses as
  a number, otherwise categorical. Missing values are `""`, `NA`, `NaN`, `nan`,
  `null`, `NULL` and `None`. Duplicate cells are an error.
- `--lr-scores`: long format `cell_id, pair_id, score` plus optional `role` (default
  `aggregate`), `method`, `normalization`, `analysis_run_id`. One numeric field
  `lr:<pair>:<role>` is created per pair and role, with the provenance values
  attached. A duplicate `(cell, pair, role)` is an error.
- `--grid ID=FILE`: a regular grid of cell centres `x_um, y_um` (configurable)
  plus any number of label/score columns. Spacing is inferred (it must be square)
  or given with `--grid-spacing-um`. Off-grid or duplicate points are an error.
  The values are stored as-is and never joined to cells. The viewer draws a grid
  smoothed by default (display only): each label's indicator is interpolated with a cubic
  B-spline onto pixels of about 2 µm (as `scipy.ndimage.zoom(order=3, mode="grid-constant",
  grid_mode=True)`) and each pixel takes the label with the largest value (missing counts
  as a label, compared last, so the tissue edge is smoothed too); numeric fields interpolate
  value and presence the same way. With "smooth" off it draws one square per point.

IDs in annotation and LR tables that are absent from `cells.parquet` are ignored
and counted in the warnings.

Column selection (`--annotation-columns`, `--grid-columns ID=…`) is a comma list of
names or `*` globs. `name:cat` imports an integer-coded column (e.g. a cluster id) as
categories; `name:num` parses text as numbers (`True`/`False` → 1/0). Selected
columns keep the listed order, so the first one is the viewer's default. Requested
non-glob columns that are missing from every table are an error. Without a selection
every column is imported. Text columns with more than 2,000 distinct values are skipped
with a warning.

### Column bundles (`--columns bundle.json`)
These hold many per-cell columns computed elsewhere, for example CytoSignal LR
scores exported from R with `scripts/cytosignal_to_xw.R`. The format is three
files next to each other:
```
<name>.json          {"format":"xenium-web-columns","version":1,"n_rows":N,
                      "cells_file":"<name>.cells.txt.gz","data_file":"<name>.f32.gz",
                      "columns":[{"id","label","kind":"numeric"|"categorical",
                                  "categories":[…],"group","provenance":{…}}]}
<name>.cells.txt.gz  original cell_id, one per row
<name>.f32.gz        column-major little-endian float32, N values per column
                     (numeric: NaN = missing; categorical: 0 = missing, 1..k)
```
Ingest validates the sizes, the cell-ID join (unknown IDs are warned, duplicates
are an error), the category codes and the column IDs. It rejects file references
outside the JSON's folder. Labels, groups and provenance are shown in the viewer.

CytoSignal mapping (`cytosignal_to_xw.R`, one bundle per sample): for each
estimable interaction and type (diffusion, contact) there are two columns:
- `cs:<diff|cont>:<CPI id>:score`: the native Raw_smooth LR score, missing where the
  interaction was not estimable in the cell's fragment (missing is never zero).
- `cs:…:hotspot`: background / hotspot (spatial FDR 0.05) / hotspot (HQ), missing
  where not estimable.

Labels read "ligand → receptor · diffusion score". Interactions not estimable
anywhere in the sample are left out. Values are copied, never recomputed.

### Registration QC (`xenium-web qc-registration`)
For up to 20,000 nucleus centres (vertex means of the Xenium nucleus polygons), the
mean H&E darkness (255 − mean RGB) is sampled at about 0.85 µm/px for every offset in
a ±20 µm grid (1 µm steps). The reported peak is the weighted centroid of the offsets
within 15% of the maximum. Verdicts: **OK** (peak ≤ 2 µm, every block ≤ 4 µm), **CHECK**
(larger offsets: misregistration or local deformation), **FAIL** (nuclei no darker than
their surroundings at the registered position). The synthetic tests confirm OK for
correct transforms and recover a deliberate 8 µm shift within 1.5 µm.
`--image morph-dapi` runs the same check on a morphology channel. For a channel image
the signal is brightness instead of darkness (DAPI-stained nuclei are bright), and the
report's `signal` field says which was used (the `darkness_*` fields hold that signal).

### UCell scores (server state, not part of the index)
```
<state_dir>/signatures.json                 {version: 1, signatures: [...]}, fsynced atomic writes
<state_dir>/ucell/<section>/<index_version>/<key>.bin    XUS1 scores
<state_dir>/ucell/<section>/<index_version>/<key>.json   metadata (written after .bin)
```
`key` = first 20 hex digits of SHA-256 over `{algorithm, genes, params, level}`.
Results of non-live index versions are deleted when a new result is stored for the
section; a `.bin` whose length does not match its metadata is ignored and recomputed.
`XUS1`: `magic, u16 ver=1, u16 level (0 cell, 1 bin), u32 n, u32 width, u32 height,
u32 0, f64 origin_x_um, f64 origin_y_um, f64 bin_um`, then `f64 score[n]` (NaN = no
score) and, for bins, `u32 n_transcripts[n]`. Bins are squares of `bin_um` aligned to
multiples of `bin_um` (row 0 at the top), covering the section bounds.

Definitions: cell level ranks each cell's QV≥20 assigned counts (`expr_csr`); bin
level ranks all QV≥20 gene transcripts whose position falls in the bin (bins with
fewer than `min_transcripts` get NaN); both rank every gene-expression feature of the
panel. The browser's "mean per bin" heatmap averages cell-level scores by centroid and
is a display, not a stored result.

## Cell identity
`cell_key = 1 + rank(cell_id)` under byte-wise string ordering within a section;
`0` = no cell. The full mapping is stored in `cells/ids.bin` (`XCI1`). Exports and
the inspector always show the original `cell_id`.

## Derived layout
```
<index_root>/<section_id> -> .versions/<section_id>/<version>/
  manifest.json            SectionManifest (schema_version 2)
  import_report.json       steps, timings, detected files, warnings
  cells/ids.bin            XCI1 string table (key order)
  cells/centroids.bin      XCE1
  cells/fields.{pack,idx}  XFD1 per field, key = position in cells.fields (XIX1 index)
  cells/cell_chunks.{pack,idx}, nucleus_chunks.{pack,idx}   XCC1 per 256 µm chunk (XIX1 index)
  transcripts/points.bin   16-byte records, gene → tile → y → x
  transcripts/gene_dirs.bin  (tile_key u32, rel_start u32, count u32) per gene
  transcripts/genes.bin    XGN1 per-gene offsets (40 B each)
  transcripts/expr_csc.bin  per gene (cell_key, count), QV≥20
  transcripts/expr_csr.{bin,idx}  per cell (gene, count), QV≥20
  images/<id>/tiles.{pack,idx}      512 px JPEG tiles, level 0 = full resolution, key =
                                    level << 26 | y << 13 | x (XIX1 index); `he`, and
                                    `morph-<channel>` per morphology channel
  grids/<grid_id>/<field>.bin       XGR1
```
Schema 2 stores the tiles of each image and all cell fields in packs, so a section is a
few dozen files rather than tens of thousands (mass small-file creation deadlocked the
ntfs3 driver of the USB index drive while the explorers read from it). The manifest
says which layout is used: `images[].tile_layout` and `cells.field_layout` are
`"packed"`, or absent for the schema-1 layout of one file per blob
(`images/<id>/<level>/<x>_<y>.jpg`, `cells/fields/<id>.bin`), which readers still
serve. A schema-1 manifest may not claim packed storage. URLs and decoded data are
the same for both layouts.

Manifest `images[]` entries of morphology channels carry an optional
`"channel": {"name": "DAPI", "window": [lo, hi], "color": [r, g, b]}` (raw intensity
window, suggested display colour). It is absent for H&E. The field is additive, so
schema version was not bumped for it, and older readers ignore it. The viewer treats the first image
without `channel` as the H&E.

Tile `(tx, ty)` covers `[tx·s, (tx+1)·s) × [ty·s, (ty+1)·s)` µm (s = 256 by default);
`tile_key = ((ty+32768) << 16) | (tx+32768)`.

## Binary formats (little-endian; every section 4-byte aligned)
| Magic | Layout |
|---|---|
| `XPT1` | `magic, u16 ver, u16 flags, u32 n, u32 nseg`, `nseg × (u32 gene, u32 count)`, `n × record` — record = `f32 x, f32 y, u32 cell_key, i16 z×100, u8 floor(qv×4), u8 flags(1 = overlaps nucleus, 2 = unknown cell)` |
| `XIX1` | pack index `magic, u32 n`, `n × (u32 key, u64 offset, u32 len)` sorted by key, into a `.pack` blob of concatenated entries |
| `XCC1` | `magic, u16 ver, u16 kind(0 cell, 1 nucleus), u32 n, u32 nverts`, `u32 keys[n]`, `u32 starts[n+1]`, `f32 xy[2·nverts]` (open rings) |
| `XCE1` | `magic, u16 ver, u16 0, u32 n, u32 0`, `f32 xy[2n]` (index = key − 1) |
| `XFD1` | `magic, u16 ver, u16 kind(0 f32, NaN = missing \| 1 u32 code, 0 = missing), u32 n, u32 ncat`, `4n bytes` |
| `XDT1` | `magic, u16 ver, u16 level, u32 w, u32 h, u32 max, u32 total`, `u32 counts[w·h]` (bin = 2^level µm, tile = 256 bins) |
| `XGT1` | `magic, u16 ver, u16 0, u32 n, u32 gene`, `n × (u32 tile_key, u32 count)` |
| `XGR1` | `magic, u16 ver, u16 kind, u32 w, u32 h, u32 ncat`, `4·w·h bytes` |

Decoders (Rust `xenium-core::codec`, TS `web/src/codecs.ts`) reject a wrong magic,
an unknown version, any length mismatch, and out-of-range codes. A shared golden
record is tested in both languages.
