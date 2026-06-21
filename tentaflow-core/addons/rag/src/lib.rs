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
    doc_parse, document_get, graph_delete_edge, graph_delete_node, graph_upsert_edge,
    graph_upsert_node, vector_delete, vector_upsert, GraphNode, GraphProp, Provenance, VectorField,
    VectorFieldValue,
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

// --- Ekstrakcja encji/relacji do grafu wiedzy (GraphRAG, Etap 2 / slice E3.0) ---

/// Nazwa kolekcji grafowej (zgodna z [[graph_collection]] w manifescie).
const KG_COLLECTION: &str = "kg";

/// Wersja ekstraktora — wpisywana w provenance kazdego wezla/krawedzi, by mozna
/// bylo pozniej (re)ekstrahowac i odroznic generacje faktow.
const EXTRACTOR_VERSION: &str = "rag-e3.0";

/// Bufor na odpowiedz ekstrakcji (JSON z lista encji/relacji — kilka KB wystarcza,
/// ale dajemy zapas na wieksze chunki).
const EXTRACT_BUFFER_SIZE: usize = 65_536;

/// Domyslna pewnosc faktu, gdy LLM jej nie poda (ekstrakcja z tekstu, nie pewnik).
const DEFAULT_CONFIDENCE: f32 = 0.6;

// Capy anti-DoS — LLM moze halucynowac dlugie listy. Nadmiar przycinamy/odrzucamy.
/// Max encji wziętych z jednego chunku.
const MAX_ENTITIES_PER_CHUNK: usize = 30;
/// Max relacji wziętych z jednego chunku.
const MAX_RELATIONS_PER_CHUNK: usize = 30;
/// Max dlugosc nazwy encji (znaki). Dluzsze odrzucamy (nie sa nazwami).
const MAX_ENTITY_NAME_CHARS: usize = 200;
/// Max laczna liczba triple'ow (relacji) na caly dokument.
const MAX_TRIPLES_PER_DOC: usize = 2000;

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

    // Akumulatory ekstrakcji grafu (best-effort). doc_triples pilnuje globalnego
    // capa MAX_TRIPLES_PER_DOC; graph_failed zaznacza, ze graf jest czesciowy.
    let mut total_entities = 0usize;
    let mut total_relations = 0usize;
    let mut doc_triples = 0usize;
    let mut graph_partial = false;

    for (index, chunk_text) in chunks.iter().enumerate() {
        let step = ingest_one_chunk(collection_id, document_id, index, chunk_text, now, &mut upserted);
        if let Err(msg) = step {
            // Cleanup-on-failure: skasuj wszystkie wektory + chunki + graf dokumentu.
            cleanup_document_artifacts(document_id, &upserted);
            return Err(format!("Blad chunka {index}: {msg}"));
        }

        // Ekstrakcja grafu — BEST-EFFORT. Wektor chunku jest juz zapisany; blad
        // ekstrakcji (LLM/parsowanie/upsert) NIE wywala ingestu, tylko oznacza graf
        // jako czesciowy i leci dalej (wektory > graf).
        // graph_partial sluzy tu DWOM zrodlom niekompletnosci: blad ekstrakcji (Err)
        // ORAZ obciecie capem/za-dluga relacja (truncated, bug 5/6). Przekazujemy je
        // jako wspolna flage `truncated` ustawiana wewnatrz extract_chunk_graph.
        let chunk_result =
            extract_chunk_graph(document_id, index, chunk_text, &mut doc_triples, &mut graph_partial);
        // Blad ekstrakcji chunku (w tym blad REJESTRU graph_artifacts, bug 4)
        // oznacza graf jako czesciowy — nigdy nie ginie cicho.
        if chunk_extraction_marks_partial(&chunk_result) {
            graph_partial = true;
        }
        match chunk_result {
            Ok((ents, rels)) => {
                total_entities += ents;
                total_relations += rels;
            }
            Err(e) => {
                log::warn(&format!(
                    "rag: ekstrakcja grafu chunka {index} dok {document_id} nieudana (graf czesciowy): {e}"
                ));
            }
        }

        // Postep 30..95% rozlozony na chunki.
        let progress = 30 + ((index + 1) * 65 / total) as i64;
        update_progress(job_id, progress.min(95));
    }

    // Zapisz liczniki ekstrakcji + flage czesciowosci na dokumencie (best-effort
    // statystyka; brak trafienia nie jest bledem ingestu wektorowego).
    let _ = sql_exec(
        "UPDATE documents SET entity_count = ?, relation_count = ?, graph_partial = ? WHERE id = ?",
        &[
            SqlValue::I64(total_entities as i64),
            SqlValue::I64(total_relations as i64),
            SqlValue::I64(if graph_partial { 1 } else { 0 }),
            SqlValue::String(document_id.to_string()),
        ],
    );

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

    // Odwracalny cleanup grafu: skasuj wezly/krawedzie wniesione przez ten dokument
    // wg rejestru graph_artifacts. Krawedzie kasujemy PRZED wezlami (kasowanie wezla
    // i tak usuwa incydentne krawedzie, ale jawne usuniecie krawedzi czysci te,
    // ktorych konce wspoldziela inny dokument i nie powinny zniknac).
    cleanup_document_graph(document_id);

    // Na koncu usun chunki dokumentu.
    let _ = sql_exec(
        "DELETE FROM chunks WHERE document_id = ?",
        &[SqlValue::String(document_id.to_string())],
    );
}

/// Decyzja refcountu: czy skasowac wezel/krawedz z grafu po usunieciu wierszy
/// rejestru kasowanego dokumentu. `remaining_refs` to liczba wierszy graph_artifacts
/// INNYCH dokumentow, ktore wciaz referuja ten sam node_id / klucz krawedzi. Wezel
/// (lub krawedz) ginie z grafu DOPIERO gdy refcount spadnie do 0 — inaczej zostaje,
/// bo wspoldzieli go inny dokument (istota multi-doc GraphRAG).
fn should_delete_from_graph(remaining_refs: i64) -> bool {
    remaining_refs <= 0
}

/// Liczy ile wierszy graph_artifacts (poza wlasnie kasowanym dokumentem) wciaz
/// referuje dany wezel. Konserwatywnie: blad zapytania -> traktujemy jak "wciaz
/// referowany" (nie kasujemy z grafu), zeby nie usunac wspoldzielonego wezla.
fn count_other_node_refs(node_id: &str, exclude_document_id: &str) -> i64 {
    sql_query_one(
        "SELECT COUNT(*) FROM graph_artifacts \
         WHERE kind = 'node' AND n_id = ? AND document_id != ?",
        &[
            SqlValue::String(node_id.to_string()),
            SqlValue::String(exclude_document_id.to_string()),
        ],
    )
    .ok()
    .flatten()
    .and_then(|row| row.first().and_then(|v| v.as_i64()))
    .unwrap_or(1)
}

/// Jak `count_other_node_refs`, ale dla krawedzi po kluczu (src, rel, dst).
fn count_other_edge_refs(src: &str, rel: &str, dst: &str, exclude_document_id: &str) -> i64 {
    sql_query_one(
        "SELECT COUNT(*) FROM graph_artifacts \
         WHERE kind = 'edge' AND src = ? AND rel = ? AND dst = ? AND document_id != ?",
        &[
            SqlValue::String(src.to_string()),
            SqlValue::String(rel.to_string()),
            SqlValue::String(dst.to_string()),
            SqlValue::String(exclude_document_id.to_string()),
        ],
    )
    .ok()
    .flatten()
    .and_then(|row| row.first().and_then(|v| v.as_i64()))
    .unwrap_or(1)
}

/// Kasuje artefakty grafu dokumentu (kolekcja 'kg') po rejestrze graph_artifacts z
/// REFCOUNTEM i czysci wlasne wiersze rejestru. Idempotentny — wolany przy
/// re-ingescie i cleanup-on-failure.
///
/// Wezly/krawedzie maja id = znormalizowana nazwa, wiec sa WSPOLDZIELONE miedzy
/// dokumentami. Kasujemy je z grafu TYLKO gdy zaden inny dokument ich juz nie
/// referuje (refcount po rejestrze == 0). Inaczej zostawiamy je w grafie, bo nalezą
/// tez do innego dokumentu (multi-doc GraphRAG). Refcount liczymy PRZED usunieciem
/// wlasnych wierszy (z warunkiem document_id != self), a usuniecie rejestru robimy
/// na koncu.
///
/// Best-effort: pojedyncze bledy host graph API sa logowane, nie przerywaja reszty
/// (graf nie moze blokowac cleanupu wektorow).
fn cleanup_document_graph(document_id: &str) {
    // Najpierw krawedzie, potem wezly (patrz komentarz w callerze). Distinct, bo ten
    // sam klucz moze pojawic sie w rejestrze dokumentu wielokrotnie (rozne chunki).
    if let Ok(rows) = sql_query(
        "SELECT DISTINCT src, rel, dst FROM graph_artifacts WHERE document_id = ? AND kind = 'edge'",
        &[SqlValue::String(document_id.to_string())],
    ) {
        for row in &rows {
            let src = row.first().and_then(|v| v.as_str()).unwrap_or("");
            let rel = row.get(1).and_then(|v| v.as_str()).unwrap_or("");
            let dst = row.get(2).and_then(|v| v.as_str()).unwrap_or("");
            if src.is_empty() || rel.is_empty() || dst.is_empty() {
                continue;
            }
            let remaining = count_other_edge_refs(src, rel, dst, document_id);
            if should_delete_from_graph(remaining) {
                if let Err(e) = graph_delete_edge(KG_COLLECTION, src, rel, dst) {
                    log::warn(&format!("rag: cleanup krawedzi '{src}-{rel}-{dst}' nieudany: {e}"));
                }
            }
        }
    }
    if let Ok(rows) = sql_query(
        "SELECT DISTINCT n_id FROM graph_artifacts WHERE document_id = ? AND kind = 'node'",
        &[SqlValue::String(document_id.to_string())],
    ) {
        for row in &rows {
            let id = row.first().and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() {
                continue;
            }
            let remaining = count_other_node_refs(id, document_id);
            if should_delete_from_graph(remaining) {
                if let Err(e) = graph_delete_node(KG_COLLECTION, id) {
                    log::warn(&format!("rag: cleanup wezla '{id}' nieudany: {e}"));
                }
            }
        }
    }

    let _ = sql_exec(
        "DELETE FROM graph_artifacts WHERE document_id = ?",
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
         (SELECT COUNT(*) FROM chunks ch WHERE ch.document_id = d.id), \
         d.entity_count, d.relation_count, d.graph_partial \
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
                "entity_count": row.get(7).and_then(|v| v.as_i64()).unwrap_or(0),
                "relation_count": row.get(8).and_then(|v| v.as_i64()).unwrap_or(0),
                "graph_partial": row.get(9).and_then(|v| v.as_i64()).unwrap_or(0) != 0,
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
// Ekstrakcja encji/relacji do grafu wiedzy (slice E3.0)
//
// Po zapisaniu wektora chunku wolamy rag-llm (llm_generate, model=rag-llm) z
// promptem ekstrakcji i parsujemy JSON {entities, relations}. Encje -> wezly,
// relacje -> krawedzie w kolekcji 'kg' z OBOWIAZKOWYM provenance (doc_id,
// chunk_index, extractor_version) i odwracalnym rejestrem (graph_artifacts).
//
// Graf jest BEST-EFFORT: gdy rag-llm jest niedostepny albo zwroci smieci, NIE
// wywalamy ingestu — wektory sa krytyczne, graf to wzbogacenie. Blad ekstrakcji
// jest logowany, dokument oznaczany jako graph_partial, ingest leci dalej.
// =============================================================================

/// Wyekstrahowana encja: znormalizowane id (dedup) + oryginalna nazwa + typ.
#[derive(Debug, Clone, PartialEq)]
struct ExtractedEntity {
    /// Znormalizowane id wezla (lowercase+trim) — podstawowy dedup. Pelna
    /// entity-resolution to E3.1.
    id: String,
    /// Oryginalna nazwa (zachowana w props.name).
    name: String,
    /// Typ encji -> label wezla.
    entity_type: String,
}

/// Wyekstrahowana relacja (triple): head -[relation]-> tail, znormalizowane konce.
#[derive(Debug, Clone, PartialEq)]
struct ExtractedRelation {
    head_id: String,
    relation: String,
    tail_id: String,
}

/// Wynik parsowania + zastosowania capow dla jednego chunku. `truncated` = TRUE gdy
/// cap per-chunk (encje/relacje) obcial liste albo za-dluga relacja zostala
/// pominieta — sygnal do oznaczenia grafu jako czesciowy (graph_partial, bug 5/6).
#[derive(Debug, Clone, PartialEq, Default)]
struct ChunkExtraction {
    entities: Vec<ExtractedEntity>,
    relations: Vec<ExtractedRelation>,
    truncated: bool,
}

/// Normalizuje nazwe encji do stabilnego id wezla: trim + lowercase + scalenie
/// bialych znakow w pojedyncza spacje. To PODSTAWOWY dedup (te same nazwy w roznej
/// wielkosci liter/odstepach -> jeden wezel). Pelna entity-resolution to E3.1.
fn normalize_entity_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Wycina tresc chat-completion lub bierze caly tekst, a nastepnie probuje
/// wyciagnac obiekt JSON {entities, relations} TOLERUJAC proze wokol (LLM czesto
/// owija JSON w komentarz albo ```json). Zwraca przyciety wg capow wynik. Smieci
/// (brak parsowalnego JSON-a z oczekiwanym ksztaltem) -> pusty wynik (Default).
fn parse_extraction_response(raw: &str) -> ChunkExtraction {
    let inner = chat_completion_content(raw).unwrap_or_else(|| raw.to_string());
    let json_slice = match extract_json_object(&inner) {
        Some(s) => s,
        None => return ChunkExtraction::default(),
    };
    let value: Value = match serde_json::from_str(json_slice) {
        Ok(v) => v,
        Err(_) => return ChunkExtraction::default(),
    };
    parse_extraction_value(&value)
}

/// Buduje `ChunkExtraction` z juz sparsowanego JSON-a, stosujac capy i dedup.
fn parse_extraction_value(value: &Value) -> ChunkExtraction {
    let mut entities: Vec<ExtractedEntity> = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();
    let mut truncated = false;

    if let Some(arr) = value.get("entities").and_then(|v| v.as_array()) {
        for item in arr {
            if entities.len() >= MAX_ENTITIES_PER_CHUNK {
                // Cap per-chunk obcial liste encji -> graf czesciowy (bug 5).
                truncated = true;
                break;
            }
            let name = match item.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.trim(),
                None => continue,
            };
            if name.is_empty() || name.chars().count() > MAX_ENTITY_NAME_CHARS {
                continue;
            }
            let id = normalize_entity_name(name);
            if id.is_empty() || seen_ids.contains(&id) {
                continue;
            }
            let entity_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Entity")
                .to_string();
            seen_ids.push(id.clone());
            entities.push(ExtractedEntity { id, name: name.to_string(), entity_type });
        }
    }

    let mut relations: Vec<ExtractedRelation> = Vec::new();
    if let Some(arr) = value.get("relations").and_then(|v| v.as_array()) {
        for item in arr {
            if relations.len() >= MAX_RELATIONS_PER_CHUNK {
                // Cap per-chunk obcial liste relacji -> graf czesciowy (bug 5).
                truncated = true;
                break;
            }
            let head = item.get("head").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
            let tail = item.get("tail").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
            let relation = item
                .get("relation")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            if head.is_empty() || tail.is_empty() || relation.is_empty() {
                continue;
            }
            // Cap dlugosci wszystkich trzech czlonow (bug 6): relation jest czescia
            // klucza krawedzi (src,rel,dst) — bez capa 65KB-owa nazwa relacji
            // rozdmuchalaby klucz w grafie i rejestrze. Za-dluga -> pomijamy triple
            // i oznaczamy obciecie.
            if head.chars().count() > MAX_ENTITY_NAME_CHARS
                || tail.chars().count() > MAX_ENTITY_NAME_CHARS
                || relation.chars().count() > MAX_ENTITY_NAME_CHARS
            {
                truncated = true;
                continue;
            }
            relations.push(ExtractedRelation {
                head_id: normalize_entity_name(head),
                relation: relation.to_string(),
                tail_id: normalize_entity_name(tail),
            });
        }
    }

    ChunkExtraction { entities, relations, truncated }
}

/// Znajduje pierwszy zbalansowany obiekt JSON `{...}` w tekscie (toleruje proze i
/// fence ```json wokol). Liczy nawiasy klamrowe POZA stringami (z obsluga escape),
/// wiec `{` w wartosci tekstowej nie psuje zliczania.
fn extract_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Wola rag-llm (llm_generate, model=rag-llm) z promptem ekstrakcji dla tekstu
/// chunku i zwraca surowa odpowiedz. Blad host-fn / pusta odpowiedz -> Err (caller
/// traktuje to best-effort: loguje i kontynuuje ingest wektorowy).
fn call_extraction_llm(chunk_text: &str) -> Result<String, String> {
    let prompt = format!(
        "Wyciagnij encje i relacje z ponizszego tekstu. Zwroc WYLACZNIE JSON o ksztalcie \
         {{\"entities\":[{{\"name\":\"...\",\"type\":\"...\"}}],\
         \"relations\":[{{\"head\":\"...\",\"relation\":\"...\",\"tail\":\"...\"}}]}}. \
         Uzywaj TYLKO faktow obecnych w tekscie, nie halucynuj. head i tail relacji musza \
         odpowiadac nazwom encji. Bez komentarza, bez markdown.\n\nTEKST:\n{chunk_text}"
    );
    let model = "rag-llm";
    let options = json!({ "task": "chat", "temperature": 0.0 });
    let options_str =
        serde_json::to_string(&options).map_err(|e| format!("Blad serializacji opcji: {e}"))?;

    let prompt_bytes = prompt.as_bytes();
    let model_bytes = model.as_bytes();
    let options_bytes = options_str.as_bytes();
    let mut buffer = vec![0u8; EXTRACT_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32, prompt_bytes.len() as i32,
            model_bytes.as_ptr() as i32, model_bytes.len() as i32,
            options_bytes.as_ptr() as i32, options_bytes.len() as i32,
            buffer.as_mut_ptr() as i32, EXTRACT_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if rc < 0 {
        return Err(format!("rag-llm zwrocil blad: {rc}"));
    }
    if out_len <= 0 {
        return Err("rag-llm zwrocil pusta odpowiedz".to_string());
    }
    Ok(String::from_utf8_lossy(&buffer[..out_len as usize]).to_string())
}

/// Buduje provenance faktu — OBOWIAZKOWE pola (doc_id, chunk_id, extractor_version)
/// + confidence. Wspolne dla wezlow i krawedzi tego chunku.
fn build_provenance(document_id: &str, chunk_index: usize) -> Provenance {
    Provenance {
        chunk_id: Some(chunk_index.to_string()),
        doc_id: Some(document_id.to_string()),
        page: None,
        span: None,
        confidence: Some(DEFAULT_CONFIDENCE),
        extractor_version: Some(EXTRACTOR_VERSION.to_string()),
    }
}

/// Czy wynik ekstrakcji chunku ma oznaczyc graf jako czesciowy. KAZDY blad
/// (LLM, parsowanie, upsert grafu, a takze blad REJESTRU graph_artifacts z bug 4)
/// wraca tu jako Err i podnosi graph_partial — rozjazd "w grafie ale nie w
/// rejestrze" / niepelna ekstrakcja nigdy nie ginie cicho. Obciecie capem jest
/// sygnalizowane osobno (flaga `truncated`), wiec sukces (Ok) NIE podnosi flagi.
fn chunk_extraction_marks_partial(result: &Result<(usize, usize), String>) -> bool {
    result.is_err()
}

/// Ekstrakcja grafu dla jednego chunku: wola rag-llm, parsuje, upsertuje wezly +
/// krawedzie do 'kg' z provenance i rejestruje artefakty (graph_artifacts) do
/// odwracalnego cleanupu. Zwraca `(liczba_encji, liczba_relacji)` realnie
/// zapisanych. BEST-EFFORT: kazdy blad (LLM/parsowanie/upsert) zwraca Err, ktory
/// caller loguje i traktuje jako graf czesciowy — NIE przerywa ingestu wektorowego.
///
/// `doc_triples_so_far` to licznik triple'ow juz zapisanych dla calego dokumentu
/// (cap MAX_TRIPLES_PER_DOC) — aktualizowany o realnie wstawione krawedzie.
/// `truncated` jest ustawiane na TRUE gdy jakikolwiek cap (per-chunk, per-doc) lub
/// pominiecie za-dlugiej relacji obcie dane — caller propaguje to do graph_partial
/// (bug 5/6).
fn extract_chunk_graph(
    document_id: &str,
    chunk_index: usize,
    chunk_text: &str,
    doc_triples_so_far: &mut usize,
    truncated: &mut bool,
) -> Result<(usize, usize), String> {
    let raw = call_extraction_llm(chunk_text)?;
    let extraction = parse_extraction_response(&raw);
    // Capy per-chunk / pominiete za-dlugie relacje sygnalizowane przez parser (bug 5/6).
    if extraction.truncated {
        *truncated = true;
    }
    if extraction.entities.is_empty() && extraction.relations.is_empty() {
        return Ok((0, 0));
    }

    let provenance = build_provenance(document_id, chunk_index);
    let now = now_unix();

    // Wezly encji. Tylko encje z tego chunku staja sie "znane" do walidacji relacji.
    // INWARIANT (bug 4): rejestr graph_artifacts MUSI byc nadzbiorem grafu — co w
    // grafie, to w rejestrze, inaczej cleanup pominie artefakt (rozjazd refcountu).
    // Dlatego rejestrujemy PRZED upsertem grafu i propagujemy blad rejestru jako Err
    // (caller traktuje to best-effort: graph_partial, wektory niezmienione). Gdyby
    // padl upsert grafu PO udanym rejestrze, rejestr ma "nadmiarowy" wiersz — cleanup
    // sprobuje skasowac nieistniejacy wezel (idempotentnie, bez szkody).
    let mut known: Vec<String> = Vec::with_capacity(extraction.entities.len());
    let mut entity_count = 0usize;
    for entity in &extraction.entities {
        record_graph_node(document_id, &entity.id, now)
            .map_err(|e| format!("rejestr wezla '{}': {e}", entity.id))?;
        let node = GraphNode {
            id: entity.id.clone(),
            label: entity.entity_type.clone(),
            props: vec![GraphProp {
                name: "name".to_string(),
                value: VectorFieldValue::Str(entity.name.clone()),
            }],
            provenance: Some(provenance.clone()),
        };
        graph_upsert_node(KG_COLLECTION, node)
            .map_err(|e| format!("upsert wezla '{}': {e}", entity.id))?;
        if !known.contains(&entity.id) {
            known.push(entity.id.clone());
        }
        entity_count += 1;
    }

    // Krawedzie relacji. head/tail musza byc znanymi encjami tego chunku; gdy
    // brakuje wezla konca, upsertujemy go (placeholder label "Entity"), by krawedz
    // nie wisiala. Respektujemy globalny cap triple'ow na dokument.
    let mut relation_count = 0usize;
    for rel in &extraction.relations {
        if *doc_triples_so_far >= MAX_TRIPLES_PER_DOC {
            // Cap per-dokument obcial reszte relacji -> graf niekompletny (bug 5).
            *truncated = true;
            break;
        }
        for endpoint in [&rel.head_id, &rel.tail_id] {
            if !known.contains(endpoint) {
                record_graph_node(document_id, endpoint, now)
                    .map_err(|e| format!("rejestr wezla konca '{endpoint}': {e}"))?;
                let node = GraphNode {
                    id: endpoint.clone(),
                    label: "Entity".to_string(),
                    props: vec![GraphProp {
                        name: "name".to_string(),
                        value: VectorFieldValue::Str(endpoint.clone()),
                    }],
                    provenance: Some(provenance.clone()),
                };
                graph_upsert_node(KG_COLLECTION, node)
                    .map_err(|e| format!("upsert wezla konca '{endpoint}': {e}"))?;
                known.push(endpoint.clone());
            }
        }
        record_graph_edge(document_id, &rel.head_id, &rel.relation, &rel.tail_id, now)
            .map_err(|e| {
                format!("rejestr krawedzi '{}-{}-{}': {e}", rel.head_id, rel.relation, rel.tail_id)
            })?;
        graph_upsert_edge(
            KG_COLLECTION,
            &rel.head_id,
            &rel.relation,
            &rel.tail_id,
            Some(DEFAULT_CONFIDENCE as f64),
            Vec::new(),
            Some(provenance.clone()),
        )
        .map_err(|e| format!("upsert krawedzi '{}-{}-{}': {e}", rel.head_id, rel.relation, rel.tail_id))?;
        relation_count += 1;
        *doc_triples_so_far += 1;
    }

    Ok((entity_count, relation_count))
}

/// Rejestruje wezel grafu utworzony dla dokumentu (do odwracalnego cleanupu +
/// refcountu). Blad INSERT jest PROPAGOWANY (bug 4): bez wiersza rejestru cleanup
/// nie znalby tego wezla i refcount bylby rozjechany, wiec lepiej oznaczyc graf
/// jako czesciowy niz zostawic artefakt-sierote.
fn record_graph_node(document_id: &str, node_id: &str, now: i64) -> Result<(), String> {
    sql_exec(
        "INSERT INTO graph_artifacts (document_id, kind, n_id, created_at) VALUES (?, 'node', ?, ?)",
        &[
            SqlValue::String(document_id.to_string()),
            SqlValue::String(node_id.to_string()),
            SqlValue::I64(now),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("{e}"))
}

/// Rejestruje krawedz grafu utworzona dla dokumentu (do odwracalnego cleanupu +
/// refcountu). Blad INSERT propagowany — patrz `record_graph_node` (bug 4).
fn record_graph_edge(document_id: &str, src: &str, rel: &str, dst: &str, now: i64) -> Result<(), String> {
    sql_exec(
        "INSERT INTO graph_artifacts (document_id, kind, src, rel, dst, created_at) \
         VALUES (?, 'edge', ?, ?, ?, ?)",
        &[
            SqlValue::String(document_id.to_string()),
            SqlValue::String(src.to_string()),
            SqlValue::String(rel.to_string()),
            SqlValue::String(dst.to_string()),
            SqlValue::I64(now),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("{e}"))
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

    // --- Ekstrakcja encji/relacji (slice E3.0) ---

    #[test]
    fn normalize_entity_name_dedup_basics() {
        // Trim + lowercase + scalenie bialych znakow -> stabilny id (dedup).
        assert_eq!(normalize_entity_name("  Ada  Lovelace "), "ada lovelace");
        assert_eq!(normalize_entity_name("ADA\tLOVELACE"), "ada lovelace");
        assert_eq!(
            normalize_entity_name("Ada Lovelace"),
            normalize_entity_name("ada   lovelace")
        );
    }

    #[test]
    fn parse_extraction_basic_entities_and_relations() {
        let raw = r#"{"entities":[{"name":"Ada","type":"Person"},
            {"name":"Babbage","type":"Person"}],
            "relations":[{"head":"Ada","relation":"KNOWS","tail":"Babbage"}]}"#;
        let ex = parse_extraction_response(raw);
        assert_eq!(ex.entities.len(), 2);
        assert_eq!(ex.entities[0].id, "ada");
        assert_eq!(ex.entities[0].name, "Ada");
        assert_eq!(ex.entities[0].entity_type, "Person");
        assert_eq!(ex.relations.len(), 1);
        assert_eq!(ex.relations[0].head_id, "ada");
        assert_eq!(ex.relations[0].relation, "KNOWS");
        assert_eq!(ex.relations[0].tail_id, "babbage");
    }

    #[test]
    fn parse_extraction_tolerates_prose_and_fences() {
        // LLM owija JSON w proze i fence ```json — i tak wyciagamy zbalansowany obiekt.
        let raw = "Oto wynik:\n```json\n{\"entities\":[{\"name\":\"Paris\",\"type\":\"City\"}],\
                   \"relations\":[]}\n```\nKoniec.";
        let ex = parse_extraction_response(raw);
        assert_eq!(ex.entities.len(), 1);
        assert_eq!(ex.entities[0].id, "paris");
        assert!(ex.relations.is_empty());
    }

    #[test]
    fn parse_extraction_handles_chat_completion_wrapper() {
        let inner = r#"{"entities":[{"name":"X","type":"T"}],"relations":[]}"#;
        let escaped = serde_json::to_string(inner).unwrap();
        let raw = format!(r#"{{"choices":[{{"message":{{"content":{escaped}}}}}]}}"#);
        let ex = parse_extraction_response(&raw);
        assert_eq!(ex.entities.len(), 1);
        assert_eq!(ex.entities[0].id, "x");
    }

    #[test]
    fn parse_extraction_rejects_garbage() {
        for raw in ["zupelnie nic", "", "12345", "<xml>nope</xml>"] {
            let ex = parse_extraction_response(raw);
            assert!(ex.entities.is_empty() && ex.relations.is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn parse_extraction_dedups_entities() {
        let raw = r#"{"entities":[{"name":"Ada","type":"Person"},
            {"name":"ada","type":"Person"},{"name":" ADA ","type":"X"}],"relations":[]}"#;
        let ex = parse_extraction_response(raw);
        assert_eq!(ex.entities.len(), 1, "rozne warianty tej samej nazwy -> jeden wezel");
        assert_eq!(ex.entities[0].id, "ada");
    }

    #[test]
    fn parse_extraction_caps_entities_at_max() {
        let mut items = String::new();
        for i in 0..50 {
            if i > 0 {
                items.push(',');
            }
            items.push_str(&format!(r#"{{"name":"E{i}","type":"T"}}"#));
        }
        let raw = format!(r#"{{"entities":[{items}],"relations":[]}}"#);
        let ex = parse_extraction_response(&raw);
        assert_eq!(ex.entities.len(), MAX_ENTITIES_PER_CHUNK, "nadmiar encji przyciety");
        assert!(ex.truncated, "obciecie capem encji musi sygnalizowac truncated (bug 5)");
    }

    #[test]
    fn parse_extraction_caps_relations_at_max() {
        let mut items = String::new();
        for i in 0..50 {
            if i > 0 {
                items.push(',');
            }
            items.push_str(&format!(r#"{{"head":"a{i}","relation":"R","tail":"b{i}"}}"#));
        }
        let raw = format!(r#"{{"entities":[],"relations":[{items}]}}"#);
        let ex = parse_extraction_response(&raw);
        assert_eq!(ex.relations.len(), MAX_RELATIONS_PER_CHUNK, "nadmiar relacji przyciety");
        assert!(ex.truncated, "obciecie capem relacji musi sygnalizowac truncated (bug 5)");
    }

    #[test]
    fn parse_extraction_within_caps_not_truncated() {
        // Dane mieszczace sie w capach NIE moga falszywie podnosic truncated.
        let raw = r#"{"entities":[{"name":"A","type":"T"}],
            "relations":[{"head":"A","relation":"R","tail":"B"}]}"#;
        let ex = parse_extraction_response(raw);
        assert!(!ex.truncated, "dane w granicach capow nie sa obciete");
    }

    #[test]
    fn parse_extraction_overlong_relation_dropped_and_truncated() {
        // Relacja > MAX_ENTITY_NAME_CHARS pomijana (klucz krawedzi nie moze byc
        // gigantyczny) i sygnalizowana jako obciecie (bug 6).
        let long_rel = "R".repeat(MAX_ENTITY_NAME_CHARS + 1);
        let raw = format!(
            r#"{{"entities":[],"relations":[
                {{"head":"a","relation":"{long_rel}","tail":"b"}},
                {{"head":"c","relation":"OK","tail":"d"}}]}}"#
        );
        let ex = parse_extraction_response(&raw);
        assert_eq!(ex.relations.len(), 1, "za dluga relacja odrzucona, krotka zostaje");
        assert_eq!(ex.relations[0].relation, "OK");
        assert!(ex.truncated, "pominiecie za-dlugiej relacji ustawia truncated (bug 6)");
    }

    // --- Refcount cleanupu wspoldzielonych wezlow/krawedzi (bug 1+2) ---

    #[test]
    fn refcount_keeps_node_while_other_doc_references_it() {
        // Dopoki INNY dokument referuje wezel/krawedz (remaining_refs > 0), NIE
        // kasujemy z grafu — wspoldzielony przez multi-doc GraphRAG.
        assert!(!should_delete_from_graph(1), "1 inny dokument trzyma wezel -> zostaw");
        assert!(!should_delete_from_graph(3), "kilka innych dokumentow -> zostaw");
    }

    #[test]
    fn refcount_deletes_node_when_no_other_doc_references_it() {
        // Refcount 0 (zaden inny dokument) -> kasujemy z grafu.
        assert!(should_delete_from_graph(0), "brak innych referencji -> kasuj");
        // Wartosc ujemna (teoretycznie niemozliwa) tez traktujemy jak 0.
        assert!(should_delete_from_graph(-1));
    }

    // Wiersz rejestru graph_artifacts (model in-memory do testu refcountu). Odwzorowuje
    // schemat tabeli: per (document_id, obiekt). Edge-key = (src, rel, dst).
    #[derive(Clone)]
    struct Artifact {
        document_id: &'static str,
        key: &'static str,
    }

    // Mirror SQL `COUNT(*) ... WHERE key = ? AND document_id != ?` z count_other_*:
    // ile INNYCH dokumentow trzyma dany klucz. To dokladnie predykat refcountu.
    fn other_refs(registry: &[Artifact], key: &str, exclude_doc: &str) -> i64 {
        registry
            .iter()
            .filter(|a| a.key == key && a.document_id != exclude_doc)
            .count() as i64
    }

    // Model cleanupu dokumentu z refcountem: zwraca klucze faktycznie kasowane z grafu
    // (refcount -> 0) i usuwa wiersze dokumentu z rejestru. Odwzorowuje
    // cleanup_document_graph: decyzja per klucz = should_delete_from_graph(other_refs).
    fn cleanup_doc_model(registry: &mut Vec<Artifact>, doc: &'static str) -> Vec<String> {
        let mut deleted = Vec::new();
        let mut own_keys: Vec<&str> =
            registry.iter().filter(|a| a.document_id == doc).map(|a| a.key).collect();
        own_keys.sort_unstable();
        own_keys.dedup();
        for key in own_keys {
            if should_delete_from_graph(other_refs(registry, key, doc)) {
                deleted.push(key.to_string());
            }
        }
        registry.retain(|a| a.document_id != doc);
        deleted
    }

    #[test]
    fn refcount_cleanup_shared_entity_across_two_docs() {
        // Doc A i doc B oba wnosza encje "einstein" i krawedz "einstein|knows|bohr".
        let mut registry = vec![
            Artifact { document_id: "A", key: "node:einstein" },
            Artifact { document_id: "A", key: "edge:einstein|knows|bohr" },
            Artifact { document_id: "B", key: "node:einstein" },
            Artifact { document_id: "B", key: "edge:einstein|knows|bohr" },
        ];

        // Cleanup A: B nadal referuje oba -> NIC nie kasujemy z grafu (refcount > 0).
        let deleted_a = cleanup_doc_model(&mut registry, "A");
        assert!(
            deleted_a.is_empty(),
            "cleanup A nie moze usunac wezla/krawedzi wspoldzielonych z B: {deleted_a:?}"
        );
        // Wiersze A znikaja z rejestru, B zostaja.
        assert_eq!(registry.iter().filter(|a| a.document_id == "B").count(), 2);
        assert_eq!(registry.iter().filter(|a| a.document_id == "A").count(), 0);

        // Cleanup B: teraz refcount obu kluczy = 0 -> kasujemy z grafu.
        let mut deleted_b = cleanup_doc_model(&mut registry, "B");
        deleted_b.sort();
        assert_eq!(
            deleted_b,
            vec!["edge:einstein|knows|bohr".to_string(), "node:einstein".to_string()],
            "po cleanup B (refcount 0) wezel i krawedz znikaja z grafu"
        );
        assert!(registry.is_empty(), "rejestr pusty po cleanupie obu dokumentow");
    }

    #[test]
    fn refcount_cleanup_deletes_unshared_artifacts() {
        // Encja unikalna dla A (zaden inny dokument) -> cleanup A kasuje ja od razu.
        let mut registry = vec![
            Artifact { document_id: "A", key: "node:solo" },
            Artifact { document_id: "B", key: "node:other" },
        ];
        let deleted = cleanup_doc_model(&mut registry, "A");
        assert_eq!(deleted, vec!["node:solo".to_string()], "wezel bez wspoldzielenia kasowany");
        assert_eq!(registry.len(), 1, "wiersz B nietkniety");
    }

    // --- Propagacja bledu rejestru graph_artifacts -> graph_partial (bug 4) ---

    #[test]
    fn registry_failure_marks_graph_partial() {
        // Blad rejestru (record_graph_node/edge) wraca z extract_chunk_graph jako Err
        // i MUSI oznaczyc graf jako czesciowy — bez tego byl cichy rozjazd
        // "w grafie ale nie w rejestrze" i cleanup nie usunalby artefaktu.
        let err: Result<(usize, usize), String> = Err("rejestr wezla 'x': zapis sql padl".into());
        assert!(chunk_extraction_marks_partial(&err), "blad rejestru -> graph_partial (bug 4)");
    }

    #[test]
    fn successful_extraction_does_not_mark_partial() {
        // Sukces ekstrakcji NIE podnosi graph_partial (obciecie sygnalizuje truncated).
        let ok: Result<(usize, usize), String> = Ok((3, 2));
        assert!(!chunk_extraction_marks_partial(&ok), "Ok nie oznacza grafu czesciowego");
    }

    #[test]
    fn parse_extraction_rejects_overlong_entity_name() {
        let long = "x".repeat(MAX_ENTITY_NAME_CHARS + 1);
        let raw = format!(
            r#"{{"entities":[{{"name":"{long}","type":"T"}},{{"name":"ok","type":"T"}}],"relations":[]}}"#
        );
        let ex = parse_extraction_response(&raw);
        assert_eq!(ex.entities.len(), 1, "za dluga nazwa odrzucona");
        assert_eq!(ex.entities[0].id, "ok");
    }

    #[test]
    fn parse_extraction_drops_incomplete_relations() {
        let raw = r#"{"entities":[],"relations":[
            {"head":"a","relation":"R","tail":"b"},
            {"head":"","relation":"R","tail":"b"},
            {"head":"a","relation":"","tail":"b"},
            {"head":"a","relation":"R","tail":""}]}"#;
        let ex = parse_extraction_response(raw);
        assert_eq!(ex.relations.len(), 1, "tylko kompletny triple przechodzi");
    }

    #[test]
    fn extract_json_object_ignores_braces_in_strings() {
        // Nawias '{' w wartosci tekstowej NIE moze psuc zliczania zbalansowania.
        let raw = r#"prefix {"name":"a {b} c","type":"T"} suffix"#;
        let slice = extract_json_object(raw).expect("powinien znalezc obiekt");
        let v: Value = serde_json::from_str(slice).unwrap();
        assert_eq!(v["name"].as_str(), Some("a {b} c"));
    }

    #[test]
    fn build_provenance_has_mandatory_fields() {
        // Provenance OBOWIAZKOWE: doc_id, chunk_id, extractor_version (wymog planu §8).
        let p = build_provenance("doc42", 7);
        assert_eq!(p.doc_id.as_deref(), Some("doc42"));
        assert_eq!(p.chunk_id.as_deref(), Some("7"));
        assert_eq!(p.extractor_version.as_deref(), Some(EXTRACTOR_VERSION));
        assert!(p.confidence.is_some());
    }
}
