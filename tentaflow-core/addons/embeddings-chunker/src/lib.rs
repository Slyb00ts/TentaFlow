// =============================================================================
// Plik: addons/embeddings-chunker/src/lib.rs
// Opis: Addon proxy do embeddingów — dzieli tekst na chunki, generuje wektory
//       przez host function llm_generate z modelem Jina Embeddings v5.
// =============================================================================

use tentaflow_addon_sdk::prelude::*;

// =============================================================================
// Bindingi do llm_generate z pelnym ABI (model + opcje)
// =============================================================================

#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn llm_generate(
        prompt_ptr: i32, prompt_len: i32,
        model_ptr: i32, model_len: i32,
        options_ptr: i32, options_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

/// Rozmiar bufora na odpowiedzi z hosta
const RESPONSE_BUFFER_SIZE: usize = 262_144; // 256KB — wektory moga byc duze

// =============================================================================
// Konfiguracja addonu
// =============================================================================

/// Konfiguracja embeddingów ladowana z addon storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingConfig {
    embedding_model: String,
    chunk_size: usize,
    chunk_overlap: usize,
    embedding_dimensions: usize,
    task_adapter: String,
    batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            embedding_model: "jina-embeddings-v5-text-small".to_string(),
            chunk_size: 512,
            chunk_overlap: 50,
            embedding_dimensions: 1024,
            task_adapter: "retrieval".to_string(),
            batch_size: 32,
        }
    }
}

/// Laduje konfiguracje z addon storage, fallback na domyslne wartosci
fn load_config() -> EmbeddingConfig {
    let mut config = EmbeddingConfig::default();

    if let Ok(Some(val)) = store_get("embedding_model") {
        if !val.is_empty() {
            config.embedding_model = val;
        }
    }
    if let Ok(Some(val)) = store_get("chunk_size") {
        if let Ok(n) = val.parse::<usize>() {
            config.chunk_size = n;
        }
    }
    if let Ok(Some(val)) = store_get("chunk_overlap") {
        if let Ok(n) = val.parse::<usize>() {
            config.chunk_overlap = n;
        }
    }
    if let Ok(Some(val)) = store_get("embedding_dimensions") {
        if let Ok(n) = val.parse::<usize>() {
            config.embedding_dimensions = n;
        }
    }
    if let Ok(Some(val)) = store_get("task_adapter") {
        if !val.is_empty() {
            config.task_adapter = val;
        }
    }
    if let Ok(Some(val)) = store_get("batch_size") {
        if let Ok(n) = val.parse::<usize>() {
            config.batch_size = n;
        }
    }

    config
}

// =============================================================================
// Lifecycle hooks
// =============================================================================

/// Instalacja — loguje informacje
#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("embeddings-chunker zainstalowany");
    0
}

/// Uruchomienie — laduje konfiguracje z storage i loguje parametry
#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    let config = load_config();
    log::info(&format!(
        "embeddings-chunker uruchomiony: model={}, chunk_size={}, overlap={}, dims={}, adapter={}",
        config.embedding_model,
        config.chunk_size,
        config.chunk_overlap,
        config.embedding_dimensions,
        config.task_adapter
    ));
    0
}

/// Zatrzymanie addonu
#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("embeddings-chunker zatrzymany");
    0
}

/// Obsluga eventow — ignorowana
#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

// =============================================================================
// Dispatcher narzedzi — on_request
// =============================================================================

/// Glowny punkt wejscia dla wywolan narzedzi.
/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
/// Input JSON: {"tool": "nazwa", "params": {...}}
#[no_mangle]
pub extern "C" fn on_request(
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let input_json = read_string(input_ptr, input_len);

    let request: Value = match serde_json::from_str(&input_json) {
        Ok(v) => v,
        Err(e) => {
            let error = json!({"ok": false, "error": format!("Blad parsowania requestu: {}", e)});
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request
        .get("tool")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let config = load_config();

    let result = match tool_name {
        "embed_text" => handle_embed_text(&params, &config),
        "embed_chunks" => handle_embed_chunks(&params, &config),
        "embed_batch" => handle_embed_batch(&params, &config),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Handlery narzedzi
// =============================================================================

/// Generuje embedding dla pojedynczego tekstu
fn handle_embed_text(params: &Value, config: &EmbeddingConfig) -> Value {
    let text = match params.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return json!({"ok": false, "error": "Brak wymaganego parametru 'text'"}),
    };

    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("document");

    let task = params
        .get("task")
        .and_then(|v| v.as_str())
        .unwrap_or(&config.task_adapter);

    match generate_embedding(text, mode, task, config) {
        Ok(vector) => {
            json!({
                "ok": true,
                "data": {
                    "vector": vector,
                    "dimensions": config.embedding_dimensions,
                    "mode": mode,
                    "task": task
                }
            })
        }
        Err(e) => json!({"ok": false, "error": e}),
    }
}

/// Dzieli tekst na chunki i generuje embedding dla kazdego
fn handle_embed_chunks(params: &Value, config: &EmbeddingConfig) -> Value {
    let text = match params.get("text").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return json!({"ok": false, "error": "Brak wymaganego parametru 'text'"}),
    };

    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("document");

    let chunk_size = params
        .get("chunk_size")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(config.chunk_size);

    let chunk_overlap = params
        .get("chunk_overlap")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(config.chunk_overlap);

    let chunks = split_into_chunks(text, chunk_size, chunk_overlap);
    let total_chunks = chunks.len();
    let mut results = Vec::with_capacity(total_chunks);

    for (index, chunk_text) in chunks.iter().enumerate() {
        match generate_embedding(chunk_text, mode, &config.task_adapter, config) {
            Ok(vector) => {
                results.push(json!({
                    "text": chunk_text,
                    "vector": vector,
                    "index": index
                }));
            }
            Err(e) => {
                return json!({
                    "ok": false,
                    "error": format!("Blad generowania embeddingu dla chunka {}: {}", index, e),
                    "partial_chunks": results,
                    "failed_at_index": index
                });
            }
        }
    }

    json!({
        "ok": true,
        "data": {
            "chunks": results,
            "total_chunks": total_chunks
        }
    })
}

/// Generuje embeddingi dla tablicy tekstow (bez chunkowania)
fn handle_embed_batch(params: &Value, config: &EmbeddingConfig) -> Value {
    let texts = match params.get("texts").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return json!({"ok": false, "error": "Brak wymaganego parametru 'texts' (tablica)"}),
    };

    let mode = params
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("document");

    let mut embeddings = Vec::with_capacity(texts.len());

    for (index, text_val) in texts.iter().enumerate() {
        let text = match text_val.as_str() {
            Some(t) => t,
            None => {
                return json!({
                    "ok": false,
                    "error": format!("Element {} w tablicy 'texts' nie jest stringiem", index)
                });
            }
        };

        match generate_embedding(text, mode, &config.task_adapter, config) {
            Ok(vector) => {
                embeddings.push(json!({
                    "text": text,
                    "vector": vector
                }));
            }
            Err(e) => {
                return json!({
                    "ok": false,
                    "error": format!("Blad generowania embeddingu dla tekstu {}: {}", index, e),
                    "partial_embeddings": embeddings,
                    "failed_at_index": index
                });
            }
        }
    }

    let count = embeddings.len();
    json!({
        "ok": true,
        "data": {
            "embeddings": embeddings,
            "count": count
        }
    })
}

// =============================================================================
// Generowanie embeddingu — wywolanie llm_generate z pelnym ABI
// =============================================================================

/// Wywoluje llm_generate z parametrami specyficznymi dla embeddingów.
/// Dodaje prefix Query:/Document: i opcje adaptera LoRA.
fn generate_embedding(
    text: &str,
    mode: &str,
    task: &str,
    config: &EmbeddingConfig,
) -> Result<Vec<f64>, String> {
    // Dodaj prefix w zaleznosci od trybu (asymetryczny retrieval)
    let prefixed_text = match mode {
        "query" => format!("Query: {}", text),
        _ => format!("Document: {}", text),
    };

    // Opcje dla llm_generate — Core routuje do serwisu embeddingów
    let options = json!({
        "task": "embedding",
        "dimensions": config.embedding_dimensions,
        "adapter": task
    });
    let options_str = serde_json::to_string(&options)
        .map_err(|e| format!("Blad serializacji opcji: {}", e))?;

    let prompt_bytes = prefixed_text.as_bytes();
    let model_bytes = config.embedding_model.as_bytes();
    let options_bytes = options_str.as_bytes();
    let mut buffer = vec![0u8; RESPONSE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let result_code = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32,
            prompt_bytes.len() as i32,
            model_bytes.as_ptr() as i32,
            model_bytes.len() as i32,
            options_bytes.as_ptr() as i32,
            options_bytes.len() as i32,
            buffer.as_mut_ptr() as i32,
            RESPONSE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };

    if result_code < 0 {
        return Err(format!("llm_generate zwrocil blad: {}", result_code));
    }

    if out_len <= 0 {
        return Err("llm_generate zwrocil pusta odpowiedz".to_string());
    }

    let response_str = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();

    // Parsuj odpowiedz — oczekujemy JSON z wektorem
    parse_embedding_response(&response_str)
}

/// Parsuje odpowiedz z llm_generate — wyciaga wektor embeddingu.
/// Obsluguje formaty: tablica floatow, obiekt z polem "embedding"/"vector"/"data".
fn parse_embedding_response(response: &str) -> Result<Vec<f64>, String> {
    let parsed: Value = serde_json::from_str(response)
        .map_err(|e| format!("Blad parsowania odpowiedzi embeddingu: {}", e))?;

    // Przypadek 1: odpowiedz to bezposrednio tablica floatow
    if let Some(arr) = parsed.as_array() {
        return extract_float_array(arr);
    }

    // Przypadek 2: obiekt z polem "embedding"
    if let Some(arr) = parsed.get("embedding").and_then(|v| v.as_array()) {
        return extract_float_array(arr);
    }

    // Przypadek 3: obiekt z polem "vector"
    if let Some(arr) = parsed.get("vector").and_then(|v| v.as_array()) {
        return extract_float_array(arr);
    }

    // Przypadek 4: obiekt z polem "data" -> pierwszy element -> "embedding"
    if let Some(data) = parsed.get("data") {
        if let Some(arr) = data.as_array() {
            if let Some(first) = arr.first() {
                if let Some(emb) = first.get("embedding").and_then(|v| v.as_array()) {
                    return extract_float_array(emb);
                }
            }
        }
        // "data" jest bezposrednio tablica floatow
        if let Some(arr) = data.as_array() {
            if arr.first().and_then(|v| v.as_f64()).is_some() {
                return extract_float_array(arr);
            }
        }
    }

    Err(format!(
        "Nie udalo sie wyciagnac wektora z odpowiedzi: {}",
        &response[..response.len().min(200)]
    ))
}

/// Konwertuje tablice JSON na Vec<f64>
fn extract_float_array(arr: &[Value]) -> Result<Vec<f64>, String> {
    let mut result = Vec::with_capacity(arr.len());
    for (i, val) in arr.iter().enumerate() {
        match val.as_f64() {
            Some(f) => result.push(f),
            None => return Err(format!("Element {} wektora nie jest liczba: {}", i, val)),
        }
    }
    Ok(result)
}

// =============================================================================
// Algorytm chunkowania tekstu
// =============================================================================

/// Dzieli tekst na chunki po zdaniach z overlap.
/// Przyblizona tokenizacja: 1 token ~ 4 znaki.
fn split_into_chunks(text: &str, chunk_size_tokens: usize, overlap_tokens: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    // Zamien rozmiary tokenowe na przyblizone znakowe (1 token ~ 4 znaki)
    let chunk_size_chars = chunk_size_tokens * 4;
    let overlap_chars = overlap_tokens * 4;

    // Rozdziel tekst na zdania
    let sentences = split_into_sentences(text);

    if sentences.is_empty() {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current_chunk = String::new();
    let mut sentence_index = 0;

    while sentence_index < sentences.len() {
        let sentence = &sentences[sentence_index];

        // Jesli dodanie zdania nie przekroczy limitu — dodaj
        if current_chunk.len() + sentence.len() <= chunk_size_chars || current_chunk.is_empty() {
            if !current_chunk.is_empty() && !current_chunk.ends_with(' ') {
                current_chunk.push(' ');
            }
            current_chunk.push_str(sentence);
            sentence_index += 1;
        } else {
            // Chunk pelny — zapisz i zacznij nowy z overlap
            chunks.push(current_chunk.trim().to_string());

            // Overlap: wez koniec poprzedniego chunka
            let prev_chunk = chunks.last().unwrap();
            if overlap_chars > 0 && prev_chunk.len() > overlap_chars {
                let overlap_start = prev_chunk.len() - overlap_chars;
                // Zacznij od granicy slowa
                let adjusted_start = prev_chunk[overlap_start..]
                    .find(' ')
                    .map(|pos| overlap_start + pos + 1)
                    .unwrap_or(overlap_start);
                current_chunk = prev_chunk[adjusted_start..].to_string();
            } else {
                current_chunk = String::new();
            }
        }
    }

    // Ostatni chunk
    let trimmed = current_chunk.trim().to_string();
    if !trimmed.is_empty() {
        chunks.push(trimmed);
    }

    // Jesli nie udalo sie podzielic — zwroc calosc
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }

    chunks
}

/// Rozdziela tekst na zdania po znakach konca zdania (. ! ? \n\n)
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // Podwojny newline — granica akapitu
        if ch == '\n' && i + 1 < len && chars[i + 1] == '\n' {
            if !current.trim().is_empty() {
                current.push(ch);
                current.push(chars[i + 1]);
                sentences.push(current.trim().to_string());
                current = String::new();
            }
            i += 2;
            continue;
        }

        current.push(ch);

        // Koniec zdania: . ! ? po ktorych jest spacja lub koniec tekstu
        if (ch == '.' || ch == '!' || ch == '?')
            && (i + 1 >= len || chars[i + 1] == ' ' || chars[i + 1] == '\n')
        {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
                current = String::new();
            }
        }

        i += 1;
    }

    // Reszta tekstu
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

    sentences
}

// =============================================================================
// Helpery — zapis odpowiedzi do bufora wyjsciowego
// =============================================================================

/// Zapisuje odpowiedz JSON do bufora wyjsciowego i ustawia dlugosc
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };

    let written = write_string(out_ptr, out_cap, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz");
        return 2;
    }

    // Zapisz dlugosc odpowiedzi (4 bajty little-endian)
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);

    0
}
