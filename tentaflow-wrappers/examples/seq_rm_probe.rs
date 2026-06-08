// =============================================================================
// Plik: seq_rm_probe.rs
// Opis: De-risk B.0 — sprawdza, czy llama_memory_seq_rm(mem, seq, p, -1) zwraca
//       true dla modelu rekurencyjnego (Qwen3.6) gdy kontekst utworzono z
//       n_rs_seq > 0. To warunek konieczny rollbacku odrzuconych draftów ngram.
// Przykład: cargo run --release --example seq_rm_probe --features llama -- \
//           --model model.gguf --gpu-layers 99 --n-rs-seq 8
// =============================================================================

use std::ffi::CString;

use tentaflow_wrappers::llama::{silence_llama_logs, sys};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut model_path = String::new();
    let mut gpu_layers: i32 = 99;
    let mut n_rs_seq: u32 = 8;
    let mut verbose = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model_path = args.next().ok_or("brak wartości --model")?,
            "--gpu-layers" => gpu_layers = args.next().ok_or("brak --gpu-layers")?.parse()?,
            "--n-rs-seq" => n_rs_seq = args.next().ok_or("brak --n-rs-seq")?.parse()?,
            "--verbose-llama" => verbose = true,
            other => return Err(format!("nieznany argument: {other}").into()),
        }
    }
    if model_path.is_empty() {
        return Err("brak --model".into());
    }

    if !verbose {
        silence_llama_logs();
    }

    unsafe {
        sys::ggml_backend_load_all();
        sys::llama_backend_init();

        let mut mparams = sys::llama_model_default_params();
        mparams.n_gpu_layers = gpu_layers;
        let c_path = CString::new(model_path.as_str())?;
        let model = sys::llama_model_load_from_file(c_path.as_ptr(), mparams);
        if model.is_null() {
            return Err("nie udało się załadować modelu".into());
        }

        // Kontekst z n_rs_seq > 0 — to jest sedno testu B.0.
        let mut cparams = sys::llama_context_default_params();
        cparams.n_ctx = 2048;
        cparams.n_batch = 512;
        cparams.n_ubatch = 512;
        cparams.n_seq_max = 4;
        cparams.n_rs_seq = n_rs_seq;
        cparams.kv_unified = false;

        let ctx = sys::llama_init_from_model(model, cparams);
        if ctx.is_null() {
            sys::llama_model_free(model);
            return Err("nie udało się utworzyć kontekstu".into());
        }

        let reported_rs = sys::llama_n_rs_seq(ctx);
        println!("zażądano n_rs_seq={n_rs_seq}, kontekst raportuje llama_n_rs_seq={reported_rs}");

        let vocab = sys::llama_model_get_vocab(model);
        let bos = sys::llama_vocab_bos(vocab);

        // Zdekoduj kilka tokenów na seq=0, budując pozycje 0..N.
        let seq: sys::llama_seq_id = 0;
        let n_decode = 8_i32;
        let mut batch = sys::llama_batch_init(1, 0, 1);
        for pos in 0..n_decode {
            // Token bez znaczenia semantycznego (BOS) — wystarczy zbudować stan KV/rs.
            *batch.token.offset(0) = bos;
            *batch.pos.offset(0) = pos;
            *batch.n_seq_id.offset(0) = 1;
            **batch.seq_id.offset(0) = seq;
            *batch.logits.offset(0) = 1;
            batch.n_tokens = 1;
            let rc = sys::llama_decode(ctx, batch);
            if rc != 0 {
                sys::llama_batch_free(batch);
                sys::llama_free(ctx);
                sys::llama_model_free(model);
                return Err(format!("llama_decode rc={rc} na pos={pos}").into());
            }
        }

        let memory = sys::llama_get_memory(ctx);

        // Rollback od pozycji p < pos do końca (-1). To dokładnie operacja, której
        // pętla speculative używa po odrzuceniu draftów.
        let rollback_from = n_decode - 3; // usuń ostatnie 3 tokeny
        let removed = sys::llama_memory_seq_rm(memory, seq, rollback_from, -1);
        println!(
            "llama_memory_seq_rm(seq={seq}, p0={rollback_from}, p1=-1) => {removed}"
        );

        // Drugi rollback z innej pozycji dla pewności.
        let removed2 = sys::llama_memory_seq_rm(memory, seq, 2, -1);
        println!("llama_memory_seq_rm(seq={seq}, p0=2, p1=-1) => {removed2}");

        sys::llama_batch_free(batch);
        sys::llama_free(ctx);
        sys::llama_model_free(model);

        if removed && removed2 {
            println!("\nWYNIK B.0: seq_rm zwraca TRUE z n_rs_seq>0 — rollback rekurencyjny działa.");
            Ok(())
        } else {
            Err(format!(
                "WYNIK B.0: seq_rm zwróciło FALSE (removed={removed}, removed2={removed2}) \
                 mimo n_rs_seq={reported_rs} — ZATRZYMAJ, projekt wymaga checkpointów"
            )
            .into())
        }
    }
}
