// =============================================================================
// Plik: addons/eureka/src/lib.rs
// Opis: Addon WASM indeksujacy publiczne informacje Eureka MF w per-addon SQLite.
//       Narzedzia LLM uruchamiaja wyszukiwanie lokalne oraz wznawialny crawler.
// =============================================================================

use std::collections::HashMap;

use tentaflow_addon_sdk::prelude::*;

const BASE_API: &str = "https://eureka.mf.gov.pl/api/public/v1";
const PUBLIC_BASE: &str = "https://eureka.mf.gov.pl/informacje/podglad";
const DEFAULT_START_ID: i64 = 1;
const DEFAULT_END_ID: i64 = 700_000;
const MAX_ENTRY_ID: i64 = 10_000_000;
const DEFAULT_BATCH_SIZE: i64 = 50;
const MAX_BATCH_SIZE: i64 = 250;
const DEFAULT_MISSING_LIMIT: i64 = 50;
const DEFAULT_MAX_ERRORS: i64 = 5;

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("eureka addon zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("eureka addon uruchomiony");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("eureka addon zatrzymany");
    0
}

#[no_mangle]
pub extern "C" fn on_event(_event_ptr: i32, _event_len: i32) -> i32 {
    0
}

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
            return write_response(
                out_ptr,
                out_cap,
                out_len_ptr,
                &json!({"ok": false, "error": format!("Niepoprawny request JSON: {}", e)}),
            );
        }
    };

    let tool_name = request.get("tool").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match tool_name {
        "search" => handle_search(&params),
        "get_entry" => handle_get_entry(&params),
        "sync_new" => handle_sync_new(&params),
        "full_dump" => handle_full_dump(&params),
        "retry_failed" => handle_retry_failed(&params),
        "recent" => handle_recent(&params),
        "stats" => handle_stats(),
        _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

fn handle_search(params: &Value) -> Value {
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if query.is_empty() {
        return json!({"ok": false, "error": "Parametr query jest wymagany"});
    }

    let limit = clamp_i64(
        params.get("limit").and_then(Value::as_i64).unwrap_or(10),
        1,
        50,
    );
    let offset = clamp_i64(
        params.get("offset").and_then(Value::as_i64).unwrap_or(0),
        0,
        100_000,
    );
    let pattern = format!("%{}%", query);
    let rows = match sql_query(
        "SELECT id, url, title, template_name, signature, thesis, publication_date, issue_date, substr(content_text, 1, 1200) \
         FROM eureka_entries \
         WHERE id = ? OR title LIKE ? OR signature LIKE ? OR thesis LIKE ? OR content_text LIKE ? \
         ORDER BY publication_date DESC, id DESC LIMIT ? OFFSET ?",
        &[
            SqlValue::I64(query.parse::<i64>().unwrap_or(-1)),
            SqlValue::String(pattern.clone()),
            SqlValue::String(pattern.clone()),
            SqlValue::String(pattern.clone()),
            SqlValue::String(pattern),
            SqlValue::I64(limit),
            SqlValue::I64(offset),
        ],
    ) {
        Ok(rows) => rows,
        Err(e) => return sql_error("search", e),
    };

    let entries: Vec<Value> = rows.iter().map(row_to_search_result).collect();
    json!({"ok": true, "data": {"query": query, "limit": limit, "offset": offset, "results": entries}})
}

fn handle_get_entry(params: &Value) -> Value {
    let id = match required_id(params) {
        Ok(id) => id,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let refresh = params
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !refresh {
        match load_entry(id) {
            Ok(Some(entry)) => {
                return json!({"ok": true, "data": {"entry": entry, "source": "sqlite"}})
            }
            Ok(None) => {}
            Err(e) => return sql_error("get_entry", e),
        }
    }

    match fetch_and_store_entry(id) {
        FetchResult::Stored(entry) => {
            json!({"ok": true, "data": {"entry": entry, "source": "eureka"}})
        }
        FetchResult::Missing => {
            json!({"ok": false, "error": format!("Nie znaleziono wpisu Eureka id={}", id)})
        }
        FetchResult::Failed(err) => json!({"ok": false, "error": err.message}),
    }
}

fn handle_sync_new(params: &Value) -> Value {
    let batch_size = normalized_batch_size(params);
    let missing_limit = clamp_i64(
        params
            .get("max_consecutive_missing")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_MISSING_LIMIT),
        1,
        1_000,
    );
    let start_id = clamp_i64(
        get_state_i64("sync_new_next_id")
            .or_else(|| max_entry_id().ok().flatten().map(|id| id + 1))
            .unwrap_or(DEFAULT_END_ID),
        DEFAULT_START_ID,
        MAX_ENTRY_ID,
    );

    let mut next_id = start_id;
    let mut checked = 0;
    let mut stored = 0;
    let mut missing = 0;
    let mut errors = Vec::new();
    let max_errors = normalized_max_errors(params);

    while checked < batch_size && missing < missing_limit && (errors.len() as i64) < max_errors {
        match fetch_and_store_entry(next_id) {
            FetchResult::Stored(_) => {
                stored += 1;
                missing = 0;
                let _ = set_state_i64("last_seen_id", next_id);
            }
            FetchResult::Missing => {
                missing += 1;
            }
            FetchResult::Failed(err) => {
                errors.push(json!({"id": next_id, "error": err.message}));
            }
        }
        checked += 1;
        next_id += 1;
        let _ = set_state_i64("sync_new_next_id", next_id);
    }

    json!({
        "ok": true,
        "data": {
            "mode": "sync_new",
            "start_id": start_id,
            "next_id": next_id,
            "checked": checked,
            "stored": stored,
            "consecutive_missing": missing,
            "finished_window": missing >= missing_limit,
            "stopped_on_errors": (errors.len() as i64) >= max_errors,
            "errors": errors
        }
    })
}

fn handle_full_dump(params: &Value) -> Value {
    let reset = params
        .get("reset")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if reset {
        let _ = set_state_i64("full_dump_next_id", 0);
    }

    let direction = params
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("desc")
        .to_ascii_lowercase();
    let descending = direction != "asc";
    let start_id = requested_id(
        params,
        "start_id",
        if descending {
            DEFAULT_END_ID
        } else {
            DEFAULT_START_ID
        },
    );
    let end_id = requested_id(
        params,
        "end_id",
        if descending {
            DEFAULT_START_ID
        } else {
            DEFAULT_END_ID
        },
    );
    let batch_size = normalized_batch_size(params);
    let checkpoint = get_state_i64("full_dump_next_id").filter(|v| *v > 0);
    let mut current_id = checkpoint.unwrap_or(start_id);
    let mut checked = 0;
    let mut stored = 0;
    let mut missing = 0;
    let mut errors = Vec::new();
    let max_errors = normalized_max_errors(params);

    while checked < batch_size
        && in_range(current_id, end_id, descending)
        && (errors.len() as i64) < max_errors
    {
        match fetch_and_store_entry(current_id) {
            FetchResult::Stored(_) => stored += 1,
            FetchResult::Missing => missing += 1,
            FetchResult::Failed(err) => {
                errors.push(json!({"id": current_id, "error": err.message}))
            }
        }
        checked += 1;
        current_id = if descending {
            current_id - 1
        } else {
            current_id + 1
        };
        let _ = set_state_i64("full_dump_next_id", current_id);
    }

    let completed = !in_range(current_id, end_id, descending);
    if completed {
        let _ = set_state_i64("full_dump_next_id", 0);
    }

    json!({
        "ok": true,
        "data": {
            "mode": "full_dump",
            "direction": if descending { "desc" } else { "asc" },
            "start_id": start_id,
            "end_id": end_id,
            "next_id": current_id,
            "checked": checked,
            "stored": stored,
            "missing": missing,
            "completed": completed,
            "stopped_on_errors": (errors.len() as i64) >= max_errors,
            "errors": errors
        }
    })
}

fn handle_retry_failed(params: &Value) -> Value {
    let batch_size = normalized_batch_size(params);
    let max_errors = normalized_max_errors(params);
    let rows = match sql_query(
        "SELECT id FROM eureka_fetch_status WHERE status = 'error' ORDER BY last_attempt_at ASC LIMIT ?",
        &[SqlValue::I64(batch_size)],
    ) {
        Ok(rows) => rows,
        Err(e) => return sql_error("retry_failed", e),
    };

    let mut checked = 0;
    let mut stored = 0;
    let mut missing = 0;
    let mut errors = Vec::new();

    for row in rows {
        if (errors.len() as i64) >= max_errors {
            break;
        }
        let id = sql_i64(&row, 0);
        match fetch_and_store_entry(id) {
            FetchResult::Stored(_) => stored += 1,
            FetchResult::Missing => missing += 1,
            FetchResult::Failed(err) => errors.push(json!({"id": id, "error": err.message})),
        }
        checked += 1;
    }

    json!({
        "ok": true,
        "data": {
            "mode": "retry_failed",
            "checked": checked,
            "stored": stored,
            "missing": missing,
            "stopped_on_errors": (errors.len() as i64) >= max_errors,
            "errors": errors
        }
    })
}

fn handle_recent(params: &Value) -> Value {
    let limit = clamp_i64(
        params.get("limit").and_then(Value::as_i64).unwrap_or(20),
        1,
        100,
    );
    let offset = clamp_i64(
        params.get("offset").and_then(Value::as_i64).unwrap_or(0),
        0,
        100_000,
    );
    let rows = match sql_query(
        "SELECT id, url, title, template_name, signature, thesis, publication_date, issue_date, substr(content_text, 1, 1200) \
         FROM eureka_entries ORDER BY publication_date DESC, id DESC LIMIT ? OFFSET ?",
        &[SqlValue::I64(limit), SqlValue::I64(offset)],
    ) {
        Ok(rows) => rows,
        Err(e) => return sql_error("recent", e),
    };
    let entries: Vec<Value> = rows.iter().map(row_to_search_result).collect();
    json!({"ok": true, "data": {"limit": limit, "offset": offset, "results": entries}})
}

fn handle_stats() -> Value {
    let row = match sql_query_one(
        "SELECT COUNT(*), MIN(id), MAX(id), MAX(fetched_at) FROM eureka_entries",
        &[],
    ) {
        Ok(Some(row)) => row,
        Ok(None) => Vec::new(),
        Err(e) => return sql_error("stats", e),
    };
    let state_rows = match sql_query(
        "SELECT key, value, updated_at FROM eureka_sync_state ORDER BY key",
        &[],
    ) {
        Ok(rows) => rows,
        Err(e) => return sql_error("stats", e),
    };
    let states: Vec<Value> = state_rows
        .iter()
        .map(|r| {
            json!({
                "key": sql_str(r, 0),
                "value": sql_str(r, 1),
                "updated_at": sql_i64(r, 2),
            })
        })
        .collect();
    let status_rows = match sql_query(
        "SELECT status, COUNT(*), MAX(last_attempt_at) FROM eureka_fetch_status GROUP BY status ORDER BY status",
        &[],
    ) {
        Ok(rows) => rows,
        Err(e) => return sql_error("stats", e),
    };
    let fetch_status: Vec<Value> = status_rows
        .iter()
        .map(|r| {
            json!({
                "status": sql_str(r, 0),
                "count": sql_i64(r, 1),
                "last_attempt_at": sql_i64(r, 2),
            })
        })
        .collect();

    json!({
        "ok": true,
        "data": {
            "count": sql_i64(&row, 0),
            "min_id": sql_i64(&row, 1),
            "max_id": sql_i64(&row, 2),
            "last_fetched_at": sql_i64(&row, 3),
            "state": states,
            "fetch_status": fetch_status
        }
    })
}

enum FetchResult {
    Stored(Value),
    Missing,
    Failed(FetchError),
}

struct FetchError {
    message: String,
}

fn fetch_and_store_entry(id: i64) -> FetchResult {
    let url = format!("{}/informacje/{}", BASE_API, id);
    let mut headers = HashMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert(
        "User-Agent".to_string(),
        "TentaFlow-Eureka-Addon/1.0".to_string(),
    );
    let response = match http_send(&HttpRequest {
        method: "GET".to_string(),
        url,
        headers,
        body: None,
    }) {
        Ok(response) => response,
        Err(e) => {
            let message = format!("HTTP blad dla id={}: {}", id, e);
            let _ = record_fetch_status(id, "error", None, &message);
            return FetchResult::Failed(FetchError { message });
        }
    };

    if response.status == 404 {
        let _ = record_fetch_status(id, "missing", Some(response.status as i64), "");
        return FetchResult::Missing;
    }
    if response.status < 200 || response.status >= 300 {
        let message = format!("Eureka zwrocila HTTP {} dla id={}", response.status, id);
        let _ = record_fetch_status(id, "error", Some(response.status as i64), &message);
        return FetchResult::Failed(FetchError { message });
    }

    let raw: Value = match serde_json::from_str(&response.body) {
        Ok(v) => v,
        Err(e) => {
            let message = format!("Niepoprawny JSON dla id={}: {}", id, e);
            let _ = record_fetch_status(id, "error", Some(response.status as i64), &message);
            return FetchResult::Failed(FetchError { message });
        }
    };
    let entry = normalize_entry(id, &raw);
    match store_entry(&entry) {
        Ok(()) => {
            let _ = record_fetch_status(id, "stored", Some(response.status as i64), "");
            FetchResult::Stored(entry.to_json())
        }
        Err(e) => {
            let message = format!("Blad zapisu SQLite dla id={}: {:?}", id, e);
            let _ = record_fetch_status(id, "error", Some(response.status as i64), &message);
            FetchResult::Failed(FetchError { message })
        }
    }
}

fn normalize_entry(id: i64, raw: &Value) -> Entry {
    let fields = raw
        .get("dokument")
        .and_then(|v| v.get("fields"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut field_map = HashMap::new();
    for field in fields {
        if let Some(key) = field.get("key").and_then(Value::as_str) {
            field_map.insert(
                key.to_string(),
                field.get("value").cloned().unwrap_or(Value::Null),
            );
        }
    }

    let content_html = string_field(&field_map, "TRESC_INTERESARIUSZ")
        .or_else(|| string_field(&field_map, "TRESC"))
        .unwrap_or_default();
    let content_text = strip_html(&content_html);
    let title = raw
        .get("nazwa")
        .and_then(Value::as_str)
        .unwrap_or("Informacja Eureka")
        .to_string();
    let thesis = string_field(&field_map, "TEZA").unwrap_or_default();
    let signature = string_field(&field_map, "SYG").unwrap_or_default();
    let publication_date = string_field(&field_map, "DATA_PUBLIKACJI").unwrap_or_default();
    let issue_date = string_field(&field_map, "DT_WYD").unwrap_or_default();
    let metadata_json = serde_json::to_string(raw).unwrap_or_else(|_| "{}".to_string());
    let source_hash = stable_hash(&metadata_json);
    let now = unix_time();

    Entry {
        id,
        url: format!("{}/{}", PUBLIC_BASE, id),
        title,
        template_name: raw
            .get("nazwa")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        signature,
        thesis,
        publication_date,
        issue_date,
        content_text,
        content_html,
        metadata_json,
        source_hash,
        fetched_at: now,
        updated_at: now,
    }
}

fn store_entry(entry: &Entry) -> Result<(), AbiError> {
    sql_exec(
        "INSERT INTO eureka_entries \
         (id, url, title, template_name, signature, thesis, publication_date, issue_date, content_text, content_html, metadata_json, source_hash, fetched_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
         url=excluded.url, title=excluded.title, template_name=excluded.template_name, signature=excluded.signature, thesis=excluded.thesis, \
         publication_date=excluded.publication_date, issue_date=excluded.issue_date, content_text=excluded.content_text, content_html=excluded.content_html, \
         metadata_json=excluded.metadata_json, source_hash=excluded.source_hash, fetched_at=excluded.fetched_at, updated_at=excluded.updated_at",
        &[
            SqlValue::I64(entry.id),
            SqlValue::String(entry.url.clone()),
            SqlValue::String(entry.title.clone()),
            SqlValue::String(entry.template_name.clone()),
            SqlValue::String(entry.signature.clone()),
            SqlValue::String(entry.thesis.clone()),
            SqlValue::String(entry.publication_date.clone()),
            SqlValue::String(entry.issue_date.clone()),
            SqlValue::String(entry.content_text.clone()),
            SqlValue::String(entry.content_html.clone()),
            SqlValue::String(entry.metadata_json.clone()),
            SqlValue::I64(entry.source_hash as i64),
            SqlValue::I64(entry.fetched_at),
            SqlValue::I64(entry.updated_at),
        ],
    )?;
    Ok(())
}

fn load_entry(id: i64) -> Result<Option<Value>, AbiError> {
    let row = sql_query_one(
        "SELECT id, url, title, template_name, signature, thesis, publication_date, issue_date, content_text, content_html, metadata_json, fetched_at, updated_at \
         FROM eureka_entries WHERE id = ?",
        &[SqlValue::I64(id)],
    )?;
    Ok(row.as_ref().map(row_to_entry))
}

fn max_entry_id() -> Result<Option<i64>, AbiError> {
    let row = sql_query_one("SELECT MAX(id) FROM eureka_entries", &[])?;
    Ok(row.and_then(|r| r.first().and_then(SqlValue::as_i64)))
}

fn get_state_i64(key: &str) -> Option<i64> {
    let row = sql_query_one(
        "SELECT value FROM eureka_sync_state WHERE key = ?",
        &[SqlValue::String(key.to_string())],
    )
    .ok()
    .flatten()?;
    row.first()?.as_str()?.parse().ok()
}

fn set_state_i64(key: &str, value: i64) -> Result<(), AbiError> {
    sql_exec(
        "INSERT INTO eureka_sync_state (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        &[
            SqlValue::String(key.to_string()),
            SqlValue::String(value.to_string()),
            SqlValue::I64(unix_time()),
        ],
    )?;
    Ok(())
}

fn record_fetch_status(
    id: i64,
    status: &str,
    http_status: Option<i64>,
    error: &str,
) -> Result<(), AbiError> {
    sql_exec(
        "INSERT INTO eureka_fetch_status (id, status, http_status, error, attempts, last_attempt_at) \
         VALUES (?, ?, ?, ?, 1, ?) \
         ON CONFLICT(id) DO UPDATE SET status=excluded.status, http_status=excluded.http_status, \
         error=excluded.error, attempts=eureka_fetch_status.attempts + 1, last_attempt_at=excluded.last_attempt_at",
        &[
            SqlValue::I64(id),
            SqlValue::String(status.to_string()),
            http_status.map(SqlValue::I64).unwrap_or(SqlValue::Null),
            SqlValue::String(error.to_string()),
            SqlValue::I64(unix_time()),
        ],
    )?;
    Ok(())
}

#[derive(Debug)]
struct Entry {
    id: i64,
    url: String,
    title: String,
    template_name: String,
    signature: String,
    thesis: String,
    publication_date: String,
    issue_date: String,
    content_text: String,
    content_html: String,
    metadata_json: String,
    source_hash: u64,
    fetched_at: i64,
    updated_at: i64,
}

impl Entry {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "url": self.url,
            "title": self.title,
            "template_name": self.template_name,
            "signature": self.signature,
            "thesis": self.thesis,
            "publication_date": self.publication_date,
            "issue_date": self.issue_date,
            "content_text": self.content_text,
            "content_html": self.content_html,
            "metadata_json": serde_json::from_str::<Value>(&self.metadata_json).unwrap_or_else(|_| json!({})),
            "fetched_at": self.fetched_at,
            "updated_at": self.updated_at
        })
    }
}

fn row_to_search_result(row: &SqlRow) -> Value {
    json!({
        "id": sql_i64(row, 0),
        "url": sql_str(row, 1),
        "title": sql_str(row, 2),
        "template_name": sql_str(row, 3),
        "signature": sql_str(row, 4),
        "thesis": sql_str(row, 5),
        "publication_date": sql_str(row, 6),
        "issue_date": sql_str(row, 7),
        "snippet": sql_str(row, 8),
    })
}

fn row_to_entry(row: &SqlRow) -> Value {
    let metadata_json = sql_str(row, 10);
    json!({
        "id": sql_i64(row, 0),
        "url": sql_str(row, 1),
        "title": sql_str(row, 2),
        "template_name": sql_str(row, 3),
        "signature": sql_str(row, 4),
        "thesis": sql_str(row, 5),
        "publication_date": sql_str(row, 6),
        "issue_date": sql_str(row, 7),
        "content_text": sql_str(row, 8),
        "content_html": sql_str(row, 9),
        "metadata_json": serde_json::from_str::<Value>(&metadata_json).unwrap_or_else(|_| json!({})),
        "fetched_at": sql_i64(row, 11),
        "updated_at": sql_i64(row, 12),
    })
}

fn string_field(fields: &HashMap<String, Value>, key: &str) -> Option<String> {
    match fields.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Array(values) => Some(
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(v) => Some(v.to_string()),
        Value::Null | Value::Object(_) => None,
    }
}

fn required_id(params: &Value) -> Result<i64, String> {
    let id = params
        .get("id")
        .and_then(Value::as_i64)
        .or_else(|| {
            params
                .get("id")
                .and_then(Value::as_str)
                .and_then(|v| v.parse().ok())
        })
        .ok_or_else(|| "Parametr id jest wymagany i musi byc liczba dodatnia".to_string())?;
    if (DEFAULT_START_ID..=MAX_ENTRY_ID).contains(&id) {
        Ok(id)
    } else {
        Err("Parametr id jest poza dozwolonym zakresem".to_string())
    }
}

fn requested_id(params: &Value, key: &str, default: i64) -> i64 {
    clamp_i64(
        params
            .get(key)
            .and_then(Value::as_i64)
            .or_else(|| {
                params
                    .get(key)
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(default),
        DEFAULT_START_ID,
        MAX_ENTRY_ID,
    )
}

fn normalized_batch_size(params: &Value) -> i64 {
    clamp_i64(
        params
            .get("batch_size")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_BATCH_SIZE),
        1,
        MAX_BATCH_SIZE,
    )
}

fn normalized_max_errors(params: &Value) -> i64 {
    clamp_i64(
        params
            .get("max_errors")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_MAX_ERRORS),
        1,
        100,
    )
}

fn in_range(current: i64, end: i64, descending: bool) -> bool {
    if descending {
        current >= end
    } else {
        current <= end
    }
}

fn sql_str(row: &SqlRow, index: usize) -> String {
    row.get(index)
        .and_then(SqlValue::as_str)
        .unwrap_or("")
        .to_string()
}

fn sql_i64(row: &SqlRow, index: usize) -> i64 {
    row.get(index).and_then(SqlValue::as_i64).unwrap_or(0)
}

fn sql_error(operation: &str, error: AbiError) -> Value {
    json!({"ok": false, "error": format!("Blad SQLite w {}: {:?}", operation, error)})
}

fn clamp_i64(value: i64, min: i64, max: i64) -> i64 {
    value.max(min).min(max)
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 14_695_981_039_346_656_037u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

fn strip_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut entity = String::new();
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            '&' if !in_tag => {
                entity.clear();
                entity.push('&');
            }
            ';' if !in_tag && entity.starts_with('&') => {
                entity.push(';');
                output.push_str(decode_entity(&entity));
                entity.clear();
            }
            _ if in_tag => {}
            _ if entity.starts_with('&') => {
                entity.push(ch);
                if entity.len() > 16 {
                    output.push_str(&entity);
                    entity.clear();
                }
            }
            _ => output.push(ch),
        }
    }
    if !entity.is_empty() {
        output.push_str(&entity);
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entity(entity: &str) -> &str {
    match entity {
        "&nbsp;" => " ",
        "&amp;" => "&",
        "&lt;" => "<",
        "&gt;" => ">",
        "&quot;" => "\"",
        "&#39;" => "'",
        _ => entity,
    }
}

fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, out_len_ptr, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz eureka");
        return ABI_OUTPUT_BUFFER_TOO_SMALL;
    }
    let len_bytes = written.to_le_bytes();
    let dest = unsafe { std::slice::from_raw_parts_mut(out_len_ptr as *mut u8, 4) };
    dest.copy_from_slice(&len_bytes);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_decodes_basic_entities_and_removes_tags() {
        let input = "<p>Ala&nbsp;&amp;&nbsp;Ola</p><br><span>&lt;test&gt;</span>";

        assert_eq!(strip_html(input), "Ala & Ola <test>");
    }

    #[test]
    fn normalize_entry_extracts_public_fields() {
        let raw = json!({
            "nazwa": "Interpretacja indywidualna",
            "dokument": {
                "fields": [
                    {"key": "TEZA", "value": "Teza wpisu"},
                    {"key": "SYG", "value": "0111-KDIB1"},
                    {"key": "DATA_PUBLIKACJI", "value": "2026-05-19T05:24:13.946Z"},
                    {"key": "DT_WYD", "value": "2026-05-14T23:55:43.907Z"},
                    {"key": "TRESC_INTERESARIUSZ", "value": "<p>Treść&nbsp;wpisu</p>"}
                ]
            }
        });

        let entry = normalize_entry(691596, &raw);

        assert_eq!(entry.id, 691596);
        assert_eq!(entry.title, "Interpretacja indywidualna");
        assert_eq!(entry.signature, "0111-KDIB1");
        assert_eq!(entry.thesis, "Teza wpisu");
        assert_eq!(entry.content_text, "Treść wpisu");
        assert_eq!(
            entry.url,
            "https://eureka.mf.gov.pl/informacje/podglad/691596"
        );
    }

    #[test]
    fn normalized_limits_are_clamped() {
        assert_eq!(normalized_batch_size(&json!({"batch_size": 10_000})), 250);
        assert_eq!(normalized_batch_size(&json!({"batch_size": 0})), 1);
        assert_eq!(normalized_max_errors(&json!({"max_errors": 500})), 100);
        assert_eq!(normalized_max_errors(&json!({"max_errors": 0})), 1);
    }
}
