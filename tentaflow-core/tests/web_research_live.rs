// =============================================================================
// Plik: web_research_live.rs
// Opis: Live test publicznego web research przez kod tentaflow-core.
// Przykład: BROWSER_RENDERER_ENDPOINT=http://127.0.0.1:18092 cargo test --test web_research_live -- --ignored --nocapture
// =============================================================================

use tentaflow_core::web_research::browser_renderer;
use tentaflow_core::web_research::reader;
use tentaflow_core::web_research::search;
use tentaflow_core::web_research::{ReadMode, ReadUrlRequest, SearchProviderConfig, SearchRequest};

#[test]
#[ignore = "requires public internet and a running browser-renderer service"]
fn iphone_17_pro_max_price_research_through_core() {
    let endpoint = std::env::var("BROWSER_RENDERER_ENDPOINT")
        .expect("BROWSER_RENDERER_ENDPOINT must point to a running browser-renderer");
    let query = std::env::var("WEB_RESEARCH_QUERY")
        .unwrap_or_else(|_| "iPhone 17 Pro Max price USA".to_string());
    let search = search::search(&SearchRequest {
        query: query.clone(),
        limit: 8,
        provider: Some(SearchProviderConfig::Duckduckgo { endpoint: None }),
        language: None,
        time_range: None,
    })
    .expect("core search should return results");

    println!("CORE_SEARCH_PROVIDER={}", search.provider);
    println!("CORE_SEARCH_QUERY={}", search.query);
    let mut combined_text = String::new();
    for result in &search.results {
        println!(
            "CORE_SEARCH_RESULT rank={} title={} url={} snippet={}",
            result.rank, result.title, result.url, result.snippet
        );
        combined_text.push_str(&result.snippet);
        combined_text.push('\n');
    }
    assert!(
        !search.results.is_empty(),
        "core search returned no results for {query}"
    );

    let mut rendered_pages = 0usize;
    for result in search.results.iter().take(5) {
        match browser_renderer::read_url(
            &endpoint,
            &ReadUrlRequest {
                url: result.url.clone(),
                max_chars: 20_000,
                mode: ReadMode::Browser,
                user_id: Some("live-test".to_string()),
            },
        ) {
            Ok(page) => {
                rendered_pages += 1;
                println!(
                    "CORE_BROWSER_PAGE status={} method={} title={} url={}",
                    page.status, page.extraction.method, page.title, page.final_url
                );
                println!("CORE_BROWSER_EXCERPT={}", page.excerpt.replace('\n', " "));
                combined_text.push_str(&page.text);
                combined_text.push('\n');
            }
            Err(err) => {
                println!("CORE_BROWSER_SKIP url={} error={}", result.url, err);
            }
        }
        match reader::read_url(&ReadUrlRequest {
            url: result.url.clone(),
            max_chars: 20_000,
            mode: ReadMode::Static,
            user_id: Some("live-test".to_string()),
        }) {
            Ok(page) => {
                println!(
                    "CORE_STATIC_PAGE status={} method={} title={} url={}",
                    page.status, page.extraction.method, page.title, page.final_url
                );
                println!("CORE_STATIC_EXCERPT={}", page.excerpt.replace('\n', " "));
                combined_text.push_str(&page.text);
                combined_text.push('\n');
            }
            Err(err) => {
                println!("CORE_STATIC_SKIP url={} error={}", result.url, err);
            }
        }
    }

    assert!(
        rendered_pages > 0,
        "browser-renderer did not render any result"
    );
    assert!(
        combined_text.contains("$1,199") || combined_text.contains("1,199"),
        "core search/read pages did not include expected iPhone 17 Pro Max starting price"
    );
}
