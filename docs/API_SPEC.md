# HTTP API (v1)

Base: `/api/v1`. Same origin as the UI. Bound to `127.0.0.1` unless
`--allow-remote`. `{sid}`, field, image and grid IDs must match
`[A-Za-z0-9_.:-]+` and are resolved through the manifest; nothing in a URL is used
as a filesystem path directly. Data URLs accept `?v=<version>`: when it equals the
live version the response is `Cache-Control: immutable`, otherwise `no-store`.
Every response carries `X-Index-Version`.
With access control, `GET`/`POST /logout` (outside `/api`) expires the session cookie
and redirects to the sign-in page; the token and password stay valid.

| Method | Path | Response |
|---|---|---|
| GET | `/healthz` | `{"status":"ok",...}` |
| GET | `/session` | `{"auth": false}`; behind sign-in `{"auth": true, "password": bool, "user": email or null}` (`user` with `--access-users-file`; the viewer shows "Sign out" only when `auth`) |
| GET | `/sections` | `{sections:[{section_id, display_name, version, cells, transcripts, images, field_groups:{group: n_fields}, sample:[[column, value], ...] or null}]}` (`sample` only with `--sample-metadata`, listed columns only) |
| GET | `/sections/{sid}/manifest` | manifest JSON (`no-cache`) |
| GET | `/sections/{sid}/cells/centroids` | XCE1 |
| GET | `/sections/{sid}/cells/fields/{field}` | XFD1 |
| GET | `/sections/{sid}/cells/chunks/{cell\|nucleus}/{tx}/{ty}` | XCC1 (empty chunk if none) |
| GET | `/sections/{sid}/cells/key/{key}` | cell JSON: original `cell_id`, centroid, field values, top 30 features (QV≥20). `?groups=a,b` = only those field groups' values, `?exclude_groups=a,b` = all but those (the viewer leaves out `lr_score,lr_hotspot` on a click and fetches them when "LR at this cell" is opened: each value is one disk read) |
| GET | `/sections/{sid}/cells/lookup?id=<cell_id>` | `{cell_key, cell_id, centroid_um}` |
| GET | `/sections/{sid}/expression/{gene}` | XFD1 numeric (QV≥20 counts per cell) |
| GET | `/sections/{sid}/transcripts/genes/{gene}/tiles` | XGT1 |
| GET | `/sections/{sid}/transcripts/tiles/{tx}/{ty}?genes=1,2&qv_min=20` | XPT1 (≤ 64 genes, ≤ 4M points) |
| GET | `/sections/{sid}/transcripts/density/{gene}/{level}/{x}/{y}?qv_min=` | XDT1 (cached server-side) |
| GET | `/sections/{sid}/images/{image}/{level}/{x}/{y}` | JPEG |
| GET | `/sections/{sid}/grids/{grid}/{field}` | XGR1 |
| POST | `/sections/{sid}/selection/export` | body `{cell_keys:[..], fields:[..]}` (≤ 32 MB, ≤ 2M cells) → CSV with `section_id, cell_id, x_um, y_um, fields…` |

### UCell (see D8 in ARCHITECTURE.md)

| Method | Path | Body / query | Response |
|---|---|---|---|
| GET | `/ucell/signatures` | | `{signatures:[{id, name, description, genes, created_unix, updated_unix}]}` |
| POST | `/ucell/signatures` | `{name, description?, genes:["CD3E","CD2","CD19-",...]}` | 201 signature (400: invalid or duplicate name) |
| GET / PUT / DELETE | `/ucell/signatures/{id}` | PUT as POST | signature / 204 |
| POST | `/sections/{sid}/ucell/coverage` | `{genes, params?}` | per-gene status (`found`/`missing`/`not_a_gene`), detection, case-insensitive suggestion, effective maxRank, error if unscorable |
| POST | `/sections/{sid}/ucell/jobs` | `{signature_ids, signatures?:[{name,genes}], params:{max_rank, w_neg, missing_genes:"impute"\|"skip"}, level:"cell"\|"bin", bin_um?, min_transcripts?}` | job (202 queued/running, 200 when all results were cached); 400 if unscorable (e.g. a signature longer than maxRank) |
| GET | `/ucell/jobs`, `/ucell/jobs/{id}` | | `{id, section_id, status: queued\|running\|done\|failed\|cancelled, phase, done, total, keys, results?, error?}` |
| DELETE | `/ucell/jobs/{id}` | | cancels (takes effect within one chunk) |
| GET | `/sections/{sid}/ucell/results` | | stored results of the live index version |
| GET | `/sections/{sid}/ucell/results/{key}` | | result metadata (signature snapshot, params, level, coverage, stats, timing) |
| DELETE | `/sections/{sid}/ucell/results/{key}` | | 204; removes the stored result for this section only (404 if absent) |
| GET | `/sections/{sid}/ucell/results/{key}/values?v=` | | XFD1 f32 per cell (cell level) or XGR1 f32 per bin (bin level); NaN = no score |
| GET | `/sections/{sid}/ucell/results/{key}/summary?by=field:<id>\|grid:<grid>/<field>` | | per category: n, mean, sd, min, p05, q1, median, q3, p95, max (cell-level results; grids joined by cell centroid) |
| GET | `/sections/{sid}/ucell/export.parquet?keys=k1,k2` | | Parquet: cells (`section_id, cell_id, cell_key, x_um, y_um, <name>_UCell...`) or bins (`bin_col, bin_row, x0_um, y0_um, bin_um, n_transcripts, ...`); metadata key `xenium_web.ucell` |

When the state directory is not writable these endpoints return 503 with the reason;
the rest of the API is unaffected.

Errors are JSON `{"error": "..."}`: 400 invalid input, 404 unknown
section/field/tile, 413 too many genes/points/cells, 503 index unreadable or corrupt
or the data disk busy (every one of the server's 32 disk-read slots taken for 20 s,
e.g. a stalled drive; retry later), 500 otherwise (details go to the server log only). CSV cells that begin with
`= + - @` are prefixed with `'` to prevent spreadsheet formula injection.
