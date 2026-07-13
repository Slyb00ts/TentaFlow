// =============================================================================
// Plik: examples/ort_cuda_probe.rs
// Opis: Harness walidacyjny FAZY 1 — sprawdza czy inferencja RF-DETR przez crate
//       `ort` (ONNX Runtime, load-dynamic) uruchamia sie na systemowej CUDA 13.3
//       przez CUDAExecutionProvider i mierzy realny czas forwardu. Krok
//       de-riskujacy przed przepisaniem detektora z Burn na ort.
// Przyklad:
//   export ORT_DYLIB_PATH=/usr/lib/libonnxruntime.so.1.24.4
//   export LD_LIBRARY_PATH=/usr/lib:/opt/cuda/lib64:$LD_LIBRARY_PATH
//   cargo run --release --example ort_cuda_probe --features inference-supertonic
// =============================================================================

use std::time::Instant;

use ndarray::Array4;
use ort::ep::{ExecutionProvider, TensorRT, CUDA};
use ort::session::Session;
use ort::value::Value;

/// Domyslna sciezka modelu RF-DETR (base-deploy, dynamic batch, wejscie 560x560).
const MODEL_PATH: &str =
    "/home/critix/repos/rust/TentaFlow/.runtime/cache/ml-training-artifacts/recog/base-deploy/rfdetr-base.onnx";

/// Nazwa wejscia ONNX oraz rozmiar tensora [1,3,560,560].
const INPUT_NAME: &str = "input";
const C: usize = 3;
const H: usize = 560;
const W: usize = 560;

/// Liczba iteracji rozgrzewki i pomiaru forwardu.
const WARMUP: usize = 5;
const ITERS: usize = 30;

fn main() -> anyhow::Result<()> {
    // Deterministyczny szum wejsciowy (xorshift) — bez zaleznosci od wersji rand.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let input: Array4<f32> = Array4::from_shape_fn((1, C, H, W), |_| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        // Mapujemy do [-1, 1].
        ((seed >> 40) as f32 / 8_388_608.0) - 1.0
    });

    let model_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| MODEL_PATH.to_string());
    println!("Model: {model_path}");
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        println!("ORT_DYLIB_PATH: {}", p.to_string_lossy());
    } else {
        println!("ORT_DYLIB_PATH: (nie ustawione — ort sprobuje domyslnych sciezek)");
    }

    // Odpytanie ONNX Runtime o dostepne providery (pierwsze wywolanie laduje dylib).
    let cuda_avail = CUDA::default().is_available().unwrap_or(false);
    let trt_avail = TensorRT::default().is_available().unwrap_or(false);
    println!("CUDA EP dostepny w onnxruntime: {cuda_avail}");
    println!("TensorRT EP dostepny w onnxruntime: {trt_avail}");

    let try_trt = std::env::var("PROBE_TRT").ok().as_deref() == Some("1");

    // Budujemy sesje probujac kolejno wybrane providery z `error_on_failure`, zeby
    // JEDNOZNACZNIE wiedziec ktory realnie sie zarejestrowal (a nie cichy fallback).
    let (mut session, provider) = build_session(&model_path, try_trt)?;
    println!("==> Realnie uzyty provider: {provider}");

    // Rozgrzewka.
    for _ in 0..WARMUP {
        let out = run_once(&mut session, &input)?;
        std::hint::black_box(&out);
    }

    // Pomiar.
    let mut times_ms = Vec::with_capacity(ITERS);
    let mut last_shapes: Option<(Vec<i64>, Vec<i64>)> = None;
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let out = run_once(&mut session, &input)?;
        let dt = t0.elapsed();
        times_ms.push(dt.as_secs_f64() * 1000.0);
        last_shapes = Some(out);
    }

    times_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sum: f64 = times_ms.iter().sum();
    let avg = sum / times_ms.len() as f64;
    let min = times_ms[0];
    let max = times_ms[times_ms.len() - 1];
    let p50 = times_ms[times_ms.len() / 2];
    let fps = 1000.0 / avg;

    println!();
    println!("=== Wynik pomiaru ({ITERS} iteracji, batch=1, {C}x{H}x{W}) ===");
    println!("provider : {provider}");
    println!("avg      : {avg:.3} ms/forward");
    println!("p50      : {p50:.3} ms");
    println!("min/max  : {min:.3} / {max:.3} ms");
    println!("fps      : {fps:.2}");

    if let Some((dets, labels)) = last_shapes {
        println!();
        println!("=== Sanity wyjsc ===");
        println!("dets   shape: {dets:?}  (oczekiwane [1, 300, 4])");
        println!("labels shape: {labels:?}  (oczekiwane [1, 300, 18])");
    }

    Ok(())
}

/// Buduje `Session` z modelu, probujac providery GPU z `error_on_failure` (aby
/// twardo wykryc czy CUDA/TensorRT sie zarejestrowal), a przy niepowodzeniu
/// spadajac na CPU. Zwraca sesje + nazwe realnie uzytego providera.
fn build_session(model_path: &str, try_trt: bool) -> anyhow::Result<(Session, &'static str)> {
    if try_trt {
        match try_commit(model_path, TensorRT::default().build().error_on_failure()) {
            Ok(s) => return Ok((s, "TensorRTExecutionProvider")),
            Err(e) => println!("TensorRT EP nieudany ({e}); probuje CUDA."),
        }
    }

    match try_commit(model_path, CUDA::default().build().error_on_failure()) {
        Ok(s) => return Ok((s, "CUDAExecutionProvider")),
        Err(e) => println!("CUDA EP nieudany ({e}); fallback na CPU."),
    }

    let s = Session::builder()?.commit_from_file(model_path)?;
    Ok((s, "CPUExecutionProvider (fallback)"))
}

/// Probuje zarejestrowac provider (`error_on_failure` => twarde niepowodzenie
/// zamiast cichego fallbacku) i skomitowac sesje z modelu.
fn try_commit(model_path: &str, ep: ort::ep::ExecutionProviderDispatch) -> anyhow::Result<Session> {
    let mut builder = Session::builder()?
        .with_execution_providers([ep])
        .map_err(|e| anyhow::anyhow!("rejestracja EP nieudana: {e}"))?;
    let session = builder.commit_from_file(model_path)?;
    Ok(session)
}

/// Pojedynczy forward. Zwraca ksztalty wyjsc `dets` i `labels`.
fn run_once(session: &mut Session, input: &Array4<f32>) -> anyhow::Result<(Vec<i64>, Vec<i64>)> {
    let value = Value::from_array(input.clone())?;
    let outputs = session.run(ort::inputs! { INPUT_NAME => value })?;
    let (dets_shape, _) = outputs["dets"].try_extract_tensor::<f32>()?;
    let (labels_shape, _) = outputs["labels"].try_extract_tensor::<f32>()?;
    Ok((dets_shape.to_vec(), labels_shape.to_vec()))
}
