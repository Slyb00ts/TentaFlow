// =============================================================================
// Plik: nvfp4_bm16_audit.rs
// Opis: Audytuje pełne logity i generację BM16 względem referencji Row B1.
// Przykład: FORGE_BM16_AUDIT_MODE=reference cargo test --release --test nvfp4_bm16_audit -- --ignored
// =============================================================================

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_engine::model::{Model, ModelConfig};
use forge_engine::sample::SeqSampleParams;
use forge_engine::weights::NvFp4CtLayoutPolicy;
use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::Device;
use forge_server::source::{kv_pool_bytes, load_model, read_descriptor};
use serde_json::json;

const BIELIK_DIR: &str = "/home/critix/repos/rust/TentaFlow/.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7";
const PROMPT_DIR: &str = "/home/critix/.cache/tentaflow-profiles/concurrency-matrix-p1024-o256";
const STEPS: usize = 256;
const PROMPT_FILES: [&str; 16] = [
    "prompt01-architecture.u32le",
    "prompt02-execution.u32le",
    "prompt03-roadmap.u32le",
    "prompt04-scheduler.u32le",
    "prompt05-memory.u32le",
    "prompt06-speculation.u32le",
    "prompt07-mtp_results.u32le",
    "prompt08-mtp_kernels.u32le",
    "prompt09-mtp_serving.u32le",
    "prompt10-comparison_a.u32le",
    "prompt11-comparison_b.u32le",
    "prompt12-comparison_c.u32le",
    "prompt13-codegen_a.u32le",
    "prompt14-codegen_b.u32le",
    "prompt15-status_a.u32le",
    "prompt16-status_b.u32le",
];

#[derive(Default)]
struct Metrics {
    rows: usize,
    finite_rows: usize,
    top1_matches: usize,
    non_finite: usize,
    rel_l2_sum: f64,
    cosine_sum: f64,
    kl_sum: f64,
    max_abs: f64,
    reference_margin_sum: f64,
    candidate_margin_sum: f64,
}

impl Metrics {
    fn observe(&mut self, reference: &[f32], candidate: &[f32]) {
        self.rows += 1;
        let non_finite = reference
            .iter()
            .chain(candidate)
            .filter(|value| !value.is_finite())
            .count();
        self.non_finite += non_finite;
        if non_finite != 0 {
            return;
        }
        self.finite_rows += 1;

        let mut diff2 = 0.0f64;
        let mut reference2 = 0.0f64;
        let mut candidate2 = 0.0f64;
        let mut dot = 0.0f64;
        for (&left, &right) in reference.iter().zip(candidate) {
            let left = left as f64;
            let right = right as f64;
            let diff = right - left;
            diff2 += diff * diff;
            reference2 += left * left;
            candidate2 += right * right;
            dot += left * right;
            self.max_abs = self.max_abs.max(diff.abs());
        }
        self.rel_l2_sum += (diff2 / reference2.max(f64::MIN_POSITIVE)).sqrt();
        self.cosine_sum += dot / (reference2 * candidate2).sqrt().max(f64::MIN_POSITIVE);

        let (reference_top1, reference_top2) = top2(reference);
        let (candidate_top1, candidate_top2) = top2(candidate);
        self.top1_matches += usize::from(reference_top1 == candidate_top1);
        self.reference_margin_sum +=
            (reference[reference_top1] - reference[reference_top2]) as f64;
        self.candidate_margin_sum +=
            (candidate[candidate_top1] - candidate[candidate_top2]) as f64;
        self.kl_sum += kl_divergence(reference, candidate);
    }

    fn report(&self) -> serde_json::Value {
        let denominator = self.finite_rows.max(1) as f64;
        json!({
            "rows": self.rows,
            "finite_rows": self.finite_rows,
            "non_finite_values": self.non_finite,
            "mean_rel_l2": self.rel_l2_sum / denominator,
            "mean_cosine": self.cosine_sum / denominator,
            "max_abs": self.max_abs,
            "mean_kl_reference_candidate": self.kl_sum / denominator,
            "top1_matches": self.top1_matches,
            "top1_rate": self.top1_matches as f64 / denominator,
            "mean_reference_top2_margin": self.reference_margin_sum / denominator,
            "mean_candidate_top2_margin": self.candidate_margin_sum / denominator,
        })
    }
}

fn top2(values: &[f32]) -> (usize, usize) {
    let mut first = 0usize;
    let mut second = 1usize;
    if values[second] > values[first] {
        std::mem::swap(&mut first, &mut second);
    }
    for index in 2..values.len() {
        if values[index] > values[first] {
            second = first;
            first = index;
        } else if values[index] > values[second] {
            second = index;
        }
    }
    (first, second)
}

fn kl_divergence(reference: &[f32], candidate: &[f32]) -> f64 {
    let reference_max = reference
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max) as f64;
    let candidate_max = candidate
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max) as f64;
    let reference_sum = reference
        .iter()
        .map(|&value| ((value as f64) - reference_max).exp())
        .sum::<f64>();
    let candidate_sum = candidate
        .iter()
        .map(|&value| ((value as f64) - candidate_max).exp())
        .sum::<f64>();
    let reference_log_z = reference_max + reference_sum.ln();
    let candidate_log_z = candidate_max + candidate_sum.ln();
    reference
        .iter()
        .zip(candidate)
        .map(|(&left, &right)| {
            let log_probability = left as f64 - reference_log_z;
            log_probability.exp()
                * (log_probability - (right as f64 - candidate_log_z))
        })
        .sum()
}

fn greedy_params() -> SeqSampleParams {
    SeqSampleParams {
        greedy: true,
        k: 1,
        inv_t: 1.0,
        top_p: 1.0,
        min_p: 0.0,
        seed: 0,
        step: 0,
        penalty: 1.0,
        frequency_penalty: 0.0,
        presence_penalty: 0.0,
        penalty_ids: Vec::new(),
        penalty_counts: Vec::new(),
    }
}

fn read_prompts() -> Vec<Vec<u32>> {
    PROMPT_FILES
        .iter()
        .map(|name| {
            let bytes = fs::read(Path::new(PROMPT_DIR).join(name)).expect("odczyt promptu");
            assert_eq!(bytes.len(), 1024 * 4, "prompt {name} ma nieprawidłowy rozmiar");
            bytes
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                .collect()
        })
        .collect()
}

fn output_dir() -> PathBuf {
    std::env::var_os("FORGE_BM16_AUDIT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/home/critix/.cache/tentaflow-profiles/nvfp4-bm16-audit")
        })
}

fn load_bielik(batch: usize, layout: NvFp4CtLayoutPolicy) -> Model {
    let path = Path::new(BIELIK_DIR);
    let descriptor = read_descriptor(path).expect("odczyt deskryptora");
    let kv_page_size = 32;
    let kv_pages = (batch.max(1) * 48).max(96);
    let device = CudaDevice::new(
        0,
        PoolSizes {
            weights: 12 << 30,
            kv_cache: kv_pool_bytes(
                &descriptor,
                kv_page_size,
                kv_pages,
                forge_engine::kv::KvQuant::F16,
                false,
            )
            .expect("rozmiar KV")
            .max(1 << 30),
            activations: 2 << 30,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    )
    .expect("urządzenie CUDA");
    let device: Arc<dyn Device> = device;
    load_model(
        device,
        path,
        ModelConfig {
            kv_page_size,
            kv_pages,
            max_seq_len: 2048,
            kv_quant: forge_engine::kv::KvQuant::F16,
            kv_tier: Default::default(),
            prefix_cache: false,
            native_mtp: false,
            nvfp4_gguf_layout: forge_engine::model::Nvfp4GgufLayout::RowMajor36,
            nvfp4_ct_layout: layout,
        },
    )
    .expect("wczytanie Bielika")
    .model
}

fn prefill_serial(model: &mut Model, seq: &mut forge_engine::kv::SeqKv, prompt: &[u32]) -> Vec<f32> {
    let chunks = prompt.chunks(128).collect::<Vec<_>>();
    for chunk in &chunks[..chunks.len() - 1] {
        model
            .prefill_chunk_device_sync(seq, chunk)
            .expect("serial prefill");
    }
    model
        .prefill_chunk(seq, chunks[chunks.len() - 1])
        .expect("finalny serial prefill")
}

fn prefill_serial_batch(
    model: &mut Model,
    seqs: &mut [forge_engine::kv::SeqKv],
    prompts: &[Vec<u32>],
) -> Vec<f32> {
    let mut logits = Vec::new();
    for (seq, prompt) in seqs.iter_mut().zip(prompts) {
        logits.extend(prefill_serial(model, seq, prompt));
    }
    logits
}

fn write_f32(writer: &mut BufWriter<File>, values: &[f32]) {
    writer
        .write_all(bytemuck::cast_slice(values))
        .expect("zapis logitów");
}

fn read_reference_row(
    reader: &mut BufReader<File>,
    prompt: usize,
    step: usize,
    vocab: usize,
    row: &mut [f32],
) {
    let offset = ((prompt * STEPS + step) * vocab * 4) as u64;
    reader
        .seek(SeekFrom::Start(offset))
        .expect("pozycjonowanie referencji");
    reader
        .read_exact(bytemuck::cast_slice_mut(row))
        .expect("odczyt referencji");
}

fn run_reference(prompts: &[Vec<u32>], directory: &Path) {
    assert_ne!(
        std::env::var("FORGE_NVFP4_CT_BM16").ok().as_deref(),
        Some("1"),
        "referencja wymaga wyłączonego BM16"
    );
    let mut model = load_bielik(1, NvFp4CtLayoutPolicy::RowMajorE4M3);
    let mut writer =
        BufWriter::new(File::create(directory.join("row-b1-logits.f32le")).expect("plik logitów"));
    let mut continuations = Vec::with_capacity(prompts.len());
    let mut vocab = 0usize;
    for prompt in prompts {
        let mut seq = model.new_seq();
        let mut logits = prefill_serial(&mut model, &mut seq, prompt);
        vocab = logits.len();
        let mut ids = Vec::with_capacity(STEPS);
        for step in 0..STEPS {
            write_f32(&mut writer, &logits);
            let token = top2(&logits).0 as u32;
            ids.push(token);
            if step + 1 < STEPS {
                logits = model.step(&mut seq, token).expect("referencyjny decode");
            }
        }
        continuations.push(ids);
        model.release_seq(&mut seq);
    }
    writer.flush().expect("opróżnienie logitów");
    fs::write(
        directory.join("row-b1-continuations.json"),
        serde_json::to_vec_pretty(&continuations).unwrap(),
    )
    .expect("zapis kontynuacji");
    fs::write(
        directory.join("row-b1-metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "prompts": prompts.len(),
            "steps": STEPS,
            "vocab": vocab,
            "layout": "row",
            "batch": 1
        }))
        .unwrap(),
    )
    .expect("zapis metadanych");
}

fn run_candidate(prompts: &[Vec<u32>], directory: &Path, batch: usize) {
    assert!(matches!(batch, 4 | 8 | 16), "BM16 wymaga B4/B8/B16");
    assert_eq!(
        std::env::var("FORGE_NVFP4_CT_BM16").ok().as_deref(),
        Some("1"),
        "kandydat wymaga FORGE_NVFP4_CT_BM16=1"
    );
    let continuations: Vec<Vec<u32>> = serde_json::from_slice(
        &fs::read(directory.join("row-b1-continuations.json")).expect("kontynuacje referencyjne"),
    )
    .expect("format kontynuacji");
    let metadata: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.join("row-b1-metadata.json")).expect("metadane referencji"),
    )
    .expect("format metadanych");
    let vocab = metadata["vocab"].as_u64().expect("vocab") as usize;
    let mut reference =
        BufReader::new(File::open(directory.join("row-b1-logits.f32le")).expect("logity referencji"));
    let mut model = load_bielik(batch, NvFp4CtLayoutPolicy::S0N64K128);
    let params = vec![greedy_params(); batch];
    let mut metrics = Metrics::default();
    let mut prefill_metrics = Metrics::default();
    let mut decode_metrics = Metrics::default();
    let mut first_divergences = vec![None; prompts.len()];
    let mut free_ids = vec![Vec::with_capacity(STEPS); prompts.len()];
    let mut reference_row = vec![0.0f32; vocab];

    for group_start in (0..prompts.len()).step_by(batch) {
        let group_prompts = &prompts[group_start..group_start + batch];
        let mut seqs = (0..batch).map(|_| model.new_seq()).collect::<Vec<_>>();
        let prefill_logits = prefill_serial_batch(&mut model, &mut seqs, group_prompts);
        for lane in 0..batch {
            read_reference_row(
                &mut reference,
                group_start + lane,
                0,
                vocab,
                &mut reference_row,
            );
            let candidate_row = &prefill_logits[lane * vocab..(lane + 1) * vocab];
            metrics.observe(&reference_row, candidate_row);
            prefill_metrics.observe(&reference_row, candidate_row);
        }
        for step in 1..STEPS {
            let tokens = (0..batch)
                .map(|lane| continuations[group_start + lane][step - 1])
                .collect::<Vec<_>>();
            let mut refs = seqs.iter_mut().collect::<Vec<_>>();
            model
                .batched_decode(&mut refs, &tokens, &params)
                .expect("teacher-forced BM16 decode");
            let logits = model
                .read_batch_logits(batch)
                .expect("pełne logity BM16");
            for lane in 0..batch {
                read_reference_row(
                    &mut reference,
                    group_start + lane,
                    step,
                    vocab,
                    &mut reference_row,
                );
                let candidate_row = &logits[lane * vocab..(lane + 1) * vocab];
                metrics.observe(&reference_row, candidate_row);
                decode_metrics.observe(&reference_row, candidate_row);
            }
        }
        for seq in &mut seqs {
            model.release_seq(seq);
        }

        let mut free_seqs = (0..batch).map(|_| model.new_seq()).collect::<Vec<_>>();
        let initial_logits = prefill_serial_batch(&mut model, &mut free_seqs, group_prompts);
        let mut current = (0..batch)
            .map(|lane| top2(&initial_logits[lane * vocab..(lane + 1) * vocab]).0 as u32)
            .collect::<Vec<_>>();
        let mut free_logits = initial_logits;
        for step in 0..STEPS {
            for lane in 0..batch {
                let prompt = group_start + lane;
                free_ids[prompt].push(current[lane]);
                if first_divergences[prompt].is_none()
                    && current[lane] != continuations[prompt][step]
                {
                    read_reference_row(
                        &mut reference,
                        prompt,
                        step,
                        vocab,
                        &mut reference_row,
                    );
                    let candidate_row = &free_logits[lane * vocab..(lane + 1) * vocab];
                    let (reference_top1, reference_top2) = top2(&reference_row);
                    let (candidate_top1, candidate_top2) = top2(candidate_row);
                    first_divergences[prompt] = Some(json!({
                        "step": step,
                        "reference_token": continuations[prompt][step],
                        "candidate_token": current[lane],
                        "reference_top2_margin":
                            reference_row[reference_top1] - reference_row[reference_top2],
                        "candidate_top2_margin":
                            candidate_row[candidate_top1] - candidate_row[candidate_top2]
                    }));
                }
            }
            if step + 1 < STEPS {
                let mut refs = free_seqs.iter_mut().collect::<Vec<_>>();
                current = model
                    .batched_decode(&mut refs, &current, &params)
                    .expect("swobodny BM16 decode");
                free_logits = model
                    .read_batch_logits(batch)
                    .expect("logity swobodnego BM16 decode");
            }
        }
        for seq in &mut free_seqs {
            model.release_seq(seq);
        }
    }

    let report = json!({
        "layout": "s0",
        "bm16": true,
        "batch": batch,
        "teacher_forced": metrics.report(),
        "prefill": prefill_metrics.report(),
        "teacher_forced_decode": decode_metrics.report(),
        "free_generation_first_divergence": first_divergences,
        "free_generation_ids": free_ids,
    });
    fs::write(
        directory.join(format!("bm16-b{batch}-audit.json")),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .expect("zapis raportu BM16");
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "batch": batch,
            "prefill": report["prefill"],
            "teacher_forced_decode": report["teacher_forced_decode"],
            "free_generation_first_divergence":
                report["free_generation_first_divergence"]
        }))
        .unwrap()
    );
}

#[test]
#[ignore = "wymaga CUDA, lokalnego Bielika i jawnego trybu audytu"]
fn nvfp4_bm16_full_logits_audit() {
    if !Path::new(BIELIK_DIR).is_dir() {
        eprintln!("pominięto: brak modelu Bielik");
        return;
    }
    let mode = std::env::var("FORGE_BM16_AUDIT_MODE").expect("ustaw tryb reference albo candidate");
    let directory = output_dir();
    fs::create_dir_all(&directory).expect("katalog audytu");
    let prompts = read_prompts();
    match mode.as_str() {
        "reference" => run_reference(&prompts, &directory),
        "candidate" => {
            let batch = std::env::var("FORGE_BM16_AUDIT_BATCH")
                .expect("ustaw batch 4, 8 albo 16")
                .parse()
                .expect("liczbowy batch");
            run_candidate(&prompts, &directory, batch);
        }
        _ => panic!("nieznany tryb audytu: {mode}"),
    }
}

/// Generuje `steps` tokenów greedy dla `batch` sekwencji jednym batchowym
/// dekodem i zwraca kontynuację per lane.
fn free_generate(model: &mut Model, prompts: &[Vec<u32>], steps: usize) -> Vec<Vec<u32>> {
    let batch = prompts.len();
    let params = vec![greedy_params(); batch];
    let mut seqs = (0..batch).map(|_| model.new_seq()).collect::<Vec<_>>();
    let initial = prefill_serial_batch(model, &mut seqs, prompts);
    let vocab = initial.len() / batch;
    let mut current = (0..batch)
        .map(|lane| top2(&initial[lane * vocab..(lane + 1) * vocab]).0 as u32)
        .collect::<Vec<_>>();
    let mut ids = vec![Vec::with_capacity(steps); batch];
    for step in 0..steps {
        for lane in 0..batch {
            ids[lane].push(current[lane]);
        }
        if step + 1 < steps {
            let mut refs = seqs.iter_mut().collect::<Vec<_>>();
            current = model
                .batched_decode(&mut refs, &current, &params)
                .expect("batchowy dekod");
        }
    }
    for seq in &mut seqs {
        model.release_seq(seq);
    }
    ids
}

/// Kafel BM32 nie ma zewnętrznej referencji pełnych logitów, więc parytet
/// sprawdzamy strukturalnie: obie połowy kafla liczą to samo dla tych samych
/// promptów, a lane 0..15 muszą zgadzać się z audytowanym kaflem BM16.
#[test]
#[ignore = "wymaga CUDA i lokalnego Bielika"]
fn nvfp4_bm32_zgadza_sie_z_bm16_i_obiema_polowkami_kafla() {
    if !Path::new(BIELIK_DIR).is_dir() {
        eprintln!("pominięto: brak modelu Bielik");
        return;
    }
    const STEPS_PARITY: usize = 32;
    let prompts = read_prompts();
    assert_eq!(prompts.len(), 16, "test zakłada 16 promptów bazowych");
    let doubled: Vec<Vec<u32>> = prompts.iter().chain(prompts.iter()).cloned().collect();

    let mut model = load_bielik(32, NvFp4CtLayoutPolicy::S0N64K128);
    let wide = free_generate(&mut model, &doubled, STEPS_PARITY);
    let narrow = free_generate(&mut model, &prompts, STEPS_PARITY);

    for lane in 0..16 {
        assert_eq!(
            wide[lane], wide[lane + 16],
            "połówki kafla BM32 rozjechały się na lane {lane}"
        );
        assert_eq!(
            wide[lane], narrow[lane],
            "BM32 rozjechał się z BM16 na lane {lane}"
        );
    }
}
