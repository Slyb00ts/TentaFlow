// =============================================================================
// Plik: rerank_probe.rs
// Opis: Diagnostyka rerankera GGUF — ocenia pytanie względem fragmentów pliku
//       tekstowego (jeden fragment = akapit rozdzielony pustą linią) i drukuje
//       wyniki od najlepszego.
// Przykład: cargo run --example rerank_probe --features llama -- --model r.gguf --query "..." --docs docs.txt
// =============================================================================

use std::path::PathBuf;

use tentaflow_wrappers::llama::{silence_llama_logs, LlamaLoadConfig, LlamaRuntime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut model = None;
    let mut query = None;
    let mut docs = None;
    let mut embed = false;
    let mut qprefix = String::new();
    let mut dprefix = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model" => model = args.next().map(PathBuf::from),
            "--query" => query = args.next(),
            "--docs" => docs = args.next().map(PathBuf::from),
            "--embed" => embed = true,
            "--qprefix" => qprefix = args.next().unwrap_or_default(),
            "--dprefix" => dprefix = args.next().unwrap_or_default(),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let (model, query, docs) = match (model, query, docs) {
        (Some(m), Some(q), Some(d)) => (m, q, d),
        _ => return Err("usage: --model <gguf> --query <text> --docs <file>".into()),
    };
    silence_llama_logs();
    let runtime = LlamaRuntime::load(&model, LlamaLoadConfig::default())?;
    let text = std::fs::read_to_string(docs)?;
    let documents: Vec<String> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .collect();
    if embed {
        let q = runtime.embeddings(&format!("{qprefix}{query}"), true)?.vector;
        let mut ranked: Vec<(usize, f32)> = Vec::new();
        for (i, d) in documents.iter().enumerate() {
            let v = runtime.embeddings(&format!("{dprefix}{d}"), true)?.vector;
            ranked.push((i, q.iter().zip(&v).map(|(a, b)| a * b).sum()));
        }
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (i, score) in ranked {
            let head: String = documents[i].chars().take(70).collect();
            println!("{score:>10.4}  #{i:<3} {head}");
        }
        return Ok(());
    }
    let result = runtime.rerank(&query, &documents)?;
    let mut ranked: Vec<(usize, f32)> = result.scores.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (i, score) in ranked {
        let head: String = documents[i].chars().take(90).collect();
        println!("{score:>14.8e}  #{i:<2} {head}");
    }
    Ok(())
}
