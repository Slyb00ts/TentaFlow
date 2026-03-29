// =============================================================================
// Plik: examples/test_bielik_mlx.rs
// Opis: Test integracyjny — ladowanie modelu Bielik 4.5B MLX i generowanie odpowiedzi.
// =============================================================================

use std::path::Path;

#[tokio::main]
async fn main() {
    // Inicjalizacja logowania
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let model_dir = Path::new(
        "/Users/critix/Library/Application Support/TentaFlow.AI/models/huggingface/vqstudio/Bielik-4.5B-v3.0-Instruct-MLX-4bit"
    );

    if !model_dir.exists() {
        eprintln!("Model nie znaleziony w: {}", model_dir.display());
        return;
    }

    println!("=== Test Bielik 4.5B MLX ===\n");

    // Test 1: Wykrycie chat template
    println!("--- Test 1: Wykrycie chat template ---");
    let template = tentaflow_core::routing::chat_template::detect_chat_template(model_dir);
    println!("Wykryty template: {:?}\n", template);

    // Test 2: Formatowanie promptu ChatML
    println!("--- Test 2: Formatowanie ChatML ---");
    use tentaflow_core::routing::chat_template::ChatMessage;
    let messages = vec![
        ChatMessage { role: "system".into(), content: "Jestes pomocnym polskim asystentem AI. Odpowiadaj krotko i zwiezle.".into() },
        ChatMessage { role: "user".into(), content: "Czesc! Kim jestes?".into() },
    ];
    let formatted = template.format_messages(&messages, true);
    println!("Sformatowany prompt:\n{}\n", formatted);
    println!("Stop tokeny: {:?}\n", template.stop_tokens());

    // Test 3: Ladowanie modelu
    println!("--- Test 3: Ladowanie modelu ---");
    let engine = tentaflow_core::inference::mlx::MlxEngine::new();

    use tentaflow_core::inference::InferenceEngine;
    match engine.load_model(model_dir, None).await {
        Ok(info) => {
            println!("Model zaladowany pomyslnie:");
            println!("  Nazwa: {}", info.name);
            println!("  Backend: {}", info.backend);
            println!("  Parametry: {}", info.parameters);
            println!("  Context: {}", info.context_length);
            println!("  Kwantyzacja: {:?}", info.quantization);
            println!("  Chat template: {:?}", info.chat_template);
            println!("  VRAM: {}MB", info.vram_used_mb);
            println!();
        }
        Err(e) => {
            eprintln!("BLAD ladowania modelu: {}", e);
            eprintln!("Szczegoly: {:?}", e);
            return;
        }
    }

    // Test 4: Generowanie odpowiedzi
    println!("--- Test 4: Generowanie odpowiedzi ---");
    let params = tentaflow_core::inference::GenerateParams {
        prompt: formatted.clone(),
        max_tokens: 100,
        temperature: 0.2,
        top_p: 0.95,
        top_k: 0,
        repeat_penalty: 1.0,
        stop_sequences: template.stop_tokens(),
        system_prompt: None, // juz wbudowany w prompt
    };

    match engine.generate(params).await {
        Ok(result) => {
            println!("Odpowiedz Bielika:");
            println!("---");
            println!("{}", result.text);
            println!("---");
            println!("Wygenerowano: {} tokenow", result.tokens_generated);
            println!("Prompt: {} tokenow", result.prompt_tokens);
            println!("Szybkosc: {:.1} tok/s", result.tokens_per_second);
            println!("Powod zatrzymania: {:?}", result.stop_reason);
            println!();
        }
        Err(e) => {
            eprintln!("BLAD generowania: {}", e);
            eprintln!("Szczegoly: {:?}", e);
            return;
        }
    }

    // Test 5: Drugie pytanie
    println!("--- Test 5: Drugie pytanie ---");
    let messages2 = vec![
        ChatMessage { role: "system".into(), content: "Jestes pomocnym polskim asystentem AI.".into() },
        ChatMessage { role: "user".into(), content: "Jakie sa pory roku w Polsce?".into() },
    ];
    let formatted2 = template.format_messages(&messages2, true);
    println!("Prompt:\n{}\n", formatted2);

    let params2 = tentaflow_core::inference::GenerateParams {
        prompt: formatted2,
        max_tokens: 150,
        temperature: 0.2,
        top_p: 0.95,
        top_k: 0,
        repeat_penalty: 1.0,
        stop_sequences: template.stop_tokens(),
        system_prompt: None,
    };

    match engine.generate(params2).await {
        Ok(result) => {
            println!("Odpowiedz Bielika:");
            println!("---");
            println!("{}", result.text);
            println!("---");
            println!("Wygenerowano: {} tokenow", result.tokens_generated);
            println!("Szybkosc: {:.1} tok/s", result.tokens_per_second);
            println!("Powod zatrzymania: {:?}", result.stop_reason);
        }
        Err(e) => {
            eprintln!("BLAD generowania: {}", e);
            eprintln!("Szczegoly: {:?}", e);
        }
    }

    println!("\n=== Koniec testow ===");
}
