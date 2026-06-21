// =============================================================================
// Plik: addons/rag/src/lib.rs
// Opis: Addon RAG (WASM). Fundament: kolekcje + pipeline INGESTU dokumentow.
//       Ingest: doc_parse (alias rag-parse) -> chunking markdown -> embedding
//       (alias rag-embeddings via llm_generate) -> vector_upsert("passages") +
//       zapis chunkow i statusu do per-instance SQLite. Query-flow i bogate GUI
//       to osobne slice'y. Komentarze po polsku.
// =============================================================================

use tentaflow_addon_sdk::prelude::*;
use tentaflow_addon_sdk::{
    doc_parse, document_get, vector_delete, vector_upsert, VectorField, VectorFieldValue,
};

// Niskopoziomowy binding llm_generate z pelnym ABI (model + opcje) — dokladnie
// ten sam mechanizm embeddingow co addon embeddings-chunker (Core routuje na
// serwis embeddingow gdy options.task == "embedding").
#[link(wasm_import_module = "tentaflow")]
extern "C" {
    fn llm_generate(
        prompt_ptr: i32, prompt_len: i32,
        model_ptr: i32, model_len: i32,
        options_ptr: i32, options_len: i32,
        out_ptr: i32, out_cap: i32, out_len_ptr: i32,
    ) -> i32;
}

/// Bufor na odpowiedz embeddingu (wektor 1024 f32 jako JSON to ~kilkadziesiat KB).
const EMBED_BUFFER_SIZE: usize = 262_144;

/// Nazwa przestrzeni wektorowej (zgodna z [[vector_namespace]] w manifescie).
const PASSAGES_NS: &str = "passages";

/// Wymiar wektora (zgodny z manifestem i suggested_default rag-embeddings).
const EMBED_DIMENSIONS: usize = 1024;

/// Domyslny rozmiar chunku w znakach i overlap (chunking po akapitach/zdaniach).
/// ~512 tokenow * ~4 znaki/token.
const CHUNK_SIZE_CHARS: usize = 2048;
const CHUNK_OVERLAP_CHARS: usize = 200;

// =============================================================================
// Lifecycle
// =============================================================================

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("rag: zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    // Rejestracja narzedzi ingestu dla LLM tool-callingu.
    register_tool(
        "create_collection",
        "Tworzy nowa kolekcje dokumentow w instancji RAG.",
        json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
    );
    register_tool(
        "ask",
        "Zadaje pytanie do kolekcji RAG: retrieval (embeddings -> vector search) -> kontekst -> LLM z cytatami. Zwraca odpowiedz i liste cytowanych pasazy.",
        json!({
            "type": "object",
            "properties": {
                "collection_id": {"type": "string"},
                "question": {"type": "string"},
                "top_k": {"type": "integer"}
            },
            "required": ["collection_id", "question"]
        }),
    );
    register_tool(
        "ingest_document",
        "Ingest dokumentu: parse -> chunk -> embedding -> upsert wektorow.",
        json!({
            "type": "object",
            "properties": {
                "collection_id": {"type": "string"},
                "doc_id_blob": {"type": "string"},
                "filename": {"type": "string"},
                "mime": {"type": "string"}
            },
            "required": ["collection_id", "doc_id_blob", "filename", "mime"]
        }),
    );

    // Minimalny panel — lista kolekcji renderowana z SQL. Pelne GUI to osobny slice.
    if let Err(e) = render_main_panel() {
        log::warn(&format!("rag: nie udalo sie wyrenderowac panelu: {e}"));
    }

    log::info("rag: uruchomiony");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("rag: zatrzymany");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

// =============================================================================
// Dispatcher narzedzi
// =============================================================================

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
            let error = json!({"ok": false, "error": format!("Blad parsowania requestu: {e}")});
            return write_response(out_ptr, out_cap, out_len_ptr, &error);
        }
    };

    let tool_name = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "create_collection" => handle_create_collection(&params),
        "list_collections" => handle_list_collections(),
        "ask" => handle_ask(&params),
        "ingest_document" => handle_ingest_document(&params),
        "list_documents" => handle_list_documents(&params),
        "ingest_status" => handle_ingest_status(&params),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {tool_name}")}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

// =============================================================================
// Handlery narzedzi
// =============================================================================

/// Tworzy kolekcje. Zwraca jej id.
fn handle_create_collection(params: &Value) -> Value {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim(),
        _ => return err("Brak wymaganego parametru 'name'"),
    };

    let id = new_id("col");
    let now = now_unix();
    match sql_exec(
        "INSERT INTO collections (id, name, created_at) VALUES (?, ?, ?)",
        &[
            SqlValue::String(id.clone()),
            SqlValue::String(name.to_string()),
            SqlValue::I64(now),
        ],
    ) {
        Ok(_) => json!({"ok": true, "data": {"collection_id": id, "name": name}}),
        Err(AbiError::SqlConstraint) => err("Kolekcja o tej nazwie juz istnieje"),
        Err(e) => err(&format!("Blad zapisu kolekcji: {e}")),
    }
}

/// Lista kolekcji z liczba dokumentow.
fn handle_list_collections() -> Value {
    let rows = match sql_query(
        "SELECT c.id, c.name, c.created_at, \
         (SELECT COUNT(*) FROM documents d WHERE d.collection_id = c.id) \
         FROM collections c ORDER BY c.created_at DESC",
        &[],
    ) {
        Ok(r) => r,
        Err(e) => return err(&format!("Blad odczytu kolekcji: {e}")),
    };

    let collections: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.first().and_then(|v| v.as_str()).unwrap_or(""),
                "name": row.get(1).and_then(|v| v.as_str()).unwrap_or(""),
                "created_at": row.get(2).and_then(|v| v.as_i64()).unwrap_or(0),
                "document_count": row.get(3).and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })
        .collect();

    json!({"ok": true, "data": {"collections": collections}})
}

/// Klucz instancyjnego KV, pod ktorym Core (przy install) zapisuje nazwe
/// published query-flow tej instancji. Addon nie zna wlasnego addon_id, wiec
/// odczytuje gotowa nazwe modelu stad.
const ENGINE_FLOW_STATE_KEY: &str = "engine_flow_model";

/// Bufor na odpowiedz query-flow (odpowiedz LLM + kontekst moga byc spore).
const ASK_BUFFER_SIZE: usize = 262_144;

/// `ask(collection_id, question, top_k?)` — wyzwala query-flow JAKO MODEL przez
/// llm_generate(model=<published name tej instancji>). Flow robi retrieval
/// (embeddings -> vector search w 'passages') -> kontekst -> LLM z cytatami.
/// Zwraca `{answer, citations:[{doc_id, chunk_index, text, score}]}`.
///
/// `collection_id`/`top_k` jada w options jako podpowiedz dla flow (filtr po
/// kolekcji i rozmiar retrievalu). v1 query-flow szuka po calej przestrzeni
/// instancji; filtr po collection_id wejdzie razem z parametryzacja vector node
/// z meta (pole jest juz indeksowane w 'passages').
fn handle_ask(params: &Value) -> Value {
    let collection_id = match params.get("collection_id").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c,
        _ => return err("Brak wymaganego parametru 'collection_id'"),
    };
    let question = match params.get("question").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim(),
        _ => return err("Brak wymaganego parametru 'question'"),
    };
    let top_k = params.get("top_k").and_then(|v| v.as_i64()).filter(|n| *n > 0);

    // Kolekcja musi istniec (czytelny blad zamiast pustego retrievalu).
    match sql_query_one(
        "SELECT id FROM collections WHERE id = ?",
        &[SqlValue::String(collection_id.to_string())],
    ) {
        Ok(Some(_)) => {}
        Ok(None) => return err("Kolekcja nie istnieje"),
        Err(e) => return err(&format!("Blad weryfikacji kolekcji: {e}")),
    }

    // Nazwa published query-flow (zapisana przez Core przy install instancji).
    let model = match state_get(ENGINE_FLOW_STATE_KEY) {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(s) if !s.is_empty() => s,
            _ => return err("Nazwa query-flow w stanie instancji jest nieprawidlowa"),
        },
        Ok(None) => {
            return err("Query-flow nie jest zarejestrowany dla tej instancji (brak engine_flow_model w stanie)")
        }
        Err(e) => return err(&format!("Blad odczytu stanu instancji: {e:?}")),
    };

    // Wyzwol flow JAKO MODEL. Pytanie jest promptem (trigger.text -> embeddings),
    // a collection_id/top_k jada w options — Core przeprowadza je do envelope.meta,
    // wiec vector node FILTRUJE retrieval po tej kolekcji (izolacja per-kolekcja).
    let mut options = json!({ "collection_id": collection_id });
    if let Some(k) = top_k {
        options["top_k"] = json!(k);
    }
    // Flow zwraca JSON `{answer, citations}` w tresci odpowiedzi: answer to tekst
    // LLM, citations to REALNE hity retrievalu (doc_id/chunk_index/text/score)
    // zebrane przez output node z meta vector node. Cytaty = dokladnie to, co
    // retrieval zwrocil — zero zmyslania, zero osobnego SELECT-a.
    let (answer, citations) = match call_query_flow(&model, question, &options) {
        Ok(a) => a,
        Err(e) => return err(&e),
    };

    json!({
        "ok": true,
        "data": {
            "answer": answer,
            "citations": citations
        }
    })
}

/// Wywoluje query-flow przez host llm_generate i zwraca `(odpowiedz, cytaty)`.
/// Tresc flow to JSON `{answer, citations}` (output node z emit_citations);
/// gdy flow zwroci goly tekst (np. inna konfiguracja), cytaty sa puste.
fn call_query_flow(model: &str, question: &str, options: &Value) -> Result<(String, Vec<Value>), String> {
    let options_str =
        serde_json::to_string(options).map_err(|e| format!("Blad serializacji opcji: {e}"))?;

    let prompt_bytes = question.as_bytes();
    let model_bytes = model.as_bytes();
    let options_bytes = options_str.as_bytes();
    let mut buffer = vec![0u8; ASK_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32, prompt_bytes.len() as i32,
            model_bytes.as_ptr() as i32, model_bytes.len() as i32,
            options_bytes.as_ptr() as i32, options_bytes.len() as i32,
            buffer.as_mut_ptr() as i32, ASK_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if rc < 0 {
        return Err(format!("Wyzwolenie query-flow ({model}) zwrocilo blad: {rc}"));
    }
    if out_len <= 0 {
        return Err("Query-flow zwrocil pusta odpowiedz".to_string());
    }

    let raw = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    Ok(parse_flow_response(&raw))
}

/// Parsuje odpowiedz flow na `(answer, citations)`. Tresc to JSON
/// `{answer, citations}` (output node RAG z emit_citations) — moze przyjsc
/// bezposrednio albo zagniezdzona w chat-completion (`choices[0].message.content`
/// jako string z tym JSON-em). Fallbacki: goly tekst / inne ksztalty => answer
/// = tekst, citations puste.
fn parse_flow_response(raw: &str) -> (String, Vec<Value>) {
    // 1. Wyciagnij wewnetrzna tresc: chat-completion content albo cala odpowiedz.
    let inner = chat_completion_content(raw).unwrap_or_else(|| raw.trim().to_string());

    // 2. Tresc to JSON {answer, citations}? Wyciagnij oba pola.
    if let Ok(v) = serde_json::from_str::<Value>(&inner) {
        if let Some(answer) = v.get("answer").and_then(|x| x.as_str()) {
            let citations = v
                .get("citations")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            return (answer.to_string(), citations);
        }
        // Inny ksztalt JSON ze znanym polem tekstowym — bez cytatow.
        for key in ["content", "text"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                return (s.to_string(), Vec::new());
            }
        }
    }

    // 3. Goly tekst — answer = tresc, brak cytatow.
    (inner, Vec::new())
}

/// Gdy `raw` to chat-completion, zwraca `choices[0].message.content` jako string.
/// W p.p. `None` (caller uzyje calego `raw`).
fn chat_completion_content(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

/// Pelny pipeline ingestu jednego dokumentu.
fn handle_ingest_document(params: &Value) -> Value {
    let collection_id = match params.get("collection_id").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c,
        _ => return err("Brak wymaganego parametru 'collection_id'"),
    };
    let doc_id_blob = match params.get("doc_id_blob").and_then(|v| v.as_str()) {
        Some(d) if !d.is_empty() => d,
        _ => return err("Brak wymaganego parametru 'doc_id_blob'"),
    };
    let filename = params.get("filename").and_then(|v| v.as_str()).unwrap_or("dokument");
    let mime = match params.get("mime").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => m,
        _ => return err("Brak wymaganego parametru 'mime'"),
    };

    // Kolekcja musi istniec.
    match sql_query_one("SELECT id FROM collections WHERE id = ?", &[SqlValue::String(collection_id.to_string())]) {
        Ok(Some(_)) => {}
        Ok(None) => return err("Kolekcja nie istnieje"),
        Err(e) => return err(&format!("Blad weryfikacji kolekcji: {e}")),
    }

    let document_id = new_id("doc");
    let job_id = new_id("job");
    let now = now_unix();

    // Wpis dokumentu (status pending) + job (queued) atomowo.
    let insert_doc = "INSERT INTO documents (id, collection_id, doc_id_blob, filename, mime, status, page_count, created_at) \
                      VALUES (?, ?, ?, ?, ?, 'pending', 0, ?)";
    let insert_job = "INSERT INTO ingest_jobs (id, document_id, status, progress, created_at, updated_at) \
                      VALUES (?, ?, 'running', 0, ?, ?)";
    if let Err(e) = sql_transaction(&[
        (insert_doc, &[
            SqlValue::String(document_id.clone()),
            SqlValue::String(collection_id.to_string()),
            SqlValue::String(doc_id_blob.to_string()),
            SqlValue::String(filename.to_string()),
            SqlValue::String(mime.to_string()),
            SqlValue::I64(now),
        ]),
        (insert_job, &[
            SqlValue::String(job_id.clone()),
            SqlValue::String(document_id.clone()),
            SqlValue::I64(now),
            SqlValue::I64(now),
        ]),
    ]) {
        return err(&format!("Blad inicjalizacji ingestu: {e}"));
    }

    // Wykonaj pipeline; przy bledzie zapisz go do joba i dokumentu.
    match run_ingest_pipeline(collection_id, &document_id, &job_id, doc_id_blob, mime) {
        Ok(chunk_count) => {
            // Status nie moze klamac: jesli nie da sie oznaczyc dokumentu/joba jako
            // ukonczony, to realny blad — wyczysc artefakty i zglos failed zamiast
            // udawac sukces przy niespojnym statusie.
            if let Err(e) = mark_ingested(&document_id, &job_id) {
                cleanup_document_artifacts(&document_id, &[]);
                let msg = format!("Ingest zakonczony, ale zapis statusu sie nie powiodl: {e}");
                fail_job(&document_id, &job_id, &msg);
                return json!({
                    "ok": false,
                    "error": msg,
                    "data": {"document_id": document_id, "job_id": job_id, "status": "failed"}
                });
            }
            json!({
                "ok": true,
                "data": {
                    "document_id": document_id,
                    "job_id": job_id,
                    "chunks": chunk_count,
                    "status": "ingested"
                }
            })
        }
        Err(msg) => {
            fail_job(&document_id, &job_id, &msg);
            json!({
                "ok": false,
                "error": msg,
                "data": {"document_id": document_id, "job_id": job_id, "status": "failed"}
            })
        }
    }
}

/// Realny pipeline: pobranie pliku -> parse/text -> chunking -> embedding ->
/// vector_upsert + zapis chunkow. Zwraca liczbe chunkow albo komunikat bledu.
///
/// Spojnosc: kazdy krok ktory moze cze ciowo zapisac artefakty (chunki/wektory)
/// jest opakowany tak, by przy bledzie skasowac WSZYSTKIE artefakty TEGO dokumentu
/// PRZED zwroceniem bledu (cleanup-on-failure). Lista upsertowanych ref_id jest
/// zbierana w trakcie, dzieki czemu po failu zostaje zero orphan wektorow i zero
/// chunkow wskazujacych nieistniejacy wektor.
fn run_ingest_pipeline(
    collection_id: &str,
    document_id: &str,
    job_id: &str,
    doc_id_blob: &str,
    mime: &str,
) -> Result<usize, String> {
    // Re-ingest tego samego document_id: czysty start — kasujemy ewentualne
    // wczesniejsze artefakty zanim cokolwiek zapiszemy (cleanup-then-reingest).
    cleanup_document_artifacts(document_id, &[]);

    // 1. Pobierz surowe bajty pliku z document store.
    let (bytes, _stored_mime) =
        document_get(doc_id_blob).map_err(|e| format!("Blad pobrania pliku ({doc_id_blob}): {e}"))?;
    if bytes.is_empty() {
        return Err("Plik zrodlowy jest pusty".to_string());
    }
    update_progress(job_id, 10);

    // 2. Klasyfikacja MIME wg allowlisty. Nieobslugiwane typy (DOCX/binarka/
    //    nieznane) odrzucamy czytelnym bledem zamiast ingestowac jako smieci UTF-8.
    let kind = classify_mime(mime)
        .ok_or_else(|| format!("Nieobslugiwany typ pliku (mime): {mime}"))?;
    let (markdown, page_count) = match kind {
        SourceKind::Parse => {
            let parsed = doc_parse(&bytes, mime, None)
                .map_err(|e| format!("Blad parsowania dokumentu: {e}"))?;
            (parsed.markdown, parsed.page_count.max(1))
        }
        SourceKind::Text => (String::from_utf8_lossy(&bytes).to_string(), 1),
    };
    let _ = sql_exec(
        "UPDATE documents SET page_count = ? WHERE id = ?",
        &[SqlValue::I64(page_count as i64), SqlValue::String(document_id.to_string())],
    );
    update_progress(job_id, 30);

    if markdown.trim().is_empty() {
        return Err("Parsowanie nie zwrocilo tekstu".to_string());
    }

    // 3. Chunking markdown (po akapitach/zdaniach z overlap).
    let chunks = split_into_chunks(&markdown, CHUNK_SIZE_CHARS, CHUNK_OVERLAP_CHARS);
    if chunks.is_empty() {
        return Err("Chunking nie wyprodukowal zadnego chunka".to_string());
    }
    let total = chunks.len();
    let now = now_unix();

    // 4. Dla kazdego chunku: embedding -> walidacja dim -> INSERT chunka (rowid =
    //    ref_id) -> vector_upsert -> UPDATE vector_ref (ze sprawdzeniem wyniku).
    //    Wszystkie upsertowane ref_id zbieramy, by po failu wyczyscic wektory.
    let mut upserted: Vec<u64> = Vec::with_capacity(total);

    for (index, chunk_text) in chunks.iter().enumerate() {
        let step = ingest_one_chunk(collection_id, document_id, index, chunk_text, now, &mut upserted);
        if let Err(msg) = step {
            // Cleanup-on-failure: skasuj wszystkie wektory + chunki tego dokumentu.
            cleanup_document_artifacts(document_id, &upserted);
            return Err(format!("Blad chunka {index}: {msg}"));
        }

        // Postep 30..95% rozlozony na chunki.
        let progress = 30 + ((index + 1) * 65 / total) as i64;
        update_progress(job_id, progress.min(95));
    }

    Ok(total)
}

/// Przetwarza pojedynczy chunk: embedding -> walidacja dim (w parse) -> INSERT
/// chunka -> vector_upsert -> UPDATE vector_ref. Po udanym upsercie dopisuje ref_id
/// do `upserted`, by cleanup-on-failure mogl go skasowac. Bledy UPDATE/upsertu nie
/// sa lykane — chunk wskazujacy nieistniejacy wektor jest tu niemozliwy.
fn ingest_one_chunk(
    collection_id: &str,
    document_id: &str,
    index: usize,
    chunk_text: &str,
    now: i64,
    upserted: &mut Vec<u64>,
) -> Result<(), String> {
    // Embedding + walidacja wymiaru (parse_embedding_response sprawdza dlugosc).
    let vector = generate_embedding(chunk_text).map_err(|e| format!("embedding: {e}"))?;

    // INSERT chunka z placeholderem vector_ref=0 — rowid (=ref_id) powstaje teraz.
    let exec = sql_exec(
        "INSERT INTO chunks (document_id, collection_id, chunk_index, text, vector_ref, created_at) \
         VALUES (?, ?, ?, ?, 0, ?)",
        &[
            SqlValue::String(document_id.to_string()),
            SqlValue::String(collection_id.to_string()),
            SqlValue::I64(index as i64),
            SqlValue::String(chunk_text.to_string()),
            SqlValue::I64(now),
        ],
    )
    .map_err(|e| format!("zapis chunka: {e}"))?;

    let rowid = exec.last_insert_id;
    if rowid <= 0 {
        // Bez poprawnego rowid nie ma stabilnego ref_id — usun wlasnie wstawiony
        // chunk (gdyby jednak powstal) i przerwij.
        let _ = sql_exec(
            "DELETE FROM chunks WHERE document_id = ? AND chunk_index = ?",
            &[SqlValue::String(document_id.to_string()), SqlValue::I64(index as i64)],
        );
        return Err(format!("INSERT chunka zwrocil nieprawidlowy rowid: {rowid}"));
    }
    let ref_id = rowid as u64;

    // Upsert wektora pod rowid. Przy bledzie usun wlasnie wstawiony chunk, by nie
    // zostawic chunku bez wektora.
    let fields = [
        VectorField { name: "doc_id".to_string(), value: VectorFieldValue::Str(document_id.to_string()) },
        VectorField { name: "chunk_index".to_string(), value: VectorFieldValue::Int(index as i64) },
        VectorField { name: "created_at".to_string(), value: VectorFieldValue::Int(now) },
        // collection_id — pozwala query-flow filtrowac retrieval po kolekcji.
        VectorField { name: "collection_id".to_string(), value: VectorFieldValue::Str(collection_id.to_string()) },
        // text — tresc chunka przy wektorze, by vector search w query-flow zwrocil
        // ja przez output_fields i zbudowal kontekst dla LLM (bez siegania do SQLite).
        VectorField { name: "text".to_string(), value: VectorFieldValue::Str(chunk_text.to_string()) },
    ];
    if let Err(e) = vector_upsert(PASSAGES_NS, ref_id, &vector, &fields) {
        let _ = sql_exec("DELETE FROM chunks WHERE id = ?", &[SqlValue::I64(rowid)]);
        return Err(format!("upsert wektora: {e}"));
    }
    upserted.push(ref_id);

    // UPDATE vector_ref = rowid ze SPRAWDZENIEM wyniku — bez tego chunk moglby
    // wskazywac placeholder 0 przy cichym bledzie.
    let updated = sql_exec(
        "UPDATE chunks SET vector_ref = ? WHERE id = ?",
        &[SqlValue::I64(rowid), SqlValue::I64(rowid)],
    )
    .map_err(|e| format!("aktualizacja vector_ref: {e}"))?;
    if updated.rows_affected == 0 {
        return Err("aktualizacja vector_ref nie objela zadnego wiersza".to_string());
    }

    Ok(())
}

/// Kasuje wszystkie artefakty dokumentu: wektory (po przekazanej liscie ref_id
/// oraz po vector_ref chunkow z bazy) i chunki. Idempotentny — wolany przy
/// czystym starcie (re-ingest) i przy cleanup-on-failure.
fn cleanup_document_artifacts(document_id: &str, known_refs: &[u64]) {
    // Najpierw skasuj wektory po juz-upsertowanych ref_id (znane z biezacego biegu).
    for &ref_id in known_refs {
        if ref_id > 0 {
            let _ = vector_delete(PASSAGES_NS, ref_id);
        }
    }

    // Dodatkowo skasuj wektory po vector_ref zapisanych w chunkach (np. z
    // wczesniejszego ingestu przy re-ingescie) — pokrywa refy spoza known_refs.
    if let Ok(rows) = sql_query(
        "SELECT vector_ref FROM chunks WHERE document_id = ? AND vector_ref > 0",
        &[SqlValue::String(document_id.to_string())],
    ) {
        for row in &rows {
            if let Some(ref_id) = row.first().and_then(|v| v.as_i64()) {
                if ref_id > 0 && !known_refs.contains(&(ref_id as u64)) {
                    let _ = vector_delete(PASSAGES_NS, ref_id as u64);
                }
            }
        }
    }

    // Na koncu usun chunki dokumentu.
    let _ = sql_exec(
        "DELETE FROM chunks WHERE document_id = ?",
        &[SqlValue::String(document_id.to_string())],
    );
}

/// Lista dokumentow w kolekcji.
fn handle_list_documents(params: &Value) -> Value {
    let collection_id = match params.get("collection_id").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c,
        _ => return err("Brak wymaganego parametru 'collection_id'"),
    };

    let rows = match sql_query(
        "SELECT d.id, d.filename, d.mime, d.status, d.page_count, d.created_at, \
         (SELECT COUNT(*) FROM chunks ch WHERE ch.document_id = d.id) \
         FROM documents d WHERE d.collection_id = ? ORDER BY d.created_at DESC",
        &[SqlValue::String(collection_id.to_string())],
    ) {
        Ok(r) => r,
        Err(e) => return err(&format!("Blad odczytu dokumentow: {e}")),
    };

    let documents: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.first().and_then(|v| v.as_str()).unwrap_or(""),
                "filename": row.get(1).and_then(|v| v.as_str()).unwrap_or(""),
                "mime": row.get(2).and_then(|v| v.as_str()).unwrap_or(""),
                "status": row.get(3).and_then(|v| v.as_str()).unwrap_or(""),
                "page_count": row.get(4).and_then(|v| v.as_i64()).unwrap_or(0),
                "created_at": row.get(5).and_then(|v| v.as_i64()).unwrap_or(0),
                "chunk_count": row.get(6).and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })
        .collect();

    json!({"ok": true, "data": {"documents": documents}})
}

/// Status joba ingestu.
fn handle_ingest_status(params: &Value) -> Value {
    let job_id = match params.get("job_id").and_then(|v| v.as_str()) {
        Some(j) if !j.is_empty() => j,
        _ => return err("Brak wymaganego parametru 'job_id'"),
    };

    match sql_query_one(
        "SELECT id, document_id, status, progress, error, updated_at FROM ingest_jobs WHERE id = ?",
        &[SqlValue::String(job_id.to_string())],
    ) {
        Ok(Some(row)) => json!({
            "ok": true,
            "data": {
                "id": row.first().and_then(|v| v.as_str()).unwrap_or(""),
                "document_id": row.get(1).and_then(|v| v.as_str()).unwrap_or(""),
                "status": row.get(2).and_then(|v| v.as_str()).unwrap_or(""),
                "progress": row.get(3).and_then(|v| v.as_i64()).unwrap_or(0),
                "error": row.get(4).and_then(|v| v.as_str()),
                "updated_at": row.get(5).and_then(|v| v.as_i64()).unwrap_or(0),
            }
        }),
        Ok(None) => err("Job nie istnieje"),
        Err(e) => err(&format!("Blad odczytu joba: {e}")),
    }
}

// =============================================================================
// Embeddingi — mechanizm jak embeddings-chunker (llm_generate, task=embedding)
// =============================================================================

/// Generuje embedding chunka przez alias rag-embeddings. Model = nazwa aliasu,
/// Core rozwiazuje go na realny serwis embeddingow; options.task == "embedding"
/// kieruje wywolanie na sciezke embeddingow (identycznie jak embeddings-chunker).
fn generate_embedding(text: &str) -> Result<Vec<f32>, String> {
    let prefixed = format!("Document: {text}");
    let model = "rag-embeddings";
    let options = json!({
        "task": "embedding",
        "dimensions": EMBED_DIMENSIONS,
        "adapter": "retrieval"
    });
    let options_str =
        serde_json::to_string(&options).map_err(|e| format!("Blad serializacji opcji: {e}"))?;

    let prompt_bytes = prefixed.as_bytes();
    let model_bytes = model.as_bytes();
    let options_bytes = options_str.as_bytes();
    let mut buffer = vec![0u8; EMBED_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32, prompt_bytes.len() as i32,
            model_bytes.as_ptr() as i32, model_bytes.len() as i32,
            options_bytes.as_ptr() as i32, options_bytes.len() as i32,
            buffer.as_mut_ptr() as i32, EMBED_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if rc < 0 {
        return Err(format!("llm_generate zwrocil blad: {rc}"));
    }
    if out_len <= 0 {
        return Err("llm_generate zwrocil pusta odpowiedz".to_string());
    }

    let response = String::from_utf8_lossy(&buffer[..out_len as usize]).to_string();
    parse_embedding_response(&response)
}

/// Wyciaga wektor f32 z odpowiedzi embeddingu (tablica floatow albo obiekt z
/// polem embedding/vector/data[0].embedding) i waliduje jego wymiar. Zla dlugosc
/// (!= EMBED_DIMENSIONS = dim namespace 'passages') to twardy blad — wektor o zlym
/// wymiarze nie moze trafic do upsertu (rozjazd z przestrzenia wektorowa).
fn parse_embedding_response(response: &str) -> Result<Vec<f32>, String> {
    let parsed: Value = serde_json::from_str(response)
        .map_err(|e| format!("Blad parsowania odpowiedzi embeddingu: {e}"))?;

    let vector = extract_vector(&parsed).ok_or_else(|| {
        format!(
            "Nie udalo sie wyciagnac wektora z odpowiedzi: {}",
            &response[..response.len().min(200)]
        )
    })??;

    if vector.len() != EMBED_DIMENSIONS {
        return Err(format!(
            "Zly wymiar embeddingu: {} (oczekiwano {EMBED_DIMENSIONS})",
            vector.len()
        ));
    }
    Ok(vector)
}

/// Rozpoznaje ksztalt odpowiedzi i zwraca surowy wektor (bez walidacji wymiaru).
/// `None` = nie rozpoznano ksztaltu; `Some(Err)` = rozpoznano, ale element nie jest
/// liczba.
fn extract_vector(parsed: &Value) -> Option<Result<Vec<f32>, String>> {
    if let Some(arr) = parsed.as_array() {
        return Some(floats_from(arr));
    }
    for key in ["embedding", "vector"] {
        if let Some(arr) = parsed.get(key).and_then(|v| v.as_array()) {
            return Some(floats_from(arr));
        }
    }
    if let Some(data) = parsed.get("data") {
        if let Some(arr) = data.as_array() {
            if let Some(emb) = arr.first().and_then(|f| f.get("embedding")).and_then(|v| v.as_array()) {
                return Some(floats_from(emb));
            }
            if arr.first().and_then(|v| v.as_f64()).is_some() {
                return Some(floats_from(arr));
            }
        }
    }
    None
}

fn floats_from(arr: &[Value]) -> Result<Vec<f32>, String> {
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        match v.as_f64() {
            Some(f) => out.push(f as f32),
            None => return Err(format!("Element {i} wektora nie jest liczba: {v}")),
        }
    }
    if out.is_empty() {
        return Err("Pusty wektor embeddingu".to_string());
    }
    Ok(out)
}

// =============================================================================
// Chunking markdown — po akapitach/zdaniach z overlap (wzor z embeddings-chunker)
// =============================================================================

/// Dzieli tekst na chunki po zdaniach/akapitach z overlap. Granice liczone w
/// znakach (UTF-8 safe — operujemy na zdaniach, nie na bajtach).
fn split_into_chunks(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let sentences = split_into_sentences(text);
    if sentences.is_empty() {
        return vec![text.trim().to_string()];
    }

    // Pojedyncze zdanie/akapit dluzsze niz chunk_size lamiemy twardo PRZED
    // skladaniem, by zaden segment nie przekroczyl chunk_size (nadwymiarowy chunk
    // = ryzyko przekroczenia limitu kontekstu embeddingu). Limit segmentu zmniejszamy
    // o overlap (+1 na spacje laczaca), bo do segmentu doklejany jest ogon overlap
    // poprzedniego chunka — inaczej zlozony chunk moglby przekroczyc chunk_size.
    let seg_max = chunk_size.saturating_sub(overlap + 1).max(1);
    let segments: Vec<String> = sentences
        .iter()
        .flat_map(|s| hard_split(s, seg_max))
        .collect();

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for sentence in &segments {
        if current.chars().count() + sentence.chars().count() <= chunk_size || current.is_empty() {
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
            current.push_str(sentence);
        } else {
            chunks.push(current.trim().to_string());
            current = overlap_tail(chunks.last().unwrap(), overlap);
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
            current.push_str(sentence);
        }
    }

    let tail = current.trim().to_string();
    if !tail.is_empty() {
        chunks.push(tail);
    }
    if chunks.is_empty() {
        chunks.push(text.trim().to_string());
    }
    chunks
}

/// Twardo lamie nadwymiarowy segment na kawalki <= chunk_size (granice liczone w
/// znakach, UTF-8 safe). Segmenty miesczace sie w limicie zwraca bez zmian.
fn hard_split(segment: &str, chunk_size: usize) -> Vec<String> {
    if chunk_size == 0 || segment.chars().count() <= chunk_size {
        return vec![segment.to_string()];
    }
    let chars: Vec<char> = segment.chars().collect();
    chars
        .chunks(chunk_size)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

/// Zwraca ogon poprzedniego chunka (overlap) zaczynajacy sie od granicy slowa.
fn overlap_tail(prev: &str, overlap: usize) -> String {
    if overlap == 0 {
        return String::new();
    }
    let chars: Vec<char> = prev.chars().collect();
    if chars.len() <= overlap {
        return prev.to_string();
    }
    let start = chars.len() - overlap;
    let tail: String = chars[start..].iter().collect();
    // Przesun do granicy slowa, by nie urywac w polowie wyrazu.
    match tail.find(' ') {
        Some(pos) => tail[pos + 1..].to_string(),
        None => tail,
    }
}

/// Rozdziela tekst na zdania po (. ! ?) i granicach akapitow (podwojny newline).
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];
        if ch == '\n' && i + 1 < len && chars[i + 1] == '\n' {
            if !current.trim().is_empty() {
                sentences.push(current.trim().to_string());
                current.clear();
            }
            i += 2;
            continue;
        }
        current.push(ch);
        if (ch == '.' || ch == '!' || ch == '?')
            && (i + 1 >= len || chars[i + 1] == ' ' || chars[i + 1] == '\n')
        {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
                current.clear();
            }
        }
        i += 1;
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }
    sentences
}

// =============================================================================
// Panel UI — minimalny (lista kolekcji). Pelne GUI to osobny slice.
// =============================================================================

/// Renderuje minimalny panel: naglowek + lista nazw kolekcji.
fn render_main_panel() -> Result<(), String> {
    let names: Vec<String> = sql_query("SELECT name FROM collections ORDER BY created_at DESC", &[])
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let items: Vec<Value> = names
        .iter()
        .map(|n| json!({"type": "text", "props": {"text": n}}))
        .collect();

    let panel = json!({
        "type": "stack",
        "props": {"direction": "vertical", "gap": "md"},
        "children": [
            {"type": "text", "props": {"text": "RAG — kolekcje", "variant": "heading"}},
            {"type": "stack", "props": {"direction": "vertical", "gap": "sm"}, "children": items}
        ]
    });

    render_panel("main", panel)
}

// =============================================================================
// Helpery
// =============================================================================

/// Aktualizuje postep joba (0..100).
fn update_progress(job_id: &str, progress: i64) {
    let _ = sql_exec(
        "UPDATE ingest_jobs SET progress = ?, updated_at = ? WHERE id = ?",
        &[
            SqlValue::I64(progress),
            SqlValue::I64(now_unix()),
            SqlValue::String(job_id.to_string()),
        ],
    );
}

/// Oznacza dokument i job jako ukonczony. Sprawdza wynik UPDATE — brak trafionego
/// wiersza traktujemy jako blad, bo status/progress nie moze klamac.
fn mark_ingested(document_id: &str, job_id: &str) -> Result<(), String> {
    let now = now_unix();
    let doc = sql_exec(
        "UPDATE documents SET status = 'ingested' WHERE id = ?",
        &[SqlValue::String(document_id.to_string())],
    )
    .map_err(|e| format!("UPDATE documents: {e}"))?;
    if doc.rows_affected == 0 {
        return Err("UPDATE documents nie objal zadnego wiersza".to_string());
    }
    let job = sql_exec(
        "UPDATE ingest_jobs SET status = 'completed', progress = 100, updated_at = ? WHERE id = ?",
        &[SqlValue::I64(now), SqlValue::String(job_id.to_string())],
    )
    .map_err(|e| format!("UPDATE ingest_jobs: {e}"))?;
    if job.rows_affected == 0 {
        return Err("UPDATE ingest_jobs nie objal zadnego wiersza".to_string());
    }
    Ok(())
}

/// Oznacza job i dokument jako failed z komunikatem bledu. Bledy zapisu statusu sa
/// logowane (nie lykane po cichu) — gdy status failed sie nie zapisze, mamy slad.
fn fail_job(document_id: &str, job_id: &str, message: &str) {
    let now = now_unix();
    match sql_exec(
        "UPDATE documents SET status = 'failed' WHERE id = ?",
        &[SqlValue::String(document_id.to_string())],
    ) {
        Ok(r) if r.rows_affected == 0 => {
            log::warn(&format!("rag: status 'failed' nie objal dokumentu {document_id}"));
        }
        Err(e) => log::error(&format!("rag: blad zapisu statusu failed dokumentu {document_id}: {e}")),
        _ => {}
    }
    match sql_exec(
        "UPDATE ingest_jobs SET status = 'failed', error = ?, updated_at = ? WHERE id = ?",
        &[
            SqlValue::String(message.to_string()),
            SqlValue::I64(now),
            SqlValue::String(job_id.to_string()),
        ],
    ) {
        Ok(r) if r.rows_affected == 0 => {
            log::warn(&format!("rag: status 'failed' nie objal joba {job_id}"));
        }
        Err(e) => log::error(&format!("rag: blad zapisu statusu failed joba {job_id}: {e}")),
        _ => {}
    }
}

/// Klasa zrodla wg MIME — wyznacza sciezke ekstrakcji tekstu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    /// Tekst wprost (UTF-8) — text/*, application/json.
    Text,
    /// Obraz/PDF -> doc_parse (alias rag-parse renderuje strony na markdown).
    Parse,
}

/// Klasyfikuje MIME wg allowlisty. Zwraca `None` dla nieobslugiwanych typow
/// (DOCX/binarki/nieznane) — takie zrodlo NIE jest ingestowane jako smieci UTF-8.
/// PDF rozpoznajemy odpornie: po prefiksie `application/pdf`, ignorujac parametry
/// (np. `application/pdf; charset=...`).
fn classify_mime(mime: &str) -> Option<SourceKind> {
    let base = mime.split(';').next().unwrap_or(mime).trim().to_ascii_lowercase();
    if base.starts_with("image/") || base == "application/pdf" {
        return Some(SourceKind::Parse);
    }
    if base.starts_with("text/") || base == "application/json" {
        return Some(SourceKind::Text);
    }
    None
}

/// Generuje unikalny id z prefiksem (czas + licznik monotoniczny).
fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{:x}_{:x}", now_unix_ms(), n)
}

/// Czas uniksowy w sekundach.
fn now_unix() -> i64 {
    now_unix_ms() as i64 / 1000
}

/// Czas uniksowy w milisekundach.
fn now_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Skrot na odpowiedz bledu.
fn err(message: &str) -> Value {
    json!({"ok": false, "error": message})
}

/// Zapisuje JSON do bufora wyjsciowego i ustawia dlugosc (4 bajty LE).
fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, &response);
    if written < 0 {
        log::error("rag: bufor wyjsciowy za maly na odpowiedz");
        return 2;
    }
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);
    0
}

// =============================================================================
// Testy jednostkowe — czyste funkcje (bez host ABI): walidacja dim, allowlista
// MIME, twardy split nadwymiarowych segmentow, chunking.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Buduje odpowiedz embeddingu jako goly JSON-array o zadanej dlugosci.
    fn array_response(len: usize) -> String {
        let arr: Vec<f32> = (0..len).map(|i| i as f32 * 0.001).collect();
        serde_json::to_string(&arr).unwrap()
    }

    #[test]
    fn parse_embedding_accepts_exact_dimension() {
        let resp = array_response(EMBED_DIMENSIONS);
        let v = parse_embedding_response(&resp).expect("powinien przejsc dla 1024");
        assert_eq!(v.len(), EMBED_DIMENSIONS);
    }

    #[test]
    fn parse_embedding_rejects_wrong_dimension() {
        for len in [1usize, 512, 1023, 1025, 2048] {
            let resp = array_response(len);
            let res = parse_embedding_response(&resp);
            assert!(res.is_err(), "dlugosc {len} powinna byc odrzucona");
            assert!(res.unwrap_err().contains("wymiar"), "blad powinien wskazywac wymiar");
        }
    }

    #[test]
    fn parse_embedding_validates_object_shapes() {
        // Pole "embedding" w obiekcie tez podlega walidacji wymiaru.
        let inner = array_response(EMBED_DIMENSIONS);
        let ok = format!(r#"{{"embedding":{inner}}}"#);
        assert!(parse_embedding_response(&ok).is_ok());

        let bad_inner = array_response(10);
        let bad = format!(r#"{{"embedding":{bad_inner}}}"#);
        assert!(parse_embedding_response(&bad).is_err());

        // data[0].embedding (ksztalt OpenAI).
        let data_ok = format!(r#"{{"data":[{{"embedding":{inner}}}]}}"#);
        assert!(parse_embedding_response(&data_ok).is_ok());
    }

    #[test]
    fn parse_embedding_rejects_unrecognized_shape() {
        assert!(parse_embedding_response(r#"{"foo":123}"#).is_err());
    }

    #[test]
    fn parse_flow_response_extracts_answer_and_real_citations() {
        // Output node serializuje {answer, citations} jako tresc; tu owinieta w
        // chat-completion (jak wraca z route_chat_completion).
        let inner = r#"{"answer":"Odpowiedz [doc1#0]","citations":[{"doc_id":"doc1","chunk_index":0,"text":"pasaz","score":0.12}]}"#;
        let escaped = serde_json::to_string(inner).unwrap();
        let raw = format!(r#"{{"choices":[{{"message":{{"content":{escaped}}}}}]}}"#);
        let (answer, citations) = parse_flow_response(&raw);
        assert_eq!(answer, "Odpowiedz [doc1#0]");
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0]["doc_id"].as_str(), Some("doc1"));
        assert_eq!(citations[0]["chunk_index"].as_i64(), Some(0));
        assert_eq!(citations[0]["text"].as_str(), Some("pasaz"));
        assert!(citations[0]["score"].as_f64().is_some());
    }

    #[test]
    fn parse_flow_response_handles_direct_answer_citations() {
        // Tresc bezposrednio jako {answer, citations} (bez owijki chat-completion).
        let raw = r#"{"answer":"A","citations":[{"doc_id":"d","chunk_index":2,"text":"t","score":0.5}]}"#;
        let (answer, citations) = parse_flow_response(raw);
        assert_eq!(answer, "A");
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0]["chunk_index"].as_i64(), Some(2));
    }

    #[test]
    fn parse_flow_response_falls_back_to_plain_text_without_citations() {
        // Goly tekst oraz inne ksztalty JSON => answer = tekst, brak cytatow
        // (zero zmyslania — cytaty istnieja tylko gdy retrieval je zwrocil).
        let (a, c) = parse_flow_response("  zwykly tekst  ");
        assert_eq!(a, "zwykly tekst");
        assert!(c.is_empty());
        let (a2, c2) = parse_flow_response(r#"{"content":"B"}"#);
        assert_eq!(a2, "B");
        assert!(c2.is_empty());
    }

    #[test]
    fn classify_mime_text_paths() {
        for m in ["text/plain", "text/markdown", "text/html", "application/json"] {
            assert_eq!(classify_mime(m), Some(SourceKind::Text), "{m}");
        }
        // Parametry MIME ignorowane.
        assert_eq!(classify_mime("text/plain; charset=utf-8"), Some(SourceKind::Text));
        assert_eq!(classify_mime("TEXT/PLAIN"), Some(SourceKind::Text));
    }

    #[test]
    fn classify_mime_parse_paths() {
        for m in ["image/png", "image/jpeg", "application/pdf"] {
            assert_eq!(classify_mime(m), Some(SourceKind::Parse), "{m}");
        }
        // PDF odporny na parametry i wielkosc liter.
        assert_eq!(classify_mime("application/pdf; version=1.7"), Some(SourceKind::Parse));
        assert_eq!(classify_mime("Application/PDF"), Some(SourceKind::Parse));
    }

    #[test]
    fn classify_mime_rejects_unsupported() {
        for m in [
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/octet-stream",
            "application/zip",
            "",
        ] {
            assert_eq!(classify_mime(m), None, "{m} powinien byc nieobslugiwany");
        }
    }

    #[test]
    fn hard_split_keeps_small_segment() {
        let out = hard_split("krotkie zdanie", 100);
        assert_eq!(out, vec!["krotkie zdanie".to_string()]);
    }

    #[test]
    fn hard_split_breaks_oversized_segment() {
        let big: String = "a".repeat(5000);
        let parts = hard_split(&big, 2048);
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert!(p.chars().count() <= 2048, "kawalek nie moze przekraczac chunk_size");
        }
        let joined: String = parts.concat();
        assert_eq!(joined, big, "twardy split nie moze gubic znakow");
    }

    #[test]
    fn split_into_chunks_never_exceeds_chunk_size_for_oversized_sentence() {
        // Pojedyncze "zdanie" bez separatorow, dluzsze niz chunk_size.
        let text: String = "x".repeat(7000);
        let chunks = split_into_chunks(&text, 2048, 200);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(
                c.chars().count() <= 2048,
                "zaden chunk nie moze przekroczyc chunk_size (byl {})",
                c.chars().count()
            );
        }
    }

    #[test]
    fn split_into_chunks_basic_paragraphs() {
        let text = "Pierwsze zdanie. Drugie zdanie.\n\nNowy akapit tutaj.";
        let chunks = split_into_chunks(text, 2048, 100);
        assert_eq!(chunks.len(), 1, "krotki tekst miesci sie w jednym chunku");
        assert!(chunks[0].contains("Pierwsze"));
        assert!(chunks[0].contains("akapit"));
    }
}
