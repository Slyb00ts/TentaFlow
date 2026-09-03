// =============================================================================
// Plik: web_research/types.rs
// Opis: Shared request and response types for web research search, page reading
//       and batch reading operations exposed to addons.
// =============================================================================

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WebResearchRequest {
    Search(SearchRequest),
    ReadUrl(ReadUrlRequest),
    ReadSearchResults(ReadSearchResultsRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub provider: Option<SearchProviderConfig>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub time_range: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadUrlRequest {
    pub url: String,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    #[serde(default)]
    pub mode: ReadMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadSearchResultsRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub search_limit: usize,
    #[serde(default = "default_read_limit")]
    pub read_limit: usize,
    #[serde(default = "default_max_chars")]
    pub max_chars_per_page: usize,
    #[serde(default)]
    pub provider: Option<SearchProviderConfig>,
    #[serde(default)]
    pub mode: ReadMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchProviderConfig {
    Searxng {
        base_url: String,
        #[serde(default, skip_serializing, skip_deserializing)]
        internal: bool,
    },
    Brave {
        endpoint: Option<String>,
        api_key: String,
    },
    Tavily {
        endpoint: Option<String>,
        api_key: String,
        #[serde(default)]
        search_depth: Option<String>,
    },
    Duckduckgo {
        #[serde(default)]
        endpoint: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    Auto,
    Static,
    Browser,
}

impl Default for ReadMode {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub rank: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadPageResult {
    pub url: String,
    pub final_url: String,
    pub title: String,
    pub text: String,
    pub excerpt: String,
    pub status: u16,
    pub content_type: String,
    pub fetched_at: i64,
    pub extraction: ExtractionInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionInfo {
    pub method: String,
    pub char_count: usize,
    pub word_count: usize,
    pub quality_score: f32,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub provider: String,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadSearchResultsResponse {
    pub query: String,
    pub search: SearchResponse,
    pub pages: Vec<ReadPageResult>,
    pub skipped: Vec<SkippedResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedResult {
    pub url: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebResearchResponse {
    Search(SearchResponse),
    ReadUrl(ReadPageResult),
    ReadSearchResults(ReadSearchResultsResponse),
}

pub fn default_limit() -> usize {
    10
}

pub fn default_read_limit() -> usize {
    5
}

pub fn default_max_chars() -> usize {
    30_000
}

/// How we identify ourselves when fetching a public page.
///
/// We sent no User-Agent at all, and that is not a cosmetic omission: Wikipedia
/// answers an unidentified client with 403, so every research query that landed
/// on it came back with nothing read. Sites are entitled to know who is
/// fetching them, and a descriptive token is what their policies ask for.
pub const WEB_RESEARCH_USER_AGENT: &str =
    concat!("TentaFlow-WebResearch/", env!("CARGO_PKG_VERSION"), " (+research agent)");
