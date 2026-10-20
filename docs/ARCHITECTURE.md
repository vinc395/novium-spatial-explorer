# Architecture and decision record

## Overview

```
 Xenium output dir (read-only)      H&E TIFF + registration       analysis tables
 cells.parquet, *_boundaries.parquet,      (JSON / 10x CSV)       (annotations, LR, grids)
 transcripts.parquet, experiment.xenium          │                        │
              └───────────────┬──────────────────┴────────────────────────┘
                     xenium-web ingest  (xenium-io readers → xenium-index)
                              │  staging → validate → atomic symlink publish
                              ▼
          <index_root>/<section>  →  .versions/<section>/<version>/
                              │
                     xenium-web serve  (Axum, 127.0.0.1)
              JSON manifest + versioned binary tiles + JPEG tiles
                              │  same origin (or SSH tunnel)
                              ▼
     Browser: React panels + ONE deck.gl WebGL2 canvas (OrthographicView, µm)
     H&E TileLayer(modelMatrix) · cell polygons/centroids · transcripts points/density
     · niche raster · selection
```

## Frames

| Name | Meaning | Where used |
|---|---|---|
| XU | Xenium µm, x right, y down | world frame for everything drawn; all vector data stored in XU |
| XM | Xenium morphology level-0 px (`XU = XM × pixel_size`) | only inside the 10x alignment adapter |
| image px | level-0 px of a served image (native H&E) | image tiles; placed with `matrix_xu_from_px` |
| CSS px | browser viewport | deck.gl camera only |

The transform is resolved once at ingest from a *named* adapter and stored in the
manifest; the browser applies it as a `modelMatrix`. Molecular data is never
transformed, so re-registering an image does not require re-indexing transcripts.

## Decision record

**D1 — Do not fork Fovea; use deck.gl (WebGL2) for rendering.** The starter guide
fixed Fovea (Rust→WASM→WebGPU) as the renderer. After auditing Fovea at
`bd11457b` and Odon at `b01faef0`:
- Fovea's server reads WSIs via OpenSlide, which does not read typical
  OME-TIFF H&E. Its overlay supports one cell layer with a fixed 64-class palette
  and no point layer. Adding transcripts, scalar colouring and unlimited categories
  would mean writing most of a renderer in WGSL/Rust.
- WebGPU on Linux browsers (the target workstation) is still not reliably enabled.
  WebGL2 works everywhere.
- deck.gl already provides tiled image pyramids with arbitrary affine placement,
  binary attributes (zero-copy typed arrays), GPU filtering, polygon tessellation
  and GPU picking.
The guide's intent is kept: one canvas and camera, viewport-only streaming, LOD,
typed frames and stable IDs.

**D2 — Write the readers instead of adapting Odon.** Odon's Xenium path reads
only the Explorer `*.zarr.zip` bundles. It never reads `transcripts.parquet`,
`cells.parquet` or `cell_boundaries.parquet`, and it has no H&E alignment code.
Its Parquet points reader rejects string `cell_id` and binary `feature_name`.
New readers on `arrow`/`parquet`/`tiff` were smaller than adapting it, and keep
the codebase free of GPL obligations. Odon was used as a reference only.

**D3 — Transcript index is gene-major, tile-sorted.** Records are 16 bytes
(x, y f32 µm; cell_key; z; qv; flags), grouped by gene, then by 256 µm tile, then
y, x. A per-gene tile directory allows:
- a viewport tile for *k* selected genes = *k* binary searches + *k* contiguous reads;
- density tiles at any level read only that gene's records in the covered tiles;
- per-gene tile counts drive the browser's points/density LOD decision before
  any points are fetched.
Built by external sort (128 gene buckets), so peak RAM is about one bucket.

**D4 — Polygons are chunked by the owning cell's centroid; queries expand by a
margin.** Each polygon is stored once (no duplicates, no double alpha). The
manifest records `query_margin_um` (max centroid→vertex Chebyshev distance), and
the client expands its viewport by it. The validator checks the margin covers
every vertex.

**D5 — Per-cell attributes are whole columns in key order.** One `f32` or
`u32` code per cell (4 MB per field for 1M cells). Recolouring is a client-side
lookup with no geometry refetch. Expression colouring uses a per-gene sparse
column built at ingest.

**D6 — Index versions are immutable; URLs carry the version.** Responses are
`immutable`-cacheable only when `?v=` matches the live version, so a re-ingest can
never mix old and new tiles.

**D7 — H&E is re-tiled at ingest into a JPEG pyramid** (512 px tiles, q85). This
costs disk (about 1–2 GB for a 40× slide) but gives one uniform, fast tile format
whatever the source TIFF layout (strips, tiles, planar, YCbCr JPEG, 16-bit).
JPEG 2000 sources are rejected explicitly.

**D8 — UCell signature scores are computed by the server, on demand, and match R.**
`crates/xenium-score` reimplements UCell (R package 2.14.0; unchanged in 2.17 devel)
rather than precomputing at ingest, because signatures are user-defined and edited
interactively. The ranking, ties (average ranks), `maxRank` cap, normalisation,
negative genes (`w_neg`, clipped at 0) and missing-gene modes follow R's code path
operation by operation; ranks are half-integers, so the sums are exact and scores
are bit-identical to R (tested against R on random edge-case matrices, a synthetic
section and a real 46,782-cell section).
- Ranks always use every gene-expression feature of the panel (QV≥20 counts),
  never only the selected/visible genes; control probes and codewords are excluded.
- Three results are kept distinct: **cell level** (each cell's own counts, from
  `expr_csr`, identical to Xenium's `cell_feature_matrix`), **bin level** (UCell of
  all QV≥20 transcripts pooled in a square, segmentation-free), and **spatially
  aggregated** (mean of cell-level scores per square, computed in the browser for
  display only). They have different keys, labels and legends.
- Jobs run one at a time on a dedicated Rayon pool (half the cores by default),
  chunked (4,096 cells or one band of bin rows per step) with progress and
  cancellation. Results are cached per index version under the state directory
  (`--state-dir`, default `<index_root>/.xenium-web-state`), keyed by a hash of the
  gene list, parameters and level, so the same key works in every section.
- Colouring reuses the existing per-cell colour path (an XFD1 column, like any
  field), so scores recolour cells without reloading geometry, as base colour or as
  the overlay. The starter guide's "Fovea WebGPU renderer" does not exist here (D1);
  the deck.gl layers are extended instead and no second renderer is added.
