// ===== File: project_studio/api_spec.rs — OpenAPI/Swagger endpoint extraction (F3) =====
//
// Turns an uploaded API description into (a) the endpoint list the UI shows and
// the api-test generator reads, and (b) a markdown digest that is ingested as a
// normal knowledge file so endpoint text is searchable.
//
// NO `openapiv3`: that crate models ONE dialect (OpenAPI 3.0) with a strict
// typed schema and rejects whole documents over a single non-conformant field —
// while `paths -> {method: {summary, parameters}}` is identical in Swagger 2.0,
// OpenAPI 3.0 and 3.1. A generic walk over the parsed document therefore covers
// every dialect in a fraction of the code and degrades on odd input instead of
// failing. YAML is parsed with `serde_yaml_ng` straight into `serde_json::Value`
// so both encodings share one code path.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Upper bound on the spec document; larger files are not hand-authored APIs.
pub const MAX_SPEC_BYTES: usize = 16 * 1024 * 1024;
/// Upper bound on the extracted endpoint list.
pub const MAX_ENDPOINTS: usize = 2000;

const HTTP_METHODS: &[&str] = &[
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

/// One extracted endpoint. Serialized as the `endpoints_json` array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Upper-case HTTP method.
    pub method: String,
    pub path: String,
    pub summary: String,
    pub operation_id: String,
    /// Shorthand parameters, e.g. `["path:id*", "query:limit"]` (`*` = required).
    pub parameters: Vec<String>,
    /// Request-body content types, e.g. `["application/json"]`.
    pub request_body: Vec<String>,
    /// Declared response status codes.
    pub responses: Vec<String>,
    pub tags: Vec<String>,
}

/// Parsed document metadata plus its endpoints.
#[derive(Debug, Clone)]
pub struct ParsedSpec {
    pub title: String,
    pub version: String,
    pub endpoints: Vec<Endpoint>,
}

/// Parses a JSON or YAML API description. The format is detected by content
/// (JSON first, YAML as the fallback) — file extensions lie often enough.
pub fn parse_spec(bytes: &[u8]) -> Result<ParsedSpec> {
    if bytes.len() > MAX_SPEC_BYTES {
        return Err(anyhow!("spec exceeds {MAX_SPEC_BYTES} bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("spec is not valid UTF-8 text"))?;
    let document: Value = match serde_json::from_str::<Value>(text) {
        Ok(value) => value,
        Err(json_err) => serde_yaml_ng::from_str::<Value>(text).map_err(|yaml_err| {
            anyhow!("spec is neither JSON ({json_err}) nor YAML ({yaml_err})")
        })?,
    };
    from_document(&document)
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Extracts endpoints from an already-parsed document.
pub fn from_document(document: &Value) -> Result<ParsedSpec> {
    let info = document.get("info").cloned().unwrap_or(Value::Null);
    let title = str_field(&info, "title");
    let version = str_field(&info, "version");

    let paths = document
        .get("paths")
        .and_then(|v| v.as_object())
        .ok_or_else(|| anyhow!("document has no 'paths' object — not an OpenAPI/Swagger spec"))?;

    let mut endpoints: Vec<Endpoint> = Vec::new();
    for (path, path_item) in paths {
        let Some(item) = path_item.as_object() else {
            continue;
        };
        // Parameters declared on the path item apply to every operation under it.
        let shared = item
            .get("parameters")
            .map(parameter_list)
            .unwrap_or_default();
        for (method, operation) in item {
            let method_lower = method.to_ascii_lowercase();
            if !HTTP_METHODS.contains(&method_lower.as_str()) {
                continue;
            }
            if endpoints.len() >= MAX_ENDPOINTS {
                return Ok(ParsedSpec {
                    title,
                    version,
                    endpoints,
                });
            }
            let mut parameters = shared.clone();
            if let Some(own) = operation.get("parameters") {
                for p in parameter_list(own) {
                    if !parameters.contains(&p) {
                        parameters.push(p);
                    }
                }
            }
            endpoints.push(Endpoint {
                method: method_lower.to_ascii_uppercase(),
                path: path.clone(),
                summary: {
                    let summary = str_field(operation, "summary");
                    if summary.is_empty() {
                        truncate(&str_field(operation, "description"), 300)
                    } else {
                        truncate(&summary, 300)
                    }
                },
                operation_id: str_field(operation, "operationId"),
                parameters,
                request_body: request_body_types(operation),
                responses: operation
                    .get("responses")
                    .and_then(|r| r.as_object())
                    .map(|r| r.keys().cloned().collect())
                    .unwrap_or_default(),
                tags: operation
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|t| {
                        t.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
    }
    endpoints.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    Ok(ParsedSpec {
        title,
        version,
        endpoints,
    })
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    input.chars().take(max).collect()
}

/// `["path:id*", "query:limit"]` — location, name and a `*` for required.
/// `$ref` parameters carry no inline name, so they surface as `ref:<name>` and
/// stay visible instead of silently disappearing.
fn parameter_list(value: &Value) -> Vec<String> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        if let Some(reference) = item.get("$ref").and_then(|v| v.as_str()) {
            let name = reference.rsplit('/').next().unwrap_or(reference);
            out.push(format!("ref:{name}"));
            continue;
        }
        let name = str_field(item, "name");
        if name.is_empty() {
            continue;
        }
        let location = {
            let raw = str_field(item, "in");
            if raw.is_empty() {
                "query".to_string()
            } else {
                raw
            }
        };
        let required = item
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        out.push(format!(
            "{location}:{name}{}",
            if required { "*" } else { "" }
        ));
    }
    out
}

/// Content types of the request body. Covers OpenAPI 3 (`requestBody.content`)
/// and Swagger 2.0 (a `body` parameter plus a document-level `consumes`).
fn request_body_types(operation: &Value) -> Vec<String> {
    if let Some(content) = operation
        .get("requestBody")
        .and_then(|b| b.get("content"))
        .and_then(|c| c.as_object())
    {
        return content.keys().cloned().collect();
    }
    let has_body_param = operation
        .get("parameters")
        .and_then(|p| p.as_array())
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("in").and_then(|v| v.as_str()) == Some("body"))
        });
    if !has_body_param {
        return Vec::new();
    }
    operation
        .get("consumes")
        .and_then(|c| c.as_array())
        .map(|c| {
            c.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_else(|| vec!["application/json".to_string()])
}

/// Markdown digest ingested as a normal knowledge file: one heading per
/// endpoint, so the chunker produces endpoint-sized passages that a search for
/// "POST /orders" actually hits.
pub fn endpoints_markdown(spec: &ParsedSpec) -> String {
    let mut out = String::with_capacity(spec.endpoints.len() * 160);
    out.push_str(&format!(
        "# API: {} {}\n\n",
        if spec.title.is_empty() {
            "(bez nazwy)"
        } else {
            &spec.title
        },
        spec.version
    ));
    for endpoint in &spec.endpoints {
        out.push_str(&format!("## {} {}\n", endpoint.method, endpoint.path));
        if !endpoint.summary.is_empty() {
            out.push_str(&format!("{}\n", endpoint.summary));
        }
        if !endpoint.operation_id.is_empty() {
            out.push_str(&format!("operationId: {}\n", endpoint.operation_id));
        }
        if !endpoint.tags.is_empty() {
            out.push_str(&format!("Tagi: {}\n", endpoint.tags.join(", ")));
        }
        if !endpoint.parameters.is_empty() {
            out.push_str(&format!("Parametry: {}\n", endpoint.parameters.join(", ")));
        }
        if !endpoint.request_body.is_empty() {
            out.push_str(&format!(
                "Request body: {}\n",
                endpoint.request_body.join(", ")
            ));
        }
        if !endpoint.responses.is_empty() {
            out.push_str(&format!("Odpowiedzi: {}\n", endpoint.responses.join(", ")));
        }
        out.push('\n');
    }
    out
}

/// Settings key under which the parsed endpoint list of a source is cached in
/// `project.db`. Deliberately NOT a new table: the list is derived data that is
/// rebuilt on every ingest of the source, and schema v3 is fixed.
pub fn endpoints_setting_key(source_id: &str) -> String {
    format!("api_spec_endpoints:{source_id}")
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    const OPENAPI_YAML: &str = r#"
openapi: 3.0.3
info:
  title: Zamówienia
  version: "1.2"
paths:
  /orders:
    parameters:
      - name: tenant
        in: header
        required: true
    get:
      summary: Lista zamówień
      operationId: listOrders
      tags: [orders]
      parameters:
        - name: limit
          in: query
      responses:
        "200": {description: ok}
    post:
      summary: Nowe zamówienie
      requestBody:
        content:
          application/json: {schema: {type: object}}
      responses:
        "201": {description: created}
  /orders/{id}:
    delete:
      operationId: deleteOrder
      parameters:
        - name: id
          in: path
          required: true
      responses:
        "204": {description: gone}
"#;

    #[test]
    fn parses_yaml_openapi_into_endpoints() {
        let spec = parse_spec(OPENAPI_YAML.as_bytes()).expect("parse");
        assert_eq!(spec.title, "Zamówienia");
        assert_eq!(spec.version, "1.2");
        assert_eq!(spec.endpoints.len(), 3);

        let get = spec
            .endpoints
            .iter()
            .find(|e| e.method == "GET")
            .expect("GET /orders");
        assert_eq!(get.path, "/orders");
        assert_eq!(get.summary, "Lista zamówień");
        assert_eq!(get.operation_id, "listOrders");
        assert_eq!(get.tags, vec!["orders"]);
        assert_eq!(get.parameters, vec!["header:tenant*", "query:limit"]);
        assert_eq!(get.responses, vec!["200"]);

        let post = spec
            .endpoints
            .iter()
            .find(|e| e.method == "POST")
            .expect("POST /orders");
        assert_eq!(post.request_body, vec!["application/json"]);

        let delete = spec
            .endpoints
            .iter()
            .find(|e| e.method == "DELETE")
            .expect("DELETE");
        assert_eq!(delete.path, "/orders/{id}");
        assert_eq!(delete.parameters, vec!["path:id*"]);

        let md = endpoints_markdown(&spec);
        assert!(md.contains("## GET /orders"));
        assert!(md.contains("## DELETE /orders/{id}"));
    }

    #[test]
    fn parses_swagger_2_json_including_body_parameters() {
        let json = serde_json::json!({
            "swagger": "2.0",
            "info": {"title": "Legacy", "version": "1"},
            "paths": {
                "/login": {
                    "post": {
                        "summary": "Logowanie",
                        "consumes": ["application/x-www-form-urlencoded"],
                        "parameters": [
                            {"name": "body", "in": "body", "required": true},
                            {"$ref": "#/parameters/TraceId"}
                        ],
                        "responses": {"200": {}, "401": {}}
                    },
                    "x-internal": {"note": "not a method"}
                }
            }
        })
        .to_string();
        let spec = parse_spec(json.as_bytes()).expect("parse");
        assert_eq!(spec.endpoints.len(), 1, "x-* keys are not HTTP methods");
        let login = &spec.endpoints[0];
        assert_eq!(login.method, "POST");
        assert_eq!(
            login.request_body,
            vec!["application/x-www-form-urlencoded"]
        );
        assert_eq!(login.parameters, vec!["body:body*", "ref:TraceId"]);
        assert_eq!(login.responses, vec!["200", "401"]);
    }

    #[test]
    fn rejects_documents_without_paths() {
        assert!(parse_spec(b"{\"info\":{\"title\":\"x\"}}").is_err());
        assert!(parse_spec(b"\x00\x01binary").is_err());
        assert!(parse_spec(b": : not: yaml: [").is_err());
    }
}
