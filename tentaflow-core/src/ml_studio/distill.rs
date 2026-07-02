// ===== File: ml_studio/distill.rs — generowanie datasetu destylacji =====
//
// Flow: zbierz PYTANIA (import istniejacego datasetu ALBO generacja modelem z
// promptu usera) -> dla kazdego pytania odpytaj TEACHER model po ODPOWIEDZ ->
// zapisz pary (question, answer) jako nowy dataset (kind="distill_qa", JSONL).
// Odpowiedzi teachera sa etykietami treningowymi ucznia (destylacja/KD lub SFT).
//
// Robota idzie w tle (tokio::spawn); UI odpytuje postep przez
// `MlStudioDistillGenerateStatusRequest` (progress + podglad par). Postep zyje
// w mapie in-memory kluczowanej dataset_id; wynik zapisywany raz na koniec.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::routing::router::Router;
use tentaflow_protocol::{MlStudioDistillGenerateRequest, MlStudioDistillQaPair};

/// Stan postepu jednego zadania generowania (in-memory, ephemeral — reset przy
/// restarcie; niedokonczone zadanie trzeba wtedy uruchomic ponownie).
#[derive(Clone)]
pub struct DistillProgress {
    pub status: String, // pending|generating_questions|answering|completed|failed
    pub total: u32,
    pub done: u32,
    pub error: Option<String>,
    pub samples: Vec<MlStudioDistillQaPair>,
}

fn jobs() -> &'static Mutex<HashMap<String, DistillProgress>> {
    static JOBS: OnceLock<Mutex<HashMap<String, DistillProgress>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Odczyt postepu zadania (dla handlera statusu). None gdy nieznane.
pub fn distill_status(dataset_id: &str) -> Option<DistillProgress> {
    jobs().lock().ok()?.get(dataset_id).cloned()
}

fn set_progress<F: FnOnce(&mut DistillProgress)>(dataset_id: &str, f: F) {
    if let Ok(mut map) = jobs().lock() {
        if let Some(p) = map.get_mut(dataset_id) {
            f(p);
        }
    }
}

/// Tworzy PUSTY dataset (status w profile_json), rejestruje postep i uruchamia
/// generacje w tle. Zwraca `dataset_id` do pollingu statusu.
pub fn spawn_distill_generation(
    router: Arc<Router>,
    user_id: String,
    req: MlStudioDistillGenerateRequest,
) -> anyhow::Result<String> {
    // Pusty dataset od razu — UI ma id do pollingu; raw_data dopiszemy na koncu.
    let profile = serde_json::json!({ "distill_status": "pending" }).to_string();
    let dataset = super::repository::create_dataset(
        &user_id,
        &req.project_id,
        &req.dataset_name,
        "distill_qa",
        0,
        2,
        &profile,
        &[],
    )?;
    let dataset_id = dataset.dataset_id.clone();

    jobs().lock().unwrap().insert(
        dataset_id.clone(),
        DistillProgress {
            status: "pending".to_string(),
            total: 0,
            done: 0,
            error: None,
            samples: Vec::new(),
        },
    );

    let ds_id = dataset_id.clone();
    tokio::spawn(async move {
        if let Err(e) = run_generation(&router, &user_id, &ds_id, &req).await {
            set_progress(&ds_id, |p| {
                p.status = "failed".to_string();
                p.error = Some(format!("{e:#}"));
            });
        }
    });

    Ok(dataset_id)
}

async fn run_generation(
    router: &Arc<Router>,
    user_id: &str,
    dataset_id: &str,
    req: &MlStudioDistillGenerateRequest,
) -> anyhow::Result<()> {
    let max_tokens = req.max_tokens.unwrap_or(768);

    // 1. Zbierz pytania.
    set_progress(dataset_id, |p| p.status = "generating_questions".to_string());
    let questions = match req.question_source.as_str() {
        "import" => gather_questions_import(user_id, req)?,
        "generate" => gather_questions_generated(router, req, max_tokens).await?,
        other => anyhow::bail!("nieznane question_source '{other}' (import|generate)"),
    };
    if questions.is_empty() {
        anyhow::bail!("brak pytan do wygenerowania odpowiedzi");
    }
    set_progress(dataset_id, |p| {
        p.status = "answering".to_string();
        p.total = questions.len() as u32;
    });

    // 2. Wariant decyduje o kształcie danych: sft/kd -> (question, answer);
    //    dpo -> (prompt, chosen, rejected) — teacher daje chosen, słabszy model rejected.
    let objective = req
        .objective
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("sft")
        .to_ascii_lowercase();
    let is_dpo = objective == "dpo";

    let mut pairs: Vec<MlStudioDistillQaPair> = Vec::with_capacity(questions.len());
    for question in questions {
        // Odpowiedź teachera (SFT/KD = answer; DPO = chosen/lepsza).
        let ans_prompt = match &req.answer_instruction {
            Some(instr) if !instr.trim().is_empty() => format!("{}\n\n{}", instr.trim(), question),
            _ => question.clone(),
        };
        let answer =
            crate::ml_studio::infer::run_local_chat(router, &req.teacher_model, &ans_prompt, max_tokens)
                .await
                .map_err(|e| anyhow::anyhow!("teacher '{}': {e:#}", req.teacher_model))?
                .trim()
                .to_string();

        // DPO: ODRZUCONA (gorsza) odpowiedź ze słabszego modelu (fallback: teacher z
        // instrukcją „odpowiedz gorzej"). Preferencja chosen>rejected uczy DPO.
        let rejected = if is_dpo {
            let rej_model = req
                .rejected_model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(req.teacher_model.as_str());
            let rej_instr = req
                .rejected_instruction
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(
                    "Odpowiedz celowo SŁABO: krótko, ogólnikowo, bez szczegółów i \
                     uzasadnienia. To ma być gorsza odpowiedź.",
                );
            // Odrzucona odpowiedź na TEN SAM prompt zadania (ans_prompt), tylko z
            // meta-instrukcją „gorzej" — inaczej chosen/rejected/prompt byłyby
            // niespójne (rejected bez answer_instruction, prompt bez instrukcji).
            let rej_prompt = format!("{}\n\n{}", rej_instr, ans_prompt);
            let r = crate::ml_studio::infer::run_local_chat(router, rej_model, &rej_prompt, max_tokens)
                .await
                .map_err(|e| anyhow::anyhow!("rejected model '{}': {e:#}", rej_model))?;
            Some(r.trim().to_string())
        } else {
            None
        };

        // DPO: zapisany prompt = ans_prompt (to, do czego pasują chosen i rejected).
        // SFT/KD: surowe pytanie (odpowiedź to etykieta pytania; instrukcja to tylko
        // scaffold generacji, uczeń uczy się zadania z samego pytania).
        let record_prompt = if is_dpo { ans_prompt } else { question };
        let pair = MlStudioDistillQaPair {
            question: record_prompt,
            answer,
            rejected,
        };
        pairs.push(pair.clone());
        set_progress(dataset_id, |p| {
            p.done += 1;
            if p.samples.len() < 5 {
                p.samples.push(pair);
            }
        });
    }

    // 3. Zapis JSONL: SFT/KD -> {question, answer}; DPO -> {prompt, chosen, rejected}
    //    (kształt wymagany przez trener DPO — server.py sprawdza te trzy pola).
    let mut jsonl = String::new();
    for pair in &pairs {
        let line = if is_dpo {
            serde_json::json!({
                "prompt": pair.question,
                "chosen": pair.answer,
                "rejected": pair.rejected.clone().unwrap_or_default(),
            })
        } else {
            serde_json::json!({ "question": pair.question, "answer": pair.answer })
        };
        jsonl.push_str(&serde_json::to_string(&line)?);
        jsonl.push('\n');
    }
    super::repository::update_dataset_data(user_id, dataset_id, pairs.len() as u64, jsonl.as_bytes())?;

    set_progress(dataset_id, |p| p.status = "completed".to_string());
    Ok(())
}

/// Import: wyciaga pytania ze zrodlowego datasetu. Obsluguje CSV/TSV/XLSX (kolumna
/// `question_field` albo pierwsza pasujaca/pierwsza) ORAZ JSONL (pole `question_field`,
/// fallback "question"->"prompt"). Format bierzemy z `dataset.kind` (nie zgadujemy).
fn gather_questions_import(
    user_id: &str,
    req: &MlStudioDistillGenerateRequest,
) -> anyhow::Result<Vec<String>> {
    let src_id = req
        .source_dataset_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("question_source=import wymaga source_dataset_id"))?;
    let dataset = super::repository::get_dataset(user_id, src_id)?
        .ok_or_else(|| anyhow::anyhow!("dataset zrodlowy niedostepny"))?;
    let raw = super::repository::get_dataset_raw(user_id, src_id)?;
    let field = req.question_field.as_deref().unwrap_or("");
    let kind = dataset.kind.to_ascii_lowercase();

    // Tabela (CSV/TSV/XLSX): wybierz kolumne z pytaniem.
    if matches!(kind.as_str(), "csv" | "tsv" | "xlsx" | "xlsm" | "xls") {
        let filename = format!("dataset.{kind}");
        let (headers, rows) = crate::ml_studio::profile::parse_table(&raw, &filename)?;
        let col = headers
            .iter()
            .position(|h| !field.is_empty() && h.eq_ignore_ascii_case(field))
            .or_else(|| {
                headers
                    .iter()
                    .position(|h| h.eq_ignore_ascii_case("question") || h.eq_ignore_ascii_case("prompt"))
            })
            .unwrap_or(0);
        let mut out = Vec::new();
        for row in rows {
            if let Some(v) = row.get(col) {
                let v = v.trim();
                if !v.is_empty() {
                    out.push(v.to_string());
                }
            }
        }
        return Ok(out);
    }

    // JSONL/JSON (lub nieznany kind — probujemy JSONL): obiekt per linia.
    let mut out = Vec::new();
    for line in raw.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Wybrane pole; fallback: "question" -> "prompt" -> pierwsza wartosc string.
        let q = if !field.is_empty() {
            v.get(field).and_then(|x| x.as_str())
        } else {
            v.get("question")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("prompt").and_then(|x| x.as_str()))
        };
        if let Some(q) = q {
            let q = q.trim();
            if !q.is_empty() {
                out.push(q.to_string());
            }
        }
    }
    Ok(out)
}

/// Generacja: model `question_model` dostaje prompt usera i ma wypisac liste pytan
/// (jedno na linie). Parsujemy linie, capujemy do `num_questions`.
async fn gather_questions_generated(
    router: &Arc<Router>,
    req: &MlStudioDistillGenerateRequest,
    max_tokens: u32,
) -> anyhow::Result<Vec<String>> {
    let base_prompt = req
        .generate_prompt
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("question_source=generate wymaga generate_prompt"))?;
    let model = req
        .question_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&req.teacher_model);
    let n = req.num_questions.unwrap_or(10).clamp(1, 500);

    let prompt = format!(
        "{base_prompt}\n\nWypisz DOKLADNIE {n} pytan/instrukcji, jedno na linie, bez \
         numeracji, bez komentarzy, bez pustych linii. Kazda linia to jedno pytanie."
    );
    let raw = crate::ml_studio::infer::run_local_chat(router, model, &prompt, max_tokens)
        .await
        .map_err(|e| anyhow::anyhow!("question_model '{model}': {e:#}"))?;

    let out: Vec<String> = raw
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == '-')
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .take(n as usize)
        .collect();
    Ok(out)
}
