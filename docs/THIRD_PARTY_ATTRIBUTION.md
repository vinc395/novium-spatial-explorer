# Third-party attribution

- **Fovea** (MIT, https://github.com/PABannier/fovea, audited at `bd11457b`).
  Design reference only (viewport-only streaming, chunked cell overlays, request
  batching). No source code copied.
- **Odon** (GPL-3.0-only, https://github.com/alexcoulton/odon, audited at
  `b01faef0`). Consulted for Xenium file conventions only. No source code copied,
  so no GPL obligations apply to this repository. If Odon code is adapted in
  future, keep its notices and do not label the combined work MIT-only.
- Rust dependencies (see `Cargo.lock`): arrow / parquet (Apache-2.0), tiff and
  jpeg-encoder (MIT / Apache-2.0), axum / tokio / tower-http (MIT), serde,
  rayon, lru, csv, flate2, sha2, ring and rustls (TLS and password hashing), jpeg2k
  with OpenJPEG (BSD-2-Clause) and pure_jpeg2k for the JPEG 2000 morphology images.
- **UCell** (Andreatta & Carmona; R package, GPL-3.0): the scoring formula is
  reimplemented from the publication; no UCell code is included. The parity tests
  compare against scores produced by R UCell, which is installed separately
  (`scripts/setup_ucell_reference.sh`), and `crates/xenium-score/tests/fixtures`
  stores R's scores for synthetic inputs.
- Web dependencies (see `web/package-lock.json`): deck.gl and luma.gl (MIT),
  React (MIT), Vite (MIT).
- Palettes: Tableau-10/20-style categorical colours; viridis, magma, inferno, plasma
  and cividis colormap stops (CC0); turbo (Apache-2.0); ColorBrewer RdBu, Greys, Blues,
  Reds, Set1, Set3, Dark2 and Paired (Apache-2.0); Okabe–Ito and Kelly's colours of
  maximum contrast (published colour sets).
