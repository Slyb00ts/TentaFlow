// =============================================================================
// Plik: tests/macos_native_engines.rs
// Opis: Integracyjne testy two-phase deploy dla wszystkich silnikow
//       embedded / native / external dostepnych na macOS. Kazdy test pobiera
//       prawdziwy manifest z `services::manifest::registry()` i wywoluje
//       `services::deploy::deploy(...)` z in-memory SQLite, sprawdzajac, ze:
//         * Result jest Ok(DeployOutcome)
//         * w `services` powstal wiersz z odpowiednim engine_id
//         * w `model_registry` powstaly wiersze z service_id powiazanym
//         * w `deployments` powstal wiersz status=success
// =============================================================================

#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};

use tentaflow_core::db::DbPool;
use tentaflow_core::services::deploy::{deploy, DeployOutcome};
use tentaflow_core::services::manifest::{registry, ServiceManifest};
use tentaflow_core::services::ports::PortAllocator;
use tentaflow_core::services_repo::services::DeployMethod;

/// Otwiera czysta in-memory SQLite z pelna migracja, identycznie jak w lib-tests.
fn open_db() -> DbPool {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    tentaflow_core::db::migrations::run(&conn).unwrap();
    Arc::new(Mutex::new(conn))
}

/// Buduje PortAllocator z duzym zakresem testowym, niezaleznym dla kazdego testu.
fn make_ports(low: u16, high: u16) -> Arc<PortAllocator> {
    Arc::new(PortAllocator::new((low, high), Default::default()).unwrap())
}

/// Pobiera manifest z globalnego rejestru lub panikuje czytelnym komunikatem.
fn load_manifest(engine_id: &str) -> ServiceManifest {
    registry()
        .by_id(engine_id)
        .cloned()
        .unwrap_or_else(|| panic!("manifest '{}' not found in registry", engine_id))
}

/// Sprawdza po deploy:
///   - w `services` jest wiersz o oczekiwanym engine_id i id == outcome.handle.id
///   - w `model_registry` jest >=1 wiersz z service_id == outcome.handle.id
///   - w `deployments` jest >=1 wiersz status='success' dla tego engine_id
fn assert_deployed(db: &DbPool, outcome: &DeployOutcome, engine_id: &str) {
    let conn = db.lock().unwrap();
    let svc_id = outcome.endpoint.handle.id;

    let row_engine: String = conn
        .query_row(
            "SELECT engine_id FROM services WHERE id = ?1",
            rusqlite::params![svc_id],
            |r| r.get(0),
        )
        .expect("services row exists");
    assert_eq!(row_engine, engine_id, "engine_id mismatch in services row");

    let model_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_registry WHERE service_id = ?1",
            rusqlite::params![svc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        model_count >= 1,
        "expected at least 1 model_registry row for service_id={}",
        svc_id
    );

    let success_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM deployments WHERE engine_id = ?1 AND status = 'success'",
            rusqlite::params![engine_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        success_count >= 1,
        "expected >=1 success row in deployments for engine_id={}",
        engine_id
    );
}

// ----- Embedded — Apple TTS (zero downloadu, system voices) ----------------

#[tokio::test]
async fn deploy_apple_tts_zosia_pl() {
    let db = open_db();
    let ports = make_ports(45_900, 45_999);
    let manifest = load_manifest("apple-tts");
    let cfg = serde_json::json!({ "model_preset_id": "zosia-pl" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("apple-tts deploy succeeds");
    assert_deployed(&db, &outcome, "apple-tts");
}

// ----- Embedded LLM (MLX, llama.cpp) ---------------------------------------
//
// Embedded LLM strategie NIE laduja modelu w prepare(); commit po prostu
// dopisuje wiersz do `services`. Modele sa lazy-loaded dopiero przy
// pierwszym requescie. Dlatego test moze biec bez sieci i bez ignored.

#[tokio::test]
async fn deploy_mlx_qwen3_5_0_8b() {
    let db = open_db();
    let ports = make_ports(46_000, 46_099);
    let manifest = load_manifest("mlx");
    let cfg = serde_json::json!({ "model_preset_id": "qwen3-5-0-8b-mlx-4bit" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("mlx deploy succeeds");
    assert_deployed(&db, &outcome, "mlx");
}

#[tokio::test]
async fn deploy_llama_cpp_qwen3_5_0_8b_q4() {
    let db = open_db();
    let ports = make_ports(46_100, 46_199);
    let manifest = load_manifest("llama-cpp");
    let cfg = serde_json::json!({ "model_preset_id": "qwen3-5-0-8b-q4" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("llama-cpp deploy succeeds");
    assert_deployed(&db, &outcome, "llama-cpp");
}

// ----- Embedded STT --------------------------------------------------------

#[tokio::test]
async fn deploy_whisper_large_v3_turbo() {
    let db = open_db();
    let ports = make_ports(46_200, 46_299);
    let manifest = load_manifest("whisper");
    let cfg = serde_json::json!({ "model_preset_id": "whisper-large-v3-turbo" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("whisper deploy succeeds");
    assert_deployed(&db, &outcome, "whisper");
}

#[tokio::test]
async fn deploy_mlx_whisper_large_v3_turbo() {
    let db = open_db();
    let ports = make_ports(46_300, 46_399);
    let manifest = load_manifest("mlx-whisper");
    let cfg = serde_json::json!({ "model_preset_id": "whisper-large-v3-turbo-4bit" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("mlx-whisper deploy succeeds");
    assert_deployed(&db, &outcome, "mlx-whisper");
}

// ----- Embedded TTS (Kokoro / Sherpa) --------------------------------------

#[tokio::test]
async fn deploy_kokoro_v1_mlx_bf16() {
    let db = open_db();
    let ports = make_ports(46_400, 46_499);
    let manifest = load_manifest("kokoro");
    let cfg = serde_json::json!({ "model_preset_id": "kokoro-v1-mlx-bf16" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("kokoro deploy succeeds");
    assert_deployed(&db, &outcome, "kokoro");
}

#[tokio::test]
async fn deploy_sherpa_onnx_jarvis_pl() {
    let db = open_db();
    let ports = make_ports(46_500, 46_599);
    let manifest = load_manifest("sherpa-onnx");
    let cfg = serde_json::json!({ "model_preset_id": "vits-piper-pl_PL-jarvis_wg_glos-medium" });
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("sherpa-onnx deploy succeeds");
    assert_deployed(&db, &outcome, "sherpa-onnx");
}

// ----- Embedded vision (tract-onnx) ----------------------------------------
//
// Vision strategy faktycznie laduje model ONNX z embedowanych bytes do tract.
// To realne side effecty — model jest registrowany w globalnym `vision::register_engine`.
// Test moze przejsc lokalnie bez sieci, bo modele sa wkompilowane w binarke.
// Wyjatek: emonet — build.rs zglosil "pusty placeholder", wiec test go ignoruje.

#[tokio::test]
async fn deploy_vision_yolov8n_pose() {
    let db = open_db();
    let ports = make_ports(46_600, 46_699);
    let manifest = load_manifest("yolov8n-pose");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("yolov8n-pose deploy succeeds");
    assert_deployed(&db, &outcome, "yolov8n-pose");
}

#[tokio::test]
async fn deploy_vision_scrfd() {
    let db = open_db();
    let ports = make_ports(46_700, 46_799);
    let manifest = load_manifest("scrfd");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("scrfd deploy succeeds");
    assert_deployed(&db, &outcome, "scrfd");
}

#[tokio::test]
#[ignore = "emonet.onnx is a placeholder in repo (see build.rs warning)"]
async fn deploy_vision_emonet() {
    let db = open_db();
    let ports = make_ports(46_800, 46_899);
    let manifest = load_manifest("emonet");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("emonet deploy succeeds");
    assert_deployed(&db, &outcome, "emonet");
}

#[tokio::test]
async fn deploy_vision_mivolo() {
    let db = open_db();
    let ports = make_ports(46_900, 46_999);
    let manifest = load_manifest("mivolo");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("mivolo deploy succeeds");
    assert_deployed(&db, &outcome, "mivolo");
}

#[tokio::test]
async fn deploy_vision_movenet_lightning() {
    let db = open_db();
    let ports = make_ports(47_000, 47_099);
    let manifest = load_manifest("movenet-lightning");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("movenet-lightning deploy succeeds");
    assert_deployed(&db, &outcome, "movenet-lightning");
}

#[tokio::test]
async fn deploy_vision_yolov8_face() {
    let db = open_db();
    let ports = make_ports(47_100, 47_199);
    let manifest = load_manifest("yolov8-face");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("yolov8-face deploy succeeds");
    assert_deployed(&db, &outcome, "yolov8-face");
}

#[tokio::test]
async fn deploy_vision_hsemotion() {
    let db = open_db();
    let ports = make_ports(47_200, 47_299);
    let manifest = load_manifest("hsemotion");
    let cfg = serde_json::json!({});
    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("hsemotion deploy succeeds");
    assert_deployed(&db, &outcome, "hsemotion");
}

// ----- Native binary (stable-diffusion-cpp) --------------------------------

#[tokio::test]
#[ignore = "requires build.sh execution and binary spawn — env-heavy"]
async fn deploy_stable_diffusion_cpp() {
    let db = open_db();
    let ports = make_ports(47_300, 47_399);
    let manifest = load_manifest("stable-diffusion-cpp");
    let cfg = serde_json::json!({ "model_preset_id": "sd-1-5-gguf" });
    let outcome = deploy(
        DeployMethod::NativeBinary,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("stable-diffusion-cpp deploy succeeds");
    assert_deployed(&db, &outcome, "stable-diffusion-cpp");
}

// ----- External (ollama) ---------------------------------------------------

#[tokio::test]
#[ignore = "requires running ollama daemon at localhost:11434"]
async fn deploy_ollama_external() {
    let db = open_db();
    let ports = make_ports(47_400, 47_499);
    let manifest = load_manifest("ollama");
    let cfg = serde_json::json!({ "model_preset_id": "qwen3-5-0-8b" });
    let outcome = deploy(
        DeployMethod::External,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("ollama deploy succeeds");
    assert_deployed(&db, &outcome, "ollama");
}

// ----- Native python-bundle ------------------------------------------------
//
// Python-bundle deploye sa zignorowane domyslnie bo wymagaja:
//   - dostepu do `python3` w PATH
//   - wykonania `pip install` (sieci)
//   - spawn procesu serwera + readiness probe
// Test sluzy do recznego uruchomienia ("cargo test -- --ignored") na
// developerskim hostu z odpowiednim env. Sprawdza ze caly pipeline deploy
// nie panikuje na przygotowaniu argumentow / sciezek.

#[tokio::test]
#[ignore = "python-bundle: needs python3, pip install, process spawn"]
async fn deploy_vllm_metal() {
    let db = open_db();
    let ports = make_ports(47_500, 47_599);
    let manifest = load_manifest("vllm-metal");
    let cfg = serde_json::json!({ "model_preset_id": "qwen3-5-0-8b-mlx-4bit" });
    let outcome = deploy(
        DeployMethod::NativePythonBundle,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("vllm-metal deploy succeeds");
    assert_deployed(&db, &outcome, "vllm-metal");
}

#[tokio::test]
#[ignore = "python-bundle: needs python3, pip install, process spawn"]
async fn deploy_chatterbox_mlx() {
    let db = open_db();
    let ports = make_ports(47_600, 47_699);
    let manifest = load_manifest("chatterbox-mlx");
    let cfg = serde_json::json!({ "model_preset_id": "chatterbox-turbo-4bit" });
    let outcome = deploy(
        DeployMethod::NativePythonBundle,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("chatterbox-mlx deploy succeeds");
    assert_deployed(&db, &outcome, "chatterbox-mlx");
}

#[tokio::test]
#[ignore = "python-bundle: needs python3, pip install, process spawn"]
async fn deploy_kyutai_tts() {
    let db = open_db();
    let ports = make_ports(47_700, 47_799);
    let manifest = load_manifest("kyutai-tts");
    let cfg = serde_json::json!({ "model_preset_id": "pocket-tts-en" });
    let outcome = deploy(
        DeployMethod::NativePythonBundle,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("kyutai-tts deploy succeeds");
    assert_deployed(&db, &outcome, "kyutai-tts");
}

#[tokio::test]
#[ignore = "python-bundle: needs python3, pip install, process spawn"]
async fn deploy_chatterbox() {
    let db = open_db();
    let ports = make_ports(47_800, 47_899);
    let manifest = load_manifest("chatterbox");
    let cfg = serde_json::json!({ "model_preset_id": "chatterbox-multilingual" });
    let outcome = deploy(
        DeployMethod::NativePythonBundle,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("chatterbox deploy succeeds");
    assert_deployed(&db, &outcome, "chatterbox");
}

#[tokio::test]
#[ignore = "python-bundle: needs python3, pip install, process spawn"]
async fn deploy_xtts() {
    let db = open_db();
    let ports = make_ports(47_900, 47_999);
    let manifest = load_manifest("xtts");
    let cfg = serde_json::json!({ "model_preset_id": "xtts-v2" });
    let outcome = deploy(
        DeployMethod::NativePythonBundle,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("xtts deploy succeeds");
    assert_deployed(&db, &outcome, "xtts");
}

#[tokio::test]
#[ignore = "python-bundle: needs python3, pip install, process spawn"]
async fn deploy_voxcpm() {
    let db = open_db();
    let ports = make_ports(48_000, 48_099);
    let manifest = load_manifest("voxcpm");
    let cfg = serde_json::json!({ "model_preset_id": "voxcpm-base" });
    let outcome = deploy(
        DeployMethod::NativePythonBundle,
        &manifest,
        &cfg,
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("voxcpm deploy succeeds");
    assert_deployed(&db, &outcome, "voxcpm");
}
