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

/// Nazwa kolekcji grafowej aktywnego widoku (zgodna z [[graph_collection]] w
/// manifescie). 'kg_active' zawiera TYLKO aktywne fakty (w D1 = wszystkie); jest
/// odtwarzalna materializacja outboxu, a zrodlem prawdy jest SQLite addona (R1/R2).
const KG_COLLECTION: &str = "kg_active";

/// Wersja ekstraktora — wpisywana w provenance kazdego wezla/krawedzi, by mozna
/// bylo pozniej (re)ekstrahowac i odroznic generacje faktow.
const EXTRACTOR_VERSION: &str = "rag-e3.0";

/// Bufor na odpowiedz ekstrakcji (JSON z lista encji/relacji — kilka KB wystarcza,
/// ale dajemy zapas na wieksze chunki).
const EXTRACT_BUFFER_SIZE: usize = 65_536;

/// Domyslna pewnosc faktu, gdy LLM jej nie poda (ekstrakcja z tekstu, nie pewnik).
const DEFAULT_CONFIDENCE: f32 = 0.6;

/// Domyslny prog tau Thematic Denoising (D2): schemat (head_type, relation,
/// tail_type) staje sie 'stable' dopiero gdy freq >= tau. Dopoki jest 'candidate',
/// jego krawedzie NIE trafiaja do 'kg_active' (sa zapisane w SQLite z active=0), wiec
/// schematy szumowe (pojedyncze wystapienia) nie zatruwaja retrievalu. tau=1 = denoising
/// wylaczony (wszystko od razu stable — sensowne dla telefonu/malego korpusu).
const DEFAULT_DENOISING_THRESHOLD: u64 = 2;

/// Klucz instancyjnego KV, pod ktorym admin moze nadpisac prog tau per-instancja.
/// To JEDYNY punkt wpiecia configu progu (state.read) — gdy brak wpisu, uzywamy
/// DEFAULT_DENOISING_THRESHOLD. Nie budujemy tu osobnego systemu configu.
const DENOISING_THRESHOLD_STATE_KEY: &str = "denoising_threshold";

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

    // A_det (MemGraphRAG D3): asynchroniczny skan konfliktow. Rejestrowany jako tool,
    // by Scheduler mogl go wolac jako scheduled job (interval) ORAZ by byl wolalny
    // recznie do testow/debugu. Parametry odpowiadaja payloadowi scheduled joba (R4).
    register_tool(
        "conflict_scan",
        "A_det (0 LLM): skanuje nowo-aktywne fakty i wykrywa symbolicznie kandydatow konfliktu (ten sam head+rel, rozny tail), zapisujac otwarte konflikty. Asynchroniczny, wznawialny po kursorze, idempotentny.",
        json!({
            "type": "object",
            "properties": {
                "collection_id": {"type": "string"},
                "batch_size": {"type": "integer"}
            }
        }),
    );

    // A_res (MemGraphRAG D4): asynchroniczna adjudykacja konfliktow przez LLM. OSOBNY tool
    // od conflict_scan (rozdzielenie: detekcja tania 0-LLM vs adjudykacja droga LLM —
    // niezalezne harmonogramy i kontrola kosztu R8). Rejestrowany jak conflict_scan: tool
    // wolalny przez Scheduler (interval) ORAZ recznie do testow/debugu.
    register_tool(
        "conflict_resolve",
        "A_res (LLM): adjudykuje OTWARTE konflikty (z conflict_scan) evidence-driven przez rag-llm. Dla kazdego konfliktu zbiera pasaze zrodlowe, prosi LLM o decyzje (keep_winner/temporal_split/merge_entities/escalate) i stosuje ja ODWRACALNIE (tombstone przegranych przez outbox). Batch z twardym capem kosztu, cache po zbiorze faktow, claim exactly-once.",
        json!({
            "type": "object",
            "properties": {
                "collection_id": {"type": "string"},
                "max_conflicts": {"type": "integer"}
            }
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
        "conflict_scan" => handle_conflict_scan(&params),
        "conflict_resolve" => handle_conflict_resolve(&params),
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

    // Reconcile na starcie ingestu (samonaprawa po crashu): domyka zalegle promocje/
    // aktywacje i re-drain outboxu (applied=0) sprzed ewentualnego crashu poprzedniego
    // ingestu (R3), zanim dolozymy nowe fakty. Best-effort — nieudany reconcile nie wywala
    // ingestu wektorowego, tylko znaczy graf jako czesciowy.
    if let Err(e) = reconcile_schemas() {
        graph_partial = true;
        log::warn(&format!(
            "rag: reconcile na starcie ingestu dok {document_id} nieudany (graf czesciowy): {e}"
        ));
    }

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

    // Reconcile PO petli chunkow (zamiast dawnego per-chunk drainu): promocja schematow
    // ktore osiagnely prog tau w tym ingescie + aktywacja partiami WSZYSTKICH zalegych
    // krawedzi stabilnych schematow + materializacja do grafu. Idempotentny i globalny —
    // sprzata tez zaleglosci rownoleglych ingestow. Best-effort (graf < wektory): blad
    // znaczy graf jako czesciowy, nie wywala ingestu.
    if let Err(e) = reconcile_schemas() {
        graph_partial = true;
        log::warn(&format!(
            "rag: reconcile po ingescie dok {document_id} nieudany (graf czesciowy): {e}"
        ));
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
        "SELECT COUNT(DISTINCT document_id) FROM graph_artifacts \
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

/// Jak `count_other_node_refs`, ale dla krawedzi po kluczu (src, rel, dst). Refcount =
/// COUNT(DISTINCT document_id) (a NIE COUNT(*)): po INSERT OR IGNORE rejestr krawedzi jest
/// unikalny per (document_id, src, rel, dst), wiec liczymy DOKUMENTY trzymajace krawedz, a
/// nie wiersze — krawedz ginie z grafu dopiero gdy ZADEN inny dokument jej nie wnosi.
fn count_other_edge_refs(src: &str, rel: &str, dst: &str, exclude_document_id: &str) -> i64 {
    sql_query_one(
        "SELECT COUNT(DISTINCT document_id) FROM graph_artifacts \
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

/// Kasuje artefakty grafu dokumentu (kolekcja 'kg_active') po rejestrze graph_artifacts
/// z REFCOUNTEM i czysci wlasne wiersze rejestru. Idempotentny — wolany przy
/// re-ingescie i cleanup-on-failure.
///
/// Wezly/krawedzie maja id = znormalizowana nazwa, wiec sa WSPOLDZIELONE miedzy
/// dokumentami. Kasujemy je z grafu TYLKO gdy zaden inny dokument ich juz nie
/// referuje (refcount po rejestrze == 0). Inaczej zostawiamy je w grafie, bo nalezą
/// tez do innego dokumentu (multi-doc GraphRAG). Refcount liczymy PRZED usunieciem
/// wlasnych wierszy (z warunkiem document_id != self).
///
/// R1: graf jest mutowany WYLACZNIE przez outbox. Cleanup NIE wola graph_delete_*
/// wprost — enqueue'uje intencje delete_node/delete_edge do graph_outbox (applied=0)
/// w TEJ SAMEJ transakcji SQLite co usuniecie wierszy graph_artifacts (atomowo), a
/// nastepnie woła drain. Logika refcountu jest bez zmian — zmienia sie tylko SPOSOB
/// usuniecia z grafu (outbox zamiast direct). Bez tego direct-delete kasowal graf z
/// pominieciem outboxu, wiec re-ingest tego samego faktu (partial-unique dedup) nie
/// tworzyl nowego pending i graf nie wracal — łamało R1/R3.
fn cleanup_document_graph(document_id: &str) {
    let now = now_unix();
    let mut tx: Vec<(String, Vec<SqlValue>)> = Vec::new();

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
                push_outbox(&mut tx, &outbox_delete_edge(src, rel, dst), now);
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
                push_outbox(&mut tx, &outbox_delete_node(id), now);
            }
        }
    }

    // Usuniecie rejestru w TEJ SAMEJ transakcji co enqueue intencji delete — atomowo:
    // albo i rejestr znika, i intencja delete jest trwala, albo nic (brak rozjazdu).
    tx.push((
        "DELETE FROM graph_artifacts WHERE document_id = ?".to_string(),
        vec![SqlValue::String(document_id.to_string())],
    ));

    let stmts: Vec<(&str, &[SqlValue])> =
        tx.iter().map(|(q, p)| (q.as_str(), p.as_slice())).collect();
    if let Err(e) = sql_transaction(&stmts) {
        log::warn(&format!("rag: cleanup grafu dokumentu '{document_id}' (zapis outboxu) nieudany: {e}"));
        return;
    }

    // Materializacja delete'ow z trwalej kolejki. Crash przed/podczas drainu jest
    // odtwarzalny (re-drain domknie applied=0).
    if let Err(e) = drain_graph_outbox() {
        log::warn(&format!("rag: drain cleanupu dokumentu '{document_id}' nieudany: {e}"));
    }
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

/// Parsuje wartosc progu tau z surowego stanu KV (bajty zapisane przez admina jako
/// tekst). Pusta/nieparsowalna/zerowa wartosc -> DEFAULT_DENOISING_THRESHOLD (nigdy 0,
/// bo tau=0 promowalby kazdy schemat przy pierwszym wystapieniu, czyli tau=1 — a
/// zarazem dzielenie semantyki "off" miedzy 0 i 1 byloby mylace). Czysta funkcja:
/// testowalna bez hosta.
fn parse_denoising_threshold(raw: Option<&[u8]>) -> u64 {
    raw.and_then(|b| std::str::from_utf8(b).ok())
        .map(str::trim)
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(DEFAULT_DENOISING_THRESHOLD)
}

/// Czyta prog tau Thematic Denoising z instancyjnego KV (state.read). Brak wpisu lub
/// blad odczytu -> DEFAULT_DENOISING_THRESHOLD. To JEDYNY punkt wpiecia configu progu.
fn denoising_threshold() -> u64 {
    let raw = state_get(DENOISING_THRESHOLD_STATE_KEY).ok().flatten();
    parse_denoising_threshold(raw.as_deref())
}

/// Czysta regula promocji reconcile: schemat osiaga prog gdy freq >= tau. W produkcji ten
/// predykat zyje WPROST w SQL reconcile_schemas (`COUNT(*) >= tau` wewnatrz UPDATE), wiec
/// tu jest wylacznie referencyjna, testowalna bez hosta forma tej samej reguly. tau>=1
/// (parse_denoising_threshold to gwarantuje), wiec tau=1 => promocja od pierwszego
/// wystapienia (denoising off).
#[cfg(test)]
fn schema_reaches_threshold(freq: u64, tau: u64) -> bool {
    freq >= tau
}

/// RECONCILE (sedno D2) — idempotentny, samonaprawialny krok PO commicie ledgera, ktory
/// (A) promuje schematy candidate->stable na podstawie AUTORYTATYWNEGO COUNT (nie predykcji)
/// i (B) aktywuje partiami WSZYSTKIE zalegle (active=0) krawedzie stabilnych schematow,
/// enqueue'ujac ich materializacje do grafu. Na koncu woła drain_graph_outbox.
///
/// DLACZEGO oddzielnie od ingestu: ingest-tx liczyl promocje z predykcji freq_after sprzed
/// commitu (stale-read) — przy rownoleglym ingescie dwa zapisy mogly NIE zauwazyc, ze
/// laczny COUNT przekroczyl prog (zgubiony prog, blokery wyscigu). Reconcile podejmuje
/// decyzje z COUNT WEWNATRZ UPDATE/SELECT na juz zacommitowanym ledgerze, wiec widzi pelny
/// stan. Globalny zasieg (wszystkie schematy, nie tylko z tego ingestu) sprzata tez
/// zaleglosci innych rownoleglych ingestow -> samonaprawa.
///
/// CONCURRENCY (exactly-once aktywacji): dwa rownolegle reconcile NIE moga podwojnie
/// zmaterializowac tej samej krawedzi. Aktywacja kazdego faktu to w jednej tx (BEGIN
/// IMMEDIATE — write-lock od startu, wiec rownolegle tx serializuja sie OD POCZATKU):
///   1. enqueue WARUNKOWANY `INSERT ... SELECT ... WHERE EXISTS(active=0)` (zero op gdy juz aktywny),
///   2. flip WARUNKOWY `UPDATE ... WHERE active=0` (zero wierszy gdy juz aktywny).
/// Pierwszy reconcile flipuje active=0->1; drugi, po serializacji, widzi active=1, wiec jego
/// enqueue wstawia 0 op i flip zmienia 0 wierszy. Crash-safe: enqueue i flip sa w jednej tx
/// (atomowo), brak okna "active=1 bez outboxu". Partial-unique outbox (WHERE applied=0) +
/// INSERT OR IGNORE artefaktow to druga warstwa dedupu. Promocja przez UPDATE status='stable'
/// (WHERE candidate) jest jednokrotna; zaden prog sie nie gubi (decyzja z COUNT, nie z predykcji).
///
/// INWARIANT (testowany w SchemaModel): po DOMKNIETYM reconcile NIE istnieje fact_state.active=0
/// dla schematu 'stable'. Inwariant jest EVENTUALLY-CONSISTENT, nie per-jedno-wywolanie: petla
/// aktywacji ma cap RECONCILE_MAX_ITERS (anty-DoS, #5); po jego trafieniu reszta zaleglosci
/// zostaje na active=0 (logowane warn z liczba pozostalych) i domyka ja nastepny reconcile/ingest.
fn reconcile_schemas() -> Result<(), String> {
    let tau = denoising_threshold() as i64;

    // (A) Promocja: jeden atomowy UPDATE. status='stable' dla kazdego schematu candidate,
    // ktorego realny COUNT par (fact_key, document_id) osiagnal prog. Decyzja jest w SQL
    // (COUNT wewnatrz WHERE), wiec nie zalezy od predykcji ani od licznika schema_registry.freq.
    let now = now_unix();
    sql_exec(
        "UPDATE schema_registry SET status = 'stable', promoted_at = ? \
         WHERE status = 'candidate' \
           AND (SELECT COUNT(*) FROM fact_schema fs WHERE fs.schema_id = schema_registry.schema_id) >= ?",
        &[SqlValue::I64(now), SqlValue::I64(tau)],
    )
    .map_err(|e| format!("promocja schematow: {e}"))?;

    // (B) Aktywacja partii: petla batchowa po zaleglych krawedziach stabilnych schematow.
    // ORDER BY fact_seq -> deterministyczna kolejnosc; LIMIT ACTIVATION_BATCH -> ograniczona
    // transakcja na batch; cap RECONCILE_MAX_ITERS -> anty-DoS. Powtarzamy az brak active=0
    // dla stable (inwariant), wiec aktywujemy tez fakty, ktore stana sie aktywne "w tej samej
    // rundzie" (kolejne batche widza zaktualizowany stan).
    //
    // Aktywujemy WYLACZNIE fakty oczekujace na aktywacje: conflict_state IS NULL (swiezo
    // wstawiony przez D1) albo 'candidate' (kandydat konfliktu D3, jeszcze nie rozsadzony).
    // Fakt z 'resolved_loser' jest CELOWO zdezaktywowany przez A_res (D4 keep_winner) i jego
    // active=0 jest TERMINALNE — bez tego filtra kolejny reconcile re-aktywowalby go (active=0
    // -> 1 + upsert_edge), cofajac rozwiazanie konfliktu i ozywiajac stombstone'owana krawedz.
    let mut cap_hit = true;
    for _ in 0..RECONCILE_MAX_ITERS {
        let rows = sql_query(
            "SELECT fact_key, head_id, rel, tail_id FROM fact_state \
             WHERE active = 0 \
               AND (conflict_state IS NULL OR conflict_state = 'candidate') \
               AND schema_id IN (SELECT schema_id FROM schema_registry WHERE status = 'stable') \
             ORDER BY fact_seq LIMIT ?",
            &[SqlValue::I64(ACTIVATION_BATCH as i64)],
        )
        .map_err(|e| format!("odczyt zalegych faktow do aktywacji: {e}"))?;

        if rows.is_empty() {
            cap_hit = false;
            break;
        }

        let batch_now = now_unix();
        let mut tx: Vec<(String, Vec<SqlValue>)> = Vec::new();
        for row in &rows {
            let fact_key = row.first().and_then(|v| v.as_str()).unwrap_or_default();
            let head_id = row.get(1).and_then(|v| v.as_str()).unwrap_or_default();
            let rel = row.get(2).and_then(|v| v.as_str()).unwrap_or_default();
            let tail_id = row.get(3).and_then(|v| v.as_str()).unwrap_or_default();
            if fact_key.is_empty() || head_id.is_empty() || rel.is_empty() || tail_id.is_empty() {
                continue;
            }

            // Edge-artifact per dokument-evidence (refcount): krawedz candidate mogla byc
            // widziana w wielu dokumentach przed promocja. INSERT OR IGNORE (bug #4) +
            // refcount COUNT(DISTINCT document_id) czynia to idempotentnym — moze zostac
            // bezwarunkowe.
            let ev_docs = fact_evidence_documents(fact_key);
            for ev_doc in &ev_docs {
                push_edge_artifact(&mut tx, ev_doc, head_id, rel, tail_id, batch_now);
            }

            // EXACTLY-ONCE (blocker 1): enqueue WARUNKOWANY na active=0 MUSI byc przed
            // flipem active=1, w tej samej tx (BEGIN IMMEDIATE serializuje rownolegle
            // reconcile). Outbox upsert_edge: rekonstrukcja krawedzi z fact_state +
            // DETERMINISTYCZNA, reprezentatywna provenance z fact_evidence (#6 ORDER BY).
            let prov = representative_provenance(fact_key);
            let op = outbox_upsert_edge(head_id, rel, tail_id, &prov);
            push_outbox_if_inactive(&mut tx, &op, fact_key, batch_now);

            // Flip WARUNKOWY active=0 -> 1 + MONOTONICZNY activation_seq (kursor A_det/D3).
            // activation_seq = MAX(activation_seq)+1 podselect W TYM SAMYM UPDATE: poniewaz
            // reconcile dziala pod BEGIN IMMEDIATE (serializacja per-instancja), kolejne flipy
            // dostaja scisle rosnace numery, a kursor skanu po activation_seq lapie KAZDA
            // aktywacje niezaleznie od fact_seq (kolejnosci ingestu) — fakt wstawiony wczesnie
            // a aktywowany pozno dostaje wysoki seq. updated_at bumpujemy nadal (audyt/diagnoza),
            // ale NIE jest juz kursorem. WHERE active=0 sprawia, ze drugi rownolegly reconcile
            // (widzacy juz active=1) zmienia 0 wierszy => nie marnuje numeru seq (lustro zerowego
            // enqueue powyzej).
            tx.push((
                "UPDATE fact_state \
                 SET active = 1, \
                     activation_seq = (SELECT COALESCE(MAX(activation_seq), 0) + 1 FROM fact_state), \
                     updated_at = ? \
                 WHERE fact_key = ? AND active = 0 \
                   AND (conflict_state IS NULL OR conflict_state = 'candidate')"
                    .to_string(),
                vec![
                    SqlValue::I64(batch_now),
                    SqlValue::String(fact_key.to_string()),
                ],
            ));
        }

        let stmts: Vec<(&str, &[SqlValue])> =
            tx.iter().map(|(q, p)| (q.as_str(), p.as_slice())).collect();
        sql_transaction(&stmts).map_err(|e| format!("aktywacja batcha faktow: {e}"))?;

        // Mniej niz pelny batch => brak dalszej zaleglosci; nie ma sensu pytac ponownie.
        if rows.len() < ACTIVATION_BATCH {
            cap_hit = false;
            break;
        }
    }

    // Cichy cap zabroniony (CLAUDE.md "no silent caps"): jesli petla wyczerpala
    // RECONCILE_MAX_ITERS z wciaz pelnymi batchami, czesc zaleglych faktow zostala
    // nieaktywowana. To jest ponawialne (inwariant "brak active=0 przy stable" jest
    // EVENTUALLY-CONSISTENT — domknie go nastepny reconcile/ingest, nie to wywolanie),
    // ale ucięcie musi byc widoczne w logu wraz z liczba pozostalych faktow.
    if cap_hit {
        let remaining = sql_query_one(
            "SELECT COUNT(*) FROM fact_state \
             WHERE active = 0 \
               AND (conflict_state IS NULL OR conflict_state = 'candidate') \
               AND schema_id IN (SELECT schema_id FROM schema_registry WHERE status = 'stable')",
            &[],
        )
        .ok()
        .flatten()
        .and_then(|r| r.first().and_then(|v| v.as_i64()))
        .unwrap_or(-1);
        log::warn(&format!(
            "rag: reconcile osiagnal cap RECONCILE_MAX_ITERS={RECONCILE_MAX_ITERS} \
             (batch={ACTIVATION_BATCH}); zaleglych active=0 dla stable: {remaining} \
             — domknie nastepny reconcile (eventually-consistent)"
        ));
    }

    // (3) Materializacja krawedzi (i ewentualnych zaleglych wezlow) z trwalej kolejki.
    drain_graph_outbox()
}

// =============================================================================
// A_det — asynchroniczna detekcja konfliktow (MemGraphRAG D3, 0 LLM, 0 embeddingow)
// =============================================================================

/// Domyslny rozmiar partii skanu A_det: ile nowo-aktywnych faktow conflict_scan
/// pobiera w jednej iteracji. Mala partia => krotka transakcja i szybkie wznawianie
/// po kursorze przy rownoleglym ingescie.
const CONFLICT_SCAN_BATCH: usize = 256;

/// Twardy limit iteracji petli skanu (anty-DoS, jak przy reconcile/drain): BATCH*ITER
/// = gorne ograniczenie pracy na jedno wywolanie. Reszta zaleglosci domknie sie przy
/// nastepnym uruchomieniu conflict_scan (kursor jest trwaly i monotoniczny).
const CONFLICT_SCAN_MAX_ITERS: usize = 4096;

/// Czas trwania blokady per-kolekcja (sekundy). Skan ustawia scan_lock_until = now+TTL
/// na starcie i zwalnia (=0) na koncu. Rownolegly start widzacy lock w przyszlosci
/// pomija przebieg (jeden skan naraz). TTL zapobiega trwalemu zablokowaniu po crashu
/// w srodku skanu (lock wygasa sam, nastepny przebieg wznawia od trwalego kursora).
const CONFLICT_SCAN_LOCK_TTL_SECS: i64 = 600;

/// Twardy cap liczby wspol-faktow (peerow) branych do JEDNEJ grupy konfliktowej w jednym
/// rozpoznaniu. Popularny head_id+rel (np. encja z setkami sprzecznych tail-i) nie moze
/// wygenerowac O(n^2) pracy. Po przekroczeniu logujemy warn (zakaz cichego capu) i bierzemy
/// pierwsze MAX_CONFLICT_PEERS wspol-faktow (deterministycznie ORDER BY fact_key).
const MAX_CONFLICT_PEERS: usize = 64;

/// Twardy cap LICZBY CZLONKOW (wierszy conflict_members) per grupa konfliktowa. Czlonkostwo
/// jest znormalizowane i rosnie INSERT OR IGNORE przez wiele skanow, wiec niezaleznie od
/// MAX_CONFLICT_PEERS (cap pojedynczego rozpoznania) trzeba ograniczyc CALKOWITY rozmiar grupy
/// — inaczej encja z setkami sprzecznych tail-i akumulowalaby nieograniczona liczbe wierszy.
/// Po przekroczeniu NIE dopisujemy nowych czlonkow + log::warn (deterministycznie, bez O(n)
/// blow-up). A_res (D4) adjudykuje grupe head+rel i nie potrzebuje wszystkich faktow —
/// capowana, reprezentatywna proba wystarcza do decyzji.
const MAX_CONFLICT_MEMBERS: i64 = 64;

/// Kursor skanu jest per-INSTANCJA, nie per-kolekcja-logiczna: graf 'kg_active' i wszystkie
/// fakty (fact_state) sa wspoldzielone w obrebie jednej instancji addona (jeden SQLite,
/// jeden graf), a fakt moze pochodzic z dokumentow wielu kolekcji. Konflikty wykrywamy
/// wiec na calej instancji. collection_id z payloadu joba odwzorowujemy na ten jeden
/// wiersz kursora (sentinel), zeby zachowac ksztalt tabeli i blokade per-strumien-skanu.
const CONFLICT_SCAN_CURSOR_KEY: &str = "__instance__";

/// Typ konfliktu wynikajacy z reguly kardynalnosci relacji (relation_cardinality.kind):
/// functional -> twardy 'mutual_exclusive', temporal -> miekki 'temporal',
/// hierarchical -> 'granularity' (kandydat). Relacja bez reguly -> None (NIE konflikt).
fn conflict_type_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "functional" => Some("mutual_exclusive"),
        "temporal" => Some("temporal"),
        "hierarchical" => Some("granularity"),
        // Relacja spoza relation_cardinality (kind nieznany) NIE tworzy konfliktu:
        // brak reguly = brak pewnosci, wiec nie zalewamy systemu false-positive.
        // Similarity-gate (D5/A_res) i tak odsialaby pozorne konflikty; tu zostaje
        // czysta detekcja symboliczna ograniczona do relacji z jawna kardynalnoscia.
        _ => None,
    }
}

/// Kanoniczny dedup_key GRUPY konfliktowej: (conflict_type, head_id, rel), length-prefixed
/// (jak schema_id/outbox dedup_key — bezkolizyjny). TOZSAMOSCIA konfliktu jest GRUPA (head,rel),
/// NIE pelny zbior faktow: A_res (D4) oczekuje jednego konfliktu na grupe. Dlatego dojscie
/// nowego faktu do grupy z istniejacym open konfliktem DOPISUJE wiersz do conflict_members
/// (INSERT OR IGNORE), a nie tworzy drugiego open. Partial-unique ux_conflicts_open(dedup_key)
/// gwarantuje najwyzej JEDEN open per grupa. dedup_key wiaze tez czlonkow (conflict_members)
/// z grupa — niezalezny od conflicts.id, bo open zamyka sie i otwiera ponownie pod tym samym
/// dedup_key. conflict_type wchodzi do klucza, bo ta sama (head,rel) nie da dwoch roznych
/// typow (typ wynika z kardynalnosci rel), ale trzymanie go w kluczu jest jednoznaczne i
/// odporne na ewentualna zmiane reguly kardynalnosci.
fn conflict_dedup_key(conflict_type: &str, head_id: &str, rel: &str) -> String {
    canonical_key(&[conflict_type, head_id, rel])
}

/// Czy fakt jest za kursorem activation_seq — czysta regula odpowiadajaca predykatowi SQL
/// `activation_seq > cursor`. W produkcji predykat zyje WPROST w SQL scan_conflicts_locked /
/// write_scan_cursor; tu jest wylacznie referencyjna, testowalna bez hosta forma tej reguly.
/// Monotonicznosc activation_seq (nadawany sekwencyjnie przy aktywacji) gwarantuje, ze fakt
/// aktywowany pozno — choc wstawiony wczesnie (niski fact_seq) — ma WYZSZY seq i nie ucieka
/// kursorowi (czego sekundowy kursor czasowy nie zapewnial).
#[cfg(test)]
fn cursor_advances(cursor: i64, activation_seq: i64) -> bool {
    activation_seq > cursor
}

/// Handler tool conflict_scan (A_det). Asynchroniczny skan nowo-aktywnych faktow:
///  1. Bierze blokade per-instancja (jeden skan naraz; pomija gdy inny trwa).
///  2. Petla batchowa po fact_state.active=1 ponad kursorem (updated_at, fact_seq).
///  3. Dla kazdego faktu szuka wspol-faktow (ten sam head_id+rel, rozny tail_id),
///     klasyfikuje typ wg relation_cardinality i INSERT OR IGNORE do conflicts(open).
///  4. Advance trwalego kursora do max (updated_at, fact_seq) batcha.
/// Zwraca licznik wykrytych konfliktow i przeskanowanych faktow (do testow/dashboardu).
fn handle_conflict_scan(params: &Value) -> Value {
    let batch = params
        .get("batch_size")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, CONFLICT_SCAN_BATCH))
        .unwrap_or(CONFLICT_SCAN_BATCH);

    match run_conflict_scan(batch) {
        Ok(stats) => json!({
            "ok": true,
            "scanned": stats.scanned,
            "conflicts_detected": stats.detected,
            "cap_hit": stats.cap_hit,
        }),
        Err(e) => err(&format!("conflict_scan: {e}")),
    }
}

/// Wynik jednego przebiegu skanu A_det (do raportu tool/dashboardu i testow).
struct ConflictScanStats {
    scanned: u64,
    detected: u64,
    cap_hit: bool,
}

/// Rdzen A_det. Patrz handle_conflict_scan. Wszystkie odczyty/decyzje WYLACZNIE z SQLite
/// (R1) — graf nie jest pytany. Blokada, kursor i zapis konfliktow ida przez ledger addona.
fn run_conflict_scan(batch: usize) -> Result<ConflictScanStats, String> {
    // (1) Blokada per-instancja: jeden skan naraz. Token wlasciciela UNIKALNY na wywolanie
    // (new_id: now_unix_ms + monotoniczny licznik atomowy) — pozwala release zwolnic TYLKO
    // wlasny lock, wiec stary skan po TTL nie wyzeruje locka nowego, ktory tymczasem przejal
    // blokade. Acquire jest ATOMOWY (warunkowy UPDATE + rows_affected==1), nie read-after.
    let now = now_unix();
    let lock_until = now + CONFLICT_SCAN_LOCK_TTL_SECS;
    let owner = new_id("scan");
    let acquired = acquire_scan_lock(now, lock_until, &owner)?;
    if !acquired {
        log::info("rag: conflict_scan pominiety — inny skan trzyma blokade per-instancja");
        return Ok(ConflictScanStats {
            scanned: 0,
            detected: 0,
            cap_hit: false,
        });
    }

    let result = scan_conflicts_locked(batch);

    // (5) Zwolnij blokade niezaleznie od wyniku (skan moze byc czesciowy — kursor jest
    // trwaly, wiec nastepny przebieg wznowi od miejsca, do ktorego udalo sie dojsc).
    // Zwalniamy TYLKO wlasny lock (owner) — gdyby ten skan przekroczyl TTL i inny skan
    // tymczasem przejal blokade, nie wolno nam jej wyzerowac.
    if let Err(e) = release_scan_lock(&owner) {
        log::warn(&format!(
            "rag: conflict_scan nie zwolnil blokady (wygasnie po TTL): {e}"
        ));
    }

    result
}

/// Atomowo bierze blokade skanu. Zwraca true gdy przyznana, false gdy inny skan ja trzyma.
///
/// ATOMOWOSC bez read-after-update (blocker 2): seed wiersza (idempotentny) + JEDEN warunkowy
/// UPDATE ustawiajacy lock_until ORAZ scan_lock_owner WHERE lock wygasl (scan_lock_until IS
/// NULL OR < :now). O przyznaniu swiadczy rows_affected==1 tego UPDATE — dwa skany w TEJ SAMEJ
/// sekundzie nie moga oba zmienic wiersza (UPDATE jest serializowany, drugi widzi juz swiezy
/// lock_until >= now i zmienia 0 wierszy). Dawny wariant czytal lock_until po update i porownywal
/// — dwa skany z tym samym lock_until oba "potwierdzaly" sukces (oba widzialy te sama wartosc).
fn acquire_scan_lock(now: i64, lock_until: i64, owner: &str) -> Result<bool, String> {
    // Seed wiersza w osobnym exec: INSERT OR IGNORE nie zaklamuje rows_affected warunkowego
    // UPDATE (gdyby byly w jednej tx, sql_transaction sumowalby zmienione wiersze).
    sql_exec(
        "INSERT OR IGNORE INTO conflict_scan_cursor (collection_id) VALUES (?)",
        &[SqlValue::String(CONFLICT_SCAN_CURSOR_KEY.to_string())],
    )
    .map_err(|e| format!("seed kursora skanu: {e}"))?;

    let res = sql_exec(
        "UPDATE conflict_scan_cursor \
         SET scan_lock_until = ?, scan_lock_owner = ? \
         WHERE collection_id = ? \
           AND (scan_lock_until IS NULL OR scan_lock_until < ?)",
        &[
            SqlValue::I64(lock_until),
            SqlValue::String(owner.to_string()),
            SqlValue::String(CONFLICT_SCAN_CURSOR_KEY.to_string()),
            SqlValue::I64(now),
        ],
    )
    .map_err(|e| format!("blokada skanu: {e}"))?;

    Ok(res.rows_affected == 1)
}

/// Zwalnia blokade skanu (lock_until=0), zostawiajac kursor (last_activation_seq) nietkniety.
/// WHERE scan_lock_owner=? — zwalniamy WYLACZNIE wlasny lock, nigdy cudzy (chroni przed
/// wyzerowaniem locka nowego skanu przez stary, ktory przekroczyl TTL).
fn release_scan_lock(owner: &str) -> Result<(), String> {
    sql_exec(
        "UPDATE conflict_scan_cursor SET scan_lock_until = 0, scan_lock_owner = NULL \
         WHERE collection_id = ? AND scan_lock_owner = ?",
        &[
            SqlValue::String(CONFLICT_SCAN_CURSOR_KEY.to_string()),
            SqlValue::String(owner.to_string()),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("zwolnienie blokady skanu: {e}"))
}

/// Petla skanu pod trzymana blokada. Patrz run_conflict_scan.
fn scan_conflicts_locked(batch: usize) -> Result<ConflictScanStats, String> {
    let mut cursor = read_scan_cursor()?;
    let mut scanned: u64 = 0;
    let mut detected: u64 = 0;
    let mut cap_hit = true;

    for _ in 0..CONFLICT_SCAN_MAX_ITERS {
        // Batch nowo-aktywnych faktow ponad kursorem activation_seq. active=1: skanujemy
        // tylko fakty wpuszczone do aktywnego widoku (po denoisingu). ORDER BY activation_seq
        // => deterministyczna kolejnosc i kursor = max(activation_seq) batcha. activation_seq
        // jest nadawany przy AKTYWACJI (reconcile), wiec lapie fakty aktywowane pozno, mimo
        // niskiego fact_seq (kolejnosc ingestu) — sedno blockera 1.
        let rows = sql_query(
            "SELECT activation_seq, fact_key, schema_id, head_id, rel, tail_id \
             FROM fact_state \
             WHERE active = 1 AND activation_seq > ? \
             ORDER BY activation_seq LIMIT ?",
            &[SqlValue::I64(cursor), SqlValue::I64(batch as i64)],
        )
        .map_err(|e| format!("odczyt nowo-aktywnych faktow: {e}"))?;

        if rows.is_empty() {
            cap_hit = false;
            break;
        }

        for row in &rows {
            let activation_seq = row.first().and_then(|v| v.as_i64()).unwrap_or(0);
            let fact_key = row.get(1).and_then(|v| v.as_str()).unwrap_or_default();
            let schema_id = row.get(2).and_then(|v| v.as_str()).unwrap_or_default();
            let head_id = row.get(3).and_then(|v| v.as_str()).unwrap_or_default();
            let rel = row.get(4).and_then(|v| v.as_str()).unwrap_or_default();
            let tail_id = row.get(5).and_then(|v| v.as_str()).unwrap_or_default();

            // Advance kursora ZAWSZE (nawet gdy fakt nie ma konfliktu), inaczej skan
            // utknalby na tym samym wierszu. Kursor = max activation_seq batcha.
            cursor = activation_seq;
            scanned += 1;

            if fact_key.is_empty() || head_id.is_empty() || rel.is_empty() {
                continue;
            }

            // Typ konfliktu z reguly kardynalnosci relacji. Relacja bez reguly -> brak
            // konfliktu (conflict_type_for_kind zwraca None) -> nie pytamy nawet o
            // wspol-fakty (oszczednosc + zero false-positive).
            let kind = relation_kind(rel)?;
            let Some(conflict_type) = kind.as_deref().and_then(conflict_type_for_kind) else {
                continue;
            };

            // Wspol-fakty SYMBOLICZNIE: aktywne fakty o TYM SAMYM head_id i rel, ale ROZNYM
            // tail_id. Indeks ix_fact_state_peer(active,head_id,rel,tail_id) zaweza do grupy.
            // TWARDY cap (LIMIT MAX_CONFLICT_PEERS+1): wykryty nadmiar (zwrocono >cap) loguje
            // warn i bierze pierwsze MAX_CONFLICT_PEERS (ORDER BY fact_key deterministyczne) —
            // popularny head_id+rel nie robi O(n^2). Calkowity rozmiar grupy ogranicza dodatkowo
            // MAX_CONFLICT_MEMBERS przy dopisywaniu czlonkow (upsert_group_conflict).
            let peers = sql_query(
                "SELECT fact_key FROM fact_state \
                 WHERE active = 1 AND head_id = ? AND rel = ? AND tail_id <> ? \
                 ORDER BY fact_key LIMIT ?",
                &[
                    SqlValue::String(head_id.to_string()),
                    SqlValue::String(rel.to_string()),
                    SqlValue::String(tail_id.to_string()),
                    SqlValue::I64(MAX_CONFLICT_PEERS as i64 + 1),
                ],
            )
            .map_err(|e| format!("odczyt wspol-faktow: {e}"))?;

            if peers.is_empty() {
                continue;
            }

            let peers_capped = peers.len() > MAX_CONFLICT_PEERS;
            if peers_capped {
                log::warn(&format!(
                    "rag: conflict_scan cap MAX_CONFLICT_PEERS={MAX_CONFLICT_PEERS} \
                     trafiony dla head_id={head_id} rel={rel}; biore pierwsze {MAX_CONFLICT_PEERS} \
                     wspol-faktow (A_res zdecyduje na probie)"
                ));
            }

            // Zbior konfliktowy = biezacy fakt + wspol-fakty (rozny tail), do capu.
            let mut fact_keys: Vec<String> = Vec::with_capacity(MAX_CONFLICT_PEERS + 1);
            fact_keys.push(fact_key.to_string());
            for p in peers.iter().take(MAX_CONFLICT_PEERS) {
                if let Some(k) = p.first().and_then(|v| v.as_str()) {
                    fact_keys.push(k.to_string());
                }
            }

            // TOZSAMOSC = GRUPA (conflict_type, head_id, rel), NIE pelny zbior faktow.
            // Upsert po grupie: zapewnia JEDEN open konflikt grupy (conflicts) i dopisuje
            // czlonkow (conflict_members) atomowym INSERT OR IGNORE — rosnacy zbior nie
            // tworzy drugiego open ani nie robi read-modify-write union (blocker 2).
            let dedup_key = conflict_dedup_key(conflict_type, head_id, rel);
            if upsert_group_conflict(
                conflict_type,
                schema_id,
                head_id,
                rel,
                &dedup_key,
                &fact_keys,
            )? {
                detected += 1;
            }
        }

        // Trwaly advance kursora po KAZDYM batchu (wznawialnosc przy crashu/cap).
        write_scan_cursor(cursor)?;

        if rows.len() < batch {
            cap_hit = false;
            break;
        }
    }

    if cap_hit {
        // Cichy cap zabroniony (CLAUDE.md): petla wyczerpala CONFLICT_SCAN_MAX_ITERS z
        // pelnymi batchami — reszta nowo-aktywnych faktow zostanie przeskanowana przy
        // nastepnym uruchomieniu (kursor trwaly), ale ucięcie musi byc widoczne w logu.
        log::warn(&format!(
            "rag: conflict_scan osiagnal cap CONFLICT_SCAN_MAX_ITERS={CONFLICT_SCAN_MAX_ITERS} \
             (batch={batch}); przeskanowano {scanned}, wykryto {detected} \
             — reszte domknie nastepny skan (kursor trwaly)"
        ));
    }

    Ok(ConflictScanStats {
        scanned,
        detected,
        cap_hit,
    })
}

/// Czyta last_activation_seq kursora skanu. Brak wiersza -> 0 (skan od poczatku) —
/// acquire_scan_lock seeduje wiersz, wiec normalnie istnieje.
fn read_scan_cursor() -> Result<i64, String> {
    let row = sql_query_one(
        "SELECT last_activation_seq FROM conflict_scan_cursor WHERE collection_id = ?",
        &[SqlValue::String(CONFLICT_SCAN_CURSOR_KEY.to_string())],
    )
    .map_err(|e| format!("odczyt kursora skanu: {e}"))?;
    Ok(row
        .and_then(|r| r.first().and_then(|v| v.as_i64()))
        .unwrap_or(0))
}

/// Trwaly advance kursora skanu do activation_seq. Monotoniczny: zapisujemy WYLACZNIE gdy
/// nowy seq jest scisle za dotychczasowym (zabezpieczenie przed cofnieciem przy rownoleglych
/// przebiegach — choc blokada juz to wyklucza, predykat jest tani).
fn write_scan_cursor(activation_seq: i64) -> Result<(), String> {
    sql_exec(
        "UPDATE conflict_scan_cursor SET last_activation_seq = ? \
         WHERE collection_id = ? AND ? > last_activation_seq",
        &[
            SqlValue::I64(activation_seq),
            SqlValue::String(CONFLICT_SCAN_CURSOR_KEY.to_string()),
            SqlValue::I64(activation_seq),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("zapis kursora skanu: {e}"))
}

/// Upsert konfliktu PO GRUPIE (dedup_key = conflict_type+head_id+rel) ze ZNORMALIZOWANYM
/// czlonkostwem (conflict_members). Zwraca true gdy powstal NOWY otwarty konflikt (do licznika
/// detected), false gdy tylko dopisano czlonkow do istniejacego open. Logika:
///  1. INSERT OR IGNORE do conflicts(...,'open') — partial-unique ux_conflicts_open(dedup_key)
///     gwarantuje DOKLADNIE jeden open per grupa. rows_affected==1 => NOWY konflikt.
///  2. Dla kazdego faktu w konflikcie: INSERT OR IGNORE do conflict_members(dedup_key,fact_key).
///     ATOMOWE i IDEMPOTENTNE (PRIMARY KEY) — bez read-modify-write union, wiec znika wyscig,
///     w ktorym dwa rownolegle skany nadpisywaly sobie wiekszy zbior (blocker 2).
///  3. members_rev = COUNT(*) czlonkow grupy (AUTORYTATYWNA liczba, nie inkrement) ustawiany
///     PODZAPYTANIEM w tej SAMEJ transakcji co inserty czlonkow — patrz nizej (TOCTOU).
///  4. Cap czlonkow: przed dopisaniem sprawdzamy COUNT(*) grupy; po MAX_CONFLICT_MEMBERS NIE
///     dodajemy nowych + log::warn (deterministycznie, bez O(n) blow-up). A_res adjudykuje
///     grupe head+rel na reprezentatywnej probie — nie potrzebuje wszystkich faktow.
///
/// ATOMOWOSC vs A_res (TOCTOU): inserty conflict_members ORAZ
/// `UPDATE members_rev=(SELECT COUNT(*) ...)` ida w JEDNEJ sql_transaction (jeden commit).
/// Dzieki temu zachodzi inwariant: "czlonek widoczny dla A_res ⟺ members_rev odzwierciedla
/// jego obecnosc". members_rev to AUTORYTATYWNY COUNT (wzor z D2 freq=COUNT(fact_schema)), a
/// NIE inkrement: eliminuje wyscig dwoch rownoleglych inkrementow i czyni licznik funkcja
/// stanu (idempotentny re-run daje ten sam COUNT). A_res czyta rev0 PRZED snapshotem czlonkow;
/// jesli zbior urosnie po jego rev0, members_rev>rev0 i KAZDY write apply/finalize (warunkowany
/// na members_rev=:rev0) staje sie no-op => decyzja na niepelnym zbiorze odrzucona, konflikt
/// wraca do 'open' do re-adjudykacji swiezego pelnego zbioru. Stale-close jest NIEMOZLIWY: nie
/// istnieje przeplot, w ktorym A_res widzi nowego czlonka, a members_rev wciaz == rev0.
///
/// Idempotencja: ponowny skan tej samej grupy z tymi samymi faktami => INSERT OR IGNORE bez
/// zmian (zero nowych wierszy), a COUNT(*) zwraca te sama liczbe => members_rev bez zmian.
/// Spojne z oczekiwaniem A_res (D4): jeden open per grupa, czlonkowie odczytywani po dedup_key.
fn upsert_group_conflict(
    conflict_type: &str,
    schema_id: &str,
    head_id: &str,
    rel: &str,
    dedup_key: &str,
    fact_keys: &[String],
) -> Result<bool, String> {
    let now = now_unix();

    // (0) Metryka `detected`/is_new: pre-check istnienia AKTYWNEGO konfliktu PRZED transakcja.
    // To WYLACZNIE licznik (ile NOWYCH grup wykrylismy w tym skanie) — nie wplywa na poprawnosc
    // zadnej sciezki (apply/finalize warunkowane sa members_rev+ownership, nie tym flagiem).
    // Dlatego drobna nieprecyzja przy wyscigu metryki (dwa rownolegle skany tego samego nowego
    // konfliktu) jest nieszkodliwa. NIE rozbijamy transakcji upsertu dla samego licznika:
    // INSERT OR IGNORE konfliktu MUSI byc w jednej tx z insertami czlonkow i members_rev=COUNT.
    let already_active = sql_query_one(
        "SELECT 1 FROM conflicts WHERE dedup_key = ? AND status IN ('open', 'resolving')",
        &[SqlValue::String(dedup_key.to_string())],
    )
    .map_err(|e| format!("pre-check istnienia konfliktu: {e}"))?
    .is_some();
    let is_new = !already_active;

    // (4) Cap czlonkow grupy: jednorazowy COUNT przed budowaniem transakcji. To filtr WEJSCIA
    // (ile faktow w ogole probowac dopisac) — soft-limit, nie musi byc atomowy z insertami:
    // narastajacy `room` chroni przed przekroczeniem, a ewentualny rownolegly insert i tak
    // wpadnie do COUNT(*) ustawiajacego members_rev (autorytatywnego), wiec rev nie sklamie.
    let member_count = sql_query_one(
        "SELECT COUNT(*) FROM conflict_members WHERE conflict_dedup_key = ?",
        &[SqlValue::String(dedup_key.to_string())],
    )
    .map_err(|e| format!("odczyt liczby czlonkow konfliktu: {e}"))?
    .and_then(|r| r.first().and_then(|v| v.as_i64()))
    .unwrap_or(0);

    // Tozsamosc grupy + inserty czlonkow + members_rev=COUNT skladamy w JEDNA sql_transaction
    // (jeden commit), zeby ZADEN konflikt nie byl widoczny dla A_res bez kompletu czlonkow i bez
    // members_rev=COUNT. Gdyby INSERT OR IGNORE konfliktu byl osobnym sql_exec przed tx czlonkow,
    // istnialoby waskie okno: swiezo utworzony open ma members_rev=0 i ZERO czlonkow, a wspolbiezny
    // A_res (osobny lock) moglby go claimnac, collect_conflict_facts zwrocilby 0 aktywnych faktow
    // (<2) i A_res blednie zamknalby konflikt; pozniejszy UPDATE ... WHERE status IN('open',
    // 'resolving') juz by go nie znalazl -> osierocony konflikt z czlonkami, ale resolved. Jedna
    // tx eliminuje to okno: po commicie open ZAWSZE ma komplet czlonkow i spojny members_rev.
    // owned: bufory String, refs: pozyczone slice'y do sql_transaction (q, params).
    // INSERT OR IGNORE + partial-unique ux_conflicts_active => max 1 open per dedup_key.
    let mut owned: Vec<(String, Vec<SqlValue>)> = Vec::with_capacity(fact_keys.len() + 2);
    owned.push((
        "INSERT OR IGNORE INTO conflicts \
           (conflict_type, schema_id, head_id, rel, dedup_key, status, created_at) \
         VALUES (?, ?, ?, ?, ?, 'open', ?)"
            .to_string(),
        vec![
            SqlValue::String(conflict_type.to_string()),
            SqlValue::String(schema_id.to_string()),
            SqlValue::String(head_id.to_string()),
            SqlValue::String(rel.to_string()),
            SqlValue::String(dedup_key.to_string()),
            SqlValue::I64(now),
        ],
    ));

    if member_count >= MAX_CONFLICT_MEMBERS {
        log::warn(&format!(
            "rag: conflict_members cap MAX_CONFLICT_MEMBERS={MAX_CONFLICT_MEMBERS} \
             osiagniety dla dedup_key={dedup_key} (head_id={head_id} rel={rel}); \
             nie dodaje nowych czlonkow (A_res zdecyduje na probie)"
        ));
    } else {
        // (2) Czlonkostwo: idempotentny INSERT OR IGNORE per fakt. Cap egzekwowany narastajaco:
        // room maleje za KAZDY zaplanowany insert (konserwatywnie, nawet jesli IGNORE pochlonie
        // duplikat) — gwarantuje, ze nie przekroczymy MAX_CONFLICT_MEMBERS nawet gdy biezacy
        // zbior faktow jest wiekszy niz wolne miejsce. members_rev i tak bedzie autorytatywnym
        // COUNT(*) po commicie, wiec ewentualne niedopelnienie nie psuje licznika dla A_res.
        let mut room = MAX_CONFLICT_MEMBERS - member_count;
        for fk in fact_keys {
            if room <= 0 {
                log::warn(&format!(
                    "rag: conflict_members cap MAX_CONFLICT_MEMBERS={MAX_CONFLICT_MEMBERS} \
                     wyczerpany w trakcie dopisywania dla dedup_key={dedup_key}; \
                     pomijam pozostale fakty biezacego zbioru"
                ));
                break;
            }
            owned.push((
                "INSERT OR IGNORE INTO conflict_members (conflict_dedup_key, fact_key, added_at) \
                 VALUES (?, ?, ?)"
                    .to_string(),
                vec![
                    SqlValue::String(dedup_key.to_string()),
                    SqlValue::String(fk.to_string()),
                    SqlValue::I64(now),
                ],
            ));
            room -= 1;
        }
    }

    // (3) members_rev = AUTORYTATYWNY COUNT(*) czlonkow grupy, w tej samej transakcji co inserty.
    // To twardy straznik TOCTOU dla A_res: po commicie members_rev ZAWSZE odpowiada realnemu
    // zbiorowi czlonkow widocznemu dla A_res. Inkrement bylby podatny na wyscig (dwa rownolegle
    // +1 moga sie zgubic) i nie bylby funkcja stanu; COUNT(*) jest idempotentny i autorytatywny
    // (wzor z D2 freq=COUNT(fact_schema)). WHERE status IN(open,resolving): domkniety konflikt
    // (resolved/escalated) nie jest juz czytany przez A_res, a nowy fakt i tak otworzy nowy open
    // w (1) przy nastepnym skanie. updated_at odswiezamy razem (recovery A_res po 'resolving').
    owned.push((
        "UPDATE conflicts SET \
           members_rev = (SELECT COUNT(*) FROM conflict_members WHERE conflict_dedup_key = ?), \
           updated_at = ? \
         WHERE dedup_key = ? AND status IN ('open', 'resolving')"
            .to_string(),
        vec![
            SqlValue::String(dedup_key.to_string()),
            SqlValue::I64(now),
            SqlValue::String(dedup_key.to_string()),
        ],
    ));

    let refs: Vec<(&str, &[SqlValue])> =
        owned.iter().map(|(q, p)| (q.as_str(), p.as_slice())).collect();
    sql_transaction(&refs).map_err(|e| format!("zapis grupy konfliktu (atomowy): {e}"))?;

    Ok(is_new)
}

/// Reguła kardynalnosci relacji z tabeli konfiguracyjnej relation_cardinality. None =
/// relacja spoza tabeli (A_det jej nie klasyfikuje -> brak konfliktu).
fn relation_kind(rel: &str) -> Result<Option<String>, String> {
    let row = sql_query_one(
        "SELECT kind FROM relation_cardinality WHERE relation = ?",
        &[SqlValue::String(rel.to_string())],
    )
    .map_err(|e| format!("odczyt kardynalnosci relacji: {e}"))?;
    Ok(row.and_then(|r| r.first().and_then(|v| v.as_str()).map(str::to_string)))
}

// =============================================================================
// A_res — asynchroniczna adjudykacja konfliktow przez LLM (MemGraphRAG D4)
// =============================================================================
//
// A_res jest OSOBNY od A_det (conflict_scan): detekcja jest tania (0 LLM) i moze biec
// czesto, adjudykacja jest droga (LLM) i musi miec twarde limity kosztu (R8). Rozdzielenie
// pozwala na niezalezne harmonogramy. A_res czyta i decyduje WYLACZNIE z SQLite (R1); graf
// jest tylko KARMIONY tombstone'ami przez graph_outbox (R3), nigdy pytany o stan.

/// Twardy cap liczby konfliktow adjudykowanych w JEDNYM przebiegu conflict_resolve (R8).
/// Kazdy konflikt to potencjalnie jedno wywolanie LLM — to gorne ograniczenie kosztu na
/// uruchomienie. Reszta open konfliktow domknie sie przy nastepnym przebiegu (kandydaci sa
/// brani deterministycznie ORDER BY id, a przejete zmieniaja status, wiec kursor nie jest
/// potrzebny — kolejny przebieg nie zobaczy juz rozwiazanych).
const MAX_CONFLICTS_PER_RUN: usize = 20;

/// Twardy cap LACZNEJ dlugosci tekstu pasazy (evidence) wlozonej do promptu LLM (R8: token
/// cap, ~znaki). Po przekroczeniu przestajemy dokladac pasaze (deterministycznie, ORDER BY
/// confidence DESC — najmocniejsze evidence jako pierwsze) i logujemy obciecie (zakaz cichego
/// capu). LLM dostaje reprezentatywna, najmocniejsza probe — wystarczy do decyzji.
const MAX_EVIDENCE_CHARS: usize = 12_000;

/// Twardy cap liczby pasazy (wierszy evidence) na CALY konflikt — druga warstwa obok
/// MAX_EVIDENCE_CHARS. Chroni przed konfliktem z setkami krotkich pasazy (kazdy ponizej
/// progu znakow, ale lacznie ogromna liczba), ktory rozdmuchalby prompt liczba pozycji.
const MAX_EVIDENCE_PASSAGES: usize = 24;

/// Maks. dlugosc pojedynczego pasazu wstrzyknietego do promptu (znaki). Jeden bardzo dlugi
/// chunk nie moze sam wyczerpac calego budzetu znakow — przycinamy go, by zmiescic evidence
/// z OBU stron konfliktu (inaczej LLM widzialby tylko jedna strone => stronnicza decyzja).
const MAX_EVIDENCE_PASSAGE_CHARS: usize = 2_000;

/// Twardy cap kandydatow evidence pobieranych z DB PER CZLONEK konfliktu (przed balansem).
/// Round-robin selekcja i tak ograniczy wynik globalnymi capami; ten limit chroni samo
/// zapytanie przed sciagnieciem ogromu wierszy gdy jeden fakt ma setki pasazy.
const MAX_EVIDENCE_PER_MEMBER_FETCH: usize = 32;

/// TTL (sekundy) dla konfliktu zaklinowanego w 'resolving' (crash A_res w trakcie adjudykacji
/// przed zapisem decyzji). Po tym czasie kolejny przebieg traktuje go jak open (re-claim).
/// Dluzszy niz realistyczny czas jednej adjudykacji LLM (latencja modelu), by NIE re-claimowac
/// konfliktu, ktory jest wlasnie adjudykowany przez wspolbiezny, zywy przebieg.
const CONFLICT_RESOLVE_RESOLVING_TTL_SECS: i64 = 900;

/// Bufor na odpowiedz adjudykacji LLM (decyzja JSON — kilkaset bajtow wystarcza, ale dajemy
/// zapas na dluzsze 'reason').
const RESOLVE_BUFFER_SIZE: usize = 16_384;

/// Decyzja adjudykacji sparsowana z odpowiedzi LLM (po odpornym parsowaniu). action steruje
/// zastosowaniem; winner_fact_key jest wymagany tylko dla keep_winner; reason jest audytowy.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolveDecision {
    action: ResolveAction,
    winner_fact_key: Option<String>,
    reason: String,
}

/// Akcja decyzji A_res (zamkniety enum zamiast luznego String — silne typowanie, brak
/// nieznanych galezi w stosowaniu decyzji). Nieznana/niesparsowalna akcja LLM => Escalate
/// (bezpieczny default: oddaj czlowiekowi, nie zgaduj).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolveAction {
    KeepWinner,
    TemporalSplit,
    MergeEntities,
    Escalate,
}

impl ResolveAction {
    /// Mapuje surowy string akcji z LLM na enum. Nieznana wartosc -> None (caller eskaluje).
    fn from_str(s: &str) -> Option<Self> {
        match s.trim() {
            "keep_winner" => Some(Self::KeepWinner),
            "temporal_split" => Some(Self::TemporalSplit),
            "merge_entities" => Some(Self::MergeEntities),
            "escalate" => Some(Self::Escalate),
            _ => None,
        }
    }

    /// Etykieta do JSON-a decyzji / audytu (stabilna, niezalezna od Debug).
    fn as_label(self) -> &'static str {
        match self {
            Self::KeepWinner => "keep_winner",
            Self::TemporalSplit => "temporal_split",
            Self::MergeEntities => "merge_entities",
            Self::Escalate => "escalate",
        }
    }
}

/// Jeden fakt nalezacy do grupy konfliktowej, z rozwinietymi danymi do promptu/decyzji.
#[derive(Debug, Clone)]
struct ConflictFact {
    fact_key: String,
    head_id: String,
    rel: String,
    tail_id: String,
}

/// Pasaz zrodlowy (evidence) jednego faktu — tekst chunku + pewnosc. confidence steruje
/// kolejnoscia wkladania do promptu (ORDER BY confidence DESC: najmocniejsze najpierw).
#[derive(Debug, Clone)]
struct EvidencePassage {
    fact_key: String,
    text: String,
    confidence: f64,
}

/// Wynik jednego przebiegu A_res (do raportu tool/dashboardu i testow).
#[derive(Debug, Default, PartialEq, Eq)]
struct ConflictResolveStats {
    /// Ile konfliktow faktycznie przejeto (claim) i przetworzono w tym przebiegu.
    processed: u64,
    /// Ile rozwiazano automatycznie (keep_winner/temporal_split -> resolved_auto).
    resolved_auto: u64,
    /// Ile oznaczono do merge (resolved_merge_pending -> D5).
    merge_pending: u64,
    /// Ile eskalowano do czlowieka (escalated).
    escalated: u64,
    /// Ile obsluzono z cache (zbior czlonkow bez zmian -> bez wywolania LLM).
    cache_hits: u64,
    /// Czy trafiono cap MAX_CONFLICTS_PER_RUN (sa jeszcze open do domkniecia).
    cap_hit: bool,
}

/// Handler tool conflict_resolve (A_res). Patrz run_conflict_resolve.
fn handle_conflict_resolve(params: &Value) -> Value {
    let max_conflicts = params
        .get("max_conflicts")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, MAX_CONFLICTS_PER_RUN))
        .unwrap_or(MAX_CONFLICTS_PER_RUN);

    match run_conflict_resolve(max_conflicts) {
        Ok(stats) => json!({
            "ok": true,
            "processed": stats.processed,
            "resolved_auto": stats.resolved_auto,
            "merge_pending": stats.merge_pending,
            "escalated": stats.escalated,
            "cache_hits": stats.cache_hits,
            "cap_hit": stats.cap_hit,
        }),
        Err(e) => err(&format!("conflict_resolve: {e}")),
    }
}

/// Rdzen A_res. Bierze do MAX_CONFLICTS_PER_RUN konfliktow OPEN (lub 'resolving' po TTL =
/// recovery), claimuje kazdy exactly-once (open->resolving), zbiera evidence z capem, decyduje
/// (cache lub LLM) i stosuje decyzje ODWRACALNIE. Wszystkie odczyty/decyzje WYLACZNIE z SQLite
/// (R1) — graf karmiony tombstone'ami przez outbox (R3).
fn run_conflict_resolve(max_conflicts: usize) -> Result<ConflictResolveStats, String> {
    let mut stats = ConflictResolveStats::default();
    let now = now_unix();
    let resolving_deadline = now - CONFLICT_RESOLVE_RESOLVING_TTL_SECS;

    // Kandydaci: realne open ORAZ 'resolving' starsze niz TTL (recovery po crashu — punkt 8).
    // ORDER BY id => deterministyczna kolejnosc; LIMIT = twardy cap kosztu LLM na przebieg (R8).
    // Bierzemy max_conflicts+1, by wykryc (i zalogowac) ze cap przycial liste (zakaz cichego capu).
    let candidates = sql_query(
        "SELECT id, conflict_type, schema_id, head_id, rel, dedup_key, decision, resolved_members_hash, members_rev \
         FROM conflicts \
         WHERE status = 'open' OR (status = 'resolving' AND COALESCE(updated_at, 0) < ?) \
         ORDER BY id LIMIT ?",
        &[
            SqlValue::I64(resolving_deadline),
            SqlValue::I64(max_conflicts as i64 + 1),
        ],
    )
    .map_err(|e| format!("odczyt otwartych konfliktow: {e}"))?;

    if candidates.len() > max_conflicts {
        stats.cap_hit = true;
        log::warn(&format!(
            "rag: conflict_resolve cap MAX_CONFLICTS_PER_RUN={max_conflicts} trafiony \
             (jest wiecej otwartych konfliktow); reszte domknie nastepny przebieg"
        ));
    }

    for row in candidates.iter().take(max_conflicts) {
        let id = row.first().and_then(|v| v.as_i64()).unwrap_or(0);
        let conflict_type = row.get(1).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let schema_id = row.get(2).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let head_id = row.get(3).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let rel = row.get(4).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let dedup_key = row.get(5).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let prior_decision = row.get(6).and_then(|v| v.as_str()).map(str::to_string);
        let prior_hash = row.get(7).and_then(|v| v.as_str()).map(str::to_string);

        // (1) Claim exactly-once: warunkowy UPDATE open/resolving-po-TTL -> resolving + stempel
        // resolve_owner=token. Tylko rows_affected==1 oznacza, ze TEN przebieg przejal konflikt;
        // wspolbiezny przebieg (lub szybszy claim) dostaje 0 i pomija. Stempel updated_at=now
        // resetuje TTL recovery. Token (unikalny new_id) identyfikuje wlasciciela: apply+finalize
        // warunkuje KAZDY write na tym tokenie, wiec gdy drugi przebieg przejmie konflikt po TTL
        // (LLM pierwszego trwa >TTL), spozniony apply pierwszego jest no-op (brak podwojnego apply).
        let owner = new_id("res");
        // rev0: members_rev odczytany ATOMOWO z udanym claimem (po przejsciu na 'resolving').
        // Od tego momentu kazdy bump przez D3 (nowy czlonek) jest TOCTOU, ktory chcemy wykryc.
        let rev0 = match claim_conflict(id, &owner, now, resolving_deadline)? {
            Some(rev0) => rev0,
            None => continue,
        };
        stats.processed += 1;

        match resolve_one_conflict(
            id,
            &owner,
            rev0,
            &conflict_type,
            &schema_id,
            &head_id,
            &rel,
            &dedup_key,
            prior_decision.as_deref(),
            prior_hash.as_deref(),
        ) {
            Ok(outcome) => match outcome {
                ResolveOutcome::ResolvedAuto { cache_hit } => {
                    stats.resolved_auto += 1;
                    if cache_hit {
                        stats.cache_hits += 1;
                    }
                }
                ResolveOutcome::MergePending => stats.merge_pending += 1,
                ResolveOutcome::Escalated => stats.escalated += 1,
            },
            Err(e) => {
                // Blad adjudykacji JEDNEGO konfliktu nie przerywa calego przebiegu — konflikt
                // zostaje w 'resolving' i wroci przez recovery po TTL (punkt 8). Reszta open
                // jest dalej przetwarzana (best-effort, jak drain/scan).
                log::warn(&format!(
                    "rag: conflict_resolve nie zaadjudykowal konfliktu id={id} \
                     (zostaje 'resolving', wroci po TTL): {e}"
                ));
            }
        }
    }

    Ok(stats)
}

/// Wynik adjudykacji jednego konfliktu (do agregacji statystyk przebiegu).
enum ResolveOutcome {
    ResolvedAuto { cache_hit: bool },
    MergePending,
    Escalated,
}

/// (1) Atomowy claim konfliktu: open (lub resolving po TTL) -> resolving, stempel updated_at
/// i resolve_owner=token. rows_affected==1 => ten przebieg przejal. Warunek lustruje selekcje
/// kandydatow, wiec dwa
/// przebiegi widzace tego samego kandydata serializuja sie na UPDATE (drugi widzi status juz
/// 'resolving' ze swiezym updated_at => 0 wierszy => pomija). Bez read-after-update (jak
/// acquire_scan_lock).
fn claim_conflict(
    id: i64,
    owner: &str,
    now: i64,
    resolving_deadline: i64,
) -> Result<Option<i64>, String> {
    // Stempel resolve_owner=token wraz z przejeciem: caly pozniejszy apply+finalize jest
    // warunkowany na (status='resolving' AND resolve_owner=token). Re-claim po TTL (recovery)
    // NADPISUJE owner nowym tokenem, wiec stary przebieg ktory wroci z LLM po przejeciu przez
    // drugiego ma juz nieaktualny owner => jego writy to no-op (brak podwojnego apply).
    let res = sql_exec(
        "UPDATE conflicts SET status = 'resolving', resolve_owner = ?, updated_at = ? \
         WHERE id = ? AND (status = 'open' OR (status = 'resolving' AND COALESCE(updated_at, 0) < ?))",
        &[
            SqlValue::String(owner.to_string()),
            SqlValue::I64(now),
            SqlValue::I64(id),
            SqlValue::I64(resolving_deadline),
        ],
    )
    .map_err(|e| format!("claim konfliktu id={id}: {e}"))?;
    if res.rows_affected != 1 {
        return Ok(None);
    }
    // rev0: members_rev po udanym claimie. Czytamy GO TU (a nie z pre-claim SELECT), bo to
    // jest punkt odniesienia TOCTOU — od chwili przejscia na 'resolving' z naszym ownerem kazdy
    // bump (D3 doklejajacy nowego czlonka konfliktowi 'resolving') zmieni members_rev != rev0,
    // co uniewazni apply/finalize tego przebiegu (decyzja na nieaktualnym zbiorze odrzucona).
    let rev0 = sql_query_one(
        "SELECT members_rev FROM conflicts WHERE id = ?",
        &[SqlValue::I64(id)],
    )
    .map_err(|e| format!("odczyt members_rev po claim id={id}: {e}"))?
    .and_then(|r| r.first().and_then(|v| v.as_i64()))
    .unwrap_or(0);
    Ok(Some(rev0))
}

/// Adjudykuje JEDEN przejety (status='resolving') konflikt: zbiera czlonkow+evidence z capem,
/// liczy member_set_hash, sprawdza cache (R8), w razie potrzeby woła LLM i stosuje decyzje
/// ODWRACALNIE. Zwraca ResolveOutcome do statystyk.
#[allow(clippy::too_many_arguments)]
fn resolve_one_conflict(
    id: i64,
    owner: &str,
    rev0: i64,
    conflict_type: &str,
    schema_id: &str,
    head_id: &str,
    rel: &str,
    dedup_key: &str,
    prior_decision: Option<&str>,
    prior_hash: Option<&str>,
) -> Result<ResolveOutcome, String> {
    // (2) Czlonkowie grupy -> fakty (fact_state). Tylko AKTYWNE fakty wchodza do adjudykacji:
    // przegrany z poprzedniej rundy (active=0) nie wraca jako kandydat. Brak >=2 aktywnych
    // faktow => konflikt sam sie rozwiazal (np. przegrany juz stombstone'owany / dokument
    // usuniety) => zamykamy jako resolved_auto bez LLM.
    let facts = collect_conflict_facts(dedup_key)?;
    let member_set_hash = member_set_hash(&facts);

    if facts.len() < 2 {
        log::info(&format!(
            "rag: conflict_resolve konflikt id={id} ma <2 aktywne fakty — \
             rozwiazal sie sam, zamykam (resolved_auto, bez LLM)"
        ));
        let decision = json!({
            "action": "keep_winner",
            "reason": "konflikt rozwiazal sie sam (mniej niz 2 aktywne fakty)",
            "members": facts.iter().map(|f| f.fact_key.clone()).collect::<Vec<_>>(),
            "auto": true,
        });
        finalize_conflict(id, owner, rev0, "resolved_auto", &decision, &member_set_hash)?;
        return Ok(ResolveOutcome::ResolvedAuto { cache_hit: false });
    }

    // (3) Cache R8: identyczny zbior czlonkow (member_set_hash) + istniejaca decyzja => NIE
    // wolaj LLM. Zbior sie nie zmienil, wiec poprzednia decyzja nadal obowiazuje. Reaplikujemy
    // ja (idempotentnie: tombstone'y delete_edge sa idempotentne) i utrzymujemy status.
    if let (Some(prev_hash), Some(prev_decision_raw)) = (prior_hash, prior_decision) {
        if prev_hash == member_set_hash {
            if let Ok(prev_decision) = serde_json::from_str::<Value>(prev_decision_raw) {
                let action = prev_decision
                    .get("action")
                    .and_then(|v| v.as_str())
                    .and_then(ResolveAction::from_str)
                    .unwrap_or(ResolveAction::Escalate);
                log::info(&format!(
                    "rag: conflict_resolve cache HIT konflikt id={id} (zbior czlonkow bez zmian) \
                     — reaplikuje decyzje '{}', bez wywolania LLM",
                    action.as_label()
                ));
                let winner = prev_decision
                    .get("winner_fact_key")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let decision = ResolveDecision {
                    action,
                    winner_fact_key: winner,
                    reason: prev_decision
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                };
                let outcome =
                    apply_decision(id, owner, rev0, &facts, &member_set_hash, &decision, head_id, rel, true)?;
                return Ok(outcome);
            }
        }
    }

    // (2) Evidence z capem (R8): pasaze zrodlowe czlonkow, ORDER BY confidence DESC, do
    // MAX_EVIDENCE_CHARS / MAX_EVIDENCE_PASSAGES. Schemat z ontologii jako kontekst typow.
    let evidence = collect_evidence(&facts)?;
    let schema = load_schema(schema_id)?;

    // (4) LLM: evidence-driven prompt -> odporne parsowanie decyzji.
    let raw = call_resolution_llm(conflict_type, head_id, rel, &schema, &facts, &evidence)?;
    let decision = parse_resolution_response(&raw, &facts);

    // (5) Zastosuj decyzje ODWRACALNIE.
    apply_decision(id, owner, rev0, &facts, &member_set_hash, &decision, head_id, rel, false)
}

/// (2) Zbiera AKTYWNE fakty grupy konfliktowej: conflict_members(dedup_key) JOIN fact_state.
/// Tylko active=1 (przegrani z poprzednich rund nie wracaja). ORDER BY fact_key =
/// deterministyczna kolejnosc (stabilny member_set_hash, powtarzalny prompt). Cap liczby
/// czlonkow juz egzekwowany przy detekcji (MAX_CONFLICT_MEMBERS), ale stosujemy go ponownie
/// jako twarda granice (defensywnie).
fn collect_conflict_facts(dedup_key: &str) -> Result<Vec<ConflictFact>, String> {
    let rows = sql_query(
        "SELECT fs.fact_key, fs.head_id, fs.rel, fs.tail_id \
         FROM conflict_members cm \
         JOIN fact_state fs ON fs.fact_key = cm.fact_key \
         WHERE cm.conflict_dedup_key = ? AND fs.active = 1 \
         ORDER BY fs.fact_key LIMIT ?",
        &[
            SqlValue::String(dedup_key.to_string()),
            SqlValue::I64(MAX_CONFLICT_MEMBERS),
        ],
    )
    .map_err(|e| format!("odczyt czlonkow konfliktu: {e}"))?;

    Ok(rows
        .iter()
        .filter_map(|r| {
            let fact_key = r.first().and_then(|v| v.as_str())?.to_string();
            let head_id = r.get(1).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let rel = r.get(2).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let tail_id = r.get(3).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            Some(ConflictFact { fact_key, head_id, rel, tail_id })
        })
        .collect())
}

/// (3) member_set_hash: kanoniczny, length-prefixed klucz POSORTOWANYCH fact_keys czlonkow.
/// Sortowanie czyni hash niezaleznym od kolejnosci zwrocenia wierszy (stabilny), a
/// length-prefix (canonical_key) bezkolizyjnym (granice kluczy jednoznaczne). To podpis ZBIORU
/// czlonkow: identyczny zbior => identyczny hash => cache HIT (R8, brak re-adjudykacji LLM).
fn member_set_hash(facts: &[ConflictFact]) -> String {
    let mut keys: Vec<&str> = facts.iter().map(|f| f.fact_key.as_str()).collect();
    keys.sort_unstable();
    canonical_key(&keys)
}

/// Schemat ontologii (head_type, relation, tail_type) jako kontekst typow dla promptu A_res.
/// Brak wiersza (schema_id nieznany) -> None (prompt poradzi sobie bez kontekstu typow).
struct SchemaContext {
    head_type: String,
    relation: String,
    tail_type: String,
}

fn load_schema(schema_id: &str) -> Result<Option<SchemaContext>, String> {
    let row = sql_query_one(
        "SELECT head_type, relation, tail_type FROM schema_registry WHERE schema_id = ?",
        &[SqlValue::String(schema_id.to_string())],
    )
    .map_err(|e| format!("odczyt schematu konfliktu: {e}"))?;
    Ok(row.map(|r| SchemaContext {
        head_type: r.first().and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        relation: r.get(1).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        tail_type: r.get(2).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    }))
}

/// (2) Zbiera pasaze zrodlowe (evidence) faktow grupy z TWARDYM capem (R8) i BALANSEM
/// per-czlonek (wazne 5). Dla kazdego faktu: fact_evidence -> chunks.text, ORDER BY confidence
/// DESC. Globalne ORDER BY confidence DESC moglo wziac evidence TYLKO jednego (najmocniejszego)
/// faktu i zaglodzic drugą strone konfliktu — LLM widzialby jedna strone => stronnicza decyzja.
/// Tu selekcja jest ROUND-ROBIN: w kolejnych pasach bierzemy po jednym najmocniejszym pasazu z
/// KAZDEGO czlonka, dopoki nie wyczerpiemy globalnego capu (MAX_EVIDENCE_PASSAGES /
/// MAX_EVIDENCE_CHARS). Dzieki temu kazdy fakt grupy dostaje reprezentacje ZANIM ktorykolwiek
/// dostanie drugi pasaz. Pojedynczy pasaz przyciety do MAX_EVIDENCE_PASSAGE_CHARS. Obciecie
/// logowane (zakaz cichego capu).
fn collect_evidence(facts: &[ConflictFact]) -> Result<Vec<EvidencePassage>, String> {
    if facts.is_empty() {
        return Ok(Vec::new());
    }

    // Pasaze kandydaci PER CZLONEK, kazdy ORDER BY confidence DESC (najmocniejsze pierwsze),
    // z twardym limitem fetchu na czlonka. Kolejnosc czlonkow = kolejnosc `facts`
    // (deterministyczna: collect_conflict_facts sortuje), wiec round-robin jest powtarzalny.
    let mut per_member: Vec<Vec<EvidencePassage>> = Vec::with_capacity(facts.len());
    for f in facts {
        let rows = sql_query(
            "SELECT fe.fact_key, c.text, fe.confidence \
             FROM fact_evidence fe \
             JOIN chunks c ON c.document_id = fe.document_id \
                           AND c.chunk_index = CAST(fe.chunk_id AS INTEGER) \
             WHERE fe.fact_key = ? \
             ORDER BY fe.confidence DESC, fe.chunk_id \
             LIMIT ?",
            &[
                SqlValue::String(f.fact_key.clone()),
                SqlValue::I64(MAX_EVIDENCE_PER_MEMBER_FETCH as i64),
            ],
        )
        .map_err(|e| format!("odczyt evidence: {e}"))?;

        let mut passages = Vec::with_capacity(rows.len());
        for r in &rows {
            let fact_key = r.first().and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let raw_text = r.get(1).and_then(|v| v.as_str()).unwrap_or_default();
            // confidence to REAL (SqlValue::F64) — sql_query mapuje liczby na I64 gdy calkowite,
            // wiec probujemy oba (jak gdzie indziej w addonie evidence ma ulamkowa pewnosc).
            let confidence = match r.get(2) {
                Some(SqlValue::F64(c)) => *c,
                Some(SqlValue::I64(i)) => *i as f64,
                _ => DEFAULT_CONFIDENCE as f64,
            };
            // Przytnij pojedynczy pasaz (znaki, nie bajty — granica UTF-8).
            let text: String = raw_text.chars().take(MAX_EVIDENCE_PASSAGE_CHARS).collect();
            passages.push(EvidencePassage { fact_key, text, confidence });
        }
        per_member.push(passages);
    }

    let (out, passages_capped, chars_capped) = balance_evidence(per_member);

    if passages_capped || chars_capped {
        log::warn(&format!(
            "rag: conflict_resolve cap evidence trafiony \
             (pasaze>{MAX_EVIDENCE_PASSAGES}={passages_capped}, znaki>{MAX_EVIDENCE_CHARS}={chars_capped}); \
             selekcja round-robin per-czlonek — LLM widzi OBIE strony konfliktu"
        ));
    }

    Ok(out)
}

/// Round-robin balans pasazy per czlonek konfliktu (wazne 5). `per_member[i]` to pasaze
/// i-tego czlonka, kazdy juz ORDER BY confidence DESC. W kolejnych pasach bierzemy po jednym
/// pasazu z KAZDEGO czlonka (najmocniejszy najpierw), dopoki nie wyczerpiemy globalnych capow
/// MAX_EVIDENCE_PASSAGES / MAX_EVIDENCE_CHARS. Gwarantuje, ze kazdy fakt grupy dostaje
/// reprezentacje zanim ktorykolwiek dostanie drugi pasaz — LLM widzi OBIE strony konfliktu.
/// Zwraca (wybrane_pasaze, passages_capped, chars_capped). Czysta funkcja (testowalna bez SQL).
fn balance_evidence(per_member: Vec<Vec<EvidencePassage>>) -> (Vec<EvidencePassage>, bool, bool) {
    let total_available: usize = per_member.iter().map(|p| p.len()).sum();
    let max_depth = per_member.iter().map(|p| p.len()).max().unwrap_or(0);

    let mut out: Vec<EvidencePassage> = Vec::new();
    let mut total_chars = 0usize;
    let mut chars_capped = false;
    let mut passages_capped = false;

    'outer: for depth in 0..max_depth {
        for member in &per_member {
            let Some(p) = member.get(depth) else { continue };
            if out.len() >= MAX_EVIDENCE_PASSAGES {
                passages_capped = true;
                break 'outer;
            }
            let len = p.text.chars().count();
            if total_chars + len > MAX_EVIDENCE_CHARS {
                chars_capped = true;
                break 'outer;
            }
            total_chars += len;
            out.push(p.clone());
        }
    }

    // Cap trafiony rowniez gdy bylo wiecej dostepnych pasazy niz weszlo do wyniku (np. fetch
    // per-czlonek przyciety LIMIT-em) — nie chowamy tego cicho.
    if out.len() < total_available && !chars_capped {
        passages_capped = true;
    }

    (out, passages_capped, chars_capped)
}

/// (4) Buduje evidence-driven prompt i woła rag-llm. Prompt niesie: schemat (kontekst typow),
/// konfliktowe fakty (head rel tail) + ich pasaze zrodlowe, kryteria akcji. Prosi o WYLACZNIE
/// JSON decyzji. Blad host-fn / pusta odpowiedz -> Err (caller: konflikt zostaje 'resolving',
/// wroci po TTL).
fn call_resolution_llm(
    conflict_type: &str,
    head_id: &str,
    rel: &str,
    schema: &Option<SchemaContext>,
    facts: &[ConflictFact],
    evidence: &[EvidencePassage],
) -> Result<String, String> {
    let schema_line = match schema {
        Some(s) => format!(
            "Schemat ontologii: ({}) -[{}]-> ({}).",
            s.head_type, s.relation, s.tail_type
        ),
        None => "Schemat ontologii: nieznany.".to_string(),
    };

    let mut facts_block = String::new();
    for f in facts {
        facts_block.push_str(&format!(
            "- fact_key={} | {} -[{}]-> {}\n",
            f.fact_key, f.head_id, f.rel, f.tail_id
        ));
    }

    let mut evidence_block = String::new();
    for e in evidence {
        evidence_block.push_str(&format!(
            "- [fact_key={} conf={:.2}] {}\n",
            e.fact_key, e.confidence, e.text
        ));
    }
    if evidence_block.is_empty() {
        evidence_block.push_str("(brak pasazy zrodlowych)\n");
    }

    let prompt = format!(
        "Jestes arbitrem konfliktow w grafie wiedzy. Encja '{head_id}' ma wiele sprzecznych \
         wartosci relacji '{rel}' (typ konfliktu: {conflict_type}). {schema_line}\n\n\
         KONFLIKTOWE FAKTY:\n{facts_block}\n\
         PASAZE ZRODLOWE (evidence):\n{evidence_block}\n\
         Na podstawie WYLACZNIE evidence wybierz akcje:\n\
         - keep_winner: jeden fakt jest poprawny, reszta to bledy/przestarzale (podaj winner_fact_key).\n\
         - temporal_split: fakty sa poprawne w roznych okresach (relacja zmienna w czasie).\n\
         - merge_entities: rozne tail to ta sama encja w roznej granularnosci/nazwie.\n\
         - escalate: silne, sprzeczne evidence po obu stronach LUB brak podstaw do decyzji.\n\n\
         Zwroc WYLACZNIE JSON: \
         {{\"action\":\"keep_winner|temporal_split|merge_entities|escalate\",\
         \"winner_fact_key\":\"...\",\"reason\":\"...\"}}. \
         winner_fact_key TYLKO dla keep_winner i MUSI byc jednym z fact_key powyzej. \
         Bez komentarza, bez markdown."
    );

    let model = "rag-llm";
    let options = json!({ "task": "chat", "temperature": 0.0 });
    let options_str =
        serde_json::to_string(&options).map_err(|e| format!("Blad serializacji opcji: {e}"))?;

    let prompt_bytes = prompt.as_bytes();
    let model_bytes = model.as_bytes();
    let options_bytes = options_str.as_bytes();
    let mut buffer = vec![0u8; RESOLVE_BUFFER_SIZE];
    let mut out_len: i32 = 0;

    let rc = unsafe {
        llm_generate(
            prompt_bytes.as_ptr() as i32, prompt_bytes.len() as i32,
            model_bytes.as_ptr() as i32, model_bytes.len() as i32,
            options_bytes.as_ptr() as i32, options_bytes.len() as i32,
            buffer.as_mut_ptr() as i32, RESOLVE_BUFFER_SIZE as i32,
            &mut out_len as *mut i32 as i32,
        )
    };
    if rc < 0 {
        return Err(format!("rag-llm (adjudykacja) zwrocil blad: {rc}"));
    }
    if out_len <= 0 {
        return Err("rag-llm (adjudykacja) zwrocil pusta odpowiedz".to_string());
    }
    Ok(String::from_utf8_lossy(&buffer[..out_len as usize]).to_string())
}

/// (4) ODPORNE parsowanie decyzji LLM (wzor parse_extraction_response): rozpakuj
/// chat-completion content, wytnij pierwszy zbalansowany obiekt JSON, sparsuj. KAZDY brak
/// (niesparsowalny JSON, nieznana akcja, brak winner dla keep_winner, winner spoza grupy) ->
/// bezpieczny default Escalate (oddaj czlowiekowi, nie zgaduj — korektnosc > automatyzacja).
fn parse_resolution_response(raw: &str, facts: &[ConflictFact]) -> ResolveDecision {
    let escalate = |reason: &str| ResolveDecision {
        action: ResolveAction::Escalate,
        winner_fact_key: None,
        reason: reason.to_string(),
    };

    let inner = chat_completion_content(raw).unwrap_or_else(|| raw.to_string());
    let Some(json_slice) = extract_json_object(&inner) else {
        return escalate("LLM nie zwrocil parsowalnego JSON-a decyzji");
    };
    let Ok(value) = serde_json::from_str::<Value>(json_slice) else {
        return escalate("LLM zwrocil niepoprawny JSON decyzji");
    };

    let action = value
        .get("action")
        .and_then(|v| v.as_str())
        .and_then(ResolveAction::from_str);
    let Some(action) = action else {
        return escalate("LLM zwrocil nieznana lub brakujaca akcje");
    };

    let reason = value
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(MAX_ENTITY_NAME_CHARS * 4)
        .collect::<String>();

    match action {
        ResolveAction::KeepWinner => {
            // winner_fact_key OBOWIAZKOWY i MUSI nalezec do grupy (anty-halucynacja: LLM nie
            // moze wskazac faktu spoza konfliktu). Brak/spoza grupy => eskalacja, nie zgadywanie.
            let winner = value.get("winner_fact_key").and_then(|v| v.as_str()).unwrap_or("");
            if winner.is_empty() || !facts.iter().any(|f| f.fact_key == winner) {
                return escalate("keep_winner bez prawidlowego winner_fact_key z grupy konfliktu");
            }
            ResolveDecision {
                action: ResolveAction::KeepWinner,
                winner_fact_key: Some(winner.to_string()),
                reason,
            }
        }
        other => ResolveDecision { action: other, winner_fact_key: None, reason },
    }
}

/// (5) Stosuje decyzje A_res ODWRACALNIE i ATOMOWO. `from_cache` rozroznia, czy decyzja
/// pochodzi z cache (reaplikacja) czy ze swiezego LLM. Wszystkie akcje zapisuja KOMPLETNY
/// decision JSON (czlonkowie + akcja + reason) => provenance i odwracalnosc.
///
/// OWNERSHIP exactly-once (blocker 3+4): caly apply (deaktywacja przegranych + enqueue
/// tombstone) ORAZ finalize ida w JEDNEJ transakcji SQLite, a KAZDY write jest warunkowany na
/// (status='resolving' AND resolve_owner=owner). Gdy ten przebieg STRACIL ownership (drugi
/// resolver przejal konflikt po TTL, bo LLM tego przebiegu trwal >TTL), wszystkie warunki sa
/// falszywe => transakcja nic nie zmienia (no-op): brak podwojnego apply i brak rozjazdu
/// "loser active=0 bez tombstone'a". drain_graph_outbox idzie PO commicie (idempotentny;
/// blad drainu nie cofa decyzji — domknie go nastepny drain).
#[allow(clippy::too_many_arguments)]
fn apply_decision(
    id: i64,
    owner: &str,
    rev0: i64,
    facts: &[ConflictFact],
    member_set_hash: &str,
    decision: &ResolveDecision,
    head_id: &str,
    rel: &str,
    from_cache: bool,
) -> Result<ResolveOutcome, String> {
    let members: Vec<String> = facts.iter().map(|f| f.fact_key.clone()).collect();

    match decision.action {
        ResolveAction::KeepWinner => {
            let winner = decision
                .winner_fact_key
                .as_deref()
                .ok_or_else(|| "keep_winner bez winner_fact_key".to_string())?;

            // Przegrani = czlonkowie != winner. Dla kazdego w TEJ SAMEJ transakcji:
            // fact_state.active=0 + conflict_state='resolved_loser' (warunkowane na ownerze)
            // ORAZ enqueue delete_edge (tombstone, tez warunkowany na ownerze). Atomowosc
            // gwarantuje INWARIANT: loser active=0 <=> tombstone enqueued (nigdy rozjazd).
            let losers: Vec<&ConflictFact> =
                facts.iter().filter(|f| f.fact_key != winner).collect();

            let decision_json = json!({
                "action": "keep_winner",
                "winner_fact_key": winner,
                "losers": losers.iter().map(|f| f.fact_key.clone()).collect::<Vec<_>>(),
                "members": members,
                "reason": decision.reason,
            });

            let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::new();
            for loser in &losers {
                stmts.push(deactivate_loser_stmt(id, owner, rev0, &loser.fact_key));
                stmts.push(enqueue_loser_tombstone_stmt(
                    id, owner, rev0, &loser.head_id, &loser.rel, &loser.tail_id,
                ));
            }
            stmts.push(finalize_conflict_stmt(id, owner, rev0, "resolved_auto", &decision_json, member_set_hash));
            run_owned_apply(id, owner, rev0, &stmts)?;

            // Drain materializuje tombstone'y do kg_active (idempotentnie) PO commicie. Blad
            // drainu nie cofa decyzji w SQLite (zrodlo prawdy) — kolejny drain/przebieg ja domknie.
            if let Err(e) = drain_graph_outbox() {
                log::warn(&format!(
                    "rag: conflict_resolve drain tombstone'ow konfliktu id={id} nie powiodl sie \
                     (domknie nastepny drain): {e}"
                ));
            }

            audit_decision(id, "keep_winner", head_id, rel, from_cache, &members);
            Ok(ResolveOutcome::ResolvedAuto { cache_hit: from_cache })
        }
        ResolveAction::TemporalSplit => {
            // Oba/wszystkie fakty ZOSTAJA aktywne (relacja zmienna w czasie). Nie tombstone'ujemy
            // — tylko zapisujemy adnotacje czasowa w decision JSON. Pelna adnotacja w props
            // krawedzi to pozniejszy refinement; tu minimalnie (nie komplikujemy).
            let decision_json = json!({
                "action": "temporal_split",
                "members": members,
                "reason": decision.reason,
                "note": "fakty zachowane jako poprawne w roznych okresach (temporal)",
            });
            let stmts = vec![finalize_conflict_stmt(id, owner, rev0, "resolved_auto", &decision_json, member_set_hash)];
            run_owned_apply(id, owner, rev0, &stmts)?;
            audit_decision(id, "temporal_split", head_id, rel, from_cache, &members);
            Ok(ResolveOutcome::ResolvedAuto { cache_hit: from_cache })
        }
        ResolveAction::MergeEntities => {
            // Granularnosc/aliasy encji -> entity merge (D5). TU tylko oznaczamy; merge robi D5.
            let decision_json = json!({
                "action": "merge_entities",
                "members": members,
                "reason": decision.reason,
            });
            let stmts = vec![finalize_conflict_stmt(id, owner, rev0, "resolved_merge_pending", &decision_json, member_set_hash)];
            run_owned_apply(id, owner, rev0, &stmts)?;
            audit_decision(id, "merge_entities", head_id, rel, from_cache, &members);
            Ok(ResolveOutcome::MergePending)
        }
        ResolveAction::Escalate => {
            // Silne evidence po obu stronach / brak podstaw -> czlowiek (panel = D7). Fakty
            // ZOSTAJA aktywne (nic nie tombstone'ujemy): eskalacja nie zmienia grafu.
            let decision_json = json!({
                "action": "escalate",
                "members": members,
                "reason": decision.reason,
            });
            let stmts = vec![finalize_conflict_stmt(id, owner, rev0, "escalated", &decision_json, member_set_hash)];
            run_owned_apply(id, owner, rev0, &stmts)?;
            audit_decision(id, "escalate", head_id, rel, from_cache, &members);
            Ok(ResolveOutcome::Escalated)
        }
    }
}

/// Wykonuje warunkowany na ownerze ORAZ na members_rev=rev0 apply jako JEDNA transakcja SQLite.
/// Brak zmienionych wierszy ma DWIE rozne przyczyny, ktore trzeba rozroznic PO commicie:
///  (a) UTRATA OWNERSHIP — drugi resolver przejal konflikt po TTL (resolve_owner != owner).
///      Wtedy NIC nie robimy: nowy owner sam zaadjudykuje (no-op, brak podwojnego apply).
///  (b) ZMIANA members_rev — zbior czlonkow urosl podczas LLM (D3 doklejil nowego czlonka do
///      'resolving'); my wciaz jestesmy ownerem, ale members_rev != rev0. Decyzja zapadla na
///      NIEAKTUALNYM (niepelnym) zbiorze => REVERT statusu do 'open' (resolve_owner=NULL), zeby
///      nastepny conflict_resolve re-claimnal i re-adjudykowal SWIEZY pelny zbior z conflict_members.
///      To NIE polega na kursorze conflict_scan — re-read czlonkow przy re-claim pokrywa nowy fakt.
/// Rozroznienie: po affected==0 czytamy aktualny (resolve_owner, members_rev) konfliktu.
fn run_owned_apply(
    id: i64,
    owner: &str,
    rev0: i64,
    stmts: &[(String, Vec<SqlValue>)],
) -> Result<(), String> {
    let refs: Vec<(&str, &[SqlValue])> =
        stmts.iter().map(|(q, p)| (q.as_str(), p.as_slice())).collect();
    let affected = sql_transaction(&refs)
        .map_err(|e| format!("atomowy apply decyzji konfliktu id={id}: {e}"))?;
    if affected != 0 {
        return Ok(());
    }

    // affected==0: rozroznij utrate ownershipu od zmiany zbioru czlonkow (TOCTOU).
    let row = sql_query_one(
        "SELECT resolve_owner, members_rev, status FROM conflicts WHERE id = ?",
        &[SqlValue::I64(id)],
    )
    .map_err(|e| format!("odczyt stanu konfliktu po nieudanym apply id={id}: {e}"))?;
    let cur_owner = row
        .as_ref()
        .and_then(|r| r.first())
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let cur_rev = row
        .as_ref()
        .and_then(|r| r.get(1))
        .and_then(|v| v.as_i64())
        .unwrap_or(rev0);
    let cur_status = row
        .as_ref()
        .and_then(|r| r.get(2))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let still_owner = cur_owner.as_deref() == Some(owner) && cur_status == "resolving";
    if still_owner && cur_rev != rev0 {
        // (b) Zbior urosl podczas LLM — decyzja odrzucona, oddajemy konflikt do re-adjudykacji.
        let reverted = sql_exec(
            "UPDATE conflicts SET status = 'open', resolve_owner = NULL, updated_at = ? \
             WHERE id = ? AND status = 'resolving' AND resolve_owner = ?",
            &[
                SqlValue::I64(now_unix()),
                SqlValue::I64(id),
                SqlValue::String(owner.to_string()),
            ],
        )
        .map_err(|e| format!("revert konfliktu id={id} do open po zmianie members_rev: {e}"))?;
        if reverted.rows_affected == 1 {
            log::info(&format!(
                "rag: conflict_resolve apply konfliktu id={id} ODRZUCONY — zbior czlonkow urosl \
                 podczas adjudykacji (members_rev {rev0} -> {cur_rev}); revert do 'open', \
                 nastepny przebieg re-adjudykuje swiezy zbior"
            ));
        }
    } else {
        // (a) Utrata ownershipu (lub konflikt juz domkniety przez kogos) — no-op.
        log::info(&format!(
            "rag: conflict_resolve apply konfliktu id={id} pominiety — utracony ownership \
             (drugi resolver przejal po TTL); brak podwojnego apply"
        ));
    }
    Ok(())
}

/// (5) Statement deaktywacji przegranego, WARUNKOWANY na ownerze: active=0 +
/// conflict_state='resolved_loser' + updated_at, ale TYLKO gdy ten przebieg wciaz wlada
/// konfliktem (EXISTS na conflicts.status='resolving' AND resolve_owner=owner). ODWRACALNE:
/// re-aktywacja (active=1) odtworzylaby krawedz; conflict_state='resolved_loser' jest jednak
/// TERMINALNE dla reconcile (nie re-aktywuje takiego faktu — blocker 1).
fn deactivate_loser_stmt(id: i64, owner: &str, rev0: i64, fact_key: &str) -> (String, Vec<SqlValue>) {
    (
        "UPDATE fact_state SET active = 0, conflict_state = 'resolved_loser', updated_at = ? \
         WHERE fact_key = ? \
           AND EXISTS (SELECT 1 FROM conflicts WHERE id = ? AND status = 'resolving' AND resolve_owner = ? AND members_rev = ?)"
            .to_string(),
        vec![
            SqlValue::I64(now_unix()),
            SqlValue::String(fact_key.to_string()),
            SqlValue::I64(id),
            SqlValue::String(owner.to_string()),
            SqlValue::I64(rev0),
        ],
    )
}

/// (5) Statement enqueue tombstone (delete_edge) przegranego, WARUNKOWANY na ownerze. Tombstone
/// w kg_active jest ODWRACALNY (D1: ponowny upsert_edge ozywia krawedz). INSERT OR IGNORE ...
/// SELECT ... WHERE EXISTS(owner): wstawia wiersz pending TYLKO gdy ten przebieg wlada
/// konfliktem (mirror deaktywacji), zachowujac inwariant loser-active=0 <=> tombstone enqueued.
fn enqueue_loser_tombstone_stmt(
    id: i64,
    owner: &str,
    rev0: i64,
    src: &str,
    rel: &str,
    dst: &str,
) -> (String, Vec<SqlValue>) {
    let op = outbox_delete_edge(src, rel, dst);
    let payload = serde_json::to_string(&op.payload).unwrap_or_else(|_| "{}".to_string());
    (
        "INSERT OR IGNORE INTO graph_outbox (dedup_key, op, collection, payload, applied, created_at) \
         SELECT ?, ?, ?, ?, 0, ? \
         WHERE EXISTS (SELECT 1 FROM conflicts WHERE id = ? AND status = 'resolving' AND resolve_owner = ? AND members_rev = ?)"
            .to_string(),
        vec![
            SqlValue::String(op.dedup_key),
            SqlValue::String(op.op.to_string()),
            SqlValue::String(KG_COLLECTION.to_string()),
            SqlValue::String(payload),
            SqlValue::I64(now_unix()),
            SqlValue::I64(id),
            SqlValue::String(owner.to_string()),
            SqlValue::I64(rev0),
        ],
    )
}

/// (5) Statement domkniecia konfliktu, WARUNKOWANY na ownerze: status, KOMPLETNY decision JSON
/// (provenance/odwracalnosc; w tym surowy reason z LLM — DB, nie log), resolved_members_hash
/// (cache R8), resolved_at, updated_at — ale TYLKO gdy ten przebieg wciaz wlada konfliktem
/// (status='resolving' AND resolve_owner=owner). Utrata ownershipu => 0 zmian (no-op).
fn finalize_conflict_stmt(
    id: i64,
    owner: &str,
    rev0: i64,
    status: &str,
    decision: &Value,
    member_set_hash: &str,
) -> (String, Vec<SqlValue>) {
    let decision_str = serde_json::to_string(decision).unwrap_or_else(|_| "{}".to_string());
    let now = now_unix();
    (
        "UPDATE conflicts \
         SET status = ?, decision = ?, resolver = 'A_res', \
             resolved_members_hash = ?, resolved_at = ?, updated_at = ? \
         WHERE id = ? AND status = 'resolving' AND resolve_owner = ? AND members_rev = ?"
            .to_string(),
        vec![
            SqlValue::String(status.to_string()),
            SqlValue::String(decision_str),
            SqlValue::String(member_set_hash.to_string()),
            SqlValue::I64(now),
            SqlValue::I64(now),
            SqlValue::I64(id),
            SqlValue::String(owner.to_string()),
            SqlValue::I64(rev0),
        ],
    )
}

/// Domkniecie konfliktu poza apply_decision (sciezka <2 aktywne fakty), WARUNKOWANE na ownerze.
/// Jeden statement w jednej transakcji (spojnosc z apply_decision: utrata ownershipu => no-op).
fn finalize_conflict(
    id: i64,
    owner: &str,
    rev0: i64,
    status: &str,
    decision: &Value,
    member_set_hash: &str,
) -> Result<(), String> {
    let stmt = finalize_conflict_stmt(id, owner, rev0, status, decision, member_set_hash);
    run_owned_apply(id, owner, rev0, std::slice::from_ref(&stmt))
}

/// (6) Audyt decyzji A_res (R8): log strukturalny per outcome — id konfliktu, akcja, encja,
/// relacja, resolver, model, zrodlo (cache/LLM), czlonkowie. NIE loguje surowego `reason` z
/// LLM: tekst uzasadnienia moze zawierac fragmenty dokumentow uzytkownika (dane wrazliwe),
/// wiec zostaje WYLACZNIE w conflicts.decision (DB), nigdy w strumieniu logu. Pelna provenance
/// (w tym reason) jest w decision JSON (finalize_conflict); log daje slad bez tresci zrodlowej.
fn audit_decision(
    id: i64,
    action: &str,
    head_id: &str,
    rel: &str,
    from_cache: bool,
    members: &[String],
) {
    let source = if from_cache { "cache" } else { "llm" };
    log::info(&format!(
        "rag: A_res decyzja id={id} action={action} head={head_id} rel={rel} \
         resolver=A_res model=rag-llm zrodlo={source} czlonkowie={}",
        members.join(",")
    ));
}

/// Czy wynik ekstrakcji chunku ma oznaczyc graf jako czesciowy. KAZDY blad
/// (LLM, parsowanie, upsert grafu, a takze blad REJESTRU graph_artifacts z bug 4)
/// wraca tu jako Err i podnosi graph_partial — rozjazd "w grafie ale nie w
/// rejestrze" / niepelna ekstrakcja nigdy nie ginie cicho. Obciecie capem jest
/// sygnalizowane osobno (flaga `truncated`), wiec sukces (Ok) NIE podnosi flagi.
fn chunk_extraction_marks_partial(result: &Result<(usize, usize), String>) -> bool {
    result.is_err()
}

/// Etykieta wezla konca relacji, gdy LLM nie wymienil go w liscie encji — placeholder
/// typ, by krawedz nie wisiala. Spojny z label uzytym przy materializacji wezla.
const FALLBACK_ENTITY_TYPE: &str = "Entity";

/// Kanoniczne, length-prefixed kodowanie listy pol w jeden bezkolizyjny klucz tekstowy.
/// Kazdy segment to "{dlugosc_w_bajtach}:{wartosc}" sklejone bez separatora. Bo dlugosc
/// poprzedza tresc, granice pol sa JEDNOZNACZNE — ("A","BC") i ("AB","C") koduja sie
/// rozn ie ("1:A2:BC" vs "2:AB1:C"), czego sklejanie przez '|' nie gwarantuje (wartosc
/// z '|' w srodku). Bez hasha => zero kolizji (addon nie ma crate'a sha, tylko sdk+serde).
fn canonical_key(parts: &[&str]) -> String {
    let mut out = String::new();
    for p in parts {
        out.push_str(&p.len().to_string());
        out.push(':');
        out.push_str(p);
    }
    out
}

/// schema_id z trojki typow (head_type, relation, tail_type). Deterministyczny =
/// ten sam schemat z roznych dokumentow trafia w ten sam wiersz schema_registry.
/// Kanoniczne kodowanie length-prefixed (bezkolizyjne granice typow).
fn derive_schema_id(head_type: &str, relation: &str, tail_type: &str) -> String {
    canonical_key(&[head_type, relation, tail_type])
}

/// Globalnie JEDNOZNACZNY klucz faktu z trojki (src, rel, dst) — kanoniczny,
/// length-prefixed (jak schema_id/dedup_key), NIE goly join przez '|'. Join '|' byl
/// dwuznaczny: id encji albo nazwa relacji moga ZAWIERAC '|' (parser ekstrakcji ich
/// nie ucieka), wiec `(a, "b|c", d)` i `(a|b, c, d)` sklejaly sie do tego samego
/// "a|b|c|d" => kolizja fact_key w fact_schema/fact_evidence/fact_state ORAZ w
/// dedup_key outboxu. Length-prefix czyni granice pol jednoznacznymi -> klucz jest
/// bezkolizyjny globalnie i propaguje sie spojnie do wszystkich tabel D1 i outboxu.
/// graph_artifacts trzyma OSOBNE kolumny src/rel/dst (nie fact_key), wiec refcount
/// cleanup jest nietkniety i NIE rozbija tego klucza po '|'.
fn fact_key_for(src: &str, rel: &str, dst: &str) -> String {
    canonical_key(&[src, rel, dst])
}

/// dedup_key outboxu: kanoniczny, length-prefixed klucz z (op, collection, klucz obiektu).
/// Bezkolizyjny i jednoznaczny => ponowny ingest tego samego faktu probuje wstawic ten
/// sam dedup_key (INSERT OR IGNORE po PARTIAL-UNIQUE applied=0), wiec kolejka nie puchnie
/// duplikatami PENDING, a re-drain jest bezpieczny. Po applied=1 klucz sie zwalnia
/// (re-materializacja po cleanupie) — patrz migracja 003.
fn outbox_dedup_key(op: &str, collection: &str, key: &str) -> String {
    canonical_key(&[op, collection, key])
}

/// Operacja outboxu czekajaca na materializacje do grafu: jeden wiersz graph_outbox.
struct OutboxOp {
    dedup_key: String,
    op: &'static str,
    payload: Value,
}

/// Serializuje pola Provenance do plaskiego JSON-a payloadu. Provenance ze sdk-spec
/// ma tylko derive Encode/Decode (CBOR), NIE serde — wiec zamiast serializowac strukture
/// trzymamy w payloadzie pola, ktorych drain potrzebuje do jej odtworzenia.
fn provenance_to_json(provenance: &Provenance) -> Value {
    json!({
        "doc_id": provenance.doc_id,
        "chunk_id": provenance.chunk_id,
        "confidence": provenance.confidence,
        "extractor_version": provenance.extractor_version,
    })
}

/// Odtwarza Provenance z plaskiego JSON-a payloadu (lustro `provenance_to_json`).
/// page/span nie sa niesione w D1 (build_provenance ich nie ustawia).
fn provenance_from_json(p: &Value) -> Provenance {
    Provenance {
        chunk_id: p.get("chunk_id").and_then(|v| v.as_str()).map(str::to_string),
        doc_id: p.get("doc_id").and_then(|v| v.as_str()).map(str::to_string),
        page: None,
        span: None,
        confidence: p.get("confidence").and_then(|v| v.as_f64()).map(|c| c as f32),
        extractor_version: p
            .get("extractor_version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// Buduje wpis outboxu upsert_node dla wezla. payload niesie wszystko, czego drain
/// potrzebuje do `graph_upsert_node` bez ponownego siegania do innych tabel.
fn outbox_upsert_node(id: &str, label: &str, name: &str, provenance: &Provenance) -> OutboxOp {
    OutboxOp {
        dedup_key: outbox_dedup_key("upsert_node", KG_COLLECTION, id),
        op: "upsert_node",
        payload: json!({
            "id": id,
            "label": label,
            "name": name,
            "provenance": provenance_to_json(provenance),
        }),
    }
}

/// Buduje wpis outboxu upsert_edge dla krawedzi (klucz = kanoniczny fact_key z src/rel/dst).
fn outbox_upsert_edge(src: &str, rel: &str, dst: &str, provenance: &Provenance) -> OutboxOp {
    OutboxOp {
        dedup_key: outbox_dedup_key("upsert_edge", KG_COLLECTION, &fact_key_for(src, rel, dst)),
        op: "upsert_edge",
        payload: json!({
            "src": src,
            "rel": rel,
            "dst": dst,
            "confidence": DEFAULT_CONFIDENCE as f64,
            "provenance": provenance_to_json(provenance),
        }),
    }
}

/// Buduje wpis outboxu delete_node. Cleanup (refcount->0) NIE kasuje grafu wprost —
/// enqueue'uje te intencje, by graf byl mutowany WYLACZNIE przez outbox (R1).
fn outbox_delete_node(id: &str) -> OutboxOp {
    OutboxOp {
        dedup_key: outbox_dedup_key("delete_node", KG_COLLECTION, id),
        op: "delete_node",
        payload: json!({ "id": id }),
    }
}

/// Buduje wpis outboxu delete_edge (klucz = kanoniczny fact_key z src/rel/dst).
fn outbox_delete_edge(src: &str, rel: &str, dst: &str) -> OutboxOp {
    OutboxOp {
        dedup_key: outbox_dedup_key("delete_edge", KG_COLLECTION, &fact_key_for(src, rel, dst)),
        op: "delete_edge",
        payload: json!({ "src": src, "rel": rel, "dst": dst }),
    }
}

/// Ekstrakcja grafu dla jednego chunku: wola rag-llm, parsuje i — zamiast pisac WPROST
/// do grafu — zapisuje stan do SQLite-ledgera (R1: schema_registry/fact_schema/
/// fact_evidence/fact_state) ORAZ TRWALA INTENCJE materializacji WEZLOW do graph_outbox
/// (R3), wszystko w JEDNEJ transakcji SQLite (sql_transaction). Zwraca `(encje, relacje)`.
/// BEST-EFFORT: kazdy blad (LLM/parsowanie/zapis SQL) -> Err -> graph_partial.
///
/// PODZIAL D2 (NAPRAWA wyscigu promocji): ta funkcja TYLKO ZAPISUJE ledger — NIE
/// przewiduje freq_after, NIE promuje schematow, NIE aktywuje krawedzi i NIE enqueue'uje
/// krawedzi. Cala promocja/aktywacja (Candidate->Stable + materializacja krawedzi) dzieje
/// sie w osobnym, idempotentnym kroku `reconcile_schemas` PO commicie ledgera, na
/// podstawie autorytatywnego COUNT — dzieki czemu nie ma stale-read (predykcja freq sprzed
/// commitu) ani niewidocznosci faktow z tej samej tx. To eliminuje oba blokery wyscigu i
/// jest samonaprawialne przy rownoleglym ingescie (reconcile globalny domyka zaleglosci).
///
/// Zapis JEDNOLITY dla KAZDEGO faktu (bez galezi stable/candidate):
///  - schema_registry: wiersz candidate jesli nie istnieje; freq liczony AUTORYTATYWNIE
///    jako COUNT(*) FROM fact_schema (po wstawieniu fact_schema w tej samej tx).
///  - fact_schema (Phi), fact_evidence (Psi): INSERT OR IGNORE / upsert idempotentny.
///  - fact_state: upsert z active=0 dla NOWYCH faktow; active istniejacych nie cofamy
///    (MAX(active, 0) = bez zmian — reconcile/promocja juz mogla je aktywowac).
///  - wezly head/tail: graph_artifacts (node) + outbox upsert_node ZAWSZE (jak D1) —
///    izolowany wezel jest nieszkodliwy dla PPR/neighbors, a materializacja wezlow nie
///    zalezy od stabilnosci schematu.
///
/// `doc_triples_so_far` to licznik triple'ow juz zapisanych dla calego dokumentu
/// (cap MAX_TRIPLES_PER_DOC) — aktualizowany o realnie wstawione krawedzie.
/// `truncated` jest ustawiane na TRUE gdy jakikolwiek cap (per-chunk, per-doc) lub
/// pominiecie za-dlugiej relacji obcie dane — caller propaguje to do graph_partial.
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
    let chunk_id = chunk_index.to_string();

    // Typ encji po znormalizowanym id — pod wyprowadzenie schema_id krawedzi. Encje
    // spoza listy (konce relacji) maja typ fallback "Entity", jak dotad.
    let mut entity_types: Vec<(String, String)> = Vec::with_capacity(extraction.entities.len());
    let type_of = |id: &str, types: &[(String, String)]| -> String {
        types
            .iter()
            .find(|(eid, _)| eid == id)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| FALLBACK_ENTITY_TYPE.to_string())
    };

    // Statementy ledgera+outboxu (tylko wezly) zbierane do JEDNEJ transakcji SQLite.
    // Materializacja KRAWEDZI jest poza ta transakcja: zrobi ja reconcile_schemas po
    // commicie (autorytatywny COUNT, zero predykcji). Atomowosc tu obejmuje caly ledger
    // faktu + intencje wezlow grafu.
    let mut tx: Vec<(String, Vec<SqlValue>)> = Vec::new();

    // Wezly encji. INWARIANT (bug 4): graph_artifacts MUSI byc nadzbiorem grafu, wiec
    // rejestr i intencja outboxu sa w tej samej transakcji co reszta ledgera. INSERT OR
    // IGNORE (bug 4 idempotencja): ux_graph_artifacts_node dedupuje per (dokument, n_id).
    let mut known: Vec<String> = Vec::with_capacity(extraction.entities.len());
    let mut entity_count = 0usize;
    for entity in &extraction.entities {
        entity_types.push((entity.id.clone(), entity.entity_type.clone()));
        push_node_artifact(&mut tx, document_id, &entity.id, now);
        let op = outbox_upsert_node(&entity.id, &entity.entity_type, &entity.name, &provenance);
        push_outbox(&mut tx, &op, now);
        if !known.contains(&entity.id) {
            known.push(entity.id.clone());
        }
        entity_count += 1;
    }

    // Krawedzie relacji. head/tail musza byc znanymi encjami tego chunku; gdy brakuje
    // wezla konca, materializujemy go (label fallback "Entity"). Respektujemy cap
    // triple'ow na dokument. KRAWEDZI tu NIE enqueue'ujemy i NIE aktywujemy — to robi
    // reconcile po commicie. Zapisujemy WYLACZNIE ledger (schema_registry/fact_schema/
    // fact_evidence/fact_state) jednolicie dla kazdego faktu.
    let mut relation_count = 0usize;
    for rel in &extraction.relations {
        if *doc_triples_so_far >= MAX_TRIPLES_PER_DOC {
            // Cap per-dokument obcial reszte relacji -> graf niekompletny (bug 5).
            *truncated = true;
            break;
        }
        for endpoint in [&rel.head_id, &rel.tail_id] {
            if !known.contains(endpoint) {
                push_node_artifact(&mut tx, document_id, endpoint, now);
                let op = outbox_upsert_node(endpoint, FALLBACK_ENTITY_TYPE, endpoint, &provenance);
                push_outbox(&mut tx, &op, now);
                known.push(endpoint.clone());
            }
        }

        let fact_key = fact_key_for(&rel.head_id, &rel.relation, &rel.tail_id);
        let head_type = type_of(&rel.head_id, &entity_types);
        let tail_type = type_of(&rel.tail_id, &entity_types);
        let schema_id = derive_schema_id(&head_type, &rel.relation, &tail_type);

        // Phi/Psi zapisujemy PRZED schema_registry, bo freq w schema_registry liczymy
        // AUTORYTATYWNIE jako COUNT(*) FROM fact_schema (sql_transaction stosuje statementy
        // sekwencyjnie w jednej transakcji, wiec INSERT fact_schema jest widoczny dla
        // kolejnego SELECT COUNT w tej samej tx — to naprawia bug #3 double-count przy
        // rownoleglym re-ingescie: freq nie zalezy od freq+1, tylko od realnej liczby par).
        push_fact_schema(&mut tx, &fact_key, &schema_id, document_id);
        push_fact_evidence(&mut tx, &fact_key, document_id, &chunk_id, &provenance);
        push_schema_registry(&mut tx, &schema_id, &head_type, &rel.relation, &tail_type, now);

        // fact_state: NOWE fakty z active=0. Istniejacych NIE cofamy (MAX(active,0)):
        // reconcile mogl je juz aktywowac, a re-ingest nie moze zabrac stabilnej krawedzi.
        push_fact_state(
            &mut tx,
            &fact_key,
            &schema_id,
            (&rel.head_id, &rel.relation, &rel.tail_id),
            now,
        );

        relation_count += 1;
        *doc_triples_so_far += 1;
    }

    // Jedna atomowa transakcja: ledger + intencje wezlow + graph_artifacts(node).
    let stmts: Vec<(&str, &[SqlValue])> =
        tx.iter().map(|(q, p)| (q.as_str(), p.as_slice())).collect();
    sql_transaction(&stmts).map_err(|e| format!("zapis ledgera grafu: {e}"))?;

    Ok((entity_count, relation_count))
}

/// Dokleja INSERT wezla do graph_artifacts (refcount cleanupu) do transakcji. INSERT OR
/// IGNORE (bug #4 idempotencja): ux_graph_artifacts_node dedupuje per (document_id, n_id),
/// wiec ponowny ingest/aktywacja nie mnozy wierszy rejestru.
fn push_node_artifact(tx: &mut Vec<(String, Vec<SqlValue>)>, document_id: &str, node_id: &str, now: i64) {
    tx.push((
        "INSERT OR IGNORE INTO graph_artifacts (document_id, kind, n_id, created_at) VALUES (?, 'node', ?, ?)"
            .to_string(),
        vec![
            SqlValue::String(document_id.to_string()),
            SqlValue::String(node_id.to_string()),
            SqlValue::I64(now),
        ],
    ));
}

/// Dokleja INSERT krawedzi do graph_artifacts (refcount cleanupu) do transakcji. INSERT OR
/// IGNORE (bug #4 idempotencja): ux_graph_artifacts_edge dedupuje per (document_id, src,
/// rel, dst). Reconcile aktywuje krawedz raz, ale evidence moze byc z wielu chunkow tego
/// dokumentu — bez OR IGNORE rejestr puchnalby duplikatami i psul refcount.
fn push_edge_artifact(
    tx: &mut Vec<(String, Vec<SqlValue>)>,
    document_id: &str,
    src: &str,
    rel: &str,
    dst: &str,
    now: i64,
) {
    tx.push((
        "INSERT OR IGNORE INTO graph_artifacts (document_id, kind, src, rel, dst, created_at) \
         VALUES (?, 'edge', ?, ?, ?, ?)"
            .to_string(),
        vec![
            SqlValue::String(document_id.to_string()),
            SqlValue::String(src.to_string()),
            SqlValue::String(rel.to_string()),
            SqlValue::String(dst.to_string()),
            SqlValue::I64(now),
        ],
    ));
}

/// UPSERT schematu: pierwsze wystapienie ustawia first_seen, kolejne aktualizuja freq.
/// freq liczony AUTORYTATYWNIE jako `COUNT(*) FROM fact_schema WHERE schema_id=?` (a NIE
/// `freq+1`) — to naprawia bug #3 double-count przy rownoleglym re-ingescie: licznik nie
/// dryfuje, bo zawsze odzwierciedla realna liczbe dystynktnych par (fact_key, document_id)
/// (PK fact_schema). MUSI byc wywolane PO push_fact_schema w tej samej transakcji, by
/// COUNT widzial wlasnie wstawiony wiersz Phi. status pozostaje 'candidate' — promocja do
/// 'stable' to osobny krok reconcile_schemas (po commicie, na podstawie COUNT>=tau).
fn push_schema_registry(
    tx: &mut Vec<(String, Vec<SqlValue>)>,
    schema_id: &str,
    head_type: &str,
    relation: &str,
    tail_type: &str,
    now: i64,
) {
    // Subselect liczy freq z fact_schema w TEJ SAMEJ transakcji (po wstawieniu Phi).
    // Pierwsze wstawienie wiersza schematu ustawia freq od razu na realny COUNT; przy
    // ON CONFLICT nadpisujemy freq tym samym COUNT. Zero zaleznosci od poprzedniej wartosci.
    tx.push((
        "INSERT INTO schema_registry (schema_id, head_type, relation, tail_type, freq, first_seen) \
         VALUES (?, ?, ?, ?, (SELECT COUNT(*) FROM fact_schema WHERE schema_id = ?), ?) \
         ON CONFLICT(schema_id) DO UPDATE SET \
           freq = (SELECT COUNT(*) FROM fact_schema WHERE schema_id = schema_registry.schema_id)"
            .to_string(),
        vec![
            SqlValue::String(schema_id.to_string()),
            SqlValue::String(head_type.to_string()),
            SqlValue::String(relation.to_string()),
            SqlValue::String(tail_type.to_string()),
            SqlValue::String(schema_id.to_string()),
            SqlValue::I64(now),
        ],
    ));
}

/// Lista DYSTYNKTNYCH dokumentow majacych evidence danego faktu — pod refcount
/// edge-artifactow przy hurtowej aktywacji (krawedz wniesiona przez kilka dokumentow).
fn fact_evidence_documents(fact_key: &str) -> Vec<String> {
    sql_query(
        "SELECT DISTINCT document_id FROM fact_evidence WHERE fact_key = ?",
        &[SqlValue::String(fact_key.to_string())],
    )
    .ok()
    .map(|rows| {
        rows.iter()
            .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
            .filter(|d| !d.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// Odczyt wartosci REAL z SqlValue jako f32. SQLite moze zwrocic confidence jako F64
/// (REAL) albo I64 (gdy zapisana wartosc byla calkowita) — pokrywamy oba.
fn sql_value_to_f32(v: &SqlValue) -> Option<f32> {
    match v {
        SqlValue::F64(f) => Some(*f as f32),
        SqlValue::I64(i) => Some(*i as f32),
        _ => None,
    }
}

/// Buduje reprezentatywna Provenance krawedzi z JEDNEGO wiersza fact_evidence danego
/// faktu (LIMIT 1). Sluzy do rekonstrukcji payloadu outboxu przy hurtowej aktywacji —
/// fakt zostal wyekstrahowany wczesniej, wiec jego dowod (doc/chunk/confidence) zyje w
/// fact_evidence. Brak evidence (teoretycznie niemozliwy, bo fact_evidence pisane razem z
/// fact_state) -> provenance z biezacym extractor_version i domyslna pewnoscia.
fn representative_provenance(fact_key: &str) -> Provenance {
    // span pomijamy: D1/D2 nie zapisuja offsetow do fact_evidence (ekstrakcja LLM ich
    // nie zwraca), wiec Provenance.span pozostaje None — spojnie z build_provenance.
    // DETERMINIZM (#6): bez ORDER BY LIMIT 1 zwracalby dowolny wiersz (zaleznie od planu
    // SQLite) — przy rownoleglej aktywacji rozne reconcile moglyby wziac rozne provenance.
    // Sortujemy po (confidence DESC, document_id, chunk_id): najpewniejszy dowod, a remis
    // rozstrzyga stabilnie po kluczach. Provenance jest tylko reprezentatywna dla grafu.
    let row = sql_query_one(
        "SELECT document_id, chunk_id, confidence FROM fact_evidence \
         WHERE fact_key = ? ORDER BY confidence DESC, document_id, chunk_id LIMIT 1",
        &[SqlValue::String(fact_key.to_string())],
    )
    .ok()
    .flatten();

    match row {
        Some(r) => {
            let doc_id = r.first().and_then(|v| v.as_str()).map(str::to_string);
            let chunk_id = r.get(1).and_then(|v| v.as_str()).map(str::to_string);
            let confidence = r
                .get(2)
                .and_then(sql_value_to_f32)
                .or(Some(DEFAULT_CONFIDENCE));
            Provenance {
                chunk_id,
                doc_id,
                page: None,
                span: None,
                confidence,
                extractor_version: Some(EXTRACTOR_VERSION.to_string()),
            }
        }
        None => Provenance {
            chunk_id: None,
            doc_id: None,
            page: None,
            span: None,
            confidence: Some(DEFAULT_CONFIDENCE),
            extractor_version: Some(EXTRACTOR_VERSION.to_string()),
        },
    }
}

/// Phi (fakt->schemat) per dokument. Idempotentne po (fact_key, document_id).
fn push_fact_schema(
    tx: &mut Vec<(String, Vec<SqlValue>)>,
    fact_key: &str,
    schema_id: &str,
    document_id: &str,
) {
    tx.push((
        "INSERT INTO fact_schema (fact_key, schema_id, document_id) VALUES (?, ?, ?) \
         ON CONFLICT(fact_key, document_id) DO UPDATE SET schema_id = excluded.schema_id"
            .to_string(),
        vec![
            SqlValue::String(fact_key.to_string()),
            SqlValue::String(schema_id.to_string()),
            SqlValue::String(document_id.to_string()),
        ],
    ));
}

/// Psi (fakt->pasaz) — evidence chunku. Idempotentne po (fact_key, document_id, chunk_id):
/// re-ingest tego samego chunku TEGO SAMEGO dokumentu nadpisuje dowod, nie duplikuje.
/// document_id MUSI byc w kluczu konfliktu, bo chunk_id = chunk_index jest lokalny dla
/// dokumentu — bez niego docA/chunk0 i docB/chunk0 nadpisywalyby sobie evidence.
fn push_fact_evidence(
    tx: &mut Vec<(String, Vec<SqlValue>)>,
    fact_key: &str,
    document_id: &str,
    chunk_id: &str,
    provenance: &Provenance,
) {
    // span nie jest niesiony w D1 (ekstrakcja LLM nie zwraca offsetow) -> NULL.
    let confidence = provenance.confidence.unwrap_or(DEFAULT_CONFIDENCE) as f64;
    tx.push((
        "INSERT INTO fact_evidence (fact_key, document_id, chunk_id, span, confidence) \
         VALUES (?, ?, ?, NULL, ?) \
         ON CONFLICT(fact_key, document_id, chunk_id) DO UPDATE SET \
           confidence = excluded.confidence"
            .to_string(),
        vec![
            SqlValue::String(fact_key.to_string()),
            SqlValue::String(document_id.to_string()),
            SqlValue::String(chunk_id.to_string()),
            SqlValue::F64(confidence),
        ],
    ));
}

/// Stan faktu (zrodlo prawdy o krawedzi). Ingest ZAWSZE wstawia NOWY fakt z active=0 —
/// aktywacja (active=1) jest WYLACZNIE domena reconcile_schemas (po promocji schematu).
/// Ponowny ingest aktualizuje schema_id/updated_at, ale NIE rusza fact_seq (kursor A_det),
/// created_at ani active. updated_at bump zawsze (kursor A_det/D3 widzi aktualizacje).
///
/// UWAGA: active jest MONOTONICZNE w gore w obrebie faktu — re-ingest wstawia active=0,
/// ale `active = MAX(active, 0)` zachowuje istniejaca jedynke. Inaczej re-ingest mogl by
/// zabrac z grafu juz zmaterializowana, stabilna krawedz (zaktywowana wczesniej w reconcile).
fn push_fact_state(
    tx: &mut Vec<(String, Vec<SqlValue>)>,
    fact_key: &str,
    schema_id: &str,
    triple: (&str, &str, &str),
    now: i64,
) {
    let (head_id, rel, tail_id) = triple;
    tx.push((
        "INSERT INTO fact_state (fact_key, schema_id, head_id, rel, tail_id, active, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, ?) \
         ON CONFLICT(fact_key) DO UPDATE SET \
           schema_id = excluded.schema_id, \
           active = MAX(active, excluded.active), \
           updated_at = excluded.updated_at"
            .to_string(),
        vec![
            SqlValue::String(fact_key.to_string()),
            SqlValue::String(schema_id.to_string()),
            SqlValue::String(head_id.to_string()),
            SqlValue::String(rel.to_string()),
            SqlValue::String(tail_id.to_string()),
            SqlValue::I64(now),
            SqlValue::I64(now),
        ],
    ));
}

/// Dokleja INSERT intencji outboxu. INSERT OR IGNORE po dedup_key z PARTIAL-UNIQUE
/// (ux_graph_outbox_pending WHERE applied=0): dedup obejmuje tylko PENDING, wiec ten
/// sam fakt z innego chunku/dokumentu nie tworzy drugiego wiersza dopoki poprzedni
/// czeka. Po applied=1 klucz sie zwalnia, wiec re-ingest/re-delete tworzy NOWY pending
/// (re-materializacja po cleanupie) — patrz migracja 003.
fn push_outbox(tx: &mut Vec<(String, Vec<SqlValue>)>, op: &OutboxOp, now: i64) {
    let payload = serde_json::to_string(&op.payload).unwrap_or_else(|_| "{}".to_string());
    tx.push((
        "INSERT OR IGNORE INTO graph_outbox (dedup_key, op, collection, payload, applied, created_at) \
         VALUES (?, ?, ?, ?, 0, ?)"
            .to_string(),
        vec![
            SqlValue::String(op.dedup_key.clone()),
            SqlValue::String(op.op.to_string()),
            SqlValue::String(KG_COLLECTION.to_string()),
            SqlValue::String(payload),
            SqlValue::I64(now),
        ],
    ));
}

/// Warunkowy enqueue uzywany WYLACZNIE przy aktywacji faktu w reconcile: wstawia op do
/// outboxu TYLKO gdy fakt jest jeszcze nieaktywny (`fact_state.active=0`). To rdzen
/// exactly-once przy rownoleglym reconcile — INSERT ... SELECT ... WHERE EXISTS(active=0)
/// jest w TEJ SAMEJ tx co warunkowy flip active=0->1, a tx leci jako BEGIN IMMEDIATE, wiec
/// drugi reconcile (po serializacji) widzi juz active=1: jego SELECT nie produkuje wiersza,
/// INSERT wstawia 0 op -> brak podwojnej materializacji. INSERT OR IGNORE po partial-unique
/// (applied=0) zostaje jako druga warstwa dedupu w obrebie jednego okna pending.
fn push_outbox_if_inactive(
    tx: &mut Vec<(String, Vec<SqlValue>)>,
    op: &OutboxOp,
    fact_key: &str,
    now: i64,
) {
    let payload = serde_json::to_string(&op.payload).unwrap_or_else(|_| "{}".to_string());
    tx.push((
        "INSERT OR IGNORE INTO graph_outbox (dedup_key, op, collection, payload, applied, created_at) \
         SELECT ?, ?, ?, ?, 0, ? \
         WHERE EXISTS (SELECT 1 FROM fact_state WHERE fact_key = ? AND active = 0)"
            .to_string(),
        vec![
            SqlValue::String(op.dedup_key.clone()),
            SqlValue::String(op.op.to_string()),
            SqlValue::String(KG_COLLECTION.to_string()),
            SqlValue::String(payload),
            SqlValue::I64(now),
            SqlValue::String(fact_key.to_string()),
        ],
    ));
}

/// Maks. liczba wierszy outboxu przetwarzanych w jednej iteracji drainu — chroni przed
/// DoS/OOM przy duzej zaleglosci (np. wiele nieprzetworzonych dokumentow po crashu).
const OUTBOX_DRAIN_BATCH: usize = 256;

/// Twardy limit iteracji petli drainu (BATCH * ITER = gorne ograniczenie pracy na
/// jedno wywolanie). Reszta zaleglosci domknie sie przy nastepnym drainie.
const OUTBOX_DRAIN_MAX_ITERS: usize = 4096;

/// Rozmiar partii aktywacji faktow w reconcile: jedna transakcja na batch faktow
/// przechodzacych z active=0 do active=1 (krawedzie stabilnych schematow). Chroni
/// przed gigantyczna pojedyncza transakcja przy duzej zaleglosci.
const ACTIVATION_BATCH: usize = 256;

/// Twardy limit iteracji petli aktywacji w reconcile (anty-DoS, jak przy drainie):
/// BATCH * ITER = gorne ograniczenie pracy na jedno wywolanie reconcile. Reszta
/// domknie sie przy nastepnym reconcile (samonaprawa).
const RECONCILE_MAX_ITERS: usize = 4096;

/// Materializuje TRWALE intencje outboxu (graph_outbox WHERE applied=0) do grafu
/// 'kg_active' i po sukcesie znacza je applied=1. Zrodlem jest SQLite (R3), NIE pamiec
/// biezacego wywolania — dzieki temu crash po commicie SQLite a przed/podczas drainu
/// jest ODTWARZALNY: nastepny drain (start kolejnego ingestu lub re-ingest) dokonczy
/// applied=0. Idempotentny: upsert grafu jest bezstanowy (ponowny upsert nieszkodliwy),
/// a flaga applied chroni przed powtorka w normalnym biegu.
///
/// Batch + cap iteracji chronia przed DoS przy duzej zaleglosci. Pierwszy blad host-fn
/// -> przerwij drain, zostaw applied=0 (domknie sie nastepnym razem), zwroc Err
/// (caller: graph_partial). Semantyka best-effort zachowana.
fn drain_graph_outbox() -> Result<(), String> {
    for _ in 0..OUTBOX_DRAIN_MAX_ITERS {
        let rows = sql_query(
            "SELECT id, op, collection, payload FROM graph_outbox \
             WHERE applied = 0 ORDER BY id LIMIT ?",
            &[SqlValue::I64(OUTBOX_DRAIN_BATCH as i64)],
        )
        .map_err(|e| format!("odczyt graph_outbox: {e}"))?;

        if rows.is_empty() {
            return Ok(());
        }

        for row in &rows {
            let id = row.first().and_then(|v| v.as_i64()).unwrap_or_default();
            let op = row.get(1).and_then(|v| v.as_str()).unwrap_or_default();
            let collection = row
                .get(2)
                .and_then(|v| v.as_str())
                .unwrap_or(KG_COLLECTION);
            let payload_raw = row.get(3).and_then(|v| v.as_str()).unwrap_or("{}");
            let payload: Value = serde_json::from_str(payload_raw)
                .map_err(|e| format!("deserializacja payloadu outboxu id={id}: {e}"))?;

            apply_outbox_op(op, collection, &payload)?;

            sql_exec(
                "UPDATE graph_outbox SET applied = 1, applied_at = ? WHERE id = ?",
                &[SqlValue::I64(now_unix()), SqlValue::I64(id)],
            )
            .map_err(|e| format!("oznaczenie applied dla outboxu id={id}: {e}"))?;
        }

        // Mniej niz pelny batch => kolejka opadla; nie ma sensu pytac ponownie.
        if rows.len() < OUTBOX_DRAIN_BATCH {
            return Ok(());
        }
    }
    Ok(())
}

/// Aplikuje pojedyncza operacje outboxu do grafu przez host-fn. Wydzielone z petli
/// drainu, by deserializacja/IO byly rozdzielone od materializacji do grafu.
fn apply_outbox_op(op: &str, collection: &str, payload: &Value) -> Result<(), String> {
    match op {
        "upsert_node" => {
            let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let label = payload.get("label").and_then(|v| v.as_str()).unwrap_or(FALLBACK_ENTITY_TYPE);
            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            let provenance = payload.get("provenance").map(provenance_from_json);
            let node = GraphNode {
                id: id.to_string(),
                label: label.to_string(),
                props: vec![GraphProp {
                    name: "name".to_string(),
                    value: VectorFieldValue::Str(name.to_string()),
                }],
                provenance,
            };
            graph_upsert_node(collection, node)
                .map(|_| ())
                .map_err(|e| format!("upsert wezla '{id}': {e}"))
        }
        "upsert_edge" => {
            let src = payload.get("src").and_then(|v| v.as_str()).unwrap_or_default();
            let rel = payload.get("rel").and_then(|v| v.as_str()).unwrap_or_default();
            let dst = payload.get("dst").and_then(|v| v.as_str()).unwrap_or_default();
            let confidence = payload.get("confidence").and_then(|v| v.as_f64());
            let provenance = payload.get("provenance").map(provenance_from_json);
            graph_upsert_edge(collection, src, rel, dst, confidence, Vec::new(), provenance)
                .map(|_| ())
                .map_err(|e| format!("upsert krawedzi '{src}-{rel}-{dst}': {e}"))
        }
        // delete_* sa idempotentne: host-fn delete = tombstone, wiec ponowne usuniecie
        // nieistniejacego wezla/krawedzi jest nieszkodliwe. Drain stosuje ops ORDER BY id
        // (FIFO), wiec delete po wczesniejszym upsercie tego samego klucza nie wyprzedzi go.
        "delete_node" => {
            let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            graph_delete_node(collection, id)
                .map(|_| ())
                .map_err(|e| format!("delete wezla '{id}': {e}"))
        }
        "delete_edge" => {
            let src = payload.get("src").and_then(|v| v.as_str()).unwrap_or_default();
            let rel = payload.get("rel").and_then(|v| v.as_str()).unwrap_or_default();
            let dst = payload.get("dst").and_then(|v| v.as_str()).unwrap_or_default();
            graph_delete_edge(collection, src, rel, dst)
                .map(|_| ())
                .map_err(|e| format!("delete krawedzi '{src}-{rel}-{dst}': {e}"))
        }
        other => Err(format!("nieznana operacja outboxu '{other}'")),
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
        // Blad zapisu ledgera/outboxu (sql_transaction) wraca z extract_chunk_graph jako
        // Err i MUSI oznaczyc graf jako czesciowy — bez tego byl cichy rozjazd
        // "w grafie ale nie w rejestrze" i cleanup nie usunalby artefaktu.
        let err: Result<(usize, usize), String> = Err("zapis ledgera grafu: sql padl".into());
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

    // --- MemGraphRAG D1: derywacja schema_id / fact_key / dedup_key, outbox, drain ---

    #[test]
    fn canonical_key_field_boundaries_are_unambiguous() {
        // Length-prefixed: granice pol jednoznaczne, brak kolizji ("A","BC") vs ("AB","C").
        assert_eq!(canonical_key(&["A", "BC"]), "1:A2:BC");
        assert_eq!(canonical_key(&["AB", "C"]), "2:AB1:C");
        assert_ne!(canonical_key(&["A", "BC"]), canonical_key(&["AB", "C"]));
        // Wartosc z separatorem '|' w srodku nie psuje kodowania (length-prefixed).
        assert_ne!(canonical_key(&["a|b", "c"]), canonical_key(&["a", "b|c"]));
    }

    #[test]
    fn schema_id_is_deterministic_and_typed() {
        // Ta sama trojka typow -> ten sam schema_id (wiersz schema_registry wspoldzielony).
        let a = derive_schema_id("Person", "KNOWS", "Person");
        let b = derive_schema_id("Person", "KNOWS", "Person");
        assert_eq!(a, b, "derywacja schema_id musi byc deterministyczna");
        // Rozna trojka -> inny schema_id (length-prefixed chroni przed kolizja sklejania).
        assert_ne!(a, derive_schema_id("Person", "KNOWS", "City"));
        assert_ne!(
            derive_schema_id("A", "BC", "D"),
            derive_schema_id("AB", "C", "D"),
            "granice pol nie moga byc dwuznaczne"
        );
    }

    #[test]
    fn fact_key_is_canonical_and_unambiguous() {
        // fact_key = kanoniczny length-prefixed klucz z (src, rel, dst).
        assert_eq!(fact_key_for("ada", "knows", "babbage"), canonical_key(&["ada", "knows", "babbage"]));
        // KLUCZOWE: '|' w id encji albo nazwie relacji NIE moze powodowac kolizji.
        // Goly join "a|b|c|d" mial te dwie trojki nierozroznialne; kanoniczny je rozdziela.
        let a = fact_key_for("a", "b|c", "d");
        let b = fact_key_for("a|b", "c", "d");
        assert_ne!(a, b, "rozne fakty z '|' w segmentach musza miec rozne fact_key");
    }

    #[test]
    fn dedup_key_is_deterministic_per_object() {
        // Idempotencja outboxu: ten sam (op, collection, klucz) -> ten sam dedup_key.
        let n1 = outbox_dedup_key("upsert_node", KG_COLLECTION, "ada");
        let n2 = outbox_dedup_key("upsert_node", KG_COLLECTION, "ada");
        assert_eq!(n1, n2);
        // Rozny obiekt albo rozna operacja -> inny dedup_key.
        assert_ne!(n1, outbox_dedup_key("upsert_node", KG_COLLECTION, "babbage"));
        assert_ne!(n1, outbox_dedup_key("delete_node", KG_COLLECTION, "ada"));
    }

    #[test]
    fn outbox_edge_op_keyed_by_fact_key() {
        // dedup_key krawedzi liczony z kanonicznego fact_key -> stabilny dla tej samej krawedzi.
        let prov = build_provenance("doc1", 0);
        let op = outbox_upsert_edge("ada", "knows", "babbage", &prov);
        assert_eq!(op.op, "upsert_edge");
        assert_eq!(
            op.dedup_key,
            outbox_dedup_key("upsert_edge", KG_COLLECTION, &fact_key_for("ada", "knows", "babbage"))
        );
        assert_eq!(op.payload["src"].as_str(), Some("ada"));
        assert_eq!(op.payload["dst"].as_str(), Some("babbage"));
    }

    #[test]
    fn outbox_node_payload_carries_label_and_name() {
        let prov = build_provenance("doc1", 3);
        let op = outbox_upsert_node("ada", "Person", "Ada", &prov);
        assert_eq!(op.op, "upsert_node");
        assert_eq!(op.payload["id"].as_str(), Some("ada"));
        assert_eq!(op.payload["label"].as_str(), Some("Person"));
        assert_eq!(op.payload["name"].as_str(), Some("Ada"));
    }

    #[test]
    fn provenance_json_round_trips_carried_fields() {
        // doc_id/chunk_id/confidence/extractor_version przezywaja serializacje do payloadu.
        // (Provenance ma tylko CBOR Encode/Decode, wiec drain odtwarza ja z plaskiego JSON.)
        let p = build_provenance("docX", 9);
        let json = provenance_to_json(&p);
        let back = provenance_from_json(&json);
        assert_eq!(back.doc_id, p.doc_id);
        assert_eq!(back.chunk_id, p.chunk_id);
        assert_eq!(back.extractor_version, p.extractor_version);
        assert_eq!(back.confidence, p.confidence);
        // page/span nie sa niesione w D1.
        assert!(back.page.is_none() && back.span.is_none());
    }

    #[test]
    fn outbox_delete_ops_keyed_and_payloaded() {
        // Cleanup enqueue'uje delete (R1: graf mutowany WYLACZNIE przez outbox).
        let dn = outbox_delete_node("ada");
        assert_eq!(dn.op, "delete_node");
        assert_eq!(dn.dedup_key, outbox_dedup_key("delete_node", KG_COLLECTION, "ada"));
        assert_eq!(dn.payload["id"].as_str(), Some("ada"));

        let de = outbox_delete_edge("ada", "knows", "babbage");
        assert_eq!(de.op, "delete_edge");
        assert_eq!(
            de.dedup_key,
            outbox_dedup_key("delete_edge", KG_COLLECTION, &fact_key_for("ada", "knows", "babbage"))
        );
        assert_eq!(de.payload["src"].as_str(), Some("ada"));
        assert_eq!(de.payload["rel"].as_str(), Some("knows"));
        assert_eq!(de.payload["dst"].as_str(), Some("babbage"));

        // delete i upsert tego samego obiektu maja ROZNY dedup_key -> partial-unique nie
        // myli ich: oba moga byc pending rownoczesnie i drain stosuje je FIFO (upsert, potem delete).
        assert_ne!(de.dedup_key, outbox_upsert_edge("ada", "knows", "babbage", &build_provenance("d", 0)).dedup_key);
    }

    #[test]
    fn outbox_dedup_key_across_documents() {
        // Idempotencja miedzy dokumentami: ten sam fakt z dwoch dokumentow daje JEDEN
        // dedup_key -> INSERT OR IGNORE nie tworzy duplikatu w graph_outbox.
        let p1 = build_provenance("docA", 0);
        let p2 = build_provenance("docB", 5);
        let a = outbox_upsert_edge("x", "rel", "y", &p1);
        let b = outbox_upsert_edge("x", "rel", "y", &p2);
        assert_eq!(a.dedup_key, b.dedup_key, "ten sam fakt -> ten sam dedup_key niezaleznie od dokumentu");
    }

    // --- MemGraphRAG D2: Thematic Denoising (prog tau, gate krawedzi, promocja) ---

    #[test]
    fn parse_threshold_defaults_and_overrides() {
        // Brak wpisu / pusty / smieci -> domyslny prog.
        assert_eq!(parse_denoising_threshold(None), DEFAULT_DENOISING_THRESHOLD);
        assert_eq!(parse_denoising_threshold(Some(b"")), DEFAULT_DENOISING_THRESHOLD);
        assert_eq!(parse_denoising_threshold(Some(b"abc")), DEFAULT_DENOISING_THRESHOLD);
        // 0 jest niedozwolone (promowaloby wszystko od razu, mylne z tau=1) -> domyslny.
        assert_eq!(parse_denoising_threshold(Some(b"0")), DEFAULT_DENOISING_THRESHOLD);
        // Realne nadpisanie: tau=1 (off), tau=3 (serwer), z otaczajacymi spacjami.
        assert_eq!(parse_denoising_threshold(Some(b"1")), 1);
        assert_eq!(parse_denoising_threshold(Some(b"3")), 3);
        assert_eq!(parse_denoising_threshold(Some(b"  5  ")), 5);
    }

    #[test]
    fn threshold_rule_below_at_and_off() {
        // Czysta regula promocji reconcile: freq<tau -> nie; freq>=tau -> tak; tau=1 (off)
        // -> juz pierwsze wystapienie promuje.
        assert!(!schema_reaches_threshold(1, 2), "freq<tau nie promuje");
        assert!(schema_reaches_threshold(2, 2), "freq==tau promuje");
        assert!(schema_reaches_threshold(7, 2), "freq>tau promuje");
        assert!(schema_reaches_threshold(1, 1), "tau=1: denoising off, promocja od razu");
    }

    // Model RECONCILE bez hosta SQL. Odwzorowuje DOKLADNIE nowy podzial:
    //  (A) ingest TYLKO zapisuje ledger: fact_schema (zrodlo freq = liczba dystynktnych
    //      (fact_key, document_id)) + fact_state z active=0 (active nigdy nie cofniete).
    //  (B) reconcile: promuje schematy z freq>=tau na 'stable' (idempotentnie) i aktywuje
    //      WSZYSTKIE zalegle (active=0) fakty stabilnych schematow (active=0 -> 1, jednokrotnie).
    // freq liczony jako COUNT dystynktnych par (push_schema_registry: freq=COUNT(*), bug #3).
    #[derive(Default)]
    struct SchemaModel {
        // (fact_key, document_id) widziane w fact_schema — zrodlo freq (dystynktne pary).
        pairs: Vec<(String, String)>,
        // fact_key -> active. Stan faktu (active=1 oznacza krawedz w kg_active). NIGDY nie
        // cofamy na false (monotonicznosc active, MAX(active,0) przy re-ingescie).
        facts: std::collections::BTreeMap<String, bool>,
        status: String,
        // Ile razy schemat zostal promowany (musi byc dokladnie raz: WHERE status='candidate').
        promotions: u32,
        // fact_key -> ile razy enqueue'owano upsert_edge tego faktu. EXACTLY-ONCE: aktywacja
        // (active 0->1) MUSI enqueue'owac op dokladnie raz nawet przy wielu/rownoleglych
        // reconcile. Lustro warunkowanego push_outbox_if_inactive (INSERT...WHERE EXISTS active=0).
        enqueues: std::collections::BTreeMap<String, u32>,
    }

    impl SchemaModel {
        fn new() -> Self {
            SchemaModel { status: "candidate".to_string(), ..Default::default() }
        }
        fn freq(&self) -> u64 {
            self.pairs.len() as u64
        }
        /// (A) Ingest jednego faktu: zapisuje pare (freq=COUNT, bez double-count) i wstawia
        /// fakt z active=0 (re-ingest NIE cofa istniejacego active). NIE promuje, NIE aktywuje
        /// (to robi reconcile). Lustro extract_chunk_graph + push_fact_state.
        fn ingest_fact(&mut self, fact_key: &str, document_id: &str) {
            let pair = (fact_key.to_string(), document_id.to_string());
            if !self.pairs.contains(&pair) {
                self.pairs.push(pair);
            }
            // INSERT ... active=0 ON CONFLICT MAX(active, 0): nowy -> false, istniejacy bez zmian.
            self.facts.entry(fact_key.to_string()).or_insert(false);
        }
        /// (B) Reconcile: promocja po COUNT>=tau (idempotentna) + aktywacja WSZYSTKICH
        /// zalegych faktow stabilnego schematu. Lustro reconcile_schemas.
        fn reconcile(&mut self, tau: u64) {
            if self.status == "candidate" && schema_reaches_threshold(self.freq(), tau) {
                self.status = "stable".to_string();
                self.promotions += 1;
            }
            if self.status == "stable" {
                // Aktywacja partii: per fakt warunkowy enqueue PRZED warunkowym flipem, oba na
                // active=0 (lustro reconcile_schemas). enqueue tylko gdy active jeszcze false
                // (INSERT...WHERE EXISTS active=0); flip tylko gdy active false (UPDATE WHERE
                // active=0). Ten porzadek czyni aktywacje exactly-once: w jednym przebiegu fakt
                // dostaje DOKLADNIE jeden enqueue, a powtorny reconcile (active juz true) zero.
                for (k, v) in self.facts.iter_mut() {
                    if !*v {
                        *self.enqueues.entry(k.clone()).or_insert(0) += 1;
                        *v = true;
                    }
                }
            }
        }
        fn active_count(&self) -> usize {
            self.facts.values().filter(|v| **v).count()
        }
        /// Laczna liczba enqueue'ow upsert_edge po wszystkich faktach (materializacje grafu).
        fn total_enqueues(&self) -> u32 {
            self.enqueues.values().sum()
        }
        /// Inwariant reconcile: NIE istnieje active=0 gdy schemat 'stable'.
        fn invariant_no_inactive_when_stable(&self) -> bool {
            self.status != "stable" || self.facts.values().all(|v| *v)
        }
    }

    #[test]
    fn freq_no_double_count_on_same_document_reingest() {
        // Ten sam fakt z tego samego dokumentu ingestowany dwukrotnie -> freq=1 (PK
        // (fact_key, document_id) blokuje double-count; freq=COUNT(*) nie freq+1, bug #3).
        // Z tau=2 reconcile NIE promuje (freq<tau) — fakt zostaje nieaktywny.
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f1", "docA");
        m.reconcile(2);
        assert_eq!(m.freq(), 1, "re-ingest tego samego (fact,doc) nie zawyza freq");
        assert_eq!(m.status, "candidate", "freq<tau -> schemat pozostaje candidate");
        assert_eq!(m.active_count(), 0, "candidate ponizej progu -> krawedz nieaktywna");
    }

    #[test]
    fn reconcile_promotes_at_threshold_and_activates_all_pending() {
        // Dwa ROZNE fakty schematu z dwoch dokumentow. Ingest oba (active=0). Reconcile po
        // 1. fakcie: freq=1<tau=2 -> bez promocji. Reconcile po 2.: freq=2>=tau -> promocja
        // + aktywacja OBU (tez zalegly f1 z poprzedniej rundy).
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.reconcile(2);
        assert_eq!(m.status, "candidate");
        assert_eq!(m.active_count(), 0, "ponizej progu -> nic nie aktywne");

        m.ingest_fact("f2", "docB");
        m.reconcile(2);
        assert_eq!(m.status, "stable", "freq>=tau -> promocja");
        assert_eq!(m.promotions, 1, "promocja dokladnie raz");
        assert_eq!(m.active_count(), 2, "reconcile aktywuje WSZYSTKIE zalegle (tez f1 z poprzedniej rundy)");
        assert!(m.invariant_no_inactive_when_stable(), "inwariant: brak active=0 przy stable");
    }

    #[test]
    fn reconcile_activates_facts_added_in_same_round() {
        // Fakty dodane "w tej samej rundzie" co przekroczenie progu MUSZA sie aktywowac w
        // tym samym reconcile (petla batchowa aktywuje wszystko az do braku active=0).
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.ingest_fact("f3", "docC");
        m.reconcile(2); // freq=3>=2 -> promocja + aktywacja calej trojki naraz
        assert_eq!(m.active_count(), 3, "wszystkie fakty z tej rundy aktywne");
        assert!(m.invariant_no_inactive_when_stable());
    }

    #[test]
    fn tau_one_activates_every_fact_after_reconcile() {
        // tau=1 (denoising off): kazdy fakt aktywny po reconcile, schemat promowany od razu.
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.reconcile(1);
        assert_eq!(m.status, "stable");
        assert_eq!(m.promotions, 1);
        assert_eq!(m.active_count(), 1, "tau=1: pierwszy fakt aktywny po reconcile");
        m.ingest_fact("f2", "docB");
        m.reconcile(1);
        assert_eq!(m.active_count(), 2, "kolejne fakty stabilnego schematu aktywne");
        assert_eq!(m.promotions, 1, "stabilny schemat nie jest promowany ponownie");
    }

    #[test]
    fn already_stable_schema_activates_new_facts_on_next_reconcile() {
        // Po promocji nowy fakt schematu jest aktywny przy nastepnym reconcile, bez ponownej
        // promocji (WHERE status='candidate' czyni promocje jednokrotna).
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.reconcile(2); // promocja
        assert_eq!(m.status, "stable");
        m.ingest_fact("f3", "docC");
        m.reconcile(2);
        assert_eq!(m.active_count(), 3, "nowy fakt stabilnego schematu aktywny po reconcile");
        assert_eq!(m.promotions, 1, "brak ponownej promocji");
        assert!(m.invariant_no_inactive_when_stable());
    }

    #[test]
    fn reconcile_is_idempotent() {
        // Wielokrotny reconcile (lustro dwoch rownoleglych reconcile) jest idempotentny:
        // promocja raz, aktywacja niezmienna, inwariant utrzymany.
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.reconcile(2);
        m.reconcile(2);
        m.reconcile(2);
        assert_eq!(m.promotions, 1, "promocja dokladnie raz mimo wielu reconcile");
        assert_eq!(m.active_count(), 2);
        assert!(m.invariant_no_inactive_when_stable());
    }

    #[test]
    fn reingest_does_not_deactivate_active_fact() {
        // Monotonicznosc active: gdy fakt jest juz aktywny (po reconcile), ponowny ingest
        // tego samego faktu wstawia active=0, ale MAX(active,0) zachowuje 1.
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.reconcile(2); // f1, f2 aktywne
        assert_eq!(m.active_count(), 2);
        m.ingest_fact("f1", "docA"); // re-ingest: active=0 ON CONFLICT MAX -> bez zmian
        assert_eq!(m.active_count(), 2, "re-ingest nie deaktywuje juz aktywnego faktu");
    }

    #[test]
    fn activation_enqueues_each_edge_exactly_once() {
        // Aktywacja faktu enqueue'uje upsert_edge DOKLADNIE raz (warunkowany na active=0).
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.reconcile(2);
        assert_eq!(m.active_count(), 2);
        assert_eq!(m.total_enqueues(), 2, "kazdy aktywowany fakt enqueue'uje sie raz");
    }

    #[test]
    fn second_reconcile_does_not_reenqueue_active_fact() {
        // Drugi (rownolegly/powtorny) reconcile widzi active=1 -> warunkowany enqueue wstawia
        // 0 op i warunkowany flip zmienia 0 wierszy. Zero podwojnej materializacji (blocker 1).
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.reconcile(2);
        let after_first = m.total_enqueues();
        m.reconcile(2); // drugi przebieg: nic juz nieaktywne
        m.reconcile(2); // i trzeci dla pewnosci
        assert_eq!(
            m.total_enqueues(),
            after_first,
            "po aktywacji kolejne reconcile NIE enqueue'uja ponownie (exactly-once)"
        );
        assert_eq!(m.total_enqueues(), 2, "lacznie dokladnie po jednym op na fakt");
    }

    #[test]
    fn conditional_enqueue_skips_already_active_fact() {
        // Bezposredni test warunku: gdy fakt JUZ active=1, druga proba aktywacji nie enqueue'uje
        // (lustro INSERT...SELECT...WHERE EXISTS(active=0), ktory wstawia 0 wierszy).
        let mut m = SchemaModel::new();
        m.ingest_fact("f1", "docA");
        m.ingest_fact("f2", "docB");
        m.reconcile(2); // f1, f2 -> active, po 1 enqueue
        m.ingest_fact("f3", "docC"); // nowy nieaktywny fakt stabilnego schematu
        m.reconcile(2);
        assert_eq!(m.total_enqueues(), 3, "tylko nowy f3 enqueue'uje sie przy drugim reconcile");
        assert_eq!(*m.enqueues.get("f1").unwrap(), 1, "f1 enqueue'owany dokladnie raz");
        assert_eq!(*m.enqueues.get("f3").unwrap(), 1, "f3 enqueue'owany dokladnie raz");
    }

    /// Lustro dedupe z migracji 004: zostaw MIN(id) per klucz naturalny. Wejscie: lista
    /// (id, kind, klucz). Wynik: zachowane id (po jednym na klucz, najmniejsze).
    fn dedupe_keep_min_id(rows: &[(i64, &str, &str)]) -> Vec<i64> {
        use std::collections::BTreeMap;
        let mut min_per_key: BTreeMap<(&str, &str), i64> = BTreeMap::new();
        for (id, kind, key) in rows {
            let e = min_per_key.entry((*kind, *key)).or_insert(*id);
            if *id < *e {
                *e = *id;
            }
        }
        let mut kept: Vec<i64> = min_per_key.into_values().collect();
        kept.sort_unstable();
        kept
    }

    #[test]
    fn migration004_dedupe_keeps_min_id_per_natural_key() {
        // Duplikaty (document_id,n_id) dla node i (document_id,src,rel,dst) dla edge musza
        // zostac zredukowane do MIN(id) PRZED unique indeksem (inaczej CREATE UNIQUE INDEX pada).
        let rows = [
            (10, "node", "docA|n1"),
            (12, "node", "docA|n1"), // duplikat -> usun (zostaje 10)
            (15, "node", "docA|n2"),
            (20, "edge", "docA|a|rel|b"),
            (5, "edge", "docA|a|rel|b"), // duplikat z mniejszym id -> zostaje 5
            (30, "edge", "docB|a|rel|b"),
        ];
        let kept = dedupe_keep_min_id(&rows);
        assert_eq!(kept, vec![5, 10, 15, 30], "zostaje MIN(id) per (kind, klucz naturalny)");
    }

    #[test]
    fn migration004_dedupe_noop_when_no_duplicates() {
        // Brak duplikatow -> dedupe nic nie usuwa (CREATE UNIQUE INDEX przejdzie).
        let rows = [
            (1, "node", "docA|n1"),
            (2, "node", "docA|n2"),
            (3, "edge", "docA|a|rel|b"),
        ];
        let kept = dedupe_keep_min_id(&rows);
        assert_eq!(kept, vec![1, 2, 3], "bez duplikatow nic nie znika");
    }

    // --- A_det (MemGraphRAG D3): klasyfikacja, dedup, kursor (czyste funkcje) ---

    #[test]
    fn conflict_type_maps_each_cardinality_kind() {
        // functional -> twardy mutual_exclusive; temporal -> miekki temporal;
        // hierarchical -> granularity-kandydat. To rdzen klasyfikacji A_det.
        assert_eq!(conflict_type_for_kind("functional"), Some("mutual_exclusive"));
        assert_eq!(conflict_type_for_kind("temporal"), Some("temporal"));
        assert_eq!(conflict_type_for_kind("hierarchical"), Some("granularity"));
    }

    #[test]
    fn conflict_type_unknown_relation_yields_no_conflict() {
        // Relacja spoza relation_cardinality (kind nieznany/pusty) NIE tworzy konfliktu:
        // brak reguly = brak pewnosci, wiec zero false-positive (decyzja D3).
        assert_eq!(conflict_type_for_kind("likes"), None);
        assert_eq!(conflict_type_for_kind(""), None);
        assert_eq!(conflict_type_for_kind("unknown_kind"), None);
    }

    #[test]
    fn conflict_dedup_key_identifies_group_not_fact_set() {
        // TOZSAMOSC = GRUPA (conflict_type, head_id, rel). Ten sam (head,rel,typ) daje TEN SAM
        // dedup_key NIEZALEZNIE od zbioru faktow — dlatego rosnacy zbior aktualizuje JEDEN open,
        // nie tworzy drugiego. Partial-unique ux_conflicts_open(dedup_key) => max 1 open per grupa.
        let two = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        let three = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        assert_eq!(two, three, "ta sama grupa => ten sam dedup_key niezaleznie od liczby faktow");
    }

    #[test]
    fn conflict_dedup_key_distinguishes_group_dimensions() {
        // Rozny typ / head_id / rel -> rozny dedup_key (rozne grupy konfliktowe).
        let base = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        assert_ne!(base, conflict_dedup_key("temporal", "Alice", "born_in"), "typ rozroznia grupe");
        assert_ne!(base, conflict_dedup_key("mutual_exclusive", "Bob", "born_in"), "head rozroznia");
        assert_ne!(base, conflict_dedup_key("mutual_exclusive", "Alice", "died_in"), "rel rozroznia");
    }

    #[test]
    fn cursor_advances_on_activation_seq() {
        // Kursor = activation_seq (monotoniczny, nadawany przy AKTYWACJI). Fakt jest "za"
        // kursorem gdy ma scisle wiekszy activation_seq. To lapie fakt aktywowany pozno mimo
        // niskiego fact_seq: dostaje WYSOKI activation_seq, wiec nie ucieka kursorowi.
        assert!(cursor_advances(5, 6), "wiekszy seq -> dalej");
        assert!(!cursor_advances(5, 5), "ten sam seq -> juz przeskanowany");
        assert!(!cursor_advances(5, 4), "mniejszy seq -> juz przeskanowany");
        // Fakt o niskim fact_seq (ingest wczesny), aktywowany pozno => wysoki activation_seq.
        // Kursor na 5 (fakty aktywowane wczesniej) NIE pomija seq=100 (pozna aktywacja).
        assert!(cursor_advances(5, 100), "pozna aktywacja (wysoki seq) zlapana");
    }

    // Model monotonicznosci activation_seq: lustro flipu reconcile
    // (activation_seq = MAX(activation_seq)+1 przy active 0->1) + kursora skanu po seq.
    // Sprawdza inwariant blockera 1: KAZDA aktywacja dostaje rosnacy seq i kursor jej nie gubi,
    // niezaleznie od kolejnosci INGESTU (fact_seq).
    #[derive(Default)]
    struct ActivationModel {
        // fact_key -> (fact_seq ingestu, Option<activation_seq>). None = nieaktywny.
        facts: std::collections::BTreeMap<String, (i64, Option<i64>)>,
        next_activation_seq: i64,
    }
    impl ActivationModel {
        // Ingest faktu w kolejnosci ingestu (rosnacy fact_seq), nieaktywny (activation_seq=None).
        fn ingest(&mut self, fact_key: &str, fact_seq: i64) {
            self.facts.entry(fact_key.to_string()).or_insert((fact_seq, None));
        }
        // Aktywacja (active 0->1): przydziel MONOTONICZNY activation_seq = MAX+1.
        fn activate(&mut self, fact_key: &str) {
            if let Some(e) = self.facts.get_mut(fact_key) {
                if e.1.is_none() {
                    self.next_activation_seq += 1;
                    e.1 = Some(self.next_activation_seq);
                }
            }
        }
        // Skan od kursora: zwraca fact_keys aktywowane z activation_seq>cursor (rosnaco),
        // i nowy kursor = max activation_seq zwroconych. Lustro scan_conflicts_locked.
        fn scan_from(&self, cursor: i64) -> (Vec<String>, i64) {
            let mut hits: Vec<(i64, String)> = self
                .facts
                .iter()
                .filter_map(|(k, (_, a))| a.filter(|s| *s > cursor).map(|s| (s, k.clone())))
                .collect();
            hits.sort_unstable();
            let new_cursor = hits.last().map(|(s, _)| *s).unwrap_or(cursor);
            (hits.into_iter().map(|(_, k)| k).collect(), new_cursor)
        }
    }

    #[test]
    fn late_activation_with_low_fact_seq_is_caught_by_cursor() {
        // SEDNO blockera 1: fA wstawiony WCZESNIE (fact_seq=1), aktywowany PO fB/fC.
        let mut m = ActivationModel::default();
        m.ingest("fA", 1);
        m.ingest("fB", 2);
        m.ingest("fC", 3);
        // Aktywujemy w kolejnosci B, C (fA jeszcze nie) -> activation_seq 1,2.
        m.activate("fB");
        m.activate("fC");
        let (first, cursor) = m.scan_from(0);
        assert_eq!(first, vec!["fB".to_string(), "fC".into()], "pierwszy skan: B,C");
        assert_eq!(cursor, 2);
        // Teraz aktywujemy fA (niski fact_seq=1, ale activation_seq=3 bo aktywowany NAJPOZNIEJ).
        m.activate("fA");
        let (second, cursor2) = m.scan_from(cursor);
        assert_eq!(second, vec!["fA".to_string()], "pozna aktywacja fA ZLAPANA mimo fact_seq=1");
        assert_eq!(cursor2, 3);
        // Kolejny skan: brak nowych aktywacji.
        let (third, cursor3) = m.scan_from(cursor2);
        assert!(third.is_empty(), "brak nowych aktywacji");
        assert_eq!(cursor3, 3, "kursor stabilny");
    }

    #[test]
    fn activation_seq_strictly_monotonic_regardless_of_ingest_order() {
        // activation_seq rosnie scisle w kolejnosci AKTYWACJI, nie ingestu.
        let mut m = ActivationModel::default();
        m.ingest("fX", 10);
        m.ingest("fY", 1); // nizszy fact_seq, ale aktywowany pierwszy
        m.activate("fY");
        m.activate("fX");
        assert_eq!(m.facts["fY"].1, Some(1), "fY aktywowany pierwszy -> seq 1");
        assert_eq!(m.facts["fX"].1, Some(2), "fX aktywowany drugi -> seq 2 (mimo fact_seq=10)");
        // Ponowna aktywacja fY (active juz 1) NIE marnuje numeru (warunek active=0).
        m.activate("fY");
        assert_eq!(m.next_activation_seq, 2, "powtorna aktywacja nie zwieksza licznika");
    }

    // Model blokady skanu: lustro acquire (rows_affected==1) + release (WHERE owner).
    struct LockModel {
        lock_until: i64,
        owner: Option<String>,
    }
    impl LockModel {
        fn new() -> Self {
            LockModel { lock_until: 0, owner: None }
        }
        // Atomowy warunkowy UPDATE: przejmuje lock TYLKO gdy wygasl (lock_until<now).
        // Zwraca rows_affected (1=przyznano, 0=zajete) — lustro acquire_scan_lock.
        fn acquire(&mut self, now: i64, lock_until: i64, owner: &str) -> u64 {
            if self.lock_until < now {
                self.lock_until = lock_until;
                self.owner = Some(owner.to_string());
                1
            } else {
                0
            }
        }
        // Release zwalnia TYLKO wlasny lock (WHERE owner=?). Lustro release_scan_lock.
        fn release(&mut self, owner: &str) {
            if self.owner.as_deref() == Some(owner) {
                self.lock_until = 0;
                self.owner = None;
            }
        }
    }

    #[test]
    fn lock_grants_exactly_one_in_same_second() {
        // Blocker 2: dwa skany w TEJ SAMEJ sekundzie (ten sam now, ten sam lock_until)
        // — dokladnie JEDEN dostaje blokade (rows_affected==1), drugi 0.
        let mut lock = LockModel::new();
        let now = 1000;
        let lock_until = now + 600;
        assert_eq!(lock.acquire(now, lock_until, "scan_A"), 1, "pierwszy bierze lock");
        assert_eq!(lock.acquire(now, lock_until, "scan_B"), 0, "drugi w tej samej sek odrzucony");
    }

    #[test]
    fn release_only_frees_own_lock() {
        // Blocker 2: stary skan (po TTL) nie wyzeruje locka nowego, ktory tymczasem przejal.
        let mut lock = LockModel::new();
        // scan_A bierze lock na [1000, 1600).
        assert_eq!(lock.acquire(1000, 1600, "scan_A"), 1);
        // scan_A przekracza TTL; scan_B po wygasnieciu (now=1601) przejmuje lock.
        assert_eq!(lock.acquire(1601, 2201, "scan_B"), 1, "B przejmuje po TTL");
        // scan_A probuje zwolnic SWOJ lock — ale wlascicielem jest juz scan_B => no-op.
        lock.release("scan_A");
        assert_eq!(lock.owner.as_deref(), Some("scan_B"), "lock scan_B nietkniety");
        assert_eq!(lock.lock_until, 2201, "TTL scan_B zachowany");
        // scan_B zwalnia wlasny lock poprawnie.
        lock.release("scan_B");
        assert!(lock.owner.is_none(), "scan_B zwolnil wlasny lock");
        assert_eq!(lock.lock_until, 0);
    }

    #[test]
    fn lock_owner_token_is_unique_per_call() {
        // Token wlasciciela (new_id) musi byc unikalny na wywolanie, inaczej release
        // moglby zwolnic cudzy lock. new_id laczy now_unix_ms z monotonicznym licznikiem.
        let a = new_id("scan");
        let b = new_id("scan");
        assert_ne!(a, b, "kolejne tokeny wlasciciela sa rozne");
        assert!(a.starts_with("scan_") && b.starts_with("scan_"));
    }

    // Model tozsamosci grupowej + ZNORMALIZOWANEGO czlonkostwa: lustro upsert_group_conflict.
    // conflicts: jeden open per dedup_key (BTreeSet open). conflict_members: BTreeSet wierszy
    // (dedup_key, fact_key) — INSERT OR IGNORE = wstaw do zbioru (idempotentne, atomowe,
    // bez read-modify-write union). Cap czlonkow per grupa egzekwowany jak w kodzie.
    // members_rev = AUTORYTATYWNY COUNT(*) czlonkow grupy ustawiany ATOMOWO z insertami w jednej
    // tx (lustro `UPDATE members_rev=(SELECT COUNT(*) ...)`), wylacznie dla open|resolving.
    #[derive(Default)]
    struct ConflictGroupModel {
        // dedup_key -> {fact_key}. Tylko jeden 'open' per dedup_key (partial-unique).
        members: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
        open_keys: std::collections::BTreeSet<String>,
        // dedup_key -> status ('open'|'resolving'|'resolved'). Domyslnie brak = nieistniejacy.
        status: std::collections::BTreeMap<String, String>,
        // dedup_key -> members_rev (COUNT czlonkow z chwili ostatniej tx upsert, gdy open|resolving).
        members_rev: std::collections::BTreeMap<String, i64>,
        inserts: u32,
        capped: u32,
    }
    impl ConflictGroupModel {
        // Lustro upsert_group_conflict: zwraca true gdy NOWY open (detected++).
        // Czlonkowie dopisywani INSERT OR IGNORE z capem MAX_CONFLICT_MEMBERS, a members_rev
        // ustawiany na COUNT(*) ATOMOWO (jeden krok modelu = jeden commit tx).
        fn upsert(&mut self, dedup_key: &str, fact_keys: &[String]) -> bool {
            // INSERT OR IGNORE conflicts(...open...): tworzy open tylko gdy grupy jeszcze nie ma.
            let is_new = self.open_keys.insert(dedup_key.to_string());
            if is_new {
                self.inserts += 1;
                self.status.insert(dedup_key.to_string(), "open".into());
            }
            let set = self.members.entry(dedup_key.to_string()).or_default();
            let mut room = MAX_CONFLICT_MEMBERS - set.len() as i64;
            for fk in fact_keys {
                if room <= 0 {
                    self.capped += 1;
                    break;
                }
                // INSERT OR IGNORE: nowy czlonek zmniejsza wolne miejsce; powtorny nie liczy sie.
                if set.insert(fk.clone()) {
                    room -= 1;
                }
            }
            // members_rev = COUNT(*) — TYLKO dla open|resolving (lustro WHERE status IN(...)).
            // Atomowe z insertami: w modelu to ten sam krok, wiec "czlonek widoczny ⟺ rev to
            // odzwierciedla". To rdzen ochrony TOCTOU A_res (stale-close niemozliwy).
            let count = set.len() as i64;
            let st = self.status.get(dedup_key).map(String::as_str).unwrap_or("");
            if st == "open" || st == "resolving" {
                self.members_rev.insert(dedup_key.to_string(), count);
            }
            is_new
        }
        fn member_count(&self, dedup_key: &str) -> usize {
            self.members.get(dedup_key).map(|s| s.len()).unwrap_or(0)
        }
        fn rev(&self, dedup_key: &str) -> i64 {
            self.members_rev.get(dedup_key).copied().unwrap_or(0)
        }
        // Lustro claim_conflict: open->resolving, zwraca rev0 = members_rev w chwili claimu
        // (czytany PO przejsciu na resolving, czyli PRZED snapshotem czlonkow przez A_res).
        fn claim(&mut self, dedup_key: &str) -> i64 {
            self.status.insert(dedup_key.to_string(), "resolving".into());
            self.rev(dedup_key)
        }
        // Lustro snapshotu A_res: collect_conflict_facts czyta czlonkow PO odczycie rev0.
        fn snapshot(&self, dedup_key: &str) -> std::collections::BTreeSet<String> {
            self.members.get(dedup_key).cloned().unwrap_or_default()
        }
        // Lustro finalize/apply warunkowanego na members_rev=:rev0 (i statusie resolving).
        // Zwraca true gdy write PRZESZEDL (rev pasuje) => konflikt domkniety; false = no-op
        // (rev urosl podczas adjudykacji => stale-set => revert do open). To dowodzi, ze
        // finalize na NIEAKTUALNYM zbiorze jest odrzucany (stale-close niemozliwy).
        fn finalize(&mut self, dedup_key: &str, rev0: i64) -> bool {
            let st = self.status.get(dedup_key).map(String::as_str).unwrap_or("");
            if st == "resolving" && self.rev(dedup_key) == rev0 {
                self.status.insert(dedup_key.to_string(), "resolved".into());
                self.open_keys.remove(dedup_key);
                true
            } else {
                false
            }
        }
        // Lustro run_owned_apply rozroznienia: gdy finalize=no-op a rev urosl, revert do open.
        fn revert_to_open(&mut self, dedup_key: &str) {
            self.status.insert(dedup_key.to_string(), "open".into());
            self.open_keys.insert(dedup_key.to_string());
        }
    }

    #[test]
    fn group_conflict_growing_set_appends_members_single_open() {
        // Ważne 4: open[A,B] + nowy fakt C -> DOPISANIE czlonka C do tej samej grupy,
        // NIE drugi open. Jeden konflikt per grupa (head,rel) — wymaganie D4 (A_res).
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        assert!(m.upsert(&dk, &["fA".into(), "fB".into()]), "pierwszy zbior -> NOWY open");
        assert!(!m.upsert(&dk, &["fA".into(), "fB".into(), "fC".into()]), "rosnacy zbior -> dopisanie");
        assert_eq!(m.inserts, 1, "DOKLADNIE jeden open dla grupy");
        assert_eq!(
            m.members[&dk],
            ["fA", "fB", "fC"].iter().map(|s| s.to_string()).collect(),
            "czlonkowie = union [A,B,C]"
        );
    }

    #[test]
    fn group_conflict_member_insert_is_idempotent() {
        // Blocker 2: ponowny skan tej samej grupy z tym samym zbiorem -> brak nowej detekcji
        // i ZERO nowych czlonkow (INSERT OR IGNORE jest idempotentny, bez read-modify-write).
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("temporal", "Acme", "ceo_of");
        assert!(m.upsert(&dk, &["f1".into(), "f2".into()]));
        assert!(!m.upsert(&dk, &["f1".into(), "f2".into()]), "ten sam zbior -> brak detekcji");
        assert!(!m.upsert(&dk, &["f2".into(), "f1".into()]), "kolejnosc bez znaczenia");
        assert_eq!(m.inserts, 1);
        assert_eq!(m.member_count(&dk), 2, "zero zdublowanych czlonkow (idempotencja)");
    }

    #[test]
    fn group_conflict_member_cap_is_enforced() {
        // Wazne 4: liczba czlonkow per grupa nie przekracza MAX_CONFLICT_MEMBERS niezaleznie
        // od liczby skanow. Po wypelnieniu capu nowe fakty sa pomijane (deterministycznie).
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("mutual_exclusive", "Hub", "born_in");
        // Wstaw wiecej unikalnych faktow niz cap, w kilku skanach.
        for batch in 0..4 {
            let facts: Vec<String> = (0..40).map(|i| format!("f{}", batch * 40 + i)).collect();
            m.upsert(&dk, &facts);
        }
        assert_eq!(
            m.member_count(&dk),
            MAX_CONFLICT_MEMBERS as usize,
            "liczba czlonkow ograniczona twardym capem"
        );
        assert!(m.capped > 0, "cap zostal trafiony i odnotowany (log::warn w kodzie)");
    }

    #[test]
    fn members_rev_equals_count_after_inserts() {
        // members_rev to AUTORYTATYWNY COUNT(*) czlonkow (nie inkrement). Po atomowym insert+rev
        // members_rev == liczba ROZNYCH czlonkow grupy. Wzor z D2 freq=COUNT(fact_schema).
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        m.upsert(&dk, &["fA".into(), "fB".into()]);
        assert_eq!(m.rev(&dk), 2, "members_rev = COUNT po dwoch czlonkach");
        m.upsert(&dk, &["fC".into()]);
        assert_eq!(m.rev(&dk), 3, "doszedl trzeci czlonek -> rev=3");
        assert_eq!(m.rev(&dk) as usize, m.member_count(&dk), "members_rev == COUNT(*) zawsze");
    }

    #[test]
    fn open_conflict_never_visible_without_its_members() {
        // D4 mikro-poprawka: CALY upsert (INSERT OR IGNORE conflicts + inserty czlonkow +
        // members_rev=COUNT) jest w JEDNEJ tx, wiec nie istnieje obserwowalny stan
        // "open + 0 czlonkow". Gdyby INSERT konfliktu byl osobny przed tx czlonkow, A_res
        // moglby claimnac memberless open (rev=0, 0 faktow), zobaczyc <2 aktywne fakty i go
        // blednie zamknac. Model: w chwili gdy konflikt jest open, ZAWSZE ma >=1 czlonka, a
        // members_rev == COUNT czlonkow. Sprawdzamy ten inwariant po upsercie.
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        assert!(m.upsert(&dk, &["fA".into(), "fB".into()]), "pierwszy upsert -> NOWY open");
        // Inwariant po commicie: open => komplet czlonkow + members_rev spojny z COUNT.
        assert!(m.open_keys.contains(&dk), "konflikt jest open");
        assert!(m.member_count(&dk) >= 1, "open NIGDY nie jest widoczny bez czlonkow");
        assert_eq!(
            m.rev(&dk) as usize,
            m.member_count(&dk),
            "members_rev == COUNT czlonkow (atomowo, brak okna memberless)"
        );
        // A_res na tak utworzonym konflikcie widzi PELNY zbior (>=2 -> realny konflikt).
        assert_eq!(m.snapshot(&dk).len(), 2, "A_res widzi komplet czlonkow, nie pusty zbior");
    }

    #[test]
    fn members_rev_unchanged_by_duplicate_insert() {
        // Idempotencja: ponowny INSERT OR IGNORE istniejacego czlonka NIE zmienia COUNT(*),
        // wiec members_rev sie nie rusza => brak falszywego odrzucenia A_res przy re-ingescie.
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("temporal", "Acme", "ceo_of");
        m.upsert(&dk, &["f1".into(), "f2".into()]);
        let rev_before = m.rev(&dk);
        m.upsert(&dk, &["f2".into(), "f1".into()]); // te same fakty, inna kolejnosc
        assert_eq!(m.rev(&dk), rev_before, "duplikat nie zmienia COUNT => members_rev staly");
        assert_eq!(m.rev(&dk), 2);
    }

    #[test]
    fn stale_close_is_impossible_with_count_rev() {
        // SEDNO blockera TOCTOU (D4 round 2): A_res claimuje konflikt (open->resolving), czyta
        // rev0 PRZED snapshotem czlonkow, decyduje DLUGO (LLM). W tym czasie D3 dopisuje NOWEGO
        // czlonka do 'resolving'. Przy members_rev=COUNT atomowym z insertem: finalize warunkowany
        // na members_rev=:rev0 musi byc NO-OP (rev urosl) => revert do open => re-adjudykacja
        // PELNEGO zbioru. Stale-close (zamkniecie na niepelnym zbiorze) jest NIEMOZLIWY.
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");

        // D3: otwiera konflikt na [fA, fB].
        m.upsert(&dk, &["fA".into(), "fB".into()]);
        assert_eq!(m.rev(&dk), 2);

        // A_res: claim (open->resolving) i odczyt rev0 (PRZED snapshotem czlonkow).
        let rev0 = m.claim(&dk);
        assert_eq!(rev0, 2, "rev0 czytany po claimie, przed snapshotem");
        let snapshot = m.snapshot(&dk);
        assert_eq!(snapshot.len(), 2, "A_res widzi [fA, fB] w chwili decyzji");

        // D3 (przeplot): dopisuje fC do konfliktu 'resolving' — atomowy insert+rev=COUNT.
        m.upsert(&dk, &["fC".into()]);
        assert_eq!(m.rev(&dk), 3, "nowy czlonek -> members_rev=COUNT=3 != rev0");

        // A_res finalize warunkowany na members_rev=:rev0 — MUSI byc no-op (rev urosl).
        assert!(!m.finalize(&dk, rev0), "finalize na nieaktualnym zbiorze ODRZUCONY (stale-close niemozliwy)");
        assert_ne!(m.status[&dk], "resolved", "konflikt NIE zostal domkniety na niepelnym zbiorze");

        // run_owned_apply: revert do open => nastepny przebieg re-claimnie PELNY zbior.
        m.revert_to_open(&dk);
        let rev1 = m.claim(&dk);
        let snapshot2 = m.snapshot(&dk);
        assert_eq!(snapshot2.len(), 3, "re-adjudykacja widzi PELNY zbior [fA, fB, fC]");
        // Tym razem zbior stabilny (zaden D3 w trakcie) => finalize przechodzi.
        assert!(m.finalize(&dk, rev1), "finalize na pelnym, stabilnym zbiorze przechodzi");
        assert_eq!(m.status[&dk], "resolved");
    }

    #[test]
    fn finalize_succeeds_when_set_stable_during_resolving() {
        // Pozytyw: gdy zbior NIE zmienia sie podczas adjudykacji (members_rev == rev0),
        // finalize przechodzi za pierwszym razem (brak zbednego revertu). To dowodzi, ze
        // straznik TOCTOU nie generuje falszywych odrzucen na stabilnym zbiorze.
        let mut m = ConflictGroupModel::default();
        let dk = conflict_dedup_key("temporal", "Acme", "ceo_of");
        m.upsert(&dk, &["f1".into(), "f2".into()]);
        let rev0 = m.claim(&dk);
        let _ = m.snapshot(&dk);
        // Zaden D3 nie dopisuje czlonka -> members_rev == rev0.
        assert!(m.finalize(&dk, rev0), "stabilny zbior => finalize przechodzi od razu");
        assert_eq!(m.status[&dk], "resolved");
    }

    // --- MemGraphRAG D4: A_res — adjudykacja konfliktow przez LLM (parsowanie, cache, akcje) ---

    /// Helper: grupa konfliktowa z N faktow o roznym tail (head+rel staly). fact_key kanoniczny.
    fn conflict_facts(head: &str, rel: &str, tails: &[&str]) -> Vec<ConflictFact> {
        tails
            .iter()
            .map(|t| ConflictFact {
                fact_key: fact_key_for(head, rel, t),
                head_id: head.to_string(),
                rel: rel.to_string(),
                tail_id: t.to_string(),
            })
            .collect()
    }

    #[test]
    fn parse_decision_keep_winner_valid() {
        // Odporne parsowanie: czysty JSON keep_winner z winner_fact_key z grupy => KeepWinner.
        let facts = conflict_facts("Alice", "born_in", &["Paris", "London"]);
        let win = facts[0].fact_key.clone();
        let raw = format!(
            "{{\"action\":\"keep_winner\",\"winner_fact_key\":\"{win}\",\"reason\":\"zrodlo A\"}}"
        );
        let d = parse_resolution_response(&raw, &facts);
        assert_eq!(d.action, ResolveAction::KeepWinner);
        assert_eq!(d.winner_fact_key.as_deref(), Some(win.as_str()));
        assert_eq!(d.reason, "zrodlo A");
    }

    #[test]
    fn parse_decision_tolerates_fence_and_prose() {
        // LLM owija JSON w ```json i prozę — extract_json_object wycina obiekt (wzor ekstrakcji).
        let facts = conflict_facts("Acme", "ceo_of", &["X", "Y"]);
        let raw = "Oto decyzja:\n```json\n{\"action\":\"temporal_split\",\"reason\":\"rozne lata\"}\n```";
        let d = parse_resolution_response(raw, &facts);
        assert_eq!(d.action, ResolveAction::TemporalSplit);
        assert_eq!(d.reason, "rozne lata");
    }

    #[test]
    fn parse_decision_unwraps_chat_completion() {
        // Odpowiedz w ksztalcie chat-completion (choices[].message.content) jest rozpakowywana.
        let facts = conflict_facts("Bob", "member_of", &["A", "B"]);
        let raw = "{\"choices\":[{\"message\":{\"content\":\"{\\\"action\\\":\\\"merge_entities\\\",\\\"reason\\\":\\\"alias\\\"}\"}}]}";
        let d = parse_resolution_response(raw, &facts);
        assert_eq!(d.action, ResolveAction::MergeEntities);
    }

    #[test]
    fn parse_decision_garbage_escalates() {
        // Niesparsowalny / pusty / bez akcji => bezpieczny default Escalate (nie zgadujemy).
        let facts = conflict_facts("Eve", "born_in", &["P", "Q"]);
        assert_eq!(parse_resolution_response("totalny smieci", &facts).action, ResolveAction::Escalate);
        assert_eq!(parse_resolution_response("{\"foo\":1}", &facts).action, ResolveAction::Escalate);
        assert_eq!(
            parse_resolution_response("{\"action\":\"wymyslona\"}", &facts).action,
            ResolveAction::Escalate
        );
    }

    #[test]
    fn parse_decision_keep_winner_hallucinated_loser_escalates() {
        // Anty-halucynacja: keep_winner z winner_fact_key SPOZA grupy => eskalacja, nie zgadywanie.
        let facts = conflict_facts("Alice", "born_in", &["Paris", "London"]);
        let raw = "{\"action\":\"keep_winner\",\"winner_fact_key\":\"3:fXX\",\"reason\":\"r\"}";
        let d = parse_resolution_response(raw, &facts);
        assert_eq!(d.action, ResolveAction::Escalate, "winner spoza grupy -> eskalacja");
        assert!(d.winner_fact_key.is_none());
    }

    #[test]
    fn parse_decision_keep_winner_missing_winner_escalates() {
        // keep_winner BEZ winner_fact_key jest nieuzyteczny => eskalacja.
        let facts = conflict_facts("Alice", "born_in", &["Paris", "London"]);
        let d = parse_resolution_response("{\"action\":\"keep_winner\",\"reason\":\"r\"}", &facts);
        assert_eq!(d.action, ResolveAction::Escalate);
    }

    #[test]
    fn action_roundtrip_labels() {
        // from_str <-> as_label spojne; nieznane -> None.
        for a in [
            ResolveAction::KeepWinner,
            ResolveAction::TemporalSplit,
            ResolveAction::MergeEntities,
            ResolveAction::Escalate,
        ] {
            assert_eq!(ResolveAction::from_str(a.as_label()), Some(a));
        }
        assert_eq!(ResolveAction::from_str("nope"), None);
        assert_eq!(ResolveAction::from_str(" keep_winner "), Some(ResolveAction::KeepWinner));
    }

    #[test]
    fn member_set_hash_is_order_independent_and_collision_free() {
        // Cache R8: hash zalezy od ZBIORU fact_keys, nie kolejnosci (stabilny przy re-skanie).
        let a = conflict_facts("H", "r", &["t1", "t2", "t3"]);
        let mut b = a.clone();
        b.reverse();
        assert_eq!(member_set_hash(&a), member_set_hash(&b), "kolejnosc bez wplywu na hash");
        // Inny zbior czlonkow -> inny hash (dojscie nowego faktu uniewaznia cache => re-LLM).
        let c = conflict_facts("H", "r", &["t1", "t2"]);
        assert_ne!(member_set_hash(&a), member_set_hash(&c), "zmiana zbioru -> inny hash");
    }

    // Model CLAIM exactly-once: lustro claim_conflict (warunkowy UPDATE open/resolving-po-TTL
    // -> resolving, rows_affected==1). Dwa wspolbiezne przebiegi widzace ten sam open: dokladnie
    // jeden przejmuje. Recovery: 'resolving' starszy niz TTL jest re-claimowalny.
    #[derive(Clone)]
    struct ConflictRow {
        status: String,
        updated_at: i64,
    }
    impl ConflictRow {
        fn open() -> Self {
            ConflictRow { status: "open".into(), updated_at: 0 }
        }
        // Lustro claim_conflict: zwraca rows_affected (1=przejal, 0=zajete/poza-warunkiem).
        fn claim(&mut self, now: i64, resolving_deadline: i64) -> u64 {
            let claimable = self.status == "open"
                || (self.status == "resolving" && self.updated_at < resolving_deadline);
            if claimable {
                self.status = "resolving".into();
                self.updated_at = now;
                1
            } else {
                0
            }
        }
    }

    #[test]
    fn claim_grants_exactly_one() {
        // Dwa przebiegi widza ten sam open -> tylko pierwszy claim==1, drugi 0 (pomija).
        let mut c = ConflictRow::open();
        let now = 10_000;
        let deadline = now - CONFLICT_RESOLVE_RESOLVING_TTL_SECS;
        assert_eq!(c.claim(now, deadline), 1, "pierwszy przejmuje open");
        assert_eq!(c.status, "resolving");
        assert_eq!(c.claim(now, deadline), 0, "drugi widzi resolving (swiezy) -> 0");
    }

    #[test]
    fn claim_recovers_stale_resolving_after_ttl() {
        // Punkt 8: crash zostawil 'resolving'; po TTL kolejny przebieg re-claimuje (jak open).
        let mut c = ConflictRow { status: "resolving".into(), updated_at: 1_000 };
        // now tuz po wstawieniu: NIE re-claim (resolving swiezy, moze byc adjudykowany teraz).
        let now_fresh = 1_000 + CONFLICT_RESOLVE_RESOLVING_TTL_SECS - 1;
        assert_eq!(
            c.claim(now_fresh, now_fresh - CONFLICT_RESOLVE_RESOLVING_TTL_SECS),
            0,
            "swiezy resolving nie jest re-claimowany"
        );
        // now po TTL: re-claim (crash uznany, konflikt wraca do adjudykacji).
        let now_late = 1_000 + CONFLICT_RESOLVE_RESOLVING_TTL_SECS + 1;
        assert_eq!(
            c.claim(now_late, now_late - CONFLICT_RESOLVE_RESOLVING_TTL_SECS),
            1,
            "resolving po TTL jest re-claimowany (recovery)"
        );
    }

    // Model ZASTOSOWANIA decyzji bez hosta SQL: odwzorowuje apply_decision na poziomie efektow:
    // keep_winner -> przegrani active=0 + tombstone (delete_edge) per przegrany; status
    // resolved_auto. temporal -> oba active, resolved_auto. merge -> resolved_merge_pending.
    // escalate -> wszyscy active, status escalated. decision JSON zawsze kompletny (odwracalnosc).
    #[derive(Default)]
    struct ApplyModel {
        active: std::collections::BTreeMap<String, bool>,
        tombstones: Vec<String>, // fact_key krawedzi enqueue'owanych do delete_edge
        status: String,
        decision_members: Vec<String>,
        resolved_hash: Option<String>,
    }
    impl ApplyModel {
        fn new(facts: &[ConflictFact]) -> Self {
            let mut active = std::collections::BTreeMap::new();
            for f in facts {
                active.insert(f.fact_key.clone(), true);
            }
            ApplyModel { active, ..Default::default() }
        }
        // Lustro apply_decision (efekty na ledger+outbox). Zwraca etykiete outcome.
        fn apply(&mut self, facts: &[ConflictFact], d: &ResolveDecision, hash: &str) -> &'static str {
            self.decision_members = facts.iter().map(|f| f.fact_key.clone()).collect();
            self.resolved_hash = Some(hash.to_string());
            match d.action {
                ResolveAction::KeepWinner => {
                    let winner = d.winner_fact_key.clone().unwrap();
                    for f in facts {
                        if f.fact_key != winner {
                            self.active.insert(f.fact_key.clone(), false);
                            self.tombstones.push(f.fact_key.clone());
                        }
                    }
                    self.status = "resolved_auto".into();
                    "resolved_auto"
                }
                ResolveAction::TemporalSplit => {
                    self.status = "resolved_auto".into();
                    "resolved_auto"
                }
                ResolveAction::MergeEntities => {
                    self.status = "resolved_merge_pending".into();
                    "resolved_merge_pending"
                }
                ResolveAction::Escalate => {
                    self.status = "escalated".into();
                    "escalated"
                }
            }
        }
    }

    #[test]
    fn apply_keep_winner_tombstones_losers_only() {
        // keep_winner: TYLKO przegrani -> active=0 + tombstone; winner zostaje aktywny.
        let facts = conflict_facts("Alice", "born_in", &["Paris", "London", "Berlin"]);
        let winner = facts[0].fact_key.clone();
        let hash = member_set_hash(&facts);
        let d = ResolveDecision {
            action: ResolveAction::KeepWinner,
            winner_fact_key: Some(winner.clone()),
            reason: "r".into(),
        };
        let mut m = ApplyModel::new(&facts);
        assert_eq!(m.apply(&facts, &d, &hash), "resolved_auto");
        assert!(m.active[&winner], "winner pozostaje aktywny");
        assert_eq!(m.active.values().filter(|v| !**v).count(), 2, "dwaj przegrani deaktywowani");
        assert_eq!(m.tombstones.len(), 2, "tombstone (delete_edge) per przegrany");
        assert!(!m.tombstones.contains(&winner), "winner NIE tombstone'owany");
        // Odwracalnosc: decision niesie wszystkich czlonkow + hash zapisany (cache + undo).
        assert_eq!(m.decision_members.len(), 3, "decision JSON kompletny (wszyscy czlonkowie)");
        assert_eq!(m.resolved_hash.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn apply_temporal_keeps_all_active() {
        // temporal_split: WSZYSTKIE fakty zostaja aktywne (zero tombstone'ow), status auto.
        let facts = conflict_facts("Acme", "ceo_of", &["X", "Y"]);
        let d = ResolveDecision { action: ResolveAction::TemporalSplit, winner_fact_key: None, reason: "lata".into() };
        let mut m = ApplyModel::new(&facts);
        assert_eq!(m.apply(&facts, &d, "h"), "resolved_auto");
        assert!(m.active.values().all(|v| *v), "oba fakty aktywne");
        assert!(m.tombstones.is_empty(), "temporal nie tombstone'uje");
    }

    #[test]
    fn apply_merge_marks_pending_no_tombstone() {
        // merge_entities: status resolved_merge_pending (D5), bez deaktywacji/tombstone tutaj.
        let facts = conflict_facts("Shanghai", "located_in", &["China", "Asia"]);
        let d = ResolveDecision { action: ResolveAction::MergeEntities, winner_fact_key: None, reason: "granular".into() };
        let mut m = ApplyModel::new(&facts);
        assert_eq!(m.apply(&facts, &d, "h"), "resolved_merge_pending");
        assert!(m.active.values().all(|v| *v));
        assert!(m.tombstones.is_empty());
    }

    #[test]
    fn apply_escalate_keeps_all_active() {
        // escalate: czlowiek (D7), graf nietkniety (wszyscy aktywni), status escalated.
        let facts = conflict_facts("Eve", "born_in", &["P", "Q"]);
        let d = ResolveDecision { action: ResolveAction::Escalate, winner_fact_key: None, reason: "sprzeczne".into() };
        let mut m = ApplyModel::new(&facts);
        assert_eq!(m.apply(&facts, &d, "h"), "escalated");
        assert!(m.active.values().all(|v| *v));
        assert!(m.tombstones.is_empty());
    }

    // Model CACHE R8: identyczny member_set_hash + istniejaca decyzja => brak ponownego LLM.
    // Lustro galezi cache w resolve_one_conflict (prior_hash == member_set_hash).
    fn cache_should_skip_llm(prior_hash: Option<&str>, current_hash: &str, has_decision: bool) -> bool {
        matches!(prior_hash, Some(h) if h == current_hash) && has_decision
    }

    #[test]
    fn cache_skips_llm_when_member_set_unchanged() {
        let facts = conflict_facts("Alice", "born_in", &["Paris", "London"]);
        let hash = member_set_hash(&facts);
        // Ten sam zbior + zapisana decyzja => cache HIT (bez LLM).
        assert!(cache_should_skip_llm(Some(&hash), &hash, true), "zbior bez zmian -> cache HIT");
        // Brak zapisanej decyzji => musi wolac LLM (pierwsza adjudykacja).
        assert!(!cache_should_skip_llm(Some(&hash), &hash, false), "brak decyzji -> LLM");
        // Zmieniony zbior (dojscie nowego faktu) => inny hash => LLM.
        let grown = conflict_facts("Alice", "born_in", &["Paris", "London", "Berlin"]);
        let grown_hash = member_set_hash(&grown);
        assert!(!cache_should_skip_llm(Some(&hash), &grown_hash, true), "zmiana zbioru -> LLM");
        // Pierwszy raz (brak prior_hash) => LLM.
        assert!(!cache_should_skip_llm(None, &hash, true), "brak prior_hash -> LLM");
    }

    #[test]
    fn run_caps_conflicts_per_run() {
        // R8: liczba kandydatow brana per przebieg jest ograniczona MAX_CONFLICTS_PER_RUN.
        // Czysta asercja na stalej + clamp w handlerze (handle_conflict_resolve).
        assert!(MAX_CONFLICTS_PER_RUN >= 1);
        let clamp = |n: u64| (n as usize).clamp(1, MAX_CONFLICTS_PER_RUN);
        assert_eq!(clamp(0), 1, "0 -> co najmniej 1");
        assert_eq!(clamp(1000), MAX_CONFLICTS_PER_RUN, "powyzej capu -> cap");
        assert_eq!(clamp(5).min(MAX_CONFLICTS_PER_RUN), 5.min(MAX_CONFLICTS_PER_RUN));
    }

    // --- Blocker 1: reconcile NIE re-aktywuje przegranych konfliktu (resolved_loser) ---

    /// Lustro predykatu aktywacji reconcile_schemas: aktywujemy fakt active=0 stabilnego
    /// schematu TYLKO gdy conflict_state oczekuje na aktywacje (NULL lub 'candidate').
    /// 'resolved_loser' jest TERMINALNY — celowa deaktywacja przez A_res nie wraca.
    fn reconcile_activates(active: i64, conflict_state: Option<&str>) -> bool {
        active == 0 && matches!(conflict_state, None | Some("candidate"))
    }

    #[test]
    fn reconcile_does_not_reactivate_resolved_loser() {
        // Blocker 1: przegrany konfliktu (active=0, conflict_state='resolved_loser') NIE jest
        // re-aktywowany przez kolejny reconcile — inaczej cofalibysmy rozwiazanie konfliktu.
        assert!(!reconcile_activates(0, Some("resolved_loser")), "loser NIE re-aktywowany");
        // Swiezy fakt (NULL) i kandydat konfliktu ('candidate') SA aktywowane (pending).
        assert!(reconcile_activates(0, None), "swiezy fakt aktywowany");
        assert!(reconcile_activates(0, Some("candidate")), "kandydat aktywowany");
        // Juz aktywny fakt nie wchodzi (predykat active=0).
        assert!(!reconcile_activates(1, None), "aktywny pominiety");
    }

    // --- Blocker 2: JEDEN aktywny konflikt per grupa w CALYM cyklu (open LUB resolving) ---

    /// Lustro partial-unique ux_conflicts_active (008) + D3 upsert_group_conflict: D3 dokleja
    /// czlonkow do istniejacego konfliktu grupy w stanie open LUB resolving, NIE tworzy drugiego.
    #[derive(Default)]
    struct LifecycleGroupModel {
        // dedup_key -> (status, {fact_key}). Najwyzej jeden aktywny (open|resolving) per grupa.
        groups: std::collections::BTreeMap<String, (String, std::collections::BTreeSet<String>)>,
        inserts: u32,
    }
    impl LifecycleGroupModel {
        fn is_active(status: &str) -> bool {
            status == "open" || status == "resolving"
        }
        // Lustro upsert_group_conflict pod indeksem open|resolving: wstaw nowy open TYLKO gdy
        // brak aktywnego (open|resolving) dla dedup_key; inaczej dolacz czlonkow do istniejacego.
        fn upsert(&mut self, dedup_key: &str, fact_keys: &[String]) -> bool {
            let entry = self.groups.entry(dedup_key.to_string());
            let is_new = match &entry {
                std::collections::btree_map::Entry::Occupied(o) => !Self::is_active(&o.get().0),
                std::collections::btree_map::Entry::Vacant(_) => true,
            };
            let slot = entry.or_insert_with(|| ("open".into(), Default::default()));
            if is_new {
                slot.0 = "open".into();
                self.inserts += 1;
            }
            for fk in fact_keys {
                slot.1.insert(fk.clone());
            }
            is_new
        }
        // Lustro claim A_res: open -> resolving (grupa pozostaje aktywna, klucz wciaz zajety).
        fn claim(&mut self, dedup_key: &str) {
            if let Some(g) = self.groups.get_mut(dedup_key) {
                if g.0 == "open" {
                    g.0 = "resolving".into();
                }
            }
        }
    }

    #[test]
    fn single_active_conflict_across_open_and_resolving() {
        // Blocker 2: gdy A_res przestawi open->resolving, D3 NIE tworzy drugiego open dla tej
        // samej grupy — dokleja czlonka do istniejacego resolving (jeden aktywny lifecycle).
        let mut m = LifecycleGroupModel::default();
        let dk = conflict_dedup_key("mutual_exclusive", "Alice", "born_in");
        assert!(m.upsert(&dk, &["fA".into(), "fB".into()]), "pierwszy -> NOWY open");
        m.claim(&dk); // A_res przejmuje: open -> resolving
        assert!(
            !m.upsert(&dk, &["fC".into()]),
            "podczas resolving NOWY open NIE powstaje (dolaczamy czlonka)"
        );
        assert_eq!(m.inserts, 1, "DOKLADNIE jeden aktywny konflikt per grupa w calym cyklu");
        assert_eq!(
            m.groups[&dk].1,
            ["fA", "fB", "fC"].iter().map(|s| s.to_string()).collect(),
            "czlonek dolaczony do istniejacego resolving (re-adjudykacja przez zmiane hash)"
        );
    }

    #[test]
    fn closed_conflict_frees_key_for_new_open() {
        // Po zamknieciu (resolved_*) klucz sie zwalnia: ponowny konflikt grupy otwiera NOWY.
        let mut m = LifecycleGroupModel::default();
        let dk = conflict_dedup_key("temporal", "Acme", "ceo_of");
        assert!(m.upsert(&dk, &["f1".into()]));
        m.groups.get_mut(&dk).unwrap().0 = "resolved_auto".into(); // domkniety
        assert!(m.upsert(&dk, &["f2".into()]), "zamkniety -> klucz wolny -> NOWY open");
        assert_eq!(m.inserts, 2);
    }

    // --- Blocker 3+4: atomowy apply (loser active=0 <=> tombstone) + ownership exactly-once ---

    /// Lustro apply_decision keep_winner jako JEDNA atomowa transakcja warunkowana na ownerze.
    /// Albo wszystkie writy ownera przechodza (apply), albo zaden (utracony ownership -> no-op).
    /// Inwariant: liczba deaktywowanych przegranych == liczba enqueue'owanych tombstone'ow.
    struct OwnedApplyModel {
        // Aktualny wlasciciel konfliktu w DB (resolving + resolve_owner). None => nie resolving.
        db_owner: Option<String>,
    }
    impl OwnedApplyModel {
        // Lustro run_owned_apply: writy przechodza TYLKO gdy owner pasuje do db_owner.
        // Zwraca (deaktywowani, enqueued_tombstones, finalized).
        fn apply_keep_winner(
            &self,
            owner: &str,
            losers: usize,
        ) -> (usize, usize, bool) {
            let owns = self.db_owner.as_deref() == Some(owner);
            if owns {
                // Atomowo: kazdy loser dostaje deaktywacje I tombstone (te same warunki ownera).
                (losers, losers, true)
            } else {
                // Utracony ownership: cala transakcja to no-op (warunki EXISTS falszywe).
                (0, 0, false)
            }
        }
    }

    #[test]
    fn apply_atomic_loser_deactivation_implies_tombstone() {
        // Blocker 3: nigdy "loser active=0 bez tombstone'a" — oba writy sa w jednej tx pod tym
        // samym warunkiem ownera, wiec liczby zawsze rowne (crash nie rozjedzie stanu).
        let m = OwnedApplyModel { db_owner: Some("res_1".into()) };
        let (deact, tombs, fin) = m.apply_keep_winner("res_1", 2);
        assert_eq!(deact, tombs, "deaktywacja przegranego <=> enqueue tombstone (inwariant)");
        assert_eq!(deact, 2);
        assert!(fin, "finalize w tej samej tx");
    }

    #[test]
    fn apply_noop_when_ownership_lost() {
        // Blocker 4: drugi resolver przejal konflikt po TTL (db_owner=res_2); spozniony apply
        // pierwszego (owner=res_1) jest NO-OP: zero deaktywacji, zero tombstone'ow, brak
        // finalize -> brak podwojnego apply i brak rozjazdu (split) stanu.
        let m = OwnedApplyModel { db_owner: Some("res_2".into()) };
        let (deact, tombs, fin) = m.apply_keep_winner("res_1", 2);
        assert_eq!((deact, tombs), (0, 0), "utracony owner -> zero zmian");
        assert!(!fin, "utracony owner -> brak finalize (no-op)");
    }

    // --- Blocker TOCTOU (runda 2): members_rev strazy zbioru czlonkow podczas adjudykacji ---

    /// Lustro D3 upsert_group_conflict (bump members_rev) + A_res claim (rev0) + run_owned_apply
    /// (warunek members_rev=rev0 i rozroznienie revert-do-open vs no-op). members_rev rosnie
    /// TYLKO przy realnie nowym czlonku; idempotentny re-insert tego samego NIE bumpuje.
    struct ToctouModel {
        status: String,
        owner: Option<String>,
        members_rev: i64,
        members: std::collections::BTreeSet<String>,
    }
    impl ToctouModel {
        fn open(members: &[&str]) -> Self {
            Self {
                status: "open".into(),
                owner: None,
                members_rev: 0,
                members: members.iter().map(|s| s.to_string()).collect(),
            }
        }
        // Lustro upsert_group_conflict: dopisanie czlonka bumpuje members_rev TYLKO gdy nowy
        // i tylko dla open|resolving. Re-insert istniejacego = idempotentny, brak bumpu.
        fn add_member(&mut self, fk: &str) {
            let newly = self.members.insert(fk.to_string());
            if newly && (self.status == "open" || self.status == "resolving") {
                self.members_rev += 1;
            }
        }
        // Lustro claim_conflict: open -> resolving, stempel ownera, zwrot rev0 po claimie.
        fn claim(&mut self, owner: &str) -> i64 {
            self.status = "resolving".into();
            self.owner = Some(owner.to_string());
            self.members_rev
        }
        // Lustro run_owned_apply: writy/finalize przechodza tylko gdy owner+members_rev=rev0.
        // Zwraca (applied, reverted_to_open). Po affected==0 rozroznia rev-change od utraty ownera.
        fn apply(&mut self, owner: &str, rev0: i64) -> (bool, bool) {
            let owns = self.owner.as_deref() == Some(owner) && self.status == "resolving";
            if owns && self.members_rev == rev0 {
                self.status = "resolved_auto".into();
                return (true, false);
            }
            // affected==0: rozroznienie.
            if owns && self.members_rev != rev0 {
                // Zbior urosl -> revert do open (re-claim re-czyta swiezy zbior).
                self.status = "open".into();
                self.owner = None;
                (false, true)
            } else {
                // Utrata ownershipu -> no-op.
                (false, false)
            }
        }
    }

    #[test]
    fn members_rev_bumps_only_on_new_member() {
        let mut m = ToctouModel::open(&["fA", "fB"]);
        assert_eq!(m.members_rev, 0);
        m.add_member("fB"); // duplikat -> brak bumpu (idempotencja)
        assert_eq!(m.members_rev, 0, "re-insert istniejacego NIE bumpuje");
        m.add_member("fC"); // nowy -> bump
        assert_eq!(m.members_rev, 1, "nowy czlonek bumpuje members_rev");
    }

    #[test]
    fn apply_noop_and_revert_when_member_set_grew_during_llm() {
        // TOCTOU: claim na rev0, podczas LLM D3 dokleja nowego czlonka (bump), apply musi byc
        // no-op (decyzja na nieaktualnym zbiorze odrzucona) i konflikt wraca do 'open'.
        let mut m = ToctouModel::open(&["fA", "fB"]);
        let rev0 = m.claim("res_1");
        m.add_member("fC"); // D3 podczas dlugiego LLM -> members_rev != rev0
        let (applied, reverted) = m.apply("res_1", rev0);
        assert!(!applied, "decyzja na starym zbiorze odrzucona (no-op)");
        assert!(reverted, "rev-change + wciaz owner -> revert do 'open'");
        assert_eq!(m.status, "open");
        assert_eq!(m.owner, None, "owner wyczyszczony przy revert");
    }

    #[test]
    fn revert_enables_readjudication_of_fresh_set() {
        // Po revert do 'open' nastepny przebieg re-claimuje i adjudykuje SWIEZY (pelny) zbior —
        // nowy fakt zlapany przez re-read conflict_members, NIE przez kursor conflict_scan.
        let mut m = ToctouModel::open(&["fA", "fB"]);
        let rev0 = m.claim("res_1");
        m.add_member("fC");
        m.apply("res_1", rev0); // revert do open
        // Re-claim: rev1 odzwierciedla pelny zbior {fA,fB,fC}.
        let rev1 = m.claim("res_2");
        assert_eq!(rev1, 1, "re-claim widzi zaktualizowany members_rev");
        let (applied, reverted) = m.apply("res_2", rev1);
        assert!(applied, "swiezy zbior zaadjudykowany");
        assert!(!reverted);
        assert_eq!(
            m.members,
            ["fA", "fB", "fC"].iter().map(|s| s.to_string()).collect(),
            "decyzja objela nowy fakt fC"
        );
    }

    #[test]
    fn lost_ownership_does_not_revert() {
        // Gdy inny resolver przejal konflikt (recovery po TTL), nasz spozniony apply jest NO-OP
        // i NIE robi revertu (to nie nasz konflikt) — odrozniamy od rev-change.
        let mut m = ToctouModel::open(&["fA", "fB"]);
        let rev0 = m.claim("res_1");
        let _ = m.claim("res_2"); // drugi resolver przejal (owner=res_2)
        let (applied, reverted) = m.apply("res_1", rev0);
        assert!(!applied, "spozniony apply pierwszego -> no-op");
        assert!(!reverted, "utrata ownershipu -> NIE revert (inny resolver dziala)");
        assert_eq!(m.owner.as_deref(), Some("res_2"), "owner drugiego nietkniety");
        assert_eq!(m.status, "resolving");
    }

    #[test]
    fn apply_succeeds_when_members_rev_unchanged() {
        // Brak TOCTOU: zbior bez zmian podczas LLM -> apply przechodzi, finalize OK.
        let mut m = ToctouModel::open(&["fA", "fB"]);
        let rev0 = m.claim("res_1");
        let (applied, reverted) = m.apply("res_1", rev0);
        assert!(applied && !reverted);
        assert_eq!(m.status, "resolved_auto");
    }

    // --- Wazne 5: evidence balance per-czlonek (LLM widzi OBIE strony konfliktu) ---

    fn ev(fact_key: &str, conf: f64, text: &str) -> EvidencePassage {
        EvidencePassage { fact_key: fact_key.into(), text: text.into(), confidence: conf }
    }

    #[test]
    fn evidence_balance_gives_each_member_representation() {
        // Wazne 5: czlonek A ma duzo mocnego evidence, czlonek B malo i slabsze. Globalne
        // ORDER BY confidence zabralo by same pasaze A; round-robin gwarantuje, ze B tez wchodzi
        // ZANIM A dostanie drugi pasaz — LLM nie jest zaglodzony jedna strona.
        let member_a: Vec<EvidencePassage> =
            (0..10).map(|i| ev("fA", 0.99, &format!("a{i}"))).collect();
        let member_b = vec![ev("fB", 0.30, "b0")];
        let (out, _pc, _cc) = balance_evidence(vec![member_a, member_b]);
        // Pierwsze dwa pasaze to po jednym z A i B (round-robin pas 0), MIMO ze B ma nizsza conf.
        assert_eq!(out[0].fact_key, "fA");
        assert_eq!(out[1].fact_key, "fB", "B wchodzi w pierwszym pasie, nie zaglodzony");
        assert!(out.iter().any(|p| p.fact_key == "fB"), "obie strony obecne");
    }

    #[test]
    fn evidence_balance_respects_global_passage_cap() {
        // Globalny cap MAX_EVIDENCE_PASSAGES nadal obowiazuje mimo balansu per-czlonek.
        let members: Vec<Vec<EvidencePassage>> = (0..8)
            .map(|m| (0..8).map(|i| ev(&format!("f{m}"), 0.5, &format!("t{m}_{i}"))).collect())
            .collect();
        let (out, passages_capped, _cc) = balance_evidence(members);
        assert!(out.len() <= MAX_EVIDENCE_PASSAGES, "globalny cap pasazy respektowany");
        assert!(passages_capped, "obciecie zgloszone (zakaz cichego capu)");
    }

    // --- Wazne 6: audit log NIE zawiera surowego reason z LLM ---

    /// Lustro audit_decision: buduje linie logu BEZ pola reason (reason zostaje w DB.decision).
    fn audit_line(id: i64, action: &str, head: &str, rel: &str, from_cache: bool, members: &[&str]) -> String {
        let source = if from_cache { "cache" } else { "llm" };
        format!(
            "rag: A_res decyzja id={id} action={action} head={head} rel={rel} \
             resolver=A_res model=rag-llm zrodlo={source} czlonkowie={}",
            members.join(",")
        )
    }

    #[test]
    fn audit_log_omits_raw_llm_reason() {
        // Wazne 6: reason z LLM (moze niesc fragmenty dokumentow usera) NIE trafia do logu.
        let line = audit_line(7, "keep_winner", "Alice", "born_in", false, &["fA", "fB"]);
        assert!(!line.contains("reason"), "linia audytu NIE zawiera reason");
        assert!(line.contains("action=keep_winner") && line.contains("czlonkowie=fA,fB"));
    }
}
