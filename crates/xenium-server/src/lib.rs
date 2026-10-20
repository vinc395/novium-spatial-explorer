//! HTTP API over published section indexes (see `docs/API_SPEC.md`).
//!
//! * Section IDs are validated, resolved under the configured index root and
//!   canonicalised; a resolved path outside the root is rejected.
//! * Data URLs carry `?v=<index version>`; responses are marked immutable only
//!   when `v` equals the live version, so caches never serve a stale index.
//! * Every disk access runs on the blocking pool behind a concurrency limit. A slot
//!   is held until its read returns, so a stalled disk ties up at most `max_blocking`
//!   threads, and requests that cannot get a slot in time fail with 503.

use anyhow::{anyhow, Result};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use lru::LruCache;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use xenium_core::codec::QV_SCALE;
use xenium_core::manifest::is_safe_id;
use xenium_index::store::StoreError;
use xenium_index::SectionStore;

mod auth;
mod samples;
mod ucell;

pub use auth::{
    generate_password, generate_token, hash_password, hash_password_rounds, normalize_email,
    parse_people, people_add, people_add_with_password, people_list, people_remove,
    people_reset_all, with_auth, Auth, AUTH_COOKIE,
};
pub use samples::SampleInfo;

pub const API_VERSION: &str = "v1";

/// How long a request waits for a disk slot before failing with 503.
const IO_WAIT: Duration = Duration::from_secs(20);

/// Every disk slot stayed busy for the whole wait (a stalled or overloaded disk).
#[derive(Debug)]
pub(crate) struct StorageBusy;

impl std::fmt::Display for StorageBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("storage busy")
    }
}

impl std::error::Error for StorageBusy {}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub index_root: PathBuf,
    pub static_dir: Option<PathBuf>,
    /// Max bytes of computed responses (density tiles, expression columns) cached in RAM.
    pub cache_bytes: usize,
    pub max_blocking: usize,
    /// Writable directory for signatures and UCell results
    /// (default `<index_root>/.xenium-web-state`).
    pub state_dir: Option<PathBuf>,
    /// Threads for UCell scoring jobs (0 = half the cores).
    pub score_threads: usize,
    /// Per-sample information shown in the Index panel (listed columns only).
    pub sample_info: Option<Arc<SampleInfo>>,
}

pub(crate) struct Section {
    target: PathBuf,
    version: String,
    store: Arc<SectionStore>,
}

struct ByteCache {
    lru: LruCache<String, Bytes>,
    bytes: usize,
    limit: usize,
}

impl ByteCache {
    fn get(&mut self, k: &str) -> Option<Bytes> {
        self.lru.get(k).cloned()
    }
    fn put(&mut self, k: String, v: Bytes) {
        if v.len() > self.limit / 4 {
            return;
        }
        self.bytes += v.len();
        if let Some(old) = self.lru.put(k, v) {
            self.bytes -= old.len();
        }
        while self.bytes > self.limit {
            match self.lru.pop_lru() {
                Some((_, v)) => self.bytes -= v.len(),
                None => break,
            }
        }
    }
}

pub struct AppState {
    root: PathBuf,
    /// Serialises (re)opening of section stores so a re-publish opens each one once.
    open_lock: tokio::sync::Mutex<()>,
    sections: Mutex<HashMap<String, Arc<Section>>>,
    cache: Mutex<ByteCache>,
    limiter: Arc<Semaphore>,
    /// How long a request waits for a disk slot ([`IO_WAIT`]).
    io_wait: Duration,
    /// CSV exports build their body in memory; allow only a couple at once.
    pub(crate) export_limiter: Semaphore,
    /// UCell scoring, or why it is unavailable (e.g. the state directory is read-only).
    ucell: std::result::Result<ucell::Ucell, String>,
    samples: Option<Arc<SampleInfo>>,
}

#[derive(Debug)]
pub struct ApiError(pub(crate) StatusCode, pub(crate) String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        if e.downcast_ref::<StorageBusy>().is_some() {
            return ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "the data disk is busy; try again shortly".into(),
            );
        }
        match e.downcast_ref::<StoreError>() {
            Some(StoreError::NotFound(s)) => {
                ApiError(StatusCode::NOT_FOUND, format!("not found: {s}"))
            }
            Some(StoreError::BadRequest(s)) => ApiError(StatusCode::BAD_REQUEST, s.clone()),
            Some(StoreError::TooLarge(s)) => ApiError(StatusCode::PAYLOAD_TOO_LARGE, s.clone()),
            None => {
                tracing::error!("internal error: {e:#}");
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error (see server log)".into(),
                )
            }
        }
    }
}

pub(crate) fn bad(s: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, s.into())
}

pub(crate) fn not_found(s: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, s.into())
}

pub(crate) type ApiResult<T> = std::result::Result<T, ApiError>;

impl AppState {
    pub fn new(cfg: &ServerConfig) -> Result<Arc<Self>> {
        let root = std::fs::canonicalize(&cfg.index_root)
            .map_err(|e| anyhow!("index root {}: {e}", cfg.index_root.display()))?;
        let state_dir = cfg
            .state_dir
            .clone()
            .unwrap_or_else(|| root.join(".xenium-web-state"));
        let threads = match cfg.score_threads {
            0 => std::thread::available_parallelism()
                .map(|n| n.get() / 2)
                .unwrap_or(1)
                .max(1),
            n => n,
        };
        let ucell = ucell::Ucell::open(&state_dir, threads).map_err(|e| {
            tracing::warn!("UCell scoring disabled: {e:#}");
            format!("{e:#}")
        });
        if let Ok(u) = &ucell {
            tracing::info!(
                "UCell: state in {}, {threads} scoring threads",
                u.state_dir.display()
            );
        }
        Ok(Arc::new(AppState {
            root,
            open_lock: tokio::sync::Mutex::new(()),
            sections: Mutex::new(HashMap::new()),
            cache: Mutex::new(ByteCache {
                lru: LruCache::unbounded(),
                bytes: 0,
                limit: cfg.cache_bytes,
            }),
            limiter: Arc::new(Semaphore::new(cfg.max_blocking.max(1))),
            io_wait: IO_WAIT,
            export_limiter: Semaphore::new(2),
            ucell,
            samples: cfg.sample_info.clone(),
        }))
    }

    /// Resolve a section id to its live version, reopening after a re-publish.
    pub(crate) async fn section(self: &Arc<Self>, id: &str) -> ApiResult<Arc<Section>> {
        if !is_safe_id(id) || id.contains(':') {
            return Err(bad("invalid section id"));
        }
        let link = self.root.join(id);
        let target = self
            .blocking(move || Ok(std::fs::canonicalize(&link).ok()))
            .await?
            .ok_or_else(|| not_found(format!("section {id:?}")))?;
        if !target.starts_with(&self.root) {
            return Err(not_found(format!("section {id:?}")));
        }
        let cached = |st: &Self| {
            st.sections
                .lock()
                .unwrap()
                .get(id)
                .filter(|s| s.target == target)
                .cloned()
        };
        if let Some(s) = cached(self) {
            return Ok(s);
        }
        let _guard = tokio::time::timeout(self.io_wait, self.open_lock.lock())
            .await
            .map_err(|_| anyhow!(StorageBusy))?;
        if let Some(s) = cached(self) {
            return Ok(s);
        }
        let t2 = target.clone();
        let opened = self
            .blocking(move || {
                Ok(t2
                    .join("manifest.json")
                    .is_file()
                    .then(|| SectionStore::open(&t2)))
            })
            .await?;
        let store = match opened {
            None => return Err(not_found(format!("section {id:?}"))),
            Some(Ok(store)) => store,
            Some(Err(e)) => {
                tracing::error!("cannot open section {id}: {e:#}");
                return Err(ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("section {id:?} index is not readable"),
                ));
            }
        };
        if store.manifest.section_id != id {
            return Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "section id mismatch in manifest".into(),
            ));
        }
        let version = target
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let s = Arc::new(Section {
            target,
            version,
            store: Arc::new(store),
        });
        let prefix = format!("{id}/");
        {
            let mut c = self.cache.lock().unwrap();
            let stale: Vec<String> = c
                .lru
                .iter()
                .map(|(k, _)| k.clone())
                .filter(|k| k.starts_with(&prefix))
                .collect();
            for k in stale {
                if let Some(v) = c.lru.pop(&k) {
                    c.bytes -= v.len();
                }
            }
        }
        self.sections
            .lock()
            .unwrap()
            .insert(id.to_string(), s.clone());
        Ok(s)
    }

    /// Run disk IO on the blocking pool, at most `max_blocking` at once. The slot
    /// moves into the task, so a read stuck in the kernel keeps it even after its
    /// client gave up, and new requests get 503 instead of more stuck threads.
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = tokio::time::timeout(self.io_wait, self.limiter.clone().acquire_owned())
            .await
            .map_err(|_| anyhow!(StorageBusy))?
            .map_err(|e| anyhow!(e))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f()
        })
        .await
        .map_err(|e| anyhow!("task failed: {e}"))?
    }

    /// Drop one computed response from the in-memory cache.
    pub(crate) fn evict(&self, key: &str) {
        let mut c = self.cache.lock().unwrap();
        if let Some(v) = c.lru.pop(key) {
            c.bytes -= v.len();
        }
    }

    pub(crate) async fn cached(
        self: &Arc<Self>,
        key: String,
        f: impl FnOnce() -> Result<Vec<u8>> + Send + 'static,
    ) -> ApiResult<Bytes> {
        if let Some(b) = self.cache.lock().unwrap().get(&key) {
            return Ok(b);
        }
        let b = Bytes::from(self.blocking(f).await?);
        self.cache.lock().unwrap().put(key, b.clone());
        Ok(b)
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct VersionQ {
    v: Option<String>,
}

pub(crate) fn binary(
    sec: &Section,
    v: &Option<String>,
    content_type: &'static str,
    body: Bytes,
) -> Response {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    let cache = if v.as_deref() == Some(sec.version.as_str()) {
        "private, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&sec.version) {
        h.insert("x-index-version", v);
    }
    (h, body).into_response()
}

pub(crate) const OCTET: &str = "application/octet-stream";

pub fn router(state: Arc<AppState>, static_dir: Option<&Path>) -> Router {
    let api = Router::new()
        .route("/sections", get(list_sections))
        .route("/sections/{sid}/manifest", get(manifest))
        .route("/sections/{sid}/cells/centroids", get(centroids))
        .route("/sections/{sid}/cells/fields/{field}", get(field))
        .route("/sections/{sid}/cells/chunks/{kind}/{tx}/{ty}", get(chunk))
        .route("/sections/{sid}/cells/key/{key}", get(cell_by_key))
        .route("/sections/{sid}/cells/lookup", get(cell_lookup))
        .route("/sections/{sid}/expression/{gene}", get(expression))
        .route(
            "/sections/{sid}/transcripts/genes/{gene}/tiles",
            get(gene_tiles),
        )
        .route(
            "/sections/{sid}/transcripts/tiles/{tx}/{ty}",
            get(points_tile),
        )
        .route(
            "/sections/{sid}/transcripts/density/{gene}/{level}/{x}/{y}",
            get(density),
        )
        .route(
            "/sections/{sid}/images/{image}/{level}/{x}/{y}",
            get(image_tile),
        )
        .route("/sections/{sid}/grids/{grid}/{field}", get(grid_field))
        .route(
            "/sections/{sid}/selection/export",
            post(export).layer(DefaultBodyLimit::max(32 << 20)),
        )
        .merge(ucell::routes())
        // Without access control there is no session; `with_auth` answers this itself.
        .route("/session", get(|| async { Json(json!({"auth": false})) }))
        .fallback(|| async { not_found("no such API endpoint") });
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .nest(&format!("/api/{API_VERSION}"), api)
        .with_state(state);
    if let Some(dir) = static_dir {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)));
    }
    app.layer(CompressionLayer::new())
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; img-src 'self' blob: data:; style-src 'self' 'unsafe-inline'; worker-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'",
            ),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
}

async fn healthz() -> Json<Value> {
    Json(
        json!({"status": "ok", "service": "xenium-web", "version": env!("CARGO_PKG_VERSION"), "api": API_VERSION}),
    )
}

async fn list_sections(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let root = st.root.clone();
    let mut ids: Vec<String> = st
        .blocking(move || {
            Ok(std::fs::read_dir(&root)?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && is_safe_id(n) && !n.contains(':'))
                .collect())
        })
        .await?;
    ids.sort();
    let mut out = vec![];
    for id in ids {
        match st.section(&id).await {
            Ok(s) => {
                let m = &s.store.manifest;
                out.push(json!({
                    "section_id": m.section_id,
                    "display_name": m.display_name,
                    "version": s.version,
                    "cells": m.cells.as_ref().map(|c| c.count),
                    "transcripts": m.transcripts.as_ref().map(|t| t.indexed),
                    "images": m.images.iter().map(|i| json!({"id": i.id, "label": i.label})).collect::<Vec<_>>(),
                    // [[column, value], ...] in the configured column order (behind auth).
                    "sample": st.samples.as_ref().and_then(|x| x.get(&m.section_id)),
                    // Number of cell fields per group (e.g. lr_score), so the UI can say
                    // which sections carry which analysis results without loading them.
                    "field_groups": m.cells.as_ref().map(|c| {
                        let mut g = serde_json::Map::new();
                        for f in &c.fields {
                            let n = g.get(&f.group).and_then(|v| v.as_u64()).unwrap_or(0);
                            g.insert(f.group.clone(), json!(n + 1));
                        }
                        g
                    }),
                }));
            }
            Err(e) => out.push(json!({"section_id": id, "error": e.1})),
        }
    }
    Ok(Json(json!({ "sections": out })))
}

async fn manifest(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(v) = HeaderValue::from_str(&s.version) {
        h.insert("x-index-version", v);
    }
    Ok((h, s.store.manifest_json.clone()).into_response())
}

async fn centroids(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    let store = s.store.clone();
    let b = st.blocking(move || store.centroids_bytes()).await?;
    Ok(binary(&s, &q.v, OCTET, b.into()))
}

async fn field(
    State(st): State<Arc<AppState>>,
    AxPath((sid, field)): AxPath<(String, String)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    if !is_safe_id(&field) {
        return Err(bad("invalid field id"));
    }
    let store = s.store.clone();
    let b = st.blocking(move || store.field_bytes(&field)).await?;
    Ok(binary(&s, &q.v, OCTET, b.into()))
}

async fn chunk(
    State(st): State<Arc<AppState>>,
    AxPath((sid, kind, tx, ty)): AxPath<(String, String, i32, i32)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    let nucleus = match kind.as_str() {
        "cell" => false,
        "nucleus" => true,
        _ => return Err(bad("kind must be 'cell' or 'nucleus'")),
    };
    let store = s.store.clone();
    let b = st
        .blocking(move || {
            Ok(store.polygon_chunk(nucleus, tx, ty)?.unwrap_or_else(|| {
                let kind = if nucleus {
                    xenium_core::codec::ShapeKind::Nucleus
                } else {
                    xenium_core::codec::ShapeKind::Cell
                };
                xenium_core::codec::encode_polygons(kind, &xenium_core::codec::PolygonChunk::new())
            }))
        })
        .await?;
    Ok(binary(&s, &q.v, OCTET, b.into()))
}

#[derive(Deserialize, Default)]
struct DetailsQ {
    /// Only the fields of these groups (comma-separated), e.g. `lr_score,lr_hotspot`.
    groups: Option<String>,
    /// Leave out the fields of these groups (comma-separated).
    exclude_groups: Option<String>,
}

async fn cell_by_key(
    State(st): State<Arc<AppState>>,
    AxPath((sid, key)): AxPath<(String, u32)>,
    Query(q): Query<DetailsQ>,
) -> ApiResult<Json<Value>> {
    let s = st.section(&sid).await?;
    let store = s.store.clone();
    let list = |v: Option<String>| -> Vec<String> {
        v.map(|v| {
            v.split(',')
                .filter(|g| !g.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
    };
    let only = q.groups.is_some().then(|| list(q.groups));
    let skip = list(q.exclude_groups);
    let keep = move |f: &xenium_core::manifest::FieldInfo| {
        only.as_ref().is_none_or(|o| o.contains(&f.group)) && !skip.contains(&f.group)
    };
    Ok(Json(
        st.blocking(move || store.cell_details_where(key, keep))
            .await?,
    ))
}

#[derive(Deserialize)]
struct LookupQ {
    id: String,
}

async fn cell_lookup(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Query(q): Query<LookupQ>,
) -> ApiResult<Json<Value>> {
    let s = st.section(&sid).await?;
    if q.id.len() > 256 {
        return Err(bad("id too long"));
    }
    let key = s
        .store
        .key_by_id(&q.id)
        .ok_or_else(|| not_found("cell id"))?;
    let c = s.store.centroid(key).unwrap();
    Ok(Json(
        json!({"cell_key": key, "cell_id": q.id, "centroid_um": c}),
    ))
}

async fn expression(
    State(st): State<Arc<AppState>>,
    AxPath((sid, gene)): AxPath<(String, u32)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    let store = s.store.clone();
    let b = st
        .cached(format!("{sid}/{}/expr/{gene}", s.version), move || {
            store.expression_field(gene)
        })
        .await?;
    Ok(binary(&s, &q.v, OCTET, b))
}

async fn gene_tiles(
    State(st): State<Arc<AppState>>,
    AxPath((sid, gene)): AxPath<(String, u32)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    let store = s.store.clone();
    let b = st.blocking(move || store.gene_tiles_bytes(gene)).await?;
    Ok(binary(&s, &q.v, OCTET, b.into()))
}

#[derive(Deserialize)]
struct PointsQ {
    genes: String,
    qv_min: Option<f64>,
    v: Option<String>,
}

/// QV threshold -> stored quantisation (`floor(qv * 4)`). Thresholds on the 0.25 grid
/// are exact; values within 1e-6 of a grid point snap to it (avoids float drift);
/// anything else rounds up to the next grid point.
fn qv_q(qv: Option<f64>) -> ApiResult<u8> {
    match qv {
        None => Ok(0),
        Some(q) if q.is_finite() && (0.0..=63.0).contains(&q) => {
            let x = q * QV_SCALE as f64;
            let r = x.round();
            Ok(if (x - r).abs() < 1e-6 { r } else { x.ceil() } as u8)
        }
        Some(_) => Err(bad("qv_min must be within 0..=63")),
    }
}

fn parse_genes(s: &str) -> ApiResult<Vec<u32>> {
    if s.len() > 4096 {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "gene list too long".into(),
        ));
    }
    s.split(',')
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.trim()
                .parse::<u32>()
                .map_err(|_| bad(format!("bad gene id {t:?}")))
        })
        .collect()
}

async fn points_tile(
    State(st): State<Arc<AppState>>,
    AxPath((sid, tx, ty)): AxPath<(String, i32, i32)>,
    Query(q): Query<PointsQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    let genes = parse_genes(&q.genes)?;
    let qmin = qv_q(q.qv_min)?;
    let store = s.store.clone();
    let b = st
        .blocking(move || store.points_tile(tx, ty, &genes, qmin))
        .await?;
    Ok(binary(&s, &q.v, OCTET, b.into()))
}

#[derive(Deserialize)]
struct DensityQ {
    qv_min: Option<f64>,
    v: Option<String>,
}

async fn density(
    State(st): State<Arc<AppState>>,
    AxPath((sid, gene, level, x, y)): AxPath<(String, u32, u32, i32, i32)>,
    Query(q): Query<DensityQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    // Density thresholds are snapped to whole QV units to bound the cache key space.
    let qmin = qv_q(q.qv_min.map(f64::floor))?;
    let store = s.store.clone();
    let key = format!("{sid}/{}/density/{gene}/{level}/{x}/{y}/{qmin}", s.version);
    let b = st
        .cached(key, move || store.density_tile(gene, level, x, y, qmin))
        .await?;
    Ok(binary(&s, &q.v, OCTET, b))
}

async fn image_tile(
    State(st): State<Arc<AppState>>,
    AxPath((sid, image, level, x, y)): AxPath<(String, String, u32, u32, u32)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    if !is_safe_id(&image) {
        return Err(bad("invalid image id"));
    }
    let store = s.store.clone();
    let b = st
        .blocking(move || store.image_tile(&image, level, x, y))
        .await?;
    Ok(binary(&s, &q.v, "image/jpeg", b.into()))
}

async fn grid_field(
    State(st): State<Arc<AppState>>,
    AxPath((sid, grid, field)): AxPath<(String, String, String)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    if !is_safe_id(&grid) || !is_safe_id(&field) {
        return Err(bad("invalid id"));
    }
    let store = s.store.clone();
    let b = st
        .blocking(move || store.grid_raster(&grid, &field))
        .await?;
    Ok(binary(&s, &q.v, OCTET, b.into()))
}

#[derive(Deserialize)]
struct ExportBody {
    cell_keys: Vec<u32>,
    #[serde(default)]
    fields: Vec<String>,
}

async fn export(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Json(body): Json<ExportBody>,
) -> ApiResult<Response> {
    let s = st.section(&sid).await?;
    if body.fields.len() > 256 {
        return Err(bad("too many fields"));
    }
    let store = s.store.clone();
    let _permit = st.export_limiter.acquire().await.map_err(|e| anyhow!(e))?;
    let csv = st
        .blocking(move || store.export_csv(&body.cell_keys, &body.fields))
        .await?;
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let name = format!("attachment; filename=\"{sid}_selection.csv\"");
    if let Ok(v) = HeaderValue::from_str(&name) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok((h, Body::from(csv)).into_response())
}

/// Serve `app` on `listener` until Ctrl-C / SIGTERM, then shut down gracefully.
pub async fn serve(listener: tokio::net::TcpListener, app: Router) -> Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = ctrl_c => {}
                _ = async { if let Some(t) = term.as_mut() { t.recv().await; } else { std::future::pending::<()>().await } } => {}
            }
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

/// Serve over HTTPS (rustls, ring provider) until Ctrl-C / SIGTERM.
pub async fn serve_tls(
    addr: std::net::SocketAddr,
    app: Router,
    cert: &Path,
    key: &Path,
) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
        .await
        .map_err(|e| anyhow!("load TLS certificate/key: {e}"))?;
    let handle = axum_server::Handle::new();
    let h2 = handle.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = ctrl_c => {}
            _ = async { if let Some(t) = term.as_mut() { t.recv().await; } else { std::future::pending::<()>().await } } => {}
        }
        tracing::info!("shutting down");
        h2.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
    });
    axum_server::bind_rustls(addr, config)
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qv_threshold_snapping() {
        assert_eq!(qv_q(None).unwrap(), 0);
        assert_eq!(qv_q(Some(20.0)).unwrap(), 80);
        assert_eq!(qv_q(Some(20.000000000000004)).unwrap(), 80);
        assert_eq!(qv_q(Some(19.999999999)).unwrap(), 80);
        assert_eq!(qv_q(Some(20.1)).unwrap(), 81);
        assert_eq!(qv_q(Some(20.25)).unwrap(), 81);
        assert!(qv_q(Some(-1.0)).is_err() && qv_q(Some(f64::NAN)).is_err());
    }

    #[tokio::test]
    async fn a_stalled_read_keeps_its_slot_and_later_requests_get_503() {
        let tmp = tempfile::tempdir().unwrap();
        let mut st = AppState::new(&ServerConfig {
            index_root: tmp.path().into(),
            static_dir: None,
            cache_bytes: 1 << 20,
            max_blocking: 1,
            state_dir: Some(tmp.path().join("state")),
            score_threads: 1,
            sample_info: None,
        })
        .unwrap();
        Arc::get_mut(&mut st).unwrap().io_wait = Duration::from_millis(300);
        // A read stuck in the kernel whose client gave up (the request was dropped).
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let (started, running) = tokio::sync::oneshot::channel();
        let st2 = st.clone();
        let request = tokio::spawn(async move {
            st2.blocking(move || {
                let _ = started.send(());
                let _ = stuck.recv();
                Ok(())
            })
            .await
        });
        running.await.unwrap();
        request.abort();
        let e = st.blocking(|| Ok(())).await.unwrap_err();
        assert!(e.downcast_ref::<StorageBusy>().is_some(), "{e:#}");
        assert_eq!(ApiError::from(e).0, StatusCode::SERVICE_UNAVAILABLE);
        // Once the read returns, its slot is free again.
        release.send(()).unwrap();
        st.blocking(|| Ok(())).await.unwrap();
    }
}
