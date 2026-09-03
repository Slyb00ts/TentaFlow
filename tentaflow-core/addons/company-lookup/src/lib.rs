// =============================================================================
// Plik: addons/company-lookup/src/lib.rs
// Opis: Addon WASM pobierajacy online dane firm z oficjalnego Wykazu VAT MF.
//       Narzedzia LLM i flow blocki zwracaja znormalizowany JSON bez cache.
// =============================================================================

use std::collections::HashMap;

use tentaflow_addon_sdk::prelude::*;

const BASE_API: &str = "https://wl-api.mf.gov.pl/api/search";
const MAX_BATCH_SIZE: usize = 30;

#[no_mangle]
pub extern "C" fn on_install() -> i32 {
    log::info("company-lookup addon zainstalowany");
    0
}

#[no_mangle]
pub extern "C" fn on_start() -> i32 {
    log::info("company-lookup addon uruchomiony");
    0
}

#[no_mangle]
pub extern "C" fn on_stop() -> i32 {
    log::info("company-lookup addon zatrzymany");
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
    let result = if let Some(block_type) = tool_name.strip_prefix("block.") {
        handle_flow_block(block_type, &params)
    } else {
        match tool_name {
            "lookup_by_nip" => handle_lookup_by_nip(&params),
            "lookup_by_regon" => handle_lookup_by_regon(&params),
            "lookup_many_by_nip" => handle_lookup_many_by_nip(&params),
            "lookup_company" => handle_lookup_company(&params),
            _ => json!({"ok": false, "error": format!("Nieznane narzedzie: {}", tool_name)}),
        }
    };

    write_response(out_ptr, out_cap, out_len_ptr, &result)
}

fn handle_lookup_by_nip(params: &Value) -> Value {
    let nip = match read_identifier(params, "nip", 10, 10) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let date = match read_date(params) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    lookup_single("nip", &nip, &date)
}

fn handle_lookup_by_regon(params: &Value) -> Value {
    let regon = match read_regon(params) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let date = match read_date(params) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    lookup_single("regon", &regon, &date)
}

fn handle_lookup_company(params: &Value) -> Value {
    let identifier = match read_any_string(params, "identifier") {
        Some(v) => digits_only(&v),
        None => return error("Parametr identifier jest wymagany"),
    };
    match identifier.len() {
        10 => handle_lookup_by_nip(
            &json!({"nip": identifier, "date": params.get("date").cloned().unwrap_or(Value::Null)}),
        ),
        9 | 14 => handle_lookup_by_regon(
            &json!({"regon": identifier, "date": params.get("date").cloned().unwrap_or(Value::Null)}),
        ),
        _ => error("Identifier musi byc NIP-em albo REGON-em"),
    }
}

fn handle_lookup_many_by_nip(params: &Value) -> Value {
    let nips = match read_nip_list(params) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let date = match read_date(params) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let url = format!("{}/nips/{}?date={}", BASE_API, nips.join(","), date);
    let raw = match request_json(&url) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let result = raw.get("result").cloned().unwrap_or_else(|| json!({}));
    let entries = result
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let companies: Vec<Value> = entries.iter().map(normalize_entry).collect();

    json!({
        "ok": true,
        "source": "mf_vat_whitelist",
        "online": true,
        "cached": false,
        "date": date,
        "request_id": string_at(&result, "requestId"),
        "request_date_time": string_at(&result, "requestDateTime"),
        "companies": companies,
        "raw": raw
    })
}

fn handle_flow_block(block_type: &str, params: &Value) -> Value {
    let payload = params
        .get("payload")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let result = match block_type {
        "lookup_by_nip" => {
            let nip = extract_payload_identifier(&payload, &["nip", "Nip", "NIP", "Text", "text"]);
            handle_lookup_by_nip(&json!({"nip": nip, "date": extract_payload_date(&payload)}))
        }
        "lookup_by_regon" => {
            let regon =
                extract_payload_identifier(&payload, &["regon", "Regon", "REGON", "Text", "text"]);
            handle_lookup_by_regon(&json!({"regon": regon, "date": extract_payload_date(&payload)}))
        }
        _ => error(format!("Nieznany flow block: {}", block_type)),
    };

    if params.get("payload").is_some() {
        let mut response = params.clone();
        if let Some(obj) = response.as_object_mut() {
            obj.insert("payload".to_string(), result);
        }
        response
    } else {
        result
    }
}

fn lookup_single(kind: &str, identifier: &str, date: &str) -> Value {
    let url = format!("{}/{}/{}?date={}", BASE_API, kind, identifier, date);
    let raw = match request_json(&url) {
        Ok(v) => v,
        Err(e) => return error(e),
    };
    let result = raw.get("result").cloned().unwrap_or_else(|| json!({}));
    let subject = result.get("subject").cloned().unwrap_or(Value::Null);

    if subject.is_null() {
        return json!({
            "ok": true,
            "found": false,
            "source": "mf_vat_whitelist",
            "online": true,
            "cached": false,
            "lookup": {"type": kind, "identifier": identifier},
            "date": date,
            "request_id": string_at(&result, "requestId"),
            "request_date_time": string_at(&result, "requestDateTime"),
            "company": Value::Null,
            "raw": raw
        });
    }

    json!({
        "ok": true,
        "found": true,
        "source": "mf_vat_whitelist",
        "online": true,
        "cached": false,
        "lookup": {"type": kind, "identifier": identifier},
        "date": date,
        "request_id": string_at(&result, "requestId"),
        "request_date_time": string_at(&result, "requestDateTime"),
        "company": normalize_subject(&subject),
        "raw": raw
    })
}

fn request_json(url: &str) -> Result<Value, String> {
    let mut headers = HashMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert(
        "User-Agent".to_string(),
        "TentaFlow-Company-Lookup-Addon/1.0".to_string(),
    );
    let response = http_send(&HttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers,
        body: None,
    })?;
    if response.status < 200 || response.status >= 300 {
        return Err(format!("API MF zwrocilo HTTP {}", response.status));
    }
    serde_json::from_str(&response.body)
        .map_err(|e| format!("API MF zwrocilo niepoprawny JSON: {}", e))
}

fn normalize_entry(entry: &Value) -> Value {
    if let Some(error_value) = entry.get("error") {
        return json!({
            "identifier": string_at(entry, "identifier"),
            "found": false,
            "error": error_value
        });
    }
    let subjects = entry
        .get("subjects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let companies: Vec<Value> = subjects.iter().map(normalize_subject).collect();
    json!({
        "identifier": string_at(entry, "identifier"),
        "found": !companies.is_empty(),
        "subjects": companies
    })
}

fn normalize_subject(subject: &Value) -> Value {
    let residence_address = string_at(subject, "residenceAddress");
    let working_address = string_at(subject, "workingAddress");
    let primary_address = if !working_address.is_empty() {
        working_address.clone()
    } else {
        residence_address.clone()
    };

    json!({
        "name": string_at(subject, "name"),
        "nip": string_at(subject, "nip"),
        "regon": string_at(subject, "regon"),
        "krs": string_at(subject, "krs"),
        "vat_status": string_at(subject, "statusVat"),
        "address": primary_address,
        "working_address": working_address,
        "residence_address": residence_address,
        "account_numbers": subject.get("accountNumbers").cloned().unwrap_or_else(|| json!([])),
        "has_virtual_accounts": subject.get("hasVirtualAccounts").cloned().unwrap_or(Value::Null),
        "registration_legal_date": string_at(subject, "registrationLegalDate"),
        "registration_denial_date": string_at(subject, "registrationDenialDate"),
        "removal_date": string_at(subject, "removalDate"),
        "restoration_date": string_at(subject, "restorationDate"),
        "representatives": subject.get("representatives").cloned().unwrap_or_else(|| json!([])),
        "authorized_clerks": subject.get("authorizedClerks").cloned().unwrap_or_else(|| json!([])),
        "partners": subject.get("partners").cloned().unwrap_or_else(|| json!([]))
    })
}

fn read_identifier(
    params: &Value,
    key: &str,
    min_len: usize,
    max_len: usize,
) -> Result<String, &'static str> {
    let raw = read_any_string(params, key).ok_or("Brak wymaganego identyfikatora")?;
    let value = digits_only(&raw);
    if value.len() < min_len || value.len() > max_len {
        return Err("Identyfikator ma niepoprawna dlugosc");
    }
    Ok(value)
}

fn read_regon(params: &Value) -> Result<String, &'static str> {
    let raw = read_any_string(params, "regon").ok_or("Parametr regon jest wymagany")?;
    let value = digits_only(&raw);
    match value.len() {
        9 | 14 => Ok(value),
        _ => Err("REGON musi miec 9 albo 14 cyfr"),
    }
}

fn read_nip_list(params: &Value) -> Result<Vec<String>, &'static str> {
    let values = match params.get("nips") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(value_to_string)
            .collect::<Vec<String>>(),
        Some(value) => value_to_string(value)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .map(str::to_string)
            .collect::<Vec<String>>(),
        None => Vec::new(),
    };
    let nips = values
        .iter()
        .map(|v| digits_only(v))
        .filter(|v| !v.is_empty())
        .collect::<Vec<String>>();
    if nips.is_empty() {
        return Err("Parametr nips jest wymagany");
    }
    if nips.len() > MAX_BATCH_SIZE {
        return Err("Jedno wywolanie moze zawierac maksymalnie 30 numerow NIP");
    }
    if nips.iter().any(|v| v.len() != 10) {
        return Err("Kazdy NIP musi miec 10 cyfr");
    }
    Ok(nips)
}

fn read_date(params: &Value) -> Result<String, &'static str> {
    match read_any_string(params, "date") {
        Some(v) if !v.trim().is_empty() => {
            let date = v.trim().to_string();
            if is_valid_date(&date) {
                Ok(date)
            } else {
                Err("Data musi miec format YYYY-MM-DD")
            }
        }
        _ => Ok(today_utc()),
    }
}

fn read_any_string(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(value_to_string)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(v) => Some(v.clone()),
        Value::Number(v) => Some(v.to_string()),
        _ => None,
    }
}

fn extract_payload_identifier(payload: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| read_any_string(payload, key))
        .unwrap_or_default()
}

fn extract_payload_date(payload: &Value) -> Value {
    read_any_string(payload, "date")
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn digits_only(value: &str) -> String {
    value.chars().filter(|ch| ch.is_ascii_digit()).collect()
}

fn is_valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, b)| idx == 4 || idx == 7 || b.is_ascii_digit())
}

fn today_utc() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

fn string_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn error(message: impl Into<String>) -> Value {
    json!({"ok": false, "error": message.into(), "cached": false})
}

fn write_response(out_ptr: i32, out_cap: i32, out_len_ptr: i32, value: &Value) -> i32 {
    let response_str = match serde_json::to_string(value) {
        Ok(s) => s,
        Err(_) => return 1,
    };
    let written = write_string(out_ptr, out_cap, out_len_ptr, &response_str);
    if written < 0 {
        log::error("Bufor wyjsciowy za maly na odpowiedz company-lookup");
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
    fn digits_only_removes_formatting() {
        assert_eq!(digits_only("123-456-32-18"), "1234563218");
    }

    #[test]
    fn date_validation_accepts_iso_date_only() {
        assert!(is_valid_date("2026-05-20"));
        assert!(!is_valid_date("20-05-2026"));
        assert!(!is_valid_date("2026-5-20"));
    }

    #[test]
    fn civil_date_matches_unix_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_593), (2026, 5, 20));
    }

    #[test]
    fn normalize_subject_prefers_working_address() {
        let subject = json!({
            "name": "ACME SP. Z O.O.",
            "nip": "1234563218",
            "regon": "012345678",
            "workingAddress": "ul. Testowa 1, Warszawa",
            "residenceAddress": "ul. Inna 2, Krakow",
            "statusVat": "Czynny"
        });

        let normalized = normalize_subject(&subject);

        assert_eq!(
            normalized.get("address").and_then(Value::as_str),
            Some("ul. Testowa 1, Warszawa")
        );
        assert_eq!(
            normalized.get("vat_status").and_then(Value::as_str),
            Some("Czynny")
        );
    }
}
