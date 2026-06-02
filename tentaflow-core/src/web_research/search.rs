// =============================================================================
// Plik: web_research/search.rs
// Opis: Configurable search providers for web research. Providers return URLs
//       and snippets only; page reading is handled by reader.rs.
// =============================================================================

use std::time::Duration;

use regex::Regex;
use reqwest::blocking::Client;
use serde_json::Value;
use url::Url;

use super::error::{Result, WebResearchError};
use super::extract::decode_html_entities;
use super::security::{resolve_public_addrs, validate_public_http_url};
use super::types::{SearchProviderConfig, SearchRequest, SearchResponse, SearchResult};

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

pub fn search(request: &SearchRequest) -> Result<SearchResponse> {
    let limit = request.limit.clamp(1, 50);
    let provider = request.provider.as_ref().ok_or_else(|| {
        WebResearchError::InvalidRequest(
            "search provider is required: searxng, duckduckgo, brave or tavily".to_string(),
        )
    })?;
    match provider {
        SearchProviderConfig::Searxng { base_url, internal } => {
            search_searxng(request, base_url, *internal, limit)
        }
        SearchProviderConfig::Brave { endpoint, api_key } => {
            search_brave(request, endpoint.as_deref(), api_key, limit)
        }
        SearchProviderConfig::Tavily {
            endpoint,
            api_key,
            search_depth,
        } => search_tavily(
            request,
            endpoint.as_deref(),
            api_key,
            search_depth.as_deref(),
            limit,
        ),
        SearchProviderConfig::Duckduckgo { endpoint } => {
            search_duckduckgo(request, endpoint.as_deref(), limit)
        }
    }
}

fn search_searxng(
    request: &SearchRequest,
    base_url: &str,
    internal: bool,
    limit: usize,
) -> Result<SearchResponse> {
    let base = if internal {
        validate_internal_service_url(base_url)?
    } else {
        validate_public_http_url(base_url)?
    };
    let mut url = base
        .join("/search")
        .map_err(|e| WebResearchError::InvalidRequest(format!("invalid searxng url: {}", e)))?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &request.query);
        qp.append_pair("format", "json");
        qp.append_pair("pageno", "1");
        if let Some(language) = request.language.as_deref() {
            qp.append_pair("language", language);
        }
        if let Some(time_range) = request.time_range.as_deref() {
            qp.append_pair("time_range", time_range);
        }
    }
    let json = get_json(url, None, internal)?;
    let results = json
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| WebResearchError::SearchProvider("searxng results missing".to_string()))?;
    let out = results
        .iter()
        .take(limit)
        .enumerate()
        .filter_map(|(idx, item)| {
            let url = item.get("url").and_then(Value::as_str)?.to_string();
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let snippet = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(SearchResult {
                title,
                url,
                snippet,
                source: "searxng".to_string(),
                rank: idx + 1,
            })
        })
        .collect();
    Ok(SearchResponse {
        query: request.query.clone(),
        provider: "searxng".to_string(),
        results: dedupe_results(out),
    })
}

fn search_brave(
    request: &SearchRequest,
    endpoint: Option<&str>,
    api_key: &str,
    limit: usize,
) -> Result<SearchResponse> {
    if api_key.trim().is_empty() {
        return Err(WebResearchError::InvalidRequest(
            "brave api_key is required".to_string(),
        ));
    }
    let endpoint = endpoint.unwrap_or("https://api.search.brave.com/res/v1/web/search");
    let mut url = validate_public_http_url(endpoint)?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &request.query);
        qp.append_pair("count", &limit.to_string());
        if let Some(language) = request.language.as_deref() {
            qp.append_pair("search_lang", language);
        }
    }
    let json = get_json(url, Some(api_key), false)?;
    let results = json
        .get("web")
        .and_then(|v| v.get("results"))
        .and_then(Value::as_array)
        .ok_or_else(|| WebResearchError::SearchProvider("brave web results missing".to_string()))?;
    let out = results
        .iter()
        .take(limit)
        .enumerate()
        .filter_map(|(idx, item)| {
            let url = item.get("url").and_then(Value::as_str)?.to_string();
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let snippet = item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(SearchResult {
                title,
                url,
                snippet,
                source: "brave".to_string(),
                rank: idx + 1,
            })
        })
        .collect();
    Ok(SearchResponse {
        query: request.query.clone(),
        provider: "brave".to_string(),
        results: dedupe_results(out),
    })
}

fn search_tavily(
    request: &SearchRequest,
    endpoint: Option<&str>,
    api_key: &str,
    search_depth: Option<&str>,
    limit: usize,
) -> Result<SearchResponse> {
    if api_key.trim().is_empty() {
        return Err(WebResearchError::InvalidRequest(
            "tavily api_key is required".to_string(),
        ));
    }
    let endpoint = endpoint.unwrap_or("https://api.tavily.com/search");
    let url = validate_public_http_url(endpoint)?;
    let body = serde_json::json!({
        "api_key": api_key,
        "query": request.query,
        "max_results": limit,
        "search_depth": search_depth.unwrap_or("basic"),
        "include_answer": false,
        "include_raw_content": false,
    });
    let json = post_json(url, &body)?;
    let results = json
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| WebResearchError::SearchProvider("tavily results missing".to_string()))?;
    let out = results
        .iter()
        .take(limit)
        .enumerate()
        .filter_map(|(idx, item)| {
            let url = item.get("url").and_then(Value::as_str)?.to_string();
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let snippet = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(SearchResult {
                title,
                url,
                snippet,
                source: "tavily".to_string(),
                rank: idx + 1,
            })
        })
        .collect();
    Ok(SearchResponse {
        query: request.query.clone(),
        provider: "tavily".to_string(),
        results: dedupe_results(out),
    })
}

fn search_duckduckgo(
    request: &SearchRequest,
    endpoint: Option<&str>,
    limit: usize,
) -> Result<SearchResponse> {
    let endpoint = endpoint.unwrap_or("https://html.duckduckgo.com/html/");
    let mut url = validate_public_http_url(endpoint)?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &request.query);
        if let Some(language) = request.language.as_deref() {
            qp.append_pair("kl", language);
        }
        if let Some(time_range) = request.time_range.as_deref() {
            qp.append_pair("df", time_range);
        }
    }
    let html = get_text(url, "text/html,application/xhtml+xml")?;
    let results = parse_duckduckgo_results(&html, limit)?;
    Ok(SearchResponse {
        query: request.query.clone(),
        provider: "duckduckgo".to_string(),
        results: dedupe_results(results),
    })
}

fn validate_internal_service_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw)
        .map_err(|e| WebResearchError::InvalidRequest(format!("invalid url: {}", e)))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(WebResearchError::PolicyDenied(
            "only http and https urls are allowed".to_string(),
        ));
    }
    match url.host_str() {
        Some("127.0.0.1") | Some("localhost") | Some("::1") => Ok(url),
        _ => Err(WebResearchError::PolicyDenied(
            "internal search provider must be a local TentaFlow service".to_string(),
        )),
    }
}

fn get_text(url: Url, accept: &str) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?
        .to_string();
    let addrs = resolve_public_addrs(&url)?;
    let client = Client::builder()
        .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS))
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|e| WebResearchError::Http(format!("client build failed: {}", e)))?;
    let response = client
        .get(url)
        .header("User-Agent", "TentaFlow-WebResearch/1.0")
        .header("Accept", accept)
        .send()
        .map_err(|e| WebResearchError::Http(e.to_string()))?;
    if !response.status().is_success() {
        return Err(WebResearchError::SearchProvider(format!(
            "provider returned {}",
            response.status()
        )));
    }
    response
        .text()
        .map_err(|e| WebResearchError::SearchProvider(format!("invalid text response: {}", e)))
}

fn get_json(url: Url, brave_key: Option<&str>, internal: bool) -> Result<Value> {
    let host = url
        .host_str()
        .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?
        .to_string();
    let mut builder = Client::builder().timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS));
    if !internal {
        let addrs = resolve_public_addrs(&url)?;
        builder = builder.resolve_to_addrs(&host, &addrs);
    }
    let client = builder
        .build()
        .map_err(|e| WebResearchError::Http(format!("client build failed: {}", e)))?;
    let mut req = client
        .get(url)
        .header("User-Agent", "TentaFlow-WebResearch/1.0")
        .header("Accept", "application/json");
    if let Some(key) = brave_key {
        req = req.header("X-Subscription-Token", key);
    }
    let response = req
        .send()
        .map_err(|e| WebResearchError::Http(e.to_string()))?;
    if !response.status().is_success() {
        return Err(WebResearchError::SearchProvider(format!(
            "provider returned {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .map_err(|e| WebResearchError::SearchProvider(format!("invalid json: {}", e)))
}

fn post_json(url: Url, body: &Value) -> Result<Value> {
    let host = url
        .host_str()
        .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?
        .to_string();
    let addrs = resolve_public_addrs(&url)?;
    let client = Client::builder()
        .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS))
        .resolve_to_addrs(&host, &addrs)
        .build()
        .map_err(|e| WebResearchError::Http(format!("client build failed: {}", e)))?;
    let response = client
        .post(url)
        .header("User-Agent", "TentaFlow-WebResearch/1.0")
        .header("Accept", "application/json")
        .json(body)
        .send()
        .map_err(|e| WebResearchError::Http(e.to_string()))?;
    if !response.status().is_success() {
        return Err(WebResearchError::SearchProvider(format!(
            "provider returned {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .map_err(|e| WebResearchError::SearchProvider(format!("invalid json: {}", e)))
}

fn dedupe_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for result in results {
        if !out
            .iter()
            .any(|existing: &SearchResult| existing.url == result.url)
        {
            out.push(result);
        }
    }
    out
}

fn parse_duckduckgo_results(html: &str, limit: usize) -> Result<Vec<SearchResult>> {
    let anchor_re = Regex::new(r#"(?is)<a\b[^>]*href=["']([^"']+)["'][^>]*>(.*?)</a>"#)
        .map_err(|e| WebResearchError::SearchProvider(format!("search parser: {}", e)))?;
    let snippet_re =
        Regex::new(r#"(?is)<a\b[^>]*class=["'][^"']*result__snippet[^"']*["'][^>]*>(.*?)</a>"#)
            .map_err(|e| WebResearchError::SearchProvider(format!("search parser: {}", e)))?;
    let snippets = snippet_re
        .captures_iter(html)
        .map(|caps| clean_html_fragment(caps.get(1).map(|m| m.as_str()).unwrap_or("")))
        .collect::<Vec<_>>();
    let mut results = Vec::new();

    for caps in anchor_re.captures_iter(html) {
        let raw_href = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let Some(url) = normalize_duckduckgo_url(raw_href) else {
            continue;
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        let title = clean_html_fragment(caps.get(2).map(|m| m.as_str()).unwrap_or(""));
        if title.is_empty() || title.eq_ignore_ascii_case("cached") {
            continue;
        }
        let rank = results.len() + 1;
        let snippet = snippets.get(results.len()).cloned().unwrap_or_default();
        results.push(SearchResult {
            title,
            url,
            snippet,
            source: "duckduckgo".to_string(),
            rank,
        });
        if results.len() >= limit {
            break;
        }
    }

    if results.is_empty() {
        return Err(WebResearchError::SearchProvider(
            "duckduckgo results missing".to_string(),
        ));
    }
    Ok(results)
}

fn normalize_duckduckgo_url(raw_href: &str) -> Option<String> {
    let decoded_href = decode_html_entities(raw_href);
    let parse_target = if decoded_href.starts_with("//") {
        format!("https:{}", decoded_href)
    } else {
        decoded_href
    };
    if let Ok(url) = Url::parse(&parse_target) {
        if let Some(uddg) = url
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned())
        {
            return Some(uddg);
        }
        return Some(url.to_string());
    }
    None
}

fn clean_html_fragment(fragment: &str) -> String {
    let without_tags = Regex::new(r"(?is)<[^>]+>")
        .map(|tag_re| tag_re.replace_all(fragment, " ").into_owned())
        .unwrap_or_else(|_| fragment.to_string());
    decode_html_entities(&without_tags)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searxng_provider_defaults_to_public_mode() {
        let provider: SearchProviderConfig =
            serde_json::from_str(r#"{"kind":"searxng","base_url":"https://search.example"}"#)
                .unwrap();

        match provider {
            SearchProviderConfig::Searxng { internal, .. } => assert!(!internal),
            _ => panic!("expected searxng provider"),
        }
    }

    #[test]
    fn searxng_provider_json_cannot_enable_internal_mode() {
        let provider: SearchProviderConfig = serde_json::from_str(
            r#"{"kind":"searxng","base_url":"http://127.0.0.1:8080","internal":true}"#,
        )
        .unwrap();

        match provider {
            SearchProviderConfig::Searxng { internal, .. } => assert!(!internal),
            _ => panic!("expected searxng provider"),
        }
    }

    #[test]
    fn internal_service_url_accepts_loopback_only() {
        assert!(validate_internal_service_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_internal_service_url("http://localhost:8080").is_ok());
        assert!(validate_internal_service_url("http://10.0.0.2:8080").is_err());
        assert!(validate_internal_service_url("https://example.com").is_err());
    }

    #[test]
    fn duckduckgo_provider_can_be_deserialized() {
        let provider: SearchProviderConfig =
            serde_json::from_str(r#"{"kind":"duckduckgo"}"#).unwrap();

        match provider {
            SearchProviderConfig::Duckduckgo { endpoint } => assert!(endpoint.is_none()),
            _ => panic!("expected duckduckgo provider"),
        }
    }

    #[test]
    fn duckduckgo_parser_extracts_result_links() {
        let html = r#"
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=abc">
                Rust Programming Language
            </a>
            <a class="result__snippet">A language empowering everyone to build reliable software.</a>
        "#;

        let results = parse_duckduckgo_results(html, 10).unwrap();

        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].title, "Rust Programming Language");
        assert!(results[0].snippet.contains("reliable software"));
    }
}
