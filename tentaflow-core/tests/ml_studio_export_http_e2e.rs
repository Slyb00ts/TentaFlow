// ===== File: tests/ml_studio_export_http_e2e.rs — signed-URL export download e2e =====
//
// Full HTTP roundtrip over loopback for `GET /ml-studio/exports/<ref>`. We spin
// up a thin hyper http1 server backed by the **real** pure handlers
// (`api::ml_studio_export::handle_ml_studio_export_url` + `read_export_file`)
// and the same range/stream logic the dashboard server uses, drive it with
// `reqwest`, and assert wire status + headers + body + the `audit_log` chain.
//
// `TENTAFLOW_CACHE_DIR` is pinned to a tempdir for the whole binary so
// `paths::ml_studio_exports_dir()` — the containment base — is under our
// control. Tests share that base and use distinct refs, so they stay parallel-safe.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use tentaflow_core::api::ml_studio_export::{
    export_archive_path, handle_ml_studio_export_url, parse_query, read_export_file,
    ExportFileOutcome, ExportOutcome, RequestContext,
};
use tentaflow_core::db::DbPool;
use tentaflow_core::services::signed_urls::{SignedUrl, SignedUrlIssuer, UrlScope};

const ISSUER_KEY: [u8; 32] = [99u8; 32];
const AUDIT_ACTION: &str = "ml_studio_export_url_access";

// -----------------------------------------------------------------------------
// Test harness
// -----------------------------------------------------------------------------

/// Pins `TENTAFLOW_CACHE_DIR` once per test binary. The tempdir is intentionally
/// leaked: `paths::ml_studio_exports_dir()` reads the env var lazily on every
/// call, so the directory must outlive every test in the process.
fn exports_base() -> &'static PathBuf {
    static BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BASE.get_or_init(|| {
        let td = Box::leak(Box::new(tempfile::TempDir::new().expect("cache tempdir")));
        std::env::set_var("TENTAFLOW_CACHE_DIR", td.path());
        let base = tentaflow_core::paths::ml_studio_exports_dir();
        std::fs::create_dir_all(&base).expect("create exports dir");
        base
    })
}

struct Env {
    addr: SocketAddr,
    db: DbPool,
    issuer: Arc<SignedUrlIssuer>,
}

fn make_db() -> DbPool {
    tentaflow_core::db::init(std::path::Path::new(":memory:")).expect("db init")
}

fn audit_log_count(db: &DbPool, result: &str) -> i64 {
    let conn = db.read().expect("db read");
    conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE action = ?1 AND result = ?2",
        rusqlite::params![AUDIT_ACTION, result],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Writes an archive under the exports base and returns its ref.
fn write_archive(uuid: &str, bytes: &[u8]) -> String {
    exports_base();
    let export_ref = format!("mlsexp_{uuid}");
    std::fs::write(export_archive_path(&export_ref), bytes).expect("write archive");
    export_ref
}

fn sign(issuer: &SignedUrlIssuer, export_ref: &str) -> SignedUrl {
    issuer
        .issue(export_ref.to_string(), 600)
        .expect("issue signed url")
}

async fn spawn_server() -> Env {
    exports_base();
    let db = make_db();
    let issuer = Arc::new(SignedUrlIssuer::new_for_tests(
        UrlScope::MlStudioExport,
        ISSUER_KEY,
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let db_loop = db.clone();
    let issuer_loop = issuer.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let db = db_loop.clone();
            let issuer = issuer_loop.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let db = db.clone();
                    let issuer = issuer.clone();
                    async move { Ok::<_, Infallible>(router(req, &db, &issuer).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(sock), svc)
                    .await;
            });
        }
    });

    Env { addr, db, issuer }
}

/// Mirrors the production route: same handlers, same range arithmetic. The body
/// is slurped here (the production server streams it) so byte assertions stay
/// exact; the status/header contract under test is identical.
async fn router(
    req: Request<hyper::body::Incoming>,
    db: &DbPool,
    issuer: &SignedUrlIssuer,
) -> Response<Full<Bytes>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query_string = uri.query().unwrap_or("").to_string();
    let range_header = req
        .headers()
        .get(hyper::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    drop(req);

    if method != Method::GET || !path.starts_with("/ml-studio/exports/") {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()))
            .unwrap();
    }

    let path_ref = path.strip_prefix("/ml-studio/exports/").unwrap_or("");
    let q = match parse_query(&query_string) {
        Ok(q) => q,
        Err(why) => {
            let body = format!("{{\"error\":\"{}\"}}", why);
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(body)))
                .unwrap();
        }
    };
    let ctx = RequestContext {
        source_ip: Some("127.0.0.1"),
        user_agent: None,
    };
    let outcome = handle_ml_studio_export_url(path_ref, &q, issuer, db, ctx);
    let auth_status = outcome.http_status();
    match outcome {
        ExportOutcome::Ok => {
            let file_outcome = read_export_file(db, path_ref, ctx).await;
            let status = file_outcome.http_status();
            match file_outcome {
                ExportFileOutcome::Ok { mut file, size } => {
                    let (code, start, length) = match parse_range(range_header.as_deref(), size) {
                        Some((s, e)) => (206u16, s, e - s + 1),
                        None => (200u16, 0, size),
                    };
                    use tokio::io::{AsyncReadExt, AsyncSeekExt};
                    if start > 0 {
                        file.seek(std::io::SeekFrom::Start(start))
                            .await
                            .expect("seek");
                    }
                    let mut bytes = vec![0u8; length as usize];
                    file.read_exact(&mut bytes).await.expect("read range");
                    let mut builder = Response::builder()
                        .status(code)
                        .header("Content-Type", "application/zip")
                        .header("Accept-Ranges", "bytes")
                        .header("Content-Length", length.to_string())
                        .header(
                            "Content-Disposition",
                            format!("attachment; filename=\"{path_ref}.zip\""),
                        );
                    if code == 206 {
                        builder = builder.header(
                            "Content-Range",
                            format!("bytes {}-{}/{}", start, start + length - 1, size),
                        );
                    }
                    builder.body(Full::new(Bytes::from(bytes))).unwrap()
                }
                _ => Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Full::new(Bytes::from_static(
                        b"{\"error\":\"export_unavailable\"}",
                    )))
                    .unwrap(),
            }
        }
        ExportOutcome::BadRequest(why) => {
            let body = format!("{{\"error\":\"{}\"}}", why);
            Response::builder()
                .status(auth_status)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(body)))
                .unwrap()
        }
        ExportOutcome::Denied(_) => Response::builder()
            .status(auth_status)
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from_static(
                b"{\"error\":\"export_denied\"}",
            )))
            .unwrap(),
    }
}

/// Byte-for-byte copy of the production `parse_byte_range` semantics (that
/// helper is private to the dashboard server module).
fn parse_range(raw: Option<&str>, size: u64) -> Option<(u64, u64)> {
    let spec = raw?.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (start, end) = if from.is_empty() {
        let n: u64 = to.parse().ok()?;
        if n == 0 {
            return None;
        }
        (size.saturating_sub(n), size.checked_sub(1)?)
    } else {
        let start: u64 = from.parse().ok()?;
        let end = if to.is_empty() {
            size.checked_sub(1)?
        } else {
            to.parse::<u64>().ok()?.min(size.checked_sub(1)?)
        };
        (start, end)
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

fn url(env: &Env, path_ref: &str, signed: &SignedUrl) -> String {
    format!(
        "http://{}/ml-studio/exports/{}?{}",
        env.addr,
        path_ref,
        signed.query_string()
    )
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[tokio::test]
async fn test_valid_token_downloads_whole_archive() {
    let env = spawn_server().await;
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let export_ref = write_archive("11111111-1111-1111-1111-111111111111", &payload);
    let signed = sign(&env.issuer, &export_ref);

    let resp = reqwest::get(url(&env, &export_ref, &signed))
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/zip"
    );
    assert_eq!(resp.headers().get("accept-ranges").unwrap(), "bytes");
    assert_eq!(
        resp.headers().get("content-disposition").unwrap(),
        &format!("attachment; filename=\"{export_ref}.zip\"")
    );
    let body = resp.bytes().await.expect("body");
    assert_eq!(body.as_ref(), payload.as_slice());
    assert!(audit_log_count(&env.db, "ok") >= 1);
}

#[tokio::test]
async fn test_tampered_token_is_denied() {
    let env = spawn_server().await;
    let export_ref = write_archive("22222222-2222-2222-2222-222222222222", b"zipzipzip");
    let mut signed = sign(&env.issuer, &export_ref);
    // Flip the last base64 char: valid encoding, wrong signature.
    let last = signed.token_b64.pop().unwrap();
    signed.token_b64.push(if last == 'A' { 'B' } else { 'A' });

    let resp = reqwest::get(url(&env, &export_ref, &signed))
        .await
        .expect("request");
    assert_eq!(resp.status(), 403);
    assert_eq!(audit_log_count(&env.db, "denied"), 1);
}

#[tokio::test]
async fn test_token_from_other_scope_is_denied() {
    // A token minted with the SAME key but the Recording scope literal must not
    // unlock an export — the scope is part of the HMAC payload.
    let env = spawn_server().await;
    let export_ref = write_archive("33333333-3333-3333-3333-333333333333", b"zipzipzip");
    let other = SignedUrlIssuer::new_for_tests(UrlScope::Recording, ISSUER_KEY);
    let signed = other
        .issue(export_ref.clone(), 600)
        .expect("issue under other scope");

    let resp = reqwest::get(url(&env, &export_ref, &signed))
        .await
        .expect("request");
    assert_eq!(resp.status(), 403);
    assert_eq!(audit_log_count(&env.db, "denied"), 1);
}

#[tokio::test]
async fn test_query_ref_mismatching_path_ref_is_rejected() {
    let env = spawn_server().await;
    let export_ref = write_archive("44444444-4444-4444-4444-444444444444", b"zipzipzip");
    let other_ref = write_archive("55555555-5555-5555-5555-555555555555", b"other");
    // Token is valid for `other_ref`, but the PATH asks for `export_ref`.
    let signed = sign(&env.issuer, &other_ref);

    let resp = reqwest::get(url(&env, &export_ref, &signed))
        .await
        .expect("request");
    assert_eq!(resp.status(), 400);
    let body = resp.text().await.expect("body");
    assert!(body.contains("ref_path_mismatch"), "got {body}");
    assert_eq!(audit_log_count(&env.db, "bad_request"), 1);
    // The mismatching request must NOT have served the other archive.
    assert_eq!(audit_log_count(&env.db, "ok"), 0);
}

#[tokio::test]
async fn test_path_traversal_ref_is_rejected() {
    let env = spawn_server().await;
    // Even with a validly signed token, a traversal ref fails the format gate
    // before any filesystem access happens.
    let evil = "mlsexp_../../../../etc/passwd";
    let signed = env
        .issuer
        .issue(evil.to_string(), 600)
        .expect("issue traversal ref");

    let resp = reqwest::Client::new()
        .get(format!(
            "http://{}/ml-studio/exports/{}?{}",
            env.addr,
            evil,
            signed.query_string()
        ))
        .send()
        .await
        .expect("request");
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "traversal must never be served, got {}",
        resp.status()
    );
    assert_eq!(audit_log_count(&env.db, "ok"), 0);
}

#[tokio::test]
async fn test_symlink_archive_is_rejected() {
    #[cfg(unix)]
    {
        let env = spawn_server().await;
        // Real secret outside the exports base; the archive slot is a symlink
        // pointing at it. Must be refused BEFORE canonicalize resolves it.
        let outside = tempfile::TempDir::new().expect("outside dir");
        let secret = outside.path().join("secret.bin");
        std::fs::write(&secret, b"TOP-SECRET").expect("write secret");

        let export_ref = "mlsexp_66666666-6666-6666-6666-666666666666".to_string();
        exports_base();
        std::os::unix::fs::symlink(&secret, export_archive_path(&export_ref))
            .expect("symlink archive slot");
        let signed = sign(&env.issuer, &export_ref);

        let resp = reqwest::get(url(&env, &export_ref, &signed))
            .await
            .expect("request");
        assert_eq!(resp.status(), 403, "symlinked archive must be refused");
        let body = resp.bytes().await.expect("body");
        assert!(!body.as_ref().windows(10).any(|w| w == b"TOP-SECRET"));
        assert_eq!(audit_log_count(&env.db, "denied"), 1);
    }
}

#[tokio::test]
async fn test_missing_archive_is_404() {
    let env = spawn_server().await;
    let export_ref = "mlsexp_77777777-7777-7777-7777-777777777777";
    let signed = sign(&env.issuer, export_ref);

    let resp = reqwest::get(url(&env, export_ref, &signed))
        .await
        .expect("request");
    assert_eq!(resp.status(), 404);
    assert_eq!(audit_log_count(&env.db, "not_found"), 1);
}

#[tokio::test]
async fn test_range_request_returns_206_slice() {
    let env = spawn_server().await;
    let payload: Vec<u8> = (0..1000u32).map(|i| (i % 256) as u8).collect();
    let export_ref = write_archive("88888888-8888-8888-8888-888888888888", &payload);
    let signed = sign(&env.issuer, &export_ref);

    let resp = reqwest::Client::new()
        .get(url(&env, &export_ref, &signed))
        .header("Range", "bytes=100-199")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.headers().get("content-length").unwrap(), "100");
    assert_eq!(
        resp.headers().get("content-range").unwrap(),
        "bytes 100-199/1000"
    );
    let body = resp.bytes().await.expect("body");
    assert_eq!(body.as_ref(), &payload[100..200]);
}

#[tokio::test]
async fn test_open_ended_and_suffix_ranges() {
    let env = spawn_server().await;
    let payload: Vec<u8> = (0..500u32).map(|i| (i % 256) as u8).collect();
    let export_ref = write_archive("99999999-9999-9999-9999-999999999999", &payload);
    let signed = sign(&env.issuer, &export_ref);
    let client = reqwest::Client::new();

    // Open-ended `bytes=450-` — the resume case for an interrupted download.
    let resp = client
        .get(url(&env, &export_ref, &signed))
        .header("Range", "bytes=450-")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 206);
    assert_eq!(
        resp.headers().get("content-range").unwrap(),
        "bytes 450-499/500"
    );
    assert_eq!(resp.bytes().await.expect("body").as_ref(), &payload[450..]);

    // Suffix `bytes=-50` — the LAST 50 bytes.
    let resp = client
        .get(url(&env, &export_ref, &signed))
        .header("Range", "bytes=-50")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 206);
    assert_eq!(
        resp.headers().get("content-range").unwrap(),
        "bytes 450-499/500"
    );
    assert_eq!(resp.bytes().await.expect("body").as_ref(), &payload[450..]);
}

#[tokio::test]
async fn test_unsatisfiable_range_serves_full_body() {
    let env = spawn_server().await;
    let payload = vec![7u8; 128];
    let export_ref = write_archive("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", &payload);
    let signed = sign(&env.issuer, &export_ref);

    let resp = reqwest::Client::new()
        .get(url(&env, &export_ref, &signed))
        .header("Range", "bytes=9999-")
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.expect("body").len(), 128);
}

#[tokio::test]
async fn test_missing_query_params_are_bad_request() {
    let env = spawn_server().await;
    let export_ref = write_archive("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", b"zip");
    let base = format!("http://{}/ml-studio/exports/{}", env.addr, export_ref);

    for (q, expect) in [
        ("", "missing_token"),
        ("token=abc", "missing_exp"),
        ("token=abc&exp=1", "missing_ref"),
    ] {
        let resp = reqwest::get(format!("{base}?{q}")).await.expect("request");
        assert_eq!(resp.status(), 400, "query {q:?}");
        let body = resp.text().await.expect("body");
        assert!(body.contains(expect), "query {q:?} → {body}");
    }
}

#[tokio::test]
async fn test_unknown_query_key_is_rejected() {
    let env = spawn_server().await;
    let export_ref = write_archive("cccccccc-cccc-cccc-cccc-cccccccccccc", b"zip");
    let signed = sign(&env.issuer, &export_ref);

    let resp = reqwest::get(format!(
        "http://{}/ml-studio/exports/{}?{}&surprise=1",
        env.addr,
        export_ref,
        signed.query_string()
    ))
    .await
    .expect("request");
    assert_eq!(resp.status(), 400);
    assert!(resp
        .text()
        .await
        .expect("body")
        .contains("unknown_query_key"));
}

#[tokio::test]
async fn test_token_is_multi_use_within_ttl() {
    let env = spawn_server().await;
    let payload = vec![3u8; 64];
    let export_ref = write_archive("dddddddd-dddd-dddd-dddd-dddddddddddd", &payload);
    let signed = sign(&env.issuer, &export_ref);

    for _ in 0..3 {
        let resp = reqwest::get(url(&env, &export_ref, &signed))
            .await
            .expect("request");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.bytes().await.expect("body").len(), 64);
    }
    assert!(audit_log_count(&env.db, "ok") >= 3);
}
