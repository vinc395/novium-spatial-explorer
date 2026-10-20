//! HTTP API tests against a synthetic index (no network: tower `oneshot`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use xenium_core::codec::{
    decode_centroids, decode_density, decode_field, decode_points, decode_polygons, FieldColumn,
};
use xenium_core::manifest::Layout;
use xenium_index::{ingest, ImageInput, IngestConfig, RegistrationInput};
use xenium_io::synth::{generate, SynthOptions};
use xenium_server::{router, AppState, ServerConfig};

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    root: std::path::PathBuf,
    xenium: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 20,
            rows: 12,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tmp.path().join("index");
    for id in ["A", "B"] {
        let mut cfg = IngestConfig::new(id, &out.xenium_dir, &root);
        if id == "A" {
            cfg.annotations.push(out.annotations_csv.clone());
        }
        ingest(&cfg).unwrap();
    }
    std::fs::write(tmp.path().join("secret.txt"), "do not serve").unwrap();
    let state = AppState::new(&ServerConfig {
        index_root: root.clone(),
        static_dir: None,
        cache_bytes: 1 << 20,
        max_blocking: 4,
        state_dir: Some(tmp.path().join("state")),
        score_threads: 2,
        sample_info: None,
    })
    .unwrap();
    Fixture {
        app: router(state, None),
        root,
        xenium: out.xenium_dir,
        _tmp: tmp,
    }
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let r = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    (
        status,
        headers,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn json(b: &[u8]) -> serde_json::Value {
    serde_json::from_slice(b).unwrap()
}

#[tokio::test]
async fn api_end_to_end() {
    let f = fixture();
    let app = &f.app;
    let (s, _, b) = get(app, "/healthz").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json(&b)["status"], "ok");

    let (_, _, b) = get(app, "/api/v1/sections").await;
    let secs = json(&b);
    assert_eq!(secs["sections"].as_array().unwrap().len(), 2);
    let version = secs["sections"][0]["version"].as_str().unwrap().to_string();

    let (s, h, b) = get(app, "/api/v1/sections/A/manifest").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["cache-control"], "no-cache");
    let m = json(&b);
    let n = m["cells"]["count"].as_u64().unwrap() as usize;

    // Versioned URLs are immutable; unversioned are not cached.
    let (_, h, b) = get(
        app,
        &format!("/api/v1/sections/A/cells/centroids?v={version}"),
    )
    .await;
    assert!(h["cache-control"].to_str().unwrap().contains("immutable"));
    assert_eq!(decode_centroids(&b).unwrap().len(), 2 * n);
    let (_, h, _) = get(app, "/api/v1/sections/A/cells/centroids?v=stale").await;
    assert_eq!(h["cache-control"], "no-store");

    let (s, _, b) = get(app, "/api/v1/sections/A/cells/fields/fine_cell_type").await;
    assert_eq!(s, StatusCode::OK);
    assert!(matches!(
        decode_field(&b).unwrap(),
        FieldColumn::Categorical { .. }
    ));
    // Section isolation: B was ingested without annotations.
    let (s, _, _) = get(app, "/api/v1/sections/B/cells/fields/fine_cell_type").await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let ch = &m["cells"]["boundaries"]["chunks"][0];
    let (s, _, b) = get(
        app,
        &format!("/api/v1/sections/A/cells/chunks/cell/{}/{}", ch[0], ch[1]),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        decode_polygons(&b).unwrap().1.len() as u64,
        ch[2].as_u64().unwrap()
    );
    let (_, _, b) = get(app, "/api/v1/sections/A/cells/chunks/nucleus/900/900").await;
    assert_eq!(decode_polygons(&b).unwrap().1.len(), 0);

    let genes = m["transcripts"]["genes"].as_array().unwrap();
    let (s, _, b) = get(
        app,
        &format!(
            "/api/v1/sections/A/transcripts/tiles/{}/{}?genes=0,1,2&qv_min=20",
            ch[0], ch[1]
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let p = decode_points(&b).unwrap();
    assert!(p.records.iter().all(|r| r.qv() >= 20.0));
    assert!(p.segments.iter().all(|sg| sg.gene_id <= 2));
    let mut total = 0u64;
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let (_, _, b) = get(
            app,
            &format!("/api/v1/sections/A/transcripts/density/0/1/{x}/{y}"),
        )
        .await;
        total += decode_density(&b).unwrap().total as u64;
    }
    assert_eq!(total, genes[0]["count"].as_u64().unwrap());

    let (_, _, b) = get(app, "/api/v1/sections/A/expression/0").await;
    match decode_field(&b).unwrap() {
        FieldColumn::Numeric(v) => assert_eq!(v.len(), n),
        _ => panic!(),
    }
    let (s, _, b) = get(app, "/api/v1/sections/A/cells/key/1").await;
    assert_eq!(s, StatusCode::OK);
    let cell = json(&b);
    let id = cell["cell_id"].as_str().unwrap().to_string();
    // Field values by group: `groups` keeps only those groups, `exclude_groups` drops them.
    let (_, _, m) = get(app, "/api/v1/sections/A/manifest").await;
    let group_of: std::collections::HashMap<String, String> = json(&m)["cells"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["id"].as_str().unwrap().into(),
                f["group"].as_str().unwrap().into(),
            )
        })
        .collect();
    let groups_in = |c: &serde_json::Value| -> std::collections::BTreeSet<String> {
        c["fields"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| group_of[k].clone())
            .collect()
    };
    let all = groups_in(&cell);
    assert!(all.contains("cells") && all.len() > 1, "{all:?}");
    assert_eq!(cell["fields"].as_object().unwrap().len(), group_of.len());
    let (_, _, b) = get(app, "/api/v1/sections/A/cells/key/1?groups=cells").await;
    assert_eq!(groups_in(&json(&b)), ["cells".to_string()].into());
    assert_eq!(json(&b)["cell_id"], id.as_str());
    let (_, _, b) = get(
        app,
        "/api/v1/sections/A/cells/key/1?exclude_groups=cells,nope",
    )
    .await;
    let rest = groups_in(&json(&b));
    assert!(
        !rest.contains("cells") && rest.len() == all.len() - 1,
        "{rest:?}"
    );
    let (_, _, b) = get(app, "/api/v1/sections/A/cells/key/1?groups=").await;
    assert!(json(&b)["fields"].as_object().unwrap().is_empty());
    let (_, _, b) = get(app, &format!("/api/v1/sections/A/cells/lookup?id={id}")).await;
    assert_eq!(json(&b)["cell_key"], 1);

    let req = Request::post("/api/v1/sections/A/selection/export")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"cell_keys":[1,2],"fields":["fine_cell_type","cell_area"]}"#,
        ))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let csv =
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(csv.starts_with("section_id,cell_id,x_um,y_um,fine_cell_type,cell_area\n"));
    assert!(csv.contains(&format!("A,{id},")));
    assert_eq!(csv.lines().count(), 3);
}

#[tokio::test]
async fn rejects_bad_input_and_traversal() {
    let f = fixture();
    let app = &f.app;
    for (uri, want) in [
        (
            "/api/v1/sections/..%2F..%2Fsecret.txt/manifest",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/.versions/manifest",
            StatusCode::BAD_REQUEST,
        ),
        ("/api/v1/sections/NOPE/manifest", StatusCode::NOT_FOUND),
        (
            "/api/v1/sections/A/cells/fields/..%2Fids",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/cells/fields/not_a_field",
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/v1/sections/A/cells/chunks/weird/0/0",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/transcripts/tiles/0/0?genes=abc",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/transcripts/tiles/0/0?genes=99999",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/transcripts/tiles/0/0?genes=0&qv_min=-1",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/transcripts/density/0/40/0/0",
            StatusCode::BAD_REQUEST,
        ),
        ("/api/v1/sections/A/cells/key/0", StatusCode::NOT_FOUND),
        ("/api/v1/sections/A/cells/key/999999", StatusCode::NOT_FOUND),
        ("/api/v1/sections/A/images/he/0/0/0", StatusCode::NOT_FOUND),
        ("/api/v1/nope", StatusCode::NOT_FOUND),
        // Out-of-range tile indices must not alias tile (0, 0).
        (
            "/api/v1/sections/A/transcripts/tiles/65536/0?genes=0",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/cells/chunks/cell/131072/65536",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/sections/A/transcripts/density/0/1/-2147483648/0",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (s, _, b) = get(app, uri).await;
        assert_eq!(s, want, "{uri}: {}", String::from_utf8_lossy(&b));
        assert!(!String::from_utf8_lossy(&b).contains("do not serve"));
    }
    let uniq: Vec<String> = (0..65).map(|i| i.to_string()).collect();
    let (s, _, _) = get(
        app,
        &format!(
            "/api/v1/sections/A/transcripts/tiles/0/0?genes={}",
            uniq.join(",")
        ),
    )
    .await;
    assert!(s == StatusCode::PAYLOAD_TOO_LARGE || s == StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn republish_is_picked_up_and_corrupt_index_is_503() {
    let f = fixture();
    let app = &f.app;
    let (_, _, b) = get(app, "/api/v1/sections").await;
    let v1 = json(&b)["sections"][0]["version"]
        .as_str()
        .unwrap()
        .to_string();
    let cfg = IngestConfig::new("A", &f.xenium, &f.root);
    ingest(&cfg).unwrap();
    let (_, h, _) = get(app, "/api/v1/sections/A/manifest").await;
    assert_ne!(h["x-index-version"].to_str().unwrap(), v1);
    // Corrupt B's live manifest: the API must fail closed (503), not serve garbage.
    let live = std::fs::canonicalize(f.root.join("B")).unwrap();
    std::fs::write(live.join("manifest.json"), b"{not json").unwrap();
    let cfg = IngestConfig::new("B", &f.xenium, &f.root);
    let _ = cfg; // B stays corrupt on purpose
                 // Force a reopen by bumping the target: create a fresh state.
    let state = AppState::new(&ServerConfig {
        index_root: f.root.clone(),
        static_dir: None,
        cache_bytes: 1 << 20,
        max_blocking: 2,
        state_dir: Some(f._tmp.path().join("state2")),
        score_threads: 1,
        sample_info: None,
    })
    .unwrap();
    let app2 = router(state, None);
    let (s, _, _) = get(&app2, "/api/v1/sections/B/manifest").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn access_token_gates_everything_but_healthz() {
    let f = fixture();
    let token = "a".repeat(64);
    let app = xenium_server::with_auth(
        f.app.clone(),
        xenium_server::Auth::new(token.clone(), true).unwrap(),
    );
    let (s, _, _) = get(&app, "/healthz").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, b) = get(&app, "/api/v1/sections").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(!String::from_utf8_lossy(&b).contains("section_id"));
    let (s, _, _) = get(&app, &format!("/?token={}", "b".repeat(64))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Valid link: cookie + redirect that drops only the token parameter.
    let (s, h, _) = get(&app, &format!("/?section=A&token={token}&genes=X")).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h["location"], "/?section=A&genes=X");
    let cookie = h["set-cookie"].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("Secure")
            && cookie.contains("SameSite=Strict")
    );
    let req = |hdr: (&str, String)| {
        Request::get("/api/v1/sections")
            .header(hdr.0, hdr.1)
            .body(Body::empty())
            .unwrap()
    };
    for hdr in [
        ("cookie", format!("other=1; xw_token={token}")),
        ("authorization", format!("Bearer {token}")),
    ] {
        let r = app.clone().oneshot(req(hdr)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
    let r = app
        .clone()
        .oneshot(req(("cookie", format!("xw_token={}", "a".repeat(63)))))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert!(xenium_server::Auth::new("short".into(), true).is_err());
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let s = r.status();
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (
        s,
        if b.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&b).unwrap()
        },
    )
}

async fn wait_job(app: &axum::Router, job: &serde_json::Value) -> serde_json::Value {
    let id = job["id"].as_str().unwrap().to_string();
    for _ in 0..500 {
        let (_, _, b) = get(app, &format!("/api/v1/ucell/jobs/{id}")).await;
        let j = json(&b);
        if !matches!(j["status"].as_str(), Some("queued" | "running")) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ucell_signatures_jobs_results_summary_export() {
    let f = fixture();
    let app = &f.app;
    let (s, _, b) = get(app, "/api/v1/ucell/signatures").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(json(&b)["signatures"].as_array().unwrap().len(), 0);

    // Create / validate / edit signatures.
    let (s, myeloid) = send(
        app,
        "POST",
        "/api/v1/ucell/signatures",
        serde_json::json!({"name": "Myeloid", "genes": ["CD68", "CXCL8", "IL1B", "CD3E-"]}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let mid = myeloid["id"].as_str().unwrap().to_string();
    let (s, _) = send(
        app,
        "POST",
        "/api/v1/ucell/signatures",
        serde_json::json!({"name": "Myeloid", "genes": ["CD68"]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "duplicate name");
    let (s, _) = send(
        app,
        "POST",
        "/api/v1/ucell/signatures",
        serde_json::json!({"name": "x", "genes": ["CD68 CD3E"]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "gene with whitespace");
    let (s, t) = send(
        app,
        "POST",
        "/api/v1/ucell/signatures",
        serde_json::json!({"name": "T", "genes": ["CD3E", "CD8A", "IL7R", "NOT_A_GENE"]}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let tid = t["id"].as_str().unwrap().to_string();
    let (s, t2) = send(
        app,
        "PUT",
        &format!("/api/v1/ucell/signatures/{tid}"),
        serde_json::json!({"name": "T cells", "genes": ["CD3E", "CD8A", "IL7R", "NOT_A_GENE"]}),
    )
    .await;
    assert_eq!((s, t2["name"].as_str()), (StatusCode::OK, Some("T cells")));

    // Coverage against section A's panel.
    let (s, c) = send(
        app,
        "POST",
        "/api/v1/sections/A/ucell/coverage",
        serde_json::json!({"genes": ["CD3E", "NOT_A_GENE", "cd68", "NegControlProbe_00001"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["positive_found"], 1);
    assert_eq!(c["genes"][2]["suggestion"], "CD68");
    assert_eq!(c["genes"][3]["status"], "not_a_gene");

    // Bad parameters are refused before anything is queued.
    let (s, e) = send(app, "POST", "/api/v1/sections/A/ucell/jobs", serde_json::json!({"signature_ids": [mid], "params": {"max_rank": 2, "w_neg": 1, "missing_genes": "impute"}})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(e["error"].as_str().unwrap().contains("maxRank"));

    // Cell-level job for both signatures.
    let (s, job) = send(
        app,
        "POST",
        "/api/v1/sections/A/ucell/jobs",
        serde_json::json!({"signature_ids": [mid, tid]}),
    )
    .await;
    assert!(s == StatusCode::ACCEPTED || s == StatusCode::OK, "{s}");
    let job = wait_job(app, &job).await;
    assert_eq!(job["status"], "done", "{job}");
    let keys: Vec<String> = job["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    assert_eq!(job["results"][1]["coverage"]["imputed"], 1);
    // Re-submitting is answered from the cache.
    let (s, again) = send(
        app,
        "POST",
        "/api/v1/sections/A/ucell/jobs",
        serde_json::json!({"signature_ids": [mid, tid]}),
    )
    .await;
    assert_eq!(
        (s, again["status"].as_str()),
        (StatusCode::OK, Some("done"))
    );

    let (_, _, b) = get(app, "/api/v1/sections/A/ucell/results").await;
    assert_eq!(json(&b)["results"].as_array().unwrap().len(), 2);
    let (s, _, b) = get(
        app,
        &format!("/api/v1/sections/A/ucell/results/{}/values", keys[0]),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let FieldColumn::Numeric(v) = decode_field(&b).unwrap() else {
        panic!()
    };
    let (_, _, mb) = get(app, "/api/v1/sections/A/manifest").await;
    assert_eq!(
        v.len() as u64,
        json(&mb)["cells"]["count"].as_u64().unwrap()
    );
    assert!(v.iter().all(|x| (0.0..=1.0).contains(x)));
    // Not computed for section B.
    let (s, _, _) = get(
        app,
        &format!("/api/v1/sections/B/ucell/results/{}/values", keys[0]),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Per cell-type summary.
    let (s, _, b) = get(
        app,
        &format!(
            "/api/v1/sections/A/ucell/results/{}/summary?by=field:parent_cell_type",
            keys[0]
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let sm = json(&b);
    assert!(sm["groups"].as_array().unwrap().len() >= 4);
    let (s, _, _) = get(
        app,
        &format!(
            "/api/v1/sections/A/ucell/results/{}/summary?by=field:cell_area",
            keys[0]
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "numeric field cannot group");

    // Parquet export.
    let (s, h, b) = get(
        app,
        &format!(
            "/api/v1/sections/A/ucell/export.parquet?keys={},{}",
            keys[0], keys[1]
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "application/vnd.apache.parquet");
    assert_eq!(&b[..4], b"PAR1");

    // Bin-level job: a raster, kept apart from the cell-level results.
    let (_, job) = send(app, "POST", "/api/v1/sections/A/ucell/jobs", serde_json::json!({"signature_ids": [mid], "level": "bin", "bin_um": 50, "min_transcripts": 3})).await;
    let job = wait_job(app, &job).await;
    assert_eq!(job["status"], "done", "{job}");
    let bkey = job["keys"][0].as_str().unwrap();
    assert_ne!(bkey, keys[0]);
    let (_, _, b) = get(
        app,
        &format!("/api/v1/sections/A/ucell/results/{bkey}/values"),
    )
    .await;
    let (w, hgt, _) = xenium_core::codec::decode_grid_raster(&b).unwrap();
    let g = &job["results"][0]["grid"];
    assert_eq!(
        (w as u64, hgt as u64),
        (g["width"].as_u64().unwrap(), g["height"].as_u64().unwrap())
    );
    let (s, _, _) = get(
        app,
        &format!("/api/v1/sections/A/ucell/results/{bkey}/summary?by=field:parent_cell_type"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "summaries are cell-level only");
    let (s, _, _) = get(
        app,
        &format!(
            "/api/v1/sections/A/ucell/export.parquet?keys={},{bkey}",
            keys[0]
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "cell and bin results are exported separately"
    );

    // Deleting the bin result: gone from this section, 404 afterwards.
    let (s, _) = send(
        app,
        "DELETE",
        &format!("/api/v1/sections/A/ucell/results/{bkey}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = get(
        app,
        &format!("/api/v1/sections/A/ucell/results/{bkey}/values"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = send(
        app,
        "DELETE",
        &format!("/api/v1/sections/A/ucell/results/{bkey}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, _, b) = get(app, "/api/v1/sections/A/ucell/results").await;
    assert_eq!(
        json(&b)["results"].as_array().unwrap().len(),
        2,
        "cell-level results remain"
    );

    // Cancel is accepted for any job id; unknown ids are 404.
    let (s, _) = send(
        app,
        "DELETE",
        &format!("/api/v1/ucell/jobs/{}", job["id"].as_str().unwrap()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        app,
        "DELETE",
        "/api/v1/ucell/jobs/job_nope",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, _) = send(
        app,
        "DELETE",
        &format!("/api/v1/ucell/signatures/{mid}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = get(app, &format!("/api/v1/ucell/signatures/{mid}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = get(app, "/api/v1/ucell/signatures/../../etc").await;
    assert!(s == StatusCode::BAD_REQUEST || s == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ucell_reports_unavailable_state_dir() {
    let f = fixture();
    let blocker = f._tmp.path().join("not_a_dir");
    std::fs::write(&blocker, "x").unwrap();
    let state = AppState::new(&ServerConfig {
        index_root: f.root.clone(),
        static_dir: None,
        cache_bytes: 1 << 20,
        max_blocking: 4,
        state_dir: Some(blocker.join("state")),
        score_threads: 1,
        sample_info: None,
    })
    .unwrap();
    let app = router(state, None);
    let (s, _, b) = get(&app, "/api/v1/ucell/signatures").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert!(json(&b)["error"]
        .as_str()
        .unwrap()
        .contains("UCell scoring is unavailable"));
    // The rest of the API is unaffected.
    let (s, _, _) = get(&app, "/api/v1/sections").await;
    assert_eq!(s, StatusCode::OK);
}

async fn post_form(app: &axum::Router, body: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let r = app
        .clone()
        .oneshot(
            Request::post("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (s, h) = (r.status(), r.headers().clone());
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (s, h, String::from_utf8_lossy(&b).into_owned())
}

#[tokio::test]
async fn password_sign_in_with_lockout() {
    let f = fixture();
    let token = "a".repeat(64);
    let auth = xenium_server::Auth::new(token.clone(), true)
        .unwrap()
        .with_password("ARBITRATE")
        .unwrap();
    let app = xenium_server::with_auth(f.app.clone(), auth);
    // The sign-in page asks for the password and posts it (never in the URL).
    let (s, _, b) = get(&app, "/?section=A").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let page = String::from_utf8_lossy(&b).into_owned();
    assert!(page.contains("method=\"post\" action=\"/login\"") && page.contains("Password"));
    assert!(
        page.contains("value=\"/?section=A\""),
        "returns to the requested page"
    );
    // Correct password: session cookie and redirect to the requested page.
    let (s, h, _) = post_form(&app, "secret=ARBITRATE&next=%2F%3Fsection%3DA").await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h["location"], "/?section=A");
    let cookie = h["set-cookie"].to_str().unwrap();
    assert!(
        cookie.starts_with("xw_token=") && cookie.contains("HttpOnly") && cookie.contains("Secure")
    );
    let r = app
        .clone()
        .oneshot(
            Request::get("/api/v1/sections")
                .header("cookie", cookie.split(';').next().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // Redirects stay on this site.
    let (_, h, _) = post_form(&app, "secret=ARBITRATE&next=https%3A%2F%2Fevil.example").await;
    assert_eq!(h["location"], "/");
    let (_, h, _) = post_form(&app, "secret=ARBITRATE&next=%2F%2Fevil.example").await;
    assert_eq!(h["location"], "/");
    // The password is not a token: not accepted in links or as a bearer token.
    let (s, _, _) = get(&app, "/?token=ARBITRATE").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let r = app
        .clone()
        .oneshot(
            Request::get("/api/v1/sections")
                .header("authorization", "Bearer ARBITRATE")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    // Wrong passwords: an error on the page, then a lockout for this address.
    let (s, _, page) = post_form(&app, "secret=arbitrate&next=%2F").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(page.contains("Wrong password"));
    let mut last = StatusCode::OK;
    for _ in 0..12 {
        last = post_form(&app, "secret=guess&next=%2F").await.0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    let (s, _, page) = post_form(&app, "secret=ARBITRATE&next=%2F").await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "locked out even with the right password"
    );
    assert!(page.contains("Too many wrong attempts"));
    // Too-short passwords are refused at start-up.
    assert!(xenium_server::Auth::new(token, true)
        .unwrap()
        .with_password("abc")
        .is_err());
}

#[tokio::test]
async fn sign_out_and_session_info() {
    let f = fixture();
    // Without access control the viewer is told there is no session to sign out of.
    let (s, _, b) = get(&f.app, "/api/v1/session").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&b).unwrap(),
        serde_json::json!({"auth": false})
    );
    let token = "a".repeat(64);
    let auth = xenium_server::Auth::new(token.clone(), true)
        .unwrap()
        .with_password("ARBITRATE")
        .unwrap();
    let app = xenium_server::with_auth(f.app.clone(), auth);
    let with_cookie = |req: axum::http::request::Builder| {
        req.header("cookie", format!("xw_token={token}"))
            .body(Body::empty())
            .unwrap()
    };
    // The session endpoint is protected like every API call.
    let (s, _, _) = get(&app, "/api/v1/session").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let r = app
        .clone()
        .oneshot(with_cookie(Request::get("/api/v1/session")))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let b = r.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&b).unwrap(),
        serde_json::json!({"auth": true, "password": true, "user": null})
    );
    // Signing out expires the cookie and returns to the sign-in page.
    for req in [Request::post("/logout"), Request::get("/logout")] {
        let r = app.clone().oneshot(with_cookie(req)).await.unwrap();
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(r.headers()["location"], "/");
        let c = r.headers()["set-cookie"].to_str().unwrap();
        assert!(
            c.starts_with("xw_token=;")
                && c.contains("Max-Age=0")
                && c.contains("HttpOnly")
                && c.contains("Secure"),
            "{c}"
        );
    }
    // The sign-in page carries the logo.
    let (s, _, b) = get(&app, "/").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let page = String::from_utf8_lossy(&b);
    assert!(page.contains("<svg") && page.contains("aria-label=\"NOVIUM Spatial Explorer\""));
    assert!(page.contains("Password"));
}

async fn login_as(
    app: &axum::Router,
    body: &str,
    visitor: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let r = app
        .clone()
        .oneshot(
            Request::post("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cf-connecting-ip", visitor)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (s, h) = (r.status(), r.headers().clone());
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (s, h, String::from_utf8_lossy(&b).into_owned())
}

async fn status_with_cookie(app: &axum::Router, path: &str, cookie: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::get(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn people_sign_in_sessions_revocation_and_per_visitor_lockout() {
    let f = fixture();
    let list = f._tmp.path().join("people.tsv");
    let hash = |pw: &str| xenium_server::hash_password_rounds(pw, 1000).unwrap();
    std::fs::write(
        &list,
        format!(
            "# test list\nann@example.org\t{}\nbob@example.org\t{}\n",
            hash("ann-pass-1234"),
            hash("bob-pass-1234")
        ),
    )
    .unwrap();
    let auth = xenium_server::Auth::people(&list, false)
        .unwrap()
        .behind_cloudflare();
    assert_eq!(auth.people_count(), Some(2));
    let app = xenium_server::with_auth(f.app.clone(), auth);
    // The sign-in page asks for email and password; token links do not work here.
    let (s, h, b) = get(&app, "/?section=A").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let page = String::from_utf8_lossy(&b);
    assert!(page.contains("name=\"email\"") && page.contains("name=\"secret\""));
    assert!(h["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    let (s, _, _) = get(&app, &format!("/?token={}", "a".repeat(64))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Sign in (the email's case does not matter): a session cookie, Secure behind Cloudflare.
    let (s, h, _) = login_as(
        &app,
        "email=Ann%40Example.org&secret=ann-pass-1234&next=%2F%3Fsection%3DA",
        "198.51.100.7",
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(h["location"], "/?section=A");
    let c = h["set-cookie"].to_str().unwrap();
    assert!(c.contains("Secure") && c.contains("HttpOnly") && c.contains("SameSite=Strict"));
    let ann = c.split(';').next().unwrap().to_string();
    assert_eq!(
        status_with_cookie(&app, "/api/v1/sections", &ann).await,
        StatusCode::OK
    );
    let r = app
        .clone()
        .oneshot(
            Request::get("/api/v1/session")
                .header("cookie", &ann)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let b = r.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&b).unwrap(),
        serde_json::json!({"auth": true, "password": true, "user": "ann@example.org"})
    );
    // A wrong password and an email that is not on the list fail the same way.
    for body in [
        "email=ann%40example.org&secret=nope",
        "email=eve%40example.org&secret=ann-pass-1234",
    ] {
        let (s, _, page) = login_as(&app, body, "198.51.100.7").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(page.contains("Wrong email or password"));
    }
    // Lockouts are per visitor: one visitor's guesses do not lock out another.
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        last = login_as(&app, "email=bob%40example.org&secret=guess", "203.0.113.9")
            .await
            .0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    let (s, h, _) = login_as(
        &app,
        "email=bob%40example.org&secret=bob-pass-1234",
        "192.0.2.44",
    )
    .await;
    assert_eq!(
        s,
        StatusCode::SEE_OTHER,
        "another visitor can still sign in"
    );
    let bob = h["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    // Removing someone from the list ends their session at their next request.
    std::fs::write(
        &list,
        format!("bob@example.org\t{}\n", hash("bob-pass-1234")),
    )
    .unwrap();
    assert_eq!(
        status_with_cookie(&app, "/api/v1/sections", &ann).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_with_cookie(&app, "/api/v1/sections", &bob).await,
        StatusCode::OK
    );
    // Signing out ends the session on the server, not only in the browser.
    let r = app
        .clone()
        .oneshot(
            Request::post("/logout")
                .header("cookie", &bob)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        status_with_cookie(&app, "/api/v1/sections", &bob).await,
        StatusCode::UNAUTHORIZED
    );
    // Without --behind-cloudflare the visitor header is ignored (it could be forged).
    let app2 = xenium_server::with_auth(
        f.app.clone(),
        xenium_server::Auth::people(&list, false).unwrap(),
    );
    for _ in 0..11 {
        login_as(&app2, "email=bob%40example.org&secret=guess", "203.0.113.9").await;
    }
    let (s, _, _) = login_as(
        &app2,
        "email=bob%40example.org&secret=bob-pass-1234",
        "192.0.2.44",
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn sections_list_carries_only_configured_sample_columns() {
    let f = fixture();
    let meta = f._tmp.path().join("samples.csv");
    std::fs::write(&meta, "sample_id,patient,Age,HLA\nA,P001,54,DRB1*04\n").unwrap();
    let info = xenium_server::SampleInfo::load(
        &meta,
        "sample_id",
        &["patient".to_string(), "Age".to_string()],
    )
    .unwrap();
    let state = AppState::new(&ServerConfig {
        index_root: f.root.clone(),
        static_dir: None,
        cache_bytes: 1 << 20,
        max_blocking: 2,
        state_dir: Some(f._tmp.path().join("state3")),
        score_threads: 1,
        sample_info: Some(std::sync::Arc::new(info)),
    })
    .unwrap();
    let app = router(state, None);
    let (_, _, b) = get(&app, "/api/v1/sections").await;
    let secs = json(&b);
    let a = secs["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["section_id"] == "A")
        .unwrap()
        .clone();
    assert_eq!(
        a["sample"],
        serde_json::json!([["patient", "P001"], ["Age", "54"]])
    );
    assert!(
        !String::from_utf8_lossy(&b).contains("DRB1"),
        "unlisted columns are never served"
    );
    let b_sec = secs["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["section_id"] == "B")
        .unwrap()
        .clone();
    assert!(b_sec["sample"].is_null());
}

#[tokio::test]
async fn image_tiles_from_packed_and_per_file_indexes() {
    let tmp = tempfile::tempdir().unwrap();
    let out = generate(
        &tmp.path().join("synth"),
        &SynthOptions {
            cols: 12,
            rows: 8,
            ..Default::default()
        },
    )
    .unwrap();
    let root = tmp.path().join("index");
    for (id, layout) in [("P", Layout::Packed), ("F", Layout::Files)] {
        let mut cfg = IngestConfig::new(id, &out.xenium_dir, &root);
        cfg.images.push(ImageInput {
            id: "he".into(),
            label: "H&E".into(),
            path: out.he_tiff.clone(),
            registration: RegistrationInput::Json(out.registration_json.clone()),
            ifds: None,
        });
        cfg.layout = layout;
        ingest(&cfg).unwrap();
    }
    let state = AppState::new(&ServerConfig {
        index_root: root.clone(),
        static_dir: None,
        cache_bytes: 1 << 20,
        max_blocking: 4,
        state_dir: Some(tmp.path().join("state")),
        score_threads: 1,
        sample_info: None,
    })
    .unwrap();
    let app = router(state, None);
    let (_, _, m) = get(&app, "/api/v1/sections/P/manifest").await;
    assert_eq!(json(&m)["images"][0]["tile_layout"], "packed");
    let mut tiles = vec![];
    for id in ["P", "F"] {
        let (st, h, b) = get(&app, &format!("/api/v1/sections/{id}/images/he/0/0/0")).await;
        assert_eq!(st, StatusCode::OK, "{id}");
        assert_eq!(h["content-type"], "image/jpeg");
        assert!(b.starts_with(&[0xFF, 0xD8]), "{id}");
        tiles.push(b);
        let (st, _, _) = get(&app, &format!("/api/v1/sections/{id}/images/he/0/999/0")).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{id}");
    }
    assert_eq!(tiles[0], tiles[1]);
}
