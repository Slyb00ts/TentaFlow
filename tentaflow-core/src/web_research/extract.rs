// =============================================================================
// Plik: web_research/extract.rs
// Opis: Generic HTML and text extraction utilities for web research page reads.
// =============================================================================

use std::io::Cursor;

use url::Url;

use super::error::{Result, WebResearchError};

#[derive(Debug, Clone)]
pub struct ExtractedContent {
    pub title: String,
    pub text: String,
    pub method: String,
    pub word_count: usize,
    pub quality_score: f32,
}

#[derive(Debug, Clone)]
struct ExtractionCandidate {
    title: String,
    text: String,
    method: String,
    quality_score: f32,
}

const MIN_USEFUL_CHARS: usize = 40;
const STRONG_READABILITY_CHARS: usize = 350;

pub fn extract_content(body: &str, content_type: &str, page_url: &Url) -> Result<ExtractedContent> {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("text/plain") || looks_like_plain_text(body) {
        let text = normalize_lines(body);
        if text.is_empty() {
            return Err(WebResearchError::Extraction(
                "empty text response".to_string(),
            ));
        }
        return Ok(ExtractedContent {
            title: String::new(),
            word_count: count_words(&text),
            quality_score: quality_score(&text, 0.0, false),
            method: "plain_text".to_string(),
            text,
        });
    }

    let mut candidates = Vec::new();
    if let Some(candidate) = readability_candidate(body, page_url) {
        candidates.push(candidate);
    }
    candidates.extend(html_candidates(body));

    let candidate = candidates
        .into_iter()
        .filter(|candidate| candidate.text.chars().count() >= MIN_USEFUL_CHARS)
        .max_by(|a, b| a.quality_score.total_cmp(&b.quality_score))
        .ok_or_else(|| {
            WebResearchError::Extraction("no readable text found in html".to_string())
        })?;

    Ok(candidate.into_extracted())
}

fn looks_like_plain_text(body: &str) -> bool {
    let sample = body.chars().take(512).collect::<String>();
    !sample.contains('<') || !sample.to_ascii_lowercase().contains("<html")
}

fn readability_candidate(html: &str, page_url: &Url) -> Option<ExtractionCandidate> {
    let mut cursor = Cursor::new(html.as_bytes());
    let product = readability::extractor::extract(&mut cursor, page_url).ok()?;
    let text = normalize_lines(&decode_html_entities(&product.text));
    if text.chars().count() < MIN_USEFUL_CHARS {
        return None;
    }
    let title = first_non_empty([
        normalize_spaces(&decode_html_entities(&product.title)),
        extract_title(html),
        extract_meta_content(html, "og:title"),
    ]);
    let score = quality_score(&text, 0.0, text.chars().count() >= STRONG_READABILITY_CHARS) + 25.0;
    Some(ExtractionCandidate {
        title,
        text,
        method: "readability".to_string(),
        quality_score: score,
    })
}

fn html_candidates(html: &str) -> Vec<ExtractionCandidate> {
    let stripped = remove_tag_blocks(html, "script");
    let stripped = remove_tag_blocks(&stripped, "style");
    let stripped = remove_tag_blocks(&stripped, "noscript");
    let stripped = remove_tag_blocks(&stripped, "svg");
    let title = first_non_empty([
        extract_meta_content(&stripped, "og:title"),
        extract_title(&stripped),
        extract_first_heading(&stripped),
    ]);
    let mut out = candidate_blocks(&stripped)
        .into_iter()
        .filter_map(|block| candidate_from_block(title.clone(), block))
        .collect::<Vec<_>>();
    let body_text = normalize_lines(&decode_html_entities(&strip_tags_with_breaks(&stripped)));
    if !body_text.is_empty() {
        out.push(ExtractionCandidate {
            title,
            quality_score: quality_score(&body_text, link_density(&stripped), false),
            method: "html_body_fallback".to_string(),
            text: body_text,
        });
    }
    out
}

fn candidate_blocks(html: &str) -> Vec<HtmlBlock> {
    let mut out = Vec::new();
    for tag in [
        "article",
        "main",
        "section",
        "div",
        "body",
        "td",
        "pre",
        "blockquote",
    ] {
        out.extend(extract_tag_blocks(html, tag));
    }
    out
}

fn candidate_from_block(title: String, block: HtmlBlock) -> Option<ExtractionCandidate> {
    let text = normalize_lines(&decode_html_entities(&strip_tags_with_breaks(&block.html)));
    if text.chars().count() < MIN_USEFUL_CHARS {
        return None;
    }
    let class_weight = class_weight(&block.attrs);
    let score = quality_score(&text, link_density(&block.html), false) + class_weight;
    Some(ExtractionCandidate {
        title,
        text,
        method: format!("html_{}", block.tag),
        quality_score: score,
    })
}

fn quality_score(text: &str, link_density: f32, strong_article: bool) -> f32 {
    let chars = text.chars().count() as f32;
    let words = count_words(text) as f32;
    let punctuation = text
        .chars()
        .filter(|c| matches!(c, '.' | '!' | '?' | ':' | ';' | ','))
        .count() as f32;
    let line_count = text.lines().filter(|line| !line.trim().is_empty()).count() as f32;
    let article_bonus = if strong_article { 150.0 } else { 0.0 };
    chars * 0.8 + words * 8.0 + punctuation * 35.0 + line_count * 4.0 + article_bonus
        - link_density * 450.0
}

fn link_density(html: &str) -> f32 {
    let text_len = strip_tags_with_breaks(html).chars().count().max(1) as f32;
    let mut link_text_len = 0usize;
    for block in extract_tag_blocks(html, "a") {
        link_text_len += strip_tags_with_breaks(&block.html).chars().count();
    }
    (link_text_len as f32 / text_len).clamp(0.0, 1.0)
}

fn class_weight(attrs: &str) -> f32 {
    let attrs = attrs.to_ascii_lowercase();
    let positive = [
        "article", "body", "content", "entry", "main", "page", "post", "story", "text",
    ]
    .iter()
    .filter(|token| attrs.contains(**token))
    .count() as f32;
    let negative = [
        "ad",
        "banner",
        "breadcrumb",
        "comment",
        "combx",
        "footer",
        "header",
        "menu",
        "meta",
        "nav",
        "promo",
        "related",
        "share",
        "sidebar",
        "sponsor",
    ]
    .iter()
    .filter(|token| attrs.contains(**token))
    .count() as f32;
    positive * 90.0 - negative * 130.0
}

#[derive(Debug, Clone)]
struct HtmlBlock {
    tag: String,
    attrs: String,
    html: String,
}

fn extract_tag_blocks(html: &str, tag: &str) -> Vec<HtmlBlock> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut blocks = Vec::new();
    let mut pos = 0;

    while let Some(start_rel) = lower[pos..].find(&open) {
        let start = pos + start_rel;
        let open_end = match lower[start..].find('>') {
            Some(v) => start + v + 1,
            None => break,
        };
        let end = match lower[open_end..].find(&close) {
            Some(v) => open_end + v + close.len(),
            None => break,
        };
        blocks.push(HtmlBlock {
            tag: tag.to_string(),
            attrs: html[start..open_end].to_string(),
            html: html[start..end].to_string(),
        });
        pos = end;
    }

    blocks
}

fn remove_tag_blocks(html: &str, tag: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut pos = 0;

    while let Some(start_rel) = lower[pos..].find(&open) {
        let start = pos + start_rel;
        out.push_str(&html[pos..start]);
        let after_start = match lower[start..].find(&close) {
            Some(end_rel) => start + end_rel + close.len(),
            None => html.len(),
        };
        pos = after_start;
    }
    out.push_str(&html[pos..]);
    out
}

fn strip_tags_with_breaks(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut inside = false;
    let mut tag = String::new();

    for ch in input.chars() {
        match ch {
            '<' => {
                inside = true;
                tag.clear();
            }
            '>' if inside => {
                inside = false;
                let tag_name = tag
                    .trim()
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if matches!(
                    tag_name,
                    "p" | "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "article" | "section"
                ) {
                    out.push('\n');
                }
            }
            _ if inside => tag.push(ch),
            _ => out.push(ch),
        }
    }

    out
}

fn extract_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let start = match lower.find("<title") {
        Some(pos) => pos,
        None => return String::new(),
    };
    let after_open = match html[start..].find('>') {
        Some(pos) => start + pos + 1,
        None => return String::new(),
    };
    let end = lower[after_open..]
        .find("</title>")
        .map(|pos| after_open + pos)
        .unwrap_or(after_open);
    normalize_spaces(&decode_html_entities(&html[after_open..end]))
}

fn extract_first_heading(html: &str) -> String {
    for tag in ["h1", "h2"] {
        if let Some(block) = extract_tag_blocks(html, tag).into_iter().next() {
            let text = normalize_lines(&decode_html_entities(&strip_tags_with_breaks(&block.html)));
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

fn extract_meta_content(html: &str, property: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let property = property.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(start_rel) = lower[pos..].find("<meta") {
        let start = pos + start_rel;
        let end = match lower[start..].find('>') {
            Some(v) => start + v + 1,
            None => break,
        };
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        if (tag_lower.contains(&format!("property=\"{}\"", property))
            || tag_lower.contains(&format!("property='{}'", property))
            || tag_lower.contains(&format!("name=\"{}\"", property))
            || tag_lower.contains(&format!("name='{}'", property)))
            && tag_lower.contains("content=")
        {
            if let Some(content) = extract_attr_value(tag, "content") {
                return normalize_spaces(&decode_html_entities(&content));
            }
        }
        pos = end;
    }
    String::new()
}

fn extract_attr_value(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pattern = format!("{}=", attr);
    let start = lower.find(&pattern)? + pattern.len();
    let rest = tag[start..].trim_start();
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let value = &rest[quote.len_utf8()..];
        let end = value.find(quote)?;
        return Some(value[..end].to_string());
    }
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches('>').to_string())
}

fn first_non_empty(values: impl IntoIterator<Item = String>) -> String {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(|ch| ch.is_alphanumeric()))
        .count()
}

impl ExtractionCandidate {
    fn into_extracted(self) -> ExtractedContent {
        let word_count = count_words(&self.text);
        ExtractedContent {
            title: self.title,
            text: self.text,
            method: self.method,
            word_count,
            quality_score: self.quality_score,
        }
    }
}

pub fn decode_html_entities(input: &str) -> String {
    let mut out = input
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ");

    out = decode_numeric_entities(&out);
    out
}

fn decode_numeric_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("&#") {
        out.push_str(&rest[..start]);
        let entity = &rest[start + 2..];
        if let Some(end) = entity.find(';') {
            let number = &entity[..end];
            let decoded = if let Some(hex) = number
                .strip_prefix('x')
                .or_else(|| number.strip_prefix('X'))
            {
                u32::from_str_radix(hex, 16).ok()
            } else {
                number.parse::<u32>().ok()
            };
            if let Some(ch) = decoded.and_then(char::from_u32) {
                out.push(ch);
                rest = &entity[end + 1..];
                continue;
            }
        }
        out.push_str("&#");
        rest = entity;
    }
    out.push_str(rest);
    out
}

fn normalize_spaces(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_lines(input: &str) -> String {
    let mut out = String::new();
    for line in input.lines() {
        let normalized = normalize_spaces(line);
        if !normalized.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&normalized);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_text_over_navigation() {
        let html = r#"
            <html><head><title>Example Article</title></head>
            <body>
              <nav><a href="/">Home</a><a href="/x">Archive</a></nav>
              <article>
                <h1>Example Article</h1>
                <p>This is the first paragraph with enough useful text for scoring.</p>
                <p>This is the second paragraph, with facts, dates, and context.</p>
              </article>
            </body></html>
        "#;

        let extracted = extract_content(html, "text/html", &test_url()).unwrap();

        assert!(extracted.text.contains("second paragraph"));
        assert!(!extracted.text.contains("Archive Home"));
    }

    #[test]
    fn removes_script_and_style_content() {
        let html = r#"
            <html><body>
              <script>window.secret = "not content";</script>
              <style>.x { color: red; }</style>
              <main><p>Visible content with a complete sentence for extraction.</p></main>
            </body></html>
        "#;

        let extracted = extract_content(html, "text/html", &test_url()).unwrap();

        assert!(!extracted.text.contains("window.secret"));
    }

    #[test]
    fn extracts_main_documentation_layout() {
        let html = r#"
            <html><body>
              <aside><a>Install</a><a>API</a><a>FAQ</a></aside>
              <main>
                <h1>Configuration Reference</h1>
                <p>The configuration file controls the runtime, search providers and extraction limits.</p>
                <pre><code>provider = "searxng"</code></pre>
                <p>Values are validated before a request leaves the process.</p>
              </main>
            </body></html>
        "#;

        let extracted = extract_content(html, "text/html", &test_url()).unwrap();

        assert!(extracted.text.contains("Configuration Reference"));
        assert!(!extracted.text.contains("Install API FAQ"));
    }

    #[test]
    fn readability_candidate_prefers_article_over_chrome() {
        let html = r#"
            <html>
              <head>
                <title>Site shell title</title>
                <meta property="og:title" content="Investigative Report">
              </head>
              <body>
                <header><a>Login</a><a>Subscribe</a><a>Markets</a></header>
                <main>
                  <article class="article-body">
                    <h1>Investigative Report</h1>
                    <p>The first paragraph explains the central finding in enough detail to be useful for a research agent.</p>
                    <p>The second paragraph adds names, context, numbers, and caveats that should be preserved.</p>
                    <p>The third paragraph gives follow-up evidence and avoids navigation boilerplate.</p>
                  </article>
                </main>
                <footer><a>Privacy</a><a>Terms</a></footer>
              </body>
            </html>
        "#;

        let extracted = extract_content(html, "text/html", &test_url()).unwrap();

        assert!(extracted.text.contains("central finding"));
        assert!(!extracted.text.contains("Login Subscribe Markets"));
        assert!(extracted.quality_score > 0.0);
        assert!(extracted.word_count >= 30);
    }

    #[test]
    fn plain_text_response_is_preserved() {
        let body = "Line one with useful content.\n\nLine two with more useful content.";

        let extracted = extract_content(body, "text/plain", &test_url()).unwrap();

        assert_eq!(
            extracted.text,
            "Line one with useful content.\nLine two with more useful content."
        );
    }

    #[test]
    fn decodes_numeric_entities() {
        let text = decode_html_entities("Tom&#39;s &#x141;odz");

        assert_eq!(text, "Tom's Łodz");
    }

    fn test_url() -> Url {
        Url::parse("https://example.com/article").unwrap()
    }
}
