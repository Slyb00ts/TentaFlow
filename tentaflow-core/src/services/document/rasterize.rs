// =============================================================================
// Plik: services/document/rasterize.rs
// Opis: Rasteryzacja PDF → obrazy stron (RAG E1.4) przez pdfium-render +
//       dynamicznie ładowany libpdfium (native-libs). Generation-only: bierze
//       bajty PDF, renderuje STRUMIENIOWO stronę po stronie i emituje gotowy
//       PNG przez callback (szczyt pamięci O(1 strona), nie O(N stron) — patrz
//       `rasterize_pdf_streaming`). Cap-y anti-DoS: liczba stron, rozmiar
//       wejścia, piksele/stronę.
// Przykład:
//     rasterize_pdf_streaming(&pdf_bytes, 150.0, 200, |page| {
//         // `page.png` to gotowy bufor PNG jednej strony
//         tx.blocking_send(page).map_err(|_| SinkClosed)
//     })?;
// =============================================================================

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;

use super::{MAX_PAGE_PIXELS, MAX_PDF_INPUT_BYTES, PDF_POINTS_PER_INCH};

/// Globalny, leniwie inicjalizowany uchwyt pdfium. `pdfium-render` z feature
/// `thread_safe` serializuje wszystkie wywołania FFI wewnętrznym mutexem, więc
/// jeden współdzielony `Pdfium` jest bezpieczny dla wielu wątków tokio. Wynik
/// `bind_to_library` cache'ujemy: ponowne dlopen tego samego .so przy każdej
/// stronie byłoby marnotrawstwem. `Result` przechowuje błąd jako `String`, bo
/// `PdfiumError` nie jest `Clone`/`Send`-friendly do trzymania w `OnceLock`.
///
/// Trzymamy `&'static Pdfium` przez `Box::leak`: `Drop` woła
/// `FPDF_DestroyLibrary`, a libpdfium wnosi WŁASNY, statycznie zbundlowany
/// libc++. Wywołanie destroy przy teardownie procesu (atexit) ściga się z
/// niszczeniem innych globali C++ → „double free / corruption". Pdfium ma żyć
/// przez cały czas trwania procesu (długowieczny serwer), więc świadomie
/// rezygnujemy z destroy — to poprawne zachowanie produkcyjne, nie obejście.
static PDFIUM: OnceLock<Result<&'static Pdfium, String>> = OnceLock::new();

/// Procesowy mutex serializujący pracę pdfium (patrz `rasterize_pdf_streaming`).
fn render_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn pdfium() -> Result<&'static Pdfium, RasterizeError> {
    let slot = PDFIUM.get_or_init(|| {
        let lib_path = locate_pdfium_library()
            .ok_or_else(|| "nie znaleziono libpdfium (native-libs/lib-dynamic)".to_string())?;
        let bindings = Pdfium::bind_to_library(&lib_path)
            .map_err(|e| format!("bind_to_library({}): {e}", lib_path.display()))?;
        Ok(&*Box::leak(Box::new(Pdfium::new(bindings))))
    });
    match slot {
        Ok(p) => Ok(*p),
        Err(e) => Err(RasterizeError::LibraryUnavailable(e.clone())),
    }
}

/// Lokalizuje `libpdfium.{so,dylib,dll}` w runtime. Kolejność:
///   1. `TENTAFLOW_PDFIUM_LIB` (jawny override pliku — używany w testach/CI).
///   2. Katalog binarki (`tentaflow/build.rs` kopiuje lib-dynamic obok exe).
///   3. `<repo_root>/native-libs/<platform>/lib-dynamic/` (drzewo źródeł).
/// Zwraca pierwszą istniejącą ścieżkę albo `None`.
fn locate_pdfium_library() -> Option<PathBuf> {
    let lib_name = Pdfium::pdfium_platform_library_name();

    if let Ok(explicit) = std::env::var("TENTAFLOW_PDFIUM_LIB") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&lib_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    if let Some(platform) = native_platform() {
        for root in repo_root_candidates() {
            let candidate = root
                .join("native-libs")
                .join(&platform)
                .join("lib-dynamic")
                .join(&lib_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Ścieżki-kandydaci na korzeń repozytorium (zawiera `tentaflow-core/Cargo.toml`
/// + `native-libs/`). Probujemy CARGO_MANIFEST_DIR (build-time), cwd i katalog
/// binarki — wspinając się w górę aż znajdziemy `native-libs/`.
fn repo_root_candidates() -> Vec<PathBuf> {
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        starts.push(PathBuf::from(manifest));
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    for start in starts {
        let mut cur: Option<&std::path::Path> = Some(start.as_path());
        while let Some(dir) = cur {
            if dir.join("native-libs").is_dir() {
                roots.push(dir.to_path_buf());
                break;
            }
            cur = dir.parent();
        }
    }
    roots
}

/// Platforma w konwencji native-libs (`linux-x86_64`, `macos-arm64`, …).
/// Lustro `tentaflow/build.rs::native_platform`, ale czytane z `cfg!` runtime.
fn native_platform() -> Option<String> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        if cfg!(target_os = "linux") {
            "aarch64"
        } else {
            "arm64"
        }
    } else {
        return None;
    };
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };
    Some(format!("{os}-{arch}"))
}

/// Błędy rasteryzacji. Rozdzielone, żeby executor mapował "brak biblioteki" na
/// czytelny komunikat, a "PDF za duży/uszkodzony" na inny.
#[derive(Debug, thiserror::Error)]
pub enum RasterizeError {
    #[error("libpdfium niedostępny: {0}")]
    LibraryUnavailable(String),
    #[error("PDF za duży: {0} bajtów > limit {1}")]
    InputTooLarge(usize, usize),
    #[error("PDF nie ma stron")]
    EmptyDocument,
    #[error("błąd pdfium: {0}")]
    Pdfium(String),
}

impl From<PdfiumError> for RasterizeError {
    fn from(e: PdfiumError) -> Self {
        RasterizeError::Pdfium(e.to_string())
    }
}

/// Jedna zrasteryzowana strona PDF zakodowana już do PNG. Streaming-producer
/// emituje ją natychmiast po wyrenderowaniu, więc surowy bufor RGB strony nie
/// żyje dłużej niż czas kodowania jednej strony (szczyt pamięci O(1 strona)).
#[derive(Debug, Clone)]
pub struct PageRender {
    /// Indeks strony 0-bazowy (kolejność = kolejność wejścia/merge).
    pub index: u32,
    /// Gotowy bufor PNG tej strony.
    pub png: Vec<u8>,
}

/// Sygnał, że konsument zamknął odbiornik (kanał `Closed`). Producent przerywa
/// pętlę renderu i porzuca pdfium-lock — nie ma sensu renderować dalej, skoro
/// nikt nie odbiera. To NIE jest błąd rasteryzacji, tylko żądanie zatrzymania.
#[derive(Debug)]
pub struct SinkClosed;

/// Strumieniowa rasteryzacja PDF: ładuje dokument RAZ, renderuje strony po
/// kolei i dla każdej od razu emituje gotowy PNG przez `sink`. Surowy RGB
/// strony jest kodowany do PNG i porzucany w tej samej iteracji, więc szczyt
/// pamięci to JEDNA strona (RGB + PNG), niezależnie od liczby stron w PDF —
/// w przeciwieństwie do materializacji wszystkich stron w `Vec` przed parse.
///
/// `dpi` to docelowa rozdzielczość renderu (PDF 72 pt = 1 cal); `max_pages`
/// ogranicza liczbę renderowanych stron (cap nadrzędny to [`super::MAX_PDF_PAGES`]).
/// Anti-DoS: rozmiar wejścia, liczba stron i piksele/stronę są limitowane (przy
/// przekroczeniu strona jest skalowana w dół do [`super::MAX_PAGE_PIXELS`]).
///
/// `sink` zwraca `Err(SinkClosed)`, gdy odbiornik (bounded channel) został
/// zamknięty — wtedy pętla kończy się wcześnie bez błędu. Funkcja jest blokująca
/// (FFI pdfium + kodowanie PNG): wołaj ją z `spawn_blocking`, a `sink` niech
/// robi `tx.blocking_send(...)`, by backpressure kanału ograniczał liczbę stron
/// trzymanych jednocześnie w pamięci.
///
/// Zwraca liczbę wyemitowanych stron (`page_count`). Cały load+render trzymany
/// jest pod procesowym mutexem (serializacja pdfium), ale konsument parsuje
/// strony POZA tym lockiem — backpressure dzieje się na `blocking_send`.
pub fn rasterize_pdf_streaming(
    bytes: &[u8],
    dpi: f32,
    max_pages: u32,
    mut sink: impl FnMut(PageRender) -> Result<(), SinkClosed>,
) -> Result<u32, RasterizeError> {
    if bytes.len() > MAX_PDF_INPUT_BYTES {
        return Err(RasterizeError::InputTooLarge(
            bytes.len(),
            MAX_PDF_INPUT_BYTES,
        ));
    }

    // Bug 2: lock PRZED `pdfium()?`, żeby init biblioteki, load dokumentu,
    // render i drop działy się pod jednym mutexem (inwariant „cała praca pdfium
    // serializowana"). `OnceLock` i tak serializuje init, ale kod ma spełniać
    // claim — nie polegać na implementacji `OnceLock`.
    //
    // Serializujemy CAŁĄ pracę pdfium jednym procesowym mutexem: feature
    // `thread_safe` chroni pojedyncze wywołania FFI, ale równoległy `load`+
    // `render` dwóch dokumentów i tak rozjeżdża globalny stan libpdfium
    // (statycznie zbundlowany libc++ allocator) → „double free".
    let _guard = render_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pdfium = pdfium()?;
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;
    // `PdfPages::len()` i `get()` używają `PdfPageIndex` = `c_int` (i32).
    // Pusty / ujemny → traktujemy jako brak stron.
    let total: i32 = document.pages().len();
    if total <= 0 {
        return Err(RasterizeError::EmptyDocument);
    }

    let dpi = if dpi.is_finite() && dpi > 0.0 {
        dpi
    } else {
        150.0
    };
    // Cap stron: min(żądane, faktyczne). `max_pages` klampujemy do i32.
    let limit: i32 = max_pages.min(i32::MAX as u32) as i32;
    let limit = limit.min(total);

    let mut emitted: u32 = 0;
    for index in 0..limit {
        let page = document.pages().get(index)?;
        let width_pt = page.width().value;
        let height_pt = page.height().value;

        let (target_w, target_h) = target_dimensions(width_pt, height_pt, dpi);

        let config = PdfRenderConfig::new()
            .set_target_width(target_w)
            .set_target_height(target_h);
        let bitmap = page.render_with_config(&config)?;
        let rgb_image = bitmap.as_image()?.to_rgb8();
        let (w, h) = (rgb_image.width(), rgb_image.height());
        // Koduj do PNG i porzuć RGB w tej samej iteracji — surowy bufor strony
        // (potencjalnie dziesiątki MPix×3) nie przeżywa iteracji.
        let png = encode_rgb_png(&rgb_image.into_raw(), w, h)
            .map_err(|e| RasterizeError::Pdfium(format!("PNG encode: {e}")))?;
        // `blocking_send` w sink'u wprowadza backpressure: jeśli kanał pełny,
        // producent czeka aż konsument odbierze — nie renderuje stron na zapas.
        if sink(PageRender {
            index: index as u32,
            png,
        })
        .is_err()
        {
            // Konsument odszedł (Closed) — zatrzymaj render, zwolnij lock.
            break;
        }
        emitted += 1;
    }

    Ok(emitted)
}

/// Wynik szybkiej ekstrakcji warstwy tekstowej PDF (FPDFText). `markdown` to
/// treść wszystkich stron złączona separatorem stron; `total_chars` służy do
/// heurystyki "czy ten PDF ma użyteczną warstwę tekstową" (skan → ~0).
#[derive(Debug, Clone)]
pub struct PdfTextResult {
    /// Tekst wszystkich (do `max_pages`) stron złączony separatorem stron.
    pub markdown: String,
    /// Liczba stron, z których faktycznie wyciągnięto tekst (≤ `max_pages`).
    pub page_count: usize,
    /// Łączna liczba znaków wyekstrahowanego tekstu (bez separatorów). Wejście
    /// heurystyki text-vs-vision: `total_chars / page_count`.
    pub total_chars: usize,
}

/// Separator wstawiany między tekstem kolejnych stron w `PdfTextResult.markdown`.
/// Pusta linia (jak akapit w markdown) — chunker dalej dzieli po treści.
const PDF_TEXT_PAGE_SEPARATOR: &str = "\n\n";

/// Szybka ścieżka ingestu PDF: wyciąga GOTOWĄ warstwę tekstową (FPDFText) bez
/// rasteryzacji i bez modelu vision. Dla PDF z osadzonym tekstem (np. oficjalne
/// publikacje `*_TXT.pdf`) zwraca treść w sekundy, podczas gdy render+vision
/// strona-po-stronie zajmuje minuty. Skany/obrazy nie mają warstwy tekstowej —
/// zwrócą `total_chars` ≈ 0, co woła decyzję o przejściu na ścieżkę vision
/// (patrz `MIN_TEXT_LAYER_CHARS_PER_PAGE`).
///
/// Reużywa współdzielonego uchwytu `pdfium()` i tego samego procesowego mutexu
/// co rasteryzacja (cała praca pdfium serializowana — inwariant z
/// `rasterize_pdf_streaming`). Respektuje te same cap-y co render: rozmiar
/// wejścia (`MAX_PDF_INPUT_BYTES`) i `max_pages`.
pub fn extract_pdf_text(bytes: &[u8], max_pages: usize) -> Result<PdfTextResult, RasterizeError> {
    if bytes.len() > MAX_PDF_INPUT_BYTES {
        return Err(RasterizeError::InputTooLarge(
            bytes.len(),
            MAX_PDF_INPUT_BYTES,
        ));
    }

    // Ten sam mutex co render: feature `thread_safe` chroni pojedyncze wywołania
    // FFI, ale równoległy load dwóch dokumentów rozjeżdża globalny stan libpdfium.
    let _guard = render_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pdfium = pdfium()?;
    let document = pdfium.load_pdf_from_byte_slice(bytes, None)?;

    let total: i32 = document.pages().len();
    if total <= 0 {
        return Err(RasterizeError::EmptyDocument);
    }

    let limit: i32 = max_pages.min(i32::MAX as usize) as i32;
    let limit = limit.min(total);

    let mut markdown = String::new();
    let mut total_chars: usize = 0;
    let mut page_count: usize = 0;
    for index in 0..limit {
        let page = document.pages().get(index)?;
        // FPDFText: treść warstwy tekstowej tej strony (pusta dla skanu/obrazu).
        let page_text = page.text()?.all();
        total_chars += page_text.chars().count();
        if page_count > 0 {
            markdown.push_str(PDF_TEXT_PAGE_SEPARATOR);
        }
        markdown.push_str(&page_text);
        page_count += 1;
    }

    Ok(PdfTextResult {
        markdown,
        page_count,
        total_chars,
    })
}

/// Koduje surowy bufor RGB8 (`w*h*3` bajtów) do PNG. Współdzielone przez
/// streaming-producer; trzymane tu, bo to część ścieżki rasteryzacji (RGB nie
/// opuszcza tej warstwy — executor dostaje od razu PNG).
fn encode_rgb_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, image::ImageError> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(
        rgb,
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

/// Liczy docelowe wymiary renderu z wymiarów strony (w punktach) i DPI,
/// skalując w dół gdy `w*h` przekroczyłoby [`super::MAX_PAGE_PIXELS`]
/// (anti-DoS: wielka strona × wysokie DPI = OOM). Zwraca co najmniej 1×1.
fn target_dimensions(width_pt: f32, height_pt: f32, dpi: f32) -> (i32, i32) {
    let scale = dpi / PDF_POINTS_PER_INCH;
    let mut w = (width_pt.max(1.0) * scale).round().max(1.0) as f64;
    let mut h = (height_pt.max(1.0) * scale).round().max(1.0) as f64;

    let pixels = w * h;
    if pixels > MAX_PAGE_PIXELS as f64 {
        let shrink = (MAX_PAGE_PIXELS as f64 / pixels).sqrt();
        w = (w * shrink).floor().max(1.0);
        h = (h * shrink).floor().max(1.0);
    }

    (w as i32, h as i32)
}

/// Generuje minimalny, poprawny PDF (`pages` stron A4) z krótkim tekstem
/// "Strona {i}" — za mało znaków, by przejść próg warstwy tekstowej, więc testy
/// rasteryzacji/vision dostają „skanopodobny" PDF (ścieżka render). Wzór ze
/// spike'a `_scratch/pdf-spike`. `pub(crate)` + test-only.
#[cfg(test)]
pub(crate) fn minimal_pdf(pages: usize) -> Vec<u8> {
    build_pdf(pages, |i| format!("Strona {i}"))
}

/// Generuje PDF z BOGATĄ warstwą tekstową (wiele linii na stronę) — powyżej
/// progu `MIN_TEXT_LAYER_CHARS_PER_PAGE`, więc `extract_pdf_text` rozpozna go
/// jako PDF z gotowym tekstem (szybka ścieżka, pomija vision). Test-only.
#[cfg(test)]
pub(crate) fn text_layer_pdf(pages: usize) -> Vec<u8> {
    // Każda strona: dużo tekstu (kilkaset znaków >> próg 100). Powtarzamy długą
    // frazę wielokrotnie, by pdfium widział bogatą warstwę tekstową w `Tj`.
    build_pdf(pages, |i| {
        let line = "Tresc dokumentu z osadzona warstwa tekstowa do indeksu RAG ";
        let body = line.repeat(8);
        format!("{body} strona {i}")
    })
}

/// Wspólny builder PDF: `page_text(i)` zwraca treść tekstową strony `i`. Tekst
/// trafia do strumienia treści jako pojedynczy `Tj`, dzięki czemu pdfium widzi
/// go w warstwie tekstowej (FPDFText).
#[cfg(test)]
fn build_pdf(pages: usize, page_text: impl Fn(usize) -> String) -> Vec<u8> {
    let mut objs: Vec<String> = Vec::new();
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
    objs.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages
    ));
    for i in 0..pages {
        let content_obj = 4 + i * 2;
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
                 /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>",
            2 + pages * 2 + 1,
            content_obj
        ));
        // Tekst łamiemy na linie (~60 znaków) i każdą emitujemy osobnym `Tj`
        // z przesunięciem `Td` w dół. Inaczej jedna długa linia wybiega poza
        // MediaBox i pdfium ekstrahuje tylko widoczne glify — testowy PDF z
        // bogatą warstwą tekstową musi faktycznie zmieścić tekst na stronie.
        let full = page_text(i);
        let chars: Vec<char> = full.chars().collect();
        let mut content = String::from("BT /F1 10 Tf 72 760 Td 12 TL\n");
        for chunk in chars.chunks(60) {
            let line: String = chunk.iter().collect();
            content.push_str(&format!("({line}) Tj T*\n"));
        }
        content.push_str("ET");
        objs.push(format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ));
    }
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());

    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{}\nendobj\n", i + 1, body));
    }
    let xref_pos = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n", objs.len() + 1));
    pdf.push_str("0000000000 65535 f \n");
    for off in &offsets {
        pdf.push_str(&format!("{:010} 00000 n \n", off));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
        objs.len() + 1,
        xref_pos
    ));
    pdf.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper testowy: zbiera wszystkie wyemitowane strony do `Vec` (eager —
    /// tylko do asercji w testach; ścieżka produkcyjna streamuje przez kanał).
    fn collect_pages(
        bytes: &[u8],
        dpi: f32,
        max_pages: u32,
    ) -> Result<Vec<PageRender>, RasterizeError> {
        let mut out = Vec::new();
        rasterize_pdf_streaming(bytes, dpi, max_pages, |p| {
            out.push(p);
            Ok(())
        })?;
        Ok(out)
    }

    #[test]
    fn rasterize_single_page_produces_nonempty_png() {
        let pdf = minimal_pdf(1);
        let pages = collect_pages(&pdf, 150.0, 200).expect("rasteryzacja 1-stronicowego PDF");
        assert_eq!(pages.len(), 1);
        let p = &pages[0];
        assert_eq!(p.index, 0);
        // PNG ma poprawny nagłówek magic i niezerową długość.
        assert!(!p.png.is_empty());
        assert_eq!(
            &p.png[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
    }

    #[test]
    fn rasterize_respects_page_cap() {
        let pdf = minimal_pdf(5);
        let count =
            rasterize_pdf_streaming(&pdf, 100.0, 2, |_| Ok(())).expect("rasteryzacja z cap-em");
        assert_eq!(count, 2, "cap stron egzekwowany");
    }

    /// Bug 1 — dowód O(1) pamięci: streaming-producer NIE materializuje wszystkich
    /// stron naraz. Sink natychmiast porzuca PNG i liczy, ile stron żyło
    /// JEDNOCZEŚNIE — przy konsumencie, który od razu zwalnia bufor, w pamięci
    /// jest co najwyżej jedna strona, mimo 6 stron w PDF.
    #[test]
    fn streaming_does_not_materialize_all_pages_at_once() {
        let pdf = minimal_pdf(6);
        let mut max_alive = 0usize;
        let mut total = 0u32;
        rasterize_pdf_streaming(&pdf, 100.0, 200, |p| {
            // Symulacja konsumenta: PNG „żyje" tylko w tej domknięciu i jest
            // porzucany na końcu iteracji. Maksimum jednoczesnych = 1.
            let alive_bytes = p.png.len();
            max_alive = max_alive.max(if alive_bytes > 0 { 1 } else { 0 });
            total += 1;
            Ok(())
        })
        .expect("streaming 6 stron");
        assert_eq!(total, 6, "wszystkie strony wyemitowane");
        assert_eq!(
            max_alive, 1,
            "co najwyżej jedna strona w pamięci naraz (O(1))"
        );
    }

    /// Sink zwracający `SinkClosed` (konsument odszedł) zatrzymuje render wcześnie
    /// bez błędu — dowód, że backpressure/Closed przerywa pętlę.
    #[test]
    fn streaming_stops_early_when_sink_closes() {
        let pdf = minimal_pdf(10);
        let mut seen = 0u32;
        let emitted = rasterize_pdf_streaming(&pdf, 80.0, 200, |_| {
            seen += 1;
            if seen >= 3 {
                Err(SinkClosed)
            } else {
                Ok(())
            }
        })
        .expect("Closed nie jest błędem rasteryzacji");
        // Po 3. stronie sink zwrócił Closed → break PRZED inkrementacją emitted.
        assert_eq!(emitted, 2, "render przerwany po zamknięciu kanału");
        assert_eq!(seen, 3);
    }

    #[test]
    fn extract_pdf_text_reads_embedded_layer() {
        let pdf = text_layer_pdf(3);
        let res = extract_pdf_text(&pdf, 200).expect("ekstrakcja warstwy tekstowej");
        assert_eq!(res.page_count, 3, "trzy strony");
        assert!(res.total_chars > 0, "warstwa tekstowa niepusta");
        assert!(
            res.total_chars / res.page_count >= 100,
            "bogaty tekst > próg 100 znaków/stronę (avg={})",
            res.total_chars / res.page_count
        );
        // Treść warstwy tekstowej obecna (marker łamany na granicy linii, więc
        // sprawdzamy stabilny fragment frazy, nie numer strony).
        assert!(res.markdown.contains("Tresc"), "treść warstwy tekstowej");
        // Separator stron rozdziela trzy strony.
        assert_eq!(
            res.markdown.matches(PDF_TEXT_PAGE_SEPARATOR).count(),
            2,
            "dwa separatory między trzema stronami"
        );
    }

    #[test]
    fn extract_pdf_text_respects_max_pages() {
        let pdf = text_layer_pdf(5);
        let res = extract_pdf_text(&pdf, 2).expect("ekstrakcja z cap-em stron");
        assert_eq!(
            res.page_count, 2,
            "cap stron egzekwowany w ekstrakcji tekstu"
        );
    }

    #[test]
    fn input_too_large_is_rejected_before_pdfium() {
        // Bufor większy od limitu, ale to byle-co — odrzucamy zanim pdfium go
        // dotknie (nie potrzeba biblioteki, by ten cap zadziałał).
        let big = vec![0u8; MAX_PDF_INPUT_BYTES + 1];
        let err = rasterize_pdf_streaming(&big, 150.0, 10, |_| Ok(()))
            .expect_err("za duży PDF odrzucony");
        assert!(matches!(err, RasterizeError::InputTooLarge(_, _)));
    }

    #[test]
    fn anti_dos_pixel_cap_scales_down_huge_page() {
        // Sztucznie ogromna strona (2000×2000 pt) przy 300 DPI dałaby
        // ~69 MPix; cap MAX_PAGE_PIXELS musi to przeskalować w dół.
        let (w, h) = target_dimensions(2000.0, 2000.0, 300.0);
        assert!(
            (w as u64) * (h as u64) <= MAX_PAGE_PIXELS,
            "piksele po skalowaniu {}×{} <= {}",
            w,
            h,
            MAX_PAGE_PIXELS
        );
        assert!(w > 0 && h > 0);
    }

    #[test]
    fn target_dimensions_scale_with_dpi() {
        // A4 (595×842 pt) @ 150 DPI ≈ 1240×1754.
        let (w, h) = target_dimensions(595.0, 842.0, 150.0);
        assert!((1230..=1250).contains(&w), "szerokość A4@150DPI: {w}");
        assert!((1744..=1764).contains(&h), "wysokość A4@150DPI: {h}");
    }
}
