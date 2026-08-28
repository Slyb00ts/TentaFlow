// =============================================================================
// Plik: web_research/browser_renderer.rs
// Opis: Client for the Browser Renderer service used to read JS-heavy pages via
//       Playwright Chromium while returning the standard web research shape.
// =============================================================================

use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use super::error::{Result, WebResearchError};
use super::reader::{count_words, truncate_chars, unix_time};
use super::security::validate_public_http_url;
use super::types::{ExtractionInfo, ReadPageResult, ReadUrlRequest};

const DEFAULT_TIMEOUT_MS: u64 = 45_000;
const DEFAULT_WAIT_UNTIL: &str = "domcontentloaded";
const DEFAULT_SETTLE_MS: u64 = 500;
const DEFAULT_SCROLLS: u8 = 3;
const DEFAULT_VIEWPORT_WIDTH: u16 = 1365;
const DEFAULT_VIEWPORT_HEIGHT: u16 = 768;

#[derive(Debug, Serialize)]
struct RenderRequest<'a> {
    url: &'a str,
    user_id: &'a str,
    wait_until: &'static str,
    timeout_ms: u64,
    settle_ms: u64,
    max_scrolls: u8,
    viewport_width: u16,
    viewport_height: u16,
    include_html: bool,
    include_screenshot: bool,
    reset_context: bool,
}

#[derive(Debug, Deserialize)]
struct RenderResponse {
    final_url: String,
    status: u16,
    title: String,
    text: String,
    /// Rendered DOM. Asked for so the SAME readability pass the static reader
    /// uses can run here too; without it the browser path returned raw page
    /// text — navigation menus, footers and cookie banners included.
    #[serde(default)]
    html: Option<String>,
}

pub fn read_url(endpoint: &str, request: &ReadUrlRequest) -> Result<ReadPageResult> {
    let max_chars = request.max_chars.clamp(500, 200_000);
    let start_url = validate_public_http_url(&request.url)?;
    let user_id = request.user_id.as_deref().unwrap_or("default");
    let render_url = render_url(endpoint)?;
    let client = Client::builder()
        .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS + 5_000))
        .build()
        .map_err(|e| WebResearchError::Http(format!("browser client build failed: {}", e)))?;
    let payload = RenderRequest {
        url: start_url.as_str(),
        user_id,
        wait_until: DEFAULT_WAIT_UNTIL,
        timeout_ms: DEFAULT_TIMEOUT_MS,
        settle_ms: DEFAULT_SETTLE_MS,
        max_scrolls: DEFAULT_SCROLLS,
        viewport_width: DEFAULT_VIEWPORT_WIDTH,
        viewport_height: DEFAULT_VIEWPORT_HEIGHT,
        include_html: true,
        include_screenshot: false,
        reset_context: false,
    };
    let response = client
        .post(render_url)
        .json(&payload)
        .send()
        .map_err(|e| WebResearchError::Http(format!("browser render failed: {}", e)))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(WebResearchError::Http(format!(
            "browser renderer status {}: {}",
            status,
            truncate_chars(&body, 500)
        )));
    }
    let rendered: RenderResponse = response
        .json()
        .map_err(|e| WebResearchError::Serialization(format!("browser render json: {}", e)))?;
    validate_public_http_url(&rendered.final_url)?;
    // Readability over the rendered DOM, exactly as the static reader does over
    // a fetched document. Taking the renderer's plain text verbatim meant the
    // first characters of a page were its navigation: apple.com came back as
    // "Store Shop Mac iPad iPhone…" and a research finding held a menu instead
    // of a specification.
    let final_url_parsed = Url::parse(&rendered.final_url).ok();
    let extracted = rendered
        .html
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .zip(final_url_parsed.as_ref())
        .and_then(|(html, url)| super::extract::extract_content(html, "text/html", url).ok());

    let (source_text, method, quality) = match extracted {
        Some(e) if !e.text.trim().is_empty() => (
            e.text,
            format!("browser-renderer+{}", e.method),
            e.quality_score,
        ),
        // No usable extraction: the raw text is worse but it is what there is,
        // and an empty page would look like a read failure it is not.
        _ => (
            rendered.text.clone(),
            "browser-renderer".to_string(),
            browser_quality_score(
                rendered.text.chars().count(),
                count_words(&rendered.text),
            ),
        ),
    };

    let original_len = source_text.chars().count();
    let text = truncate_chars(&source_text, max_chars);
    let excerpt = truncate_chars(&text, 900);
    let char_count = text.chars().count();
    let word_count = count_words(&text);

    Ok(ReadPageResult {
        url: request.url.clone(),
        final_url: rendered.final_url,
        title: rendered.title,
        text,
        excerpt,
        status: rendered.status,
        content_type: "text/html; rendered=browser".to_string(),
        fetched_at: unix_time(),
        extraction: ExtractionInfo {
            method,
            char_count,
            word_count,
            quality_score: quality,
            truncated: original_len > max_chars,
        },
    })
}

fn render_url(endpoint: &str) -> Result<Url> {
    let mut base = endpoint.trim().trim_end_matches('/').to_string();
    base.push('/');
    let url = Url::parse(&base).map_err(|e| {
        WebResearchError::InvalidRequest(format!("invalid browser endpoint: {}", e))
    })?;
    url.join("render")
        .map_err(|e| WebResearchError::InvalidRequest(format!("invalid browser render url: {}", e)))
}

fn browser_quality_score(char_count: usize, word_count: usize) -> f32 {
    if char_count < 200 || word_count < 30 {
        return 0.2;
    }
    if char_count > 5_000 && word_count > 600 {
        return 0.95;
    }
    if char_count > 1_500 && word_count > 180 {
        return 0.8;
    }
    0.55
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn render_url_appends_render_path() {
        let url = render_url("http://127.0.0.1:8092").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8092/render");
    }

    #[test]
    fn browser_quality_score_rates_empty_pages_low() {
        assert!(browser_quality_score(50, 5) < 0.3);
        assert!(browser_quality_score(6_000, 800) > 0.9);
    }

    #[test]
    fn read_url_posts_to_renderer_and_maps_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let len = stream.read(&mut request).unwrap();
            let body = String::from_utf8_lossy(&request[..len]);
            assert!(body.contains("POST /render HTTP/1.1"));
            assert!(body.contains("\"user_id\":\"alice\""));
            let response_body = r#"{
                "final_url":"http://93.184.216.34/",
                "status":200,
                "title":"Rendered",
                "text":"Rendered text from Chromium with enough words to pass quality scoring."
            }"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let page = read_url(
            &endpoint,
            &ReadUrlRequest {
                url: "http://93.184.216.34/".to_string(),
                max_chars: 10_000,
                mode: super::super::types::ReadMode::Browser,
                user_id: Some("alice".to_string()),
            },
        )
        .unwrap();

        handle.join().unwrap();
        assert_eq!(page.final_url, "http://93.184.216.34/");
        assert_eq!(page.title, "Rendered");
        assert_eq!(page.extraction.method, "browser-renderer");
        assert!(page.text.contains("Rendered text from Chromium"));
    }
}
