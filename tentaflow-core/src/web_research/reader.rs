// =============================================================================
// Plik: web_research/reader.rs
// Opis: Public web page reader with redirect revalidation, DNS pinning, content
//       size limits and generic text extraction.
// =============================================================================

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::StatusCode;
use url::Url;

use super::error::{Result, WebResearchError};
use super::extract::extract_content;
use super::security::{resolve_public_addrs, validate_public_http_url};
use super::types::{ExtractionInfo, ReadPageResult, ReadUrlRequest};

const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const DEFAULT_TIMEOUT_MS: u64 = 20_000;

pub fn read_url(request: &ReadUrlRequest) -> Result<ReadPageResult> {
    let max_chars = request.max_chars.clamp(500, 200_000);
    let start_url = validate_public_http_url(&request.url)?;
    let (final_url, status, content_type, body) = fetch_with_redirects(start_url)?;
    let extracted = extract_content(&body, &content_type, &final_url)?;
    let original_len = extracted.text.chars().count();
    let text = truncate_chars(&extracted.text, max_chars);
    let excerpt = truncate_chars(&text, 900);
    let char_count = text.chars().count();
    let word_count = count_words(&text);

    Ok(ReadPageResult {
        url: request.url.clone(),
        final_url: final_url.to_string(),
        title: extracted.title,
        text,
        excerpt,
        status: status.as_u16(),
        content_type,
        fetched_at: unix_time(),
        extraction: ExtractionInfo {
            method: extracted.method,
            char_count,
            word_count,
            quality_score: extracted.quality_score,
            truncated: original_len > max_chars,
        },
    })
}

fn fetch_with_redirects(mut url: Url) -> Result<(Url, StatusCode, String, String)> {
    for _ in 0..=MAX_REDIRECTS {
        let addrs = resolve_public_addrs(&url)?;
        let host = url
            .host_str()
            .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?
            .to_string();
        let client = Client::builder()
            .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS))
            .redirect(Policy::none())
            .resolve_to_addrs(&host, &addrs)
            .build()
            .map_err(|e| WebResearchError::Http(format!("client build failed: {}", e)))?;
        let response = client
            .get(url.clone())
            .header("User-Agent", "TentaFlow-WebResearch/1.0")
            .header("Accept", "text/html,text/plain,application/xhtml+xml")
            .send()
            .map_err(|e| WebResearchError::Http(e.to_string()))?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| WebResearchError::Http("redirect without location".to_string()))?;
            url = url
                .join(location)
                .map_err(|e| WebResearchError::Http(format!("invalid redirect: {}", e)))?;
            validate_public_http_url(url.as_str())?;
            continue;
        }

        let status = response.status();
        if !status.is_success() {
            return Err(WebResearchError::Http(format!("http status {}", status)));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        validate_readable_content_type(&content_type)?;
        let mut limited = Vec::new();
        response
            .take(MAX_BODY_BYTES + 1)
            .read_to_end(&mut limited)
            .map_err(|e| WebResearchError::Http(format!("body read failed: {}", e)))?;
        if limited.len() as u64 > MAX_BODY_BYTES {
            return Err(WebResearchError::Http(
                "response body too large".to_string(),
            ));
        }
        let body = String::from_utf8_lossy(&limited).to_string();
        return Ok((url, status, content_type, body));
    }

    Err(WebResearchError::Http("too many redirects".to_string()))
}

fn validate_readable_content_type(content_type: &str) -> Result<()> {
    let ct = content_type.to_ascii_lowercase();
    if ct.is_empty()
        || ct.contains("text/html")
        || ct.contains("text/plain")
        || ct.contains("application/xhtml+xml")
        || ct.contains("application/xml")
        || ct.contains("text/xml")
    {
        return Ok(());
    }
    Err(WebResearchError::Extraction(format!(
        "unsupported content type: {}",
        content_type
    )))
}

pub(crate) fn truncate_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

pub(crate) fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(|ch| ch.is_alphanumeric()))
        .count()
}

pub(crate) fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
