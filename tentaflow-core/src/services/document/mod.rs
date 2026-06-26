// =============================================================================
// Plik: services/document/mod.rs
// Opis: Warstwa dokumentów dla doc_parse (RAG E1.4) — rasteryzacja PDF → obrazy
//       stron + scalanie wyników parse per-strona. Rasteryzer (pdfium) jest
//       BEZWARUNKOWY — PDF musi działać na każdym urządzeniu.
// =============================================================================

pub mod extract;
pub mod rasterize;

use crate::services::runtime::executor::{DocBlock, DocumentParseResponse};

/// MIME PDF rozpoznawany przez pipeline doc_parse. Trzymane w jednym miejscu,
/// żeby executor i host-fn nie rozjeżdżały się w stringu.
pub const PDF_MIME: &str = "application/pdf";

/// Domyślne DPI renderu (PDF: 72 pt = 1 cal). 150 DPI to kompromis czytelność
/// vs rozmiar (A4 ≈ 1240×1754 px) — wystarcza modelom vision-parse.
pub const DEFAULT_RENDER_DPI: f32 = 150.0;

/// Punktów typograficznych na cal (stała PDF). Render: piksele = punkty × DPI/72.
pub const PDF_POINTS_PER_INCH: f32 = 72.0;

/// Anti-DoS: maksymalna liczba renderowanych stron. PDF deklarujący tysiące
/// stron nie może zmusić serwera do tysięcy wywołań vision-parse.
pub const MAX_PDF_PAGES: u32 = 200;

/// Anti-DoS: maksymalny rozmiar wejściowego PDF (50 MiB). Większy plik jest
/// odrzucany zanim pdfium go w ogóle dotknie.
pub const MAX_PDF_INPUT_BYTES: usize = 50 * 1024 * 1024;

/// Anti-DoS: górny limit pikseli na stronę po rasteryzacji. Wielka strona ×
/// wysokie DPI = OOM; gdy `width*height` przekroczy ten próg, DPI jest
/// skalowane w dół tak, by zmieścić się w limicie (≈ format A0 @ 150 DPI).
pub const MAX_PAGE_PIXELS: u64 = 40_000_000;

/// Próg warstwy tekstowej: minimalna ŚREDNIA liczba znaków na stronę, przy
/// której PDF traktujemy jako PDF z gotową warstwą tekstową (ekstrakcja przez
/// FPDFText) zamiast skanu/obrazu (rasteryzacja + vision-parse). Skany dają ~0
/// znaków na stronę (cała treść to obraz), publikacje z warstwą tekstową —
/// setki/tysiące. 100 znaków/stronę bezpiecznie oddziela te dwa przypadki:
/// poniżej progu render+vision (jakość), powyżej — ekstrakcja tekstu (sekundy
/// zamiast minut, bo pomijamy model strona-po-stronie).
pub const MIN_TEXT_LAYER_CHARS_PER_PAGE: usize = 100;

/// Czy `mime` oznacza PDF (case-insensitive, ignoruje parametry typu `; q=…`).
pub fn is_pdf_mime(mime: &str) -> bool {
    mime.split(';')
        .next()
        .map(|m| m.trim().eq_ignore_ascii_case(PDF_MIME))
        .unwrap_or(false)
}

/// Scala wyniki parse poszczególnych stron (w kolejności 0..N) w jeden
/// `DocumentParseResponse`. Markdown stron łączony separatorem `\n\n`; bloki
/// przepisane z poprawnym, narastającym numerem `page` (0-bazowym) niezależnie
/// od tego, jaki `page` zwrócił backend dla pojedynczego obrazu (zwykle 0).
/// Provenance: kolejność stron = kolejność wejścia. `usage` sumowane gdy
/// którykolwiek backend je zaraportował.
pub fn merge_page_responses(pages: Vec<DocumentParseResponse>) -> DocumentParseResponse {
    let mut markdown_parts: Vec<String> = Vec::with_capacity(pages.len());
    let mut blocks: Vec<DocBlock> = Vec::new();
    let mut usage_acc: Option<crate::api::openai::types::Usage> = None;

    for (page_idx, page) in pages.into_iter().enumerate() {
        let page_no = page_idx as u32;
        if !page.markdown.is_empty() {
            markdown_parts.push(page.markdown);
        }
        for mut block in page.blocks {
            // Numer strony nadaje pipeline, nie backend — backend widzi tylko
            // pojedynczy obraz i zawsze raportuje page=0.
            block.page = page_no;
            blocks.push(block);
        }
        if let Some(u) = page.usage {
            usage_acc = Some(match usage_acc.take() {
                Some(acc) => crate::api::openai::types::Usage {
                    prompt_tokens: acc.prompt_tokens.saturating_add(u.prompt_tokens),
                    completion_tokens: acc.completion_tokens.saturating_add(u.completion_tokens),
                    total_tokens: acc.total_tokens.saturating_add(u.total_tokens),
                },
                None => u,
            });
        }
    }

    DocumentParseResponse {
        markdown: markdown_parts.join("\n\n"),
        blocks,
        usage: usage_acc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(class: &str, text: &str) -> DocBlock {
        DocBlock {
            page: 0,
            class: class.into(),
            bbox: [0.0, 0.0, 1.0, 1.0],
            text: text.into(),
            confidence: None,
        }
    }

    #[test]
    fn pdf_mime_detection_is_case_and_param_tolerant() {
        assert!(is_pdf_mime("application/pdf"));
        assert!(is_pdf_mime("Application/PDF"));
        assert!(is_pdf_mime("application/pdf; charset=binary"));
        assert!(!is_pdf_mime("image/png"));
        assert!(!is_pdf_mime("application/pdfx"));
    }

    #[test]
    fn merge_renumbers_pages_and_joins_markdown() {
        let p0 = DocumentParseResponse {
            markdown: "# Strona 0".into(),
            blocks: vec![block("Title", "# Strona 0"), block("Text", "tekst 0")],
            usage: None,
        };
        let p1 = DocumentParseResponse {
            markdown: "# Strona 1".into(),
            blocks: vec![block("Text", "tekst 1")],
            usage: None,
        };
        let merged = merge_page_responses(vec![p0, p1]);
        assert_eq!(merged.markdown, "# Strona 0\n\n# Strona 1");
        assert_eq!(merged.blocks.len(), 3);
        // Bloki strony 0 mają page=0, bloku strony 1 — page=1.
        assert_eq!(merged.blocks[0].page, 0);
        assert_eq!(merged.blocks[1].page, 0);
        assert_eq!(merged.blocks[2].page, 1);
        assert_eq!(merged.blocks[2].text, "tekst 1");
    }

    /// RAG E1.4 — pipeline PDF→strony→(mock parse per-strona)→merge. Rasteryzuje
    /// realny 2-stronicowy PDF (pdfium), symuluje odpowiedź vision per-strona
    /// (markdown + 1 blok), scala i sprawdza numery stron + złączony markdown.
    /// NIE uruchamia realnego vision service — dispatch zastąpiony mockiem.
    #[test]
    fn pdf_to_pages_to_merge_assigns_page_numbers() {
        let pdf = super::rasterize::minimal_pdf(2);
        // Streaming-rasteryzer emituje po jednej stronie (PNG) przez sink —
        // zbieramy je tu eager tylko do asercji merge'u.
        let mut indices: Vec<u32> = Vec::new();
        super::rasterize::rasterize_pdf_streaming(&pdf, 100.0, MAX_PDF_PAGES, |p| {
            indices.push(p.index);
            Ok(())
        })
        .expect("rasteryzacja 2-stronicowego PDF");
        assert_eq!(indices, vec![0, 1]);

        // Mock dispatchu vision: każda strona zwraca markdown + 1 blok (page=0,
        // jak realny backend dla pojedynczego obrazu).
        let per_page: Vec<DocumentParseResponse> = indices
            .iter()
            .map(|&i| DocumentParseResponse {
                markdown: format!("# Strona {i}"),
                blocks: vec![block("Text", &format!("tresc {i}"))],
                usage: None,
            })
            .collect();

        let merged = merge_page_responses(per_page);
        assert_eq!(merged.markdown, "# Strona 0\n\n# Strona 1");
        assert_eq!(merged.blocks.len(), 2);
        assert_eq!(merged.blocks[0].page, 0);
        assert_eq!(merged.blocks[1].page, 1);
        assert_eq!(merged.blocks[1].text, "tresc 1");
    }

    #[test]
    fn merge_sums_usage_when_reported() {
        let mk = |p: u32| DocumentParseResponse {
            markdown: "x".into(),
            blocks: vec![],
            usage: Some(crate::api::openai::types::Usage {
                prompt_tokens: p,
                completion_tokens: 1,
                total_tokens: p + 1,
            }),
        };
        let merged = merge_page_responses(vec![mk(10), mk(20)]);
        let u = merged.usage.expect("usage summed");
        assert_eq!(u.prompt_tokens, 30);
        assert_eq!(u.completion_tokens, 2);
        assert_eq!(u.total_tokens, 32);
    }
}
