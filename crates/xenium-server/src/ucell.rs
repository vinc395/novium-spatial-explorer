//! UCell endpoints: signatures (CRUD), gene coverage, scoring jobs (progress,
//! cancellation), cached results, per-category summaries and Parquet export.
//!
//! Jobs run one at a time on a dedicated Rayon pool (half the cores by default) so
//! that tile requests stay responsive while scoring. Results are stored per index
//! version under the state directory and survive restarts.

use crate::{bad, binary, not_found, ApiError, ApiResult, AppState, VersionQ, OCTET};
use anyhow::anyhow;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use xenium_core::codec::{encode_field, encode_grid_raster, FieldColumn};
use xenium_score::cache::{is_key, Level, ResultCache, ResultMeta, SignatureSnapshot};
use xenium_score::engine::{self, JobSpec};
use xenium_score::progress::{Cancelled, Progress};
use xenium_score::section::{coverage, SectionPanel};
use xenium_score::signatures::{is_signature_id, SignatureInput, SignatureStore};
use xenium_score::summary::{summarize, GroupBy};
use xenium_score::ucell::UcellParams;

const MAX_JOBS_KEPT: usize = 200;
const MAX_QUEUED: usize = 16;

pub struct Ucell {
    pub state_dir: PathBuf,
    signatures: SignatureStore,
    cache: Arc<ResultCache>,
    pool: Arc<rayon::ThreadPool>,
    /// One scoring job at a time.
    run_lock: Arc<tokio::sync::Semaphore>,
    jobs: Mutex<Vec<Arc<Job>>>,
}

impl Ucell {
    pub fn open(state_dir: &Path, threads: usize) -> anyhow::Result<Self> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| anyhow!("state directory {}: {e}", state_dir.display()))?;
        let probe = state_dir.join(".write-test");
        std::fs::write(&probe, b"ok").map_err(|e| {
            anyhow!(
                "state directory {} is not writable: {e}",
                state_dir.display()
            )
        })?;
        let _ = std::fs::remove_file(probe);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|i| format!("ucell-{i}"))
            .build()?;
        Ok(Ucell {
            state_dir: state_dir.to_path_buf(),
            signatures: SignatureStore::open(state_dir)?,
            cache: Arc::new(ResultCache::new(state_dir)),
            pool: Arc::new(pool),
            run_lock: Arc::new(tokio::sync::Semaphore::new(1)),
            jobs: Mutex::new(vec![]),
        })
    }
}

#[derive(Clone)]
enum JobState {
    Queued,
    Running,
    Done(Vec<ResultMeta>),
    Failed(String),
    Cancelled,
}

struct Job {
    id: String,
    section_id: String,
    version: String,
    spec: JobSpec,
    keys: Vec<String>,
    progress: Arc<Progress>,
    state: Mutex<JobState>,
    started: Instant,
    created_unix: u64,
}

impl Job {
    fn finished(&self) -> bool {
        matches!(
            *self.state.lock().unwrap(),
            JobState::Done(_) | JobState::Failed(_) | JobState::Cancelled
        )
    }

    fn json(&self) -> Value {
        let st = self.state.lock().unwrap().clone();
        let p = self.progress.snapshot();
        let (status, error, results) = match &st {
            JobState::Queued => ("queued", None, None),
            JobState::Running => ("running", None, None),
            JobState::Done(r) => ("done", None, Some(r.clone())),
            JobState::Failed(e) => ("failed", Some(e.clone()), None),
            JobState::Cancelled => ("cancelled", None, None),
        };
        json!({
            "id": self.id,
            "section_id": self.section_id,
            "index_version": self.version,
            "status": status,
            "phase": p.phase,
            "done": p.done,
            "total": p.total,
            "elapsed_ms": self.started.elapsed().as_millis() as u64,
            "created_unix": self.created_unix,
            "level": self.spec.level,
            "params": self.spec.params,
            "signatures": self.spec.signatures.iter().map(|s| &s.name).collect::<Vec<_>>(),
            "keys": self.keys,
            "error": error,
            "results": results,
        })
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/ucell/signatures",
            get(list_signatures).post(create_signature),
        )
        .route(
            "/ucell/signatures/{id}",
            get(get_signature)
                .put(update_signature)
                .delete(delete_signature),
        )
        .route("/ucell/jobs", get(list_jobs))
        .route("/ucell/jobs/{id}", get(get_job).delete(cancel_job))
        .route("/sections/{sid}/ucell/coverage", post(section_coverage))
        .route("/sections/{sid}/ucell/jobs", post(submit_job))
        .route("/sections/{sid}/ucell/results", get(list_results))
        .route(
            "/sections/{sid}/ucell/results/{key}",
            get(result_meta).delete(delete_result),
        )
        .route(
            "/sections/{sid}/ucell/results/{key}/values",
            get(result_values),
        )
        .route(
            "/sections/{sid}/ucell/results/{key}/summary",
            get(result_summary),
        )
        .route("/sections/{sid}/ucell/export.parquet", get(export_parquet))
        .layer(axum::extract::DefaultBodyLimit::max(1 << 20))
}

fn ucell(st: &AppState) -> ApiResult<&Ucell> {
    st.ucell.as_ref().map_err(|e| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("UCell scoring is unavailable: {e}"),
        )
    })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn bad_any(e: anyhow::Error) -> ApiError {
    bad(format!("{e:#}"))
}

// ---------------------------------------------------------------- signatures

async fn list_signatures(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "signatures": ucell(&st)?.signatures.list() })))
}

fn check_sig_id(id: &str) -> ApiResult<()> {
    if is_signature_id(id) {
        Ok(())
    } else {
        Err(bad("invalid signature id"))
    }
}

async fn get_signature(
    State(st): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    check_sig_id(&id)?;
    let s = ucell(&st)?
        .signatures
        .get(&id)
        .ok_or_else(|| not_found("signature"))?;
    Ok(Json(json!(s)))
}

async fn create_signature(
    State(st): State<Arc<AppState>>,
    Json(body): Json<SignatureInput>,
) -> ApiResult<Response> {
    let u = ucell(&st)?;
    let s = u.signatures.create(body).map_err(bad_any)?;
    Ok((StatusCode::CREATED, Json(json!(s))).into_response())
}

async fn update_signature(
    State(st): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
    Json(body): Json<SignatureInput>,
) -> ApiResult<Json<Value>> {
    check_sig_id(&id)?;
    let s = ucell(&st)?
        .signatures
        .update(&id, body)
        .map_err(bad_any)?
        .ok_or_else(|| not_found("signature"))?;
    Ok(Json(json!(s)))
}

async fn delete_signature(
    State(st): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> ApiResult<StatusCode> {
    check_sig_id(&id)?;
    if ucell(&st)?.signatures.delete(&id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found("signature"))
    }
}

// ---------------------------------------------------------------- coverage

#[derive(Deserialize)]
struct CoverageBody {
    genes: Vec<String>,
    #[serde(default)]
    params: Option<UcellParams>,
}

async fn section_coverage(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Json(body): Json<CoverageBody>,
) -> ApiResult<Json<Value>> {
    ucell(&st)?;
    if body.genes.len() > xenium_score::signatures::MAX_GENES {
        return Err(bad("too many genes"));
    }
    let s = st.section(&sid).await?;
    let store = s.store.clone();
    let p = body.params.unwrap_or_default();
    let c = st
        .blocking(move || {
            let panel = SectionPanel::from_manifest(&store.manifest)?;
            Ok(coverage(&store, &panel, &body.genes, &p))
        })
        .await
        .map_err(bad_any)?;
    Ok(Json(json!(c)))
}

// ---------------------------------------------------------------- jobs

#[derive(Deserialize)]
struct AdHoc {
    name: String,
    genes: Vec<String>,
}

#[derive(Deserialize)]
struct JobBody {
    #[serde(default)]
    signature_ids: Vec<String>,
    /// Unsaved signatures (scored under their given name).
    #[serde(default)]
    signatures: Vec<AdHoc>,
    #[serde(default)]
    params: Option<UcellParams>,
    /// "cell" (default) or "bin".
    #[serde(default)]
    level: Option<String>,
    bin_um: Option<f64>,
    min_transcripts: Option<u32>,
}

async fn submit_job(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Json(body): Json<JobBody>,
) -> ApiResult<Response> {
    let u = ucell(&st)?;
    let s = st.section(&sid).await?;
    let mut snaps = vec![];
    for id in &body.signature_ids {
        check_sig_id(id)?;
        let sig = u
            .signatures
            .get(id)
            .ok_or_else(|| not_found(format!("signature {id}")))?;
        snaps.push(SignatureSnapshot {
            id: Some(sig.id),
            name: sig.name,
            genes: sig.genes,
        });
    }
    for a in body.signatures {
        let v = xenium_score::signatures::validate(SignatureInput {
            name: a.name,
            description: String::new(),
            genes: a.genes,
        })
        .map_err(bad_any)?;
        snaps.push(SignatureSnapshot {
            id: None,
            name: v.name,
            genes: v.genes,
        });
    }
    let level = match body.level.as_deref().unwrap_or("cell") {
        "cell" => Level::Cell,
        "bin" => Level::Bin {
            bin_um: body.bin_um.unwrap_or(50.0),
            min_transcripts: body.min_transcripts.unwrap_or(10),
        },
        other => return Err(bad(format!("level must be 'cell' or 'bin', not {other:?}"))),
    };
    let spec = JobSpec {
        signatures: snaps,
        params: body.params.unwrap_or_default(),
        level,
    };
    // Validate against this section's panel before queueing (maxRank, lengths, bins).
    let store = s.store.clone();
    let spec2 = spec.clone();
    st.blocking(move || {
        let panel = SectionPanel::from_manifest(&store.manifest)?;
        spec2.check(&panel)?;
        if let Level::Bin { bin_um, .. } = spec2.level {
            xenium_score::section::BinGrid::for_bounds(store.manifest.bounds_um, bin_um)?;
        }
        Ok(())
    })
    .await
    .map_err(bad_any)?;
    let keys = spec.keys();
    // Same work already queued or running for this section version: return that job.
    {
        let jobs = u.jobs.lock().unwrap();
        if let Some(j) = jobs.iter().find(|j| {
            j.section_id == sid && j.version == s.version && j.keys == keys && !j.finished()
        }) {
            return Ok((StatusCode::ACCEPTED, Json(j.json())).into_response());
        }
        if jobs.iter().filter(|j| !j.finished()).count() >= MAX_QUEUED {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "too many scoring jobs queued".into(),
            ));
        }
    }
    let id = format!("job_{}", &crate::generate_token()?[..12]);
    let job = Arc::new(Job {
        id,
        section_id: sid.clone(),
        version: s.version.clone(),
        spec,
        keys,
        progress: Arc::new(Progress::new()),
        state: Mutex::new(JobState::Queued),
        started: Instant::now(),
        created_unix: now(),
    });
    {
        let mut jobs = u.jobs.lock().unwrap();
        jobs.push(job.clone());
        while jobs.len() > MAX_JOBS_KEPT {
            match jobs.iter().position(|j| j.finished()) {
                Some(i) => {
                    jobs.remove(i);
                }
                None => break,
            }
        }
    }
    let (lock, pool, cache, store) = (
        u.run_lock.clone(),
        u.pool.clone(),
        u.cache.clone(),
        s.store.clone(),
    );
    let j = job.clone();
    tokio::spawn(async move {
        let Ok(_permit) = lock.acquire_owned().await else {
            return;
        };
        if j.progress.is_cancelled() {
            *j.state.lock().unwrap() = JobState::Cancelled;
            return;
        }
        *j.state.lock().unwrap() = JobState::Running;
        let j2 = j.clone();
        let r = tokio::task::spawn_blocking(move || {
            pool.install(|| engine::run(&store, &j2.version, &cache, &j2.spec, &j2.progress))
        })
        .await;
        let state = match r {
            Ok(Ok(metas)) => JobState::Done(metas),
            Ok(Err(e)) if e.downcast_ref::<Cancelled>().is_some() => JobState::Cancelled,
            Ok(Err(e)) => {
                tracing::error!("UCell job {} failed: {e:#}", j.id);
                JobState::Failed(format!("{e:#}"))
            }
            Err(e) => JobState::Failed(format!("job panicked: {e}")),
        };
        *j.state.lock().unwrap() = state;
    });
    // Let an all-cached job finish before answering, so the client can skip polling.
    for _ in 0..20 {
        if job.finished() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let code = if job.finished() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((code, Json(job.json())).into_response())
}

async fn list_jobs(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let jobs: Vec<Value> = ucell(&st)?
        .jobs
        .lock()
        .unwrap()
        .iter()
        .rev()
        .map(|j| j.json())
        .collect();
    Ok(Json(json!({ "jobs": jobs })))
}

fn find_job(u: &Ucell, id: &str) -> ApiResult<Arc<Job>> {
    u.jobs
        .lock()
        .unwrap()
        .iter()
        .find(|j| j.id == id)
        .cloned()
        .ok_or_else(|| not_found("job"))
}

async fn get_job(
    State(st): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(find_job(ucell(&st)?, &id)?.json()))
}

async fn cancel_job(
    State(st): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> ApiResult<Json<Value>> {
    let j = find_job(ucell(&st)?, &id)?;
    j.progress.cancel();
    {
        let mut s = j.state.lock().unwrap();
        if matches!(*s, JobState::Queued) {
            *s = JobState::Cancelled;
        }
    }
    Ok(Json(j.json()))
}

// ---------------------------------------------------------------- results

async fn list_results(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
) -> ApiResult<Json<Value>> {
    let u = ucell(&st)?;
    let s = st.section(&sid).await?;
    let (cache, version) = (u.cache.clone(), s.version.clone());
    let list = st.blocking(move || cache.list(&sid, &version)).await?;
    Ok(Json(json!({ "index_version": s.version, "results": list })))
}

async fn load_meta(
    st: &Arc<AppState>,
    sid: &str,
    key: &str,
) -> ApiResult<(Arc<crate::Section>, ResultMeta)> {
    let u = ucell(st)?;
    if !is_key(key) {
        return Err(bad("invalid result key"));
    }
    let s = st.section(sid).await?;
    let (cache, sid2, version, key2) = (
        u.cache.clone(),
        sid.to_string(),
        s.version.clone(),
        key.to_string(),
    );
    let m = st
        .blocking(move || {
            Ok(cache
                .meta(&sid2, &version, &key2)?
                .filter(|m| cache.is_complete(m)))
        })
        .await?
        .ok_or_else(|| {
            not_found(format!(
                "UCell result {key} (not computed for this section)"
            ))
        })?;
    Ok((s, m))
}

async fn result_meta(
    State(st): State<Arc<AppState>>,
    AxPath((sid, key)): AxPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(load_meta(&st, &sid, &key).await?.1)))
}

/// Delete one stored result for this section (other sections keep theirs).
async fn delete_result(
    State(st): State<Arc<AppState>>,
    AxPath((sid, key)): AxPath<(String, String)>,
) -> ApiResult<StatusCode> {
    let u = ucell(&st)?;
    if !is_key(&key) {
        return Err(bad("invalid result key"));
    }
    let s = st.section(&sid).await?;
    let (cache, sid2, version, key2) =
        (u.cache.clone(), sid.clone(), s.version.clone(), key.clone());
    let found = st
        .blocking(move || cache.delete(&sid2, &version, &key2))
        .await?;
    st.evict(&format!("{sid}/{}/ucell/{key}", s.version));
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found("UCell result"))
    }
}

/// Display values: XFD1 (f32 per cell, NaN = missing) or XGR1 (f32 per bin).
async fn result_values(
    State(st): State<Arc<AppState>>,
    AxPath((sid, key)): AxPath<(String, String)>,
    Query(q): Query<VersionQ>,
) -> ApiResult<Response> {
    let (s, m) = load_meta(&st, &sid, &key).await?;
    let cache = ucell(&st)?.cache.clone();
    let b = st
        .cached(format!("{sid}/{}/ucell/{key}", s.version), move || {
            let v = cache.values(&m)?;
            let col = FieldColumn::Numeric(v.scores.iter().map(|&x| x as f32).collect());
            Ok(match m.grid {
                None => encode_field(&col),
                Some(g) => encode_grid_raster(g.width, g.height, &col)?,
            })
        })
        .await?;
    Ok(binary(&s, &q.v, OCTET, b))
}

#[derive(Deserialize)]
struct SummaryQ {
    by: String,
}

async fn result_summary(
    State(st): State<Arc<AppState>>,
    AxPath((sid, key)): AxPath<(String, String)>,
    Query(q): Query<SummaryQ>,
) -> ApiResult<Json<Value>> {
    let (s, m) = load_meta(&st, &sid, &key).await?;
    if m.level != Level::Cell {
        return Err(bad("summaries are computed from cell-level results"));
    }
    let by = GroupBy::parse(&q.by).map_err(bad_any)?;
    let ids_ok = match &by {
        GroupBy::Field(f) => xenium_core::manifest::is_safe_id(f),
        GroupBy::Grid { grid, field } => {
            xenium_core::manifest::is_safe_id(grid) && xenium_core::manifest::is_safe_id(field)
        }
    };
    if !ids_ok {
        return Err(bad("invalid id"));
    }
    let (cache, store) = (ucell(&st)?.cache.clone(), s.store.clone());
    let out = st
        .blocking(move || {
            let v = cache.values(&m)?;
            summarize(&store, &v.scores, &by)
                .map_err(|e| xenium_index::store::StoreError::BadRequest(format!("{e:#}")).into())
        })
        .await?;
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct ExportQ {
    keys: String,
}

async fn export_parquet(
    State(st): State<Arc<AppState>>,
    AxPath(sid): AxPath<String>,
    Query(q): Query<ExportQ>,
) -> ApiResult<Response> {
    let keys: Vec<String> = q
        .keys
        .split(',')
        .filter(|k| !k.is_empty())
        .map(String::from)
        .collect();
    if keys.is_empty() || keys.len() > 64 || keys.iter().any(|k| !is_key(k)) {
        return Err(bad("keys must be 1-64 result keys"));
    }
    let mut metas = vec![];
    let mut s = None;
    for k in &keys {
        let (sec, m) = load_meta(&st, &sid, k).await?;
        s = Some(sec);
        metas.push(m);
    }
    let s = s.unwrap();
    let cells = metas[0].level == Level::Cell;
    if metas.iter().any(|m| (m.level == Level::Cell) != cells) {
        return Err(bad("export cell-level and bin-level results separately"));
    }
    let (cache, store, sid2) = (ucell(&st)?.cache.clone(), s.store.clone(), sid.clone());
    let _permit = st.export_limiter.acquire().await.map_err(|e| anyhow!(e))?;
    let bytes = st
        .blocking(move || {
            let results = metas
                .into_iter()
                .map(|m| cache.values(&m).map(|v| (m, v)))
                .collect::<anyhow::Result<Vec<_>>>()?;
            if cells {
                xenium_score::export::cells_parquet(&store, &results)
            } else {
                xenium_score::export::bins_parquet(&sid2, &results).map_err(|e| {
                    xenium_index::store::StoreError::BadRequest(format!("{e:#}")).into()
                })
            }
        })
        .await?;
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apache.parquet"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let name = format!(
        "attachment; filename=\"{sid}_ucell_{}.parquet\"",
        if cells { "cells" } else { "bins" }
    );
    if let Ok(v) = HeaderValue::from_str(&name) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok((h, bytes).into_response())
}
