// =============================================================================
// Plik: examples/web_research_smoke.rs
// Opis: Narzedzie diagnostyczne — sprawdza na zywo czy modul web_research
//       naprawde szuka w sieci i ekstrahuje tresc stron.
//       Uzycie: cargo run --release --example web_research_smoke -- \
//               --query "Ox Alpha" --op search
// =============================================================================

use std::process::ExitCode;
use std::time::Instant;

use tentaflow_core::web_research::{
    self, ReadMode, ReadSearchResultsRequest, ReadUrlRequest, SearchProviderConfig, SearchRequest,
    WebResearchRequest,
};

/// Operacja do wykonania — jawny enum zamiast luznego stringa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Search,
    Read,
    ReadResults,
}

impl Op {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "search" => Ok(Self::Search),
            "read" => Ok(Self::Read),
            "read-results" | "read_results" => Ok(Self::ReadResults),
            other => Err(format!(
                "nieznana operacja '{}': uzyj search|read|read-results",
                other
            )),
        }
    }
}

/// Wybor dostawcy wyszukiwania podany z linii polecen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    Duckduckgo,
    Searxng,
}

impl ProviderKind {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "duckduckgo" | "ddg" => Ok(Self::Duckduckgo),
            "searxng" => Ok(Self::Searxng),
            other => Err(format!(
                "nieznany provider '{}': uzyj duckduckgo|searxng",
                other
            )),
        }
    }
}

struct Args {
    query: String,
    op: Op,
    provider: ProviderKind,
    base_url: Option<String>,
    search_limit: usize,
    read_limit: usize,
    max_chars: usize,
    mode: ReadMode,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            query: "Ox Alpha".to_string(),
            op: Op::Search,
            provider: ProviderKind::Duckduckgo,
            base_url: None,
            search_limit: 10,
            read_limit: 5,
            max_chars: 30_000,
            mode: ReadMode::Static,
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut idx = 0usize;
    while idx < raw.len() {
        let flag = raw[idx].as_str();
        // Kazda flaga w tym narzedziu wymaga wartosci, wiec pobieramy ja jednolicie.
        let value = || -> Result<String, String> {
            raw.get(idx + 1)
                .cloned()
                .ok_or_else(|| format!("brak wartosci dla {}", flag))
        };
        match flag {
            "--query" | "-q" => args.query = value()?,
            "--op" => args.op = Op::parse(&value()?)?,
            "--provider" => args.provider = ProviderKind::parse(&value()?)?,
            "--base-url" => args.base_url = Some(value()?),
            "--search-limit" => {
                args.search_limit = value()?
                    .parse()
                    .map_err(|e| format!("--search-limit: {}", e))?
            }
            "--read-limit" => {
                args.read_limit = value()?
                    .parse()
                    .map_err(|e| format!("--read-limit: {}", e))?
            }
            "--max-chars" => {
                args.max_chars = value()?
                    .parse()
                    .map_err(|e| format!("--max-chars: {}", e))?
            }
            "--mode" => {
                args.mode = match value()?.as_str() {
                    "auto" => ReadMode::Auto,
                    "static" => ReadMode::Static,
                    "browser" => ReadMode::Browser,
                    other => return Err(format!("nieznany tryb '{}'", other)),
                }
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("nieznana flaga '{}'", other)),
        }
        idx += 2;
    }
    Ok(args)
}

fn print_usage() {
    println!(
        "web_research_smoke — diagnostyka web research\n\
         \n\
         --query <tekst>            zapytanie (dla --op read: adres URL), domyslnie \"Ox Alpha\"\n\
         --op search|read|read-results   operacja, domyslnie search\n\
         --provider duckduckgo|searxng   dostawca wyszukiwania, domyslnie duckduckgo\n\
         --base-url <url>           adres SearXNG (wymagany dla --provider searxng)\n\
         --search-limit <n>         ile wynikow wyszukiwania, domyslnie 10\n\
         --read-limit <n>           ile stron pobrac w read-results, domyslnie 5\n\
         --max-chars <n>            limit znakow na strone, domyslnie 30000\n\
         --mode auto|static|browser tryb czytania, domyslnie static"
    );
}

/// Buduje konfiguracje dostawcy — provider jest ZAWSZE jawny, zeby test nie
/// zalezal od stanu lokalnych serwisow w bazie.
fn build_provider(args: &Args) -> Result<SearchProviderConfig, String> {
    match args.provider {
        ProviderKind::Duckduckgo => Ok(SearchProviderConfig::Duckduckgo {
            endpoint: args.base_url.clone(),
        }),
        ProviderKind::Searxng => {
            let base_url = args
                .base_url
                .clone()
                .ok_or_else(|| "--provider searxng wymaga --base-url".to_string())?;
            Ok(SearchProviderConfig::Searxng {
                base_url,
                internal: true,
            })
        }
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("BLAD ARGUMENTOW: {}", e);
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    let request = match args.op {
        Op::Search => {
            let provider = match build_provider(&args) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("BLAD ARGUMENTOW: {}", e);
                    return ExitCode::FAILURE;
                }
            };
            WebResearchRequest::Search(SearchRequest {
                query: args.query.clone(),
                limit: args.search_limit,
                provider: Some(provider),
                language: None,
                time_range: None,
            })
        }
        Op::Read => WebResearchRequest::ReadUrl(ReadUrlRequest {
            url: args.query.clone(),
            max_chars: args.max_chars,
            mode: args.mode,
            user_id: None,
        }),
        Op::ReadResults => {
            let provider = match build_provider(&args) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("BLAD ARGUMENTOW: {}", e);
                    return ExitCode::FAILURE;
                }
            };
            WebResearchRequest::ReadSearchResults(ReadSearchResultsRequest {
                query: args.query.clone(),
                search_limit: args.search_limit,
                read_limit: args.read_limit,
                max_chars_per_page: args.max_chars,
                provider: Some(provider),
                mode: args.mode,
                user_id: None,
            })
        }
    };

    println!("=== Zadanie ===");
    match serde_json::to_string_pretty(&request) {
        Ok(json) => println!("{}", json),
        Err(e) => println!("(nie udalo sie zserializowac zadania: {})", e),
    }

    let started = Instant::now();
    let outcome = web_research::execute(request);
    let elapsed = started.elapsed();

    match outcome {
        Ok(response) => {
            println!("\n=== Odpowiedz ({} ms) ===", elapsed.as_millis());
            match serde_json::to_string_pretty(&response) {
                Ok(json) => println!("{}", json),
                Err(e) => {
                    eprintln!("BLAD SERIALIZACJI ODPOWIEDZI: {}", e);
                    return ExitCode::FAILURE;
                }
            }
            println!("\n=== Podsumowanie ===");
            print_summary(&response);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\n=== BLAD ({} ms) ===", elapsed.as_millis());
            eprintln!("{}", e);
            eprintln!("debug: {:?}", e);
            ExitCode::FAILURE
        }
    }
}

/// Skrot dla czlowieka — najwazniejsze liczby bez czytania calego JSON-a.
fn print_summary(response: &web_research::WebResearchResponse) {
    match response {
        web_research::WebResearchResponse::Search(search) => {
            println!("provider: {}", search.provider);
            println!("wynikow: {}", search.results.len());
            for result in &search.results {
                println!(
                    "  [{}] {}\n       {}",
                    result.rank, result.title, result.url
                );
            }
        }
        web_research::WebResearchResponse::ReadUrl(page) => {
            print_page(page);
        }
        web_research::WebResearchResponse::ReadSearchResults(batch) => {
            println!("provider: {}", batch.search.provider);
            println!(
                "wynikow wyszukiwania: {}, pobranych stron: {}, pominietych: {}",
                batch.search.results.len(),
                batch.pages.len(),
                batch.skipped.len()
            );
            for page in &batch.pages {
                print_page(page);
            }
            for skipped in &batch.skipped {
                println!("  POMINIETO {} -> {}", skipped.url, skipped.reason);
            }
        }
    }
}

fn print_page(page: &tentaflow_core::web_research::ReadPageResult) {
    println!(
        "  OK {} (status {}, {})\n     tytul: {}\n     metoda: {}, znakow: {}, slow: {}, jakosc: {:.3}, uciete: {}",
        page.final_url,
        page.status,
        page.content_type,
        page.title,
        page.extraction.method,
        page.extraction.char_count,
        page.extraction.word_count,
        page.extraction.quality_score,
        page.extraction.truncated
    );
}
