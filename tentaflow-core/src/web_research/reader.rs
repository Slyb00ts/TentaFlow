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
    let (final_url, status, content_type, body) = fetch_with_redirects(
        start_url,
        MAX_BODY_BYTES,
        "text/html,text/plain,application/xhtml+xml",
        ContentTypeGate::Readable,
    )?;
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

/// Which content types a fetch will accept. The page reader only takes
/// human-readable HTML/text (it runs readability afterwards); the skills hub
/// also needs to fetch the GitHub Contents API, which returns JSON.
#[derive(Clone, Copy)]
enum ContentTypeGate {
    Readable,
    ReadableOrJson,
}

/// Fetches a public URL through the same SSRF guard the page reader uses —
/// DNS pinning (`resolve_public_addrs`), per-hop redirect revalidation
/// (`validate_public_http_url`) and a hard body-size cap — but returns the raw
/// body. The skills hub reuses this for the GitHub Contents API and raw
/// SKILL.md fetches instead of standing up a second HTTP client that would have
/// to re-implement the guard. `body_cap` lets the caller pick a smaller ceiling
/// than the reader's 8 MiB; `accept` is sent verbatim as the Accept header.
pub fn fetch_raw_public_url(url: &str, body_cap: u64, accept: &str) -> Result<(String, String)> {
    let start_url = validate_public_http_url(url)?;
    let (_final_url, _status, content_type, body) =
        fetch_with_redirects(start_url, body_cap, accept, ContentTypeGate::ReadableOrJson)?;
    Ok((content_type, body))
}

fn fetch_with_redirects(
    mut url: Url,
    body_cap: u64,
    accept: &str,
    gate: ContentTypeGate,
) -> Result<(Url, StatusCode, String, String)> {
    for _ in 0..=MAX_REDIRECTS {
        let addrs = resolve_public_addrs(&url)?;
        let host = url
            .host_str()
            .ok_or_else(|| WebResearchError::PolicyDenied("url has no host".to_string()))?
            .to_string();
        let client = Client::builder()
        .user_agent(crate::web_research::types::WEB_RESEARCH_USER_AGENT)
            .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS))
            .redirect(Policy::none())
            .resolve_to_addrs(&host, &addrs)
            .build()
            .map_err(|e| WebResearchError::Http(format!("client build failed: {}", e)))?;
        let response = client
            .get(url.clone())
            .header("User-Agent", "TentaFlow-WebResearch/1.0")
            .header("Accept", accept)
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
        match gate {
            ContentTypeGate::Readable => validate_readable_content_type(&content_type)?,
            ContentTypeGate::ReadableOrJson => {
                validate_readable_or_json_content_type(&content_type)?
            }
        }
        let mut limited = Vec::new();
        response
            .take(body_cap + 1)
            .read_to_end(&mut limited)
            .map_err(|e| WebResearchError::Http(format!("body read failed: {}", e)))?;
        if limited.len() as u64 > body_cap {
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

fn validate_readable_or_json_content_type(content_type: &str) -> Result<()> {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("application/json")
        || ct.contains("application/vnd.github")
        || ct.contains("text/markdown")
    {
        return Ok(());
    }
    validate_readable_content_type(content_type)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_fetch_rejects_private_and_non_https_targets() {
        // The skills hub reuses this for GitHub/SKILL.md fetches — the SSRF guard
        // must deny loopback, link-local and non-http schemes before any socket.
        assert!(fetch_raw_public_url("http://127.0.0.1/x", 1024, "application/json").is_err());
        assert!(fetch_raw_public_url("http://localhost/x", 1024, "application/json").is_err());
        assert!(
            fetch_raw_public_url("http://169.254.169.254/latest", 1024, "application/json")
                .is_err()
        );
        assert!(fetch_raw_public_url("file:///etc/passwd", 1024, "text/plain").is_err());
    }
}
