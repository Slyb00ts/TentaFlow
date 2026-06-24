// =============================================================================
// Plik: services/document/extract.rs
// Opis: Czysto-rustowa ekstrakcja tekstu z dokumentów biurowych (XLSX/DOCX/PPTX)
//       do markdown GFM oraz klasyfikacja typu pliku (mime + magic-bytes) i
//       chunking. Portowane z addona WASM `addons/rag` do core, by ingest RAG
//       działał na każdym urządzeniu (telefon) bez Pythona. Reużywane przez
//       node-adaptery document_router / excel_extract / word_extract /
//       pptx_extract / chunk.
// Przykład:
//     let kind = classify_source(mime, &bytes);
//     let md = xlsx_to_markdown(&bytes)?;
//     let chunks = split_into_chunks(&md, CHUNK_SIZE_CHARS, CHUNK_OVERLAP_CHARS);
// =============================================================================

use std::io::{Cursor, Read};

/// Domyślny rozmiar chunka w znakach (lustro stałej z addona RAG).
pub const CHUNK_SIZE_CHARS: usize = 2048;
/// Domyślny overlap chunków w znakach.
pub const CHUNK_OVERLAP_CHARS: usize = 200;

/// Klasa źródła wyznaczona z mime + magic-bytes — wybiera ścieżkę ekstrakcji.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// PDF — rasteryzacja do obrazów stron (vision-parse downstream).
    Pdf,
    /// Obraz (PNG/JPEG/WEBP/…) — vision-parse bezpośrednio.
    Image,
    /// Arkusz XLSX — calamine → tabele markdown.
    Xlsx,
    /// Dokument DOCX — quick-xml nad word/document.xml.
    Docx,
    /// Prezentacja PPTX — quick-xml nad ppt/slides/slideN.xml.
    Pptx,
    /// Tekst wprost (UTF-8) — text/*, application/json.
    Text,
    /// Nierozpoznany typ — node routera kieruje na port `unknown`.
    Unknown,
}

impl SourceKind {
    /// Stabilna nazwa portu wyjściowego routera dla tej klasy.
    pub fn router_port(self) -> &'static str {
        match self {
            SourceKind::Pdf => "pdf",
            SourceKind::Image => "image",
            SourceKind::Xlsx => "xlsx",
            SourceKind::Docx => "docx",
            SourceKind::Pptx => "pptx",
            SourceKind::Text => "text",
            SourceKind::Unknown => "unknown",
        }
    }
}

/// Bazowy mime bez parametrów (`application/pdf; charset=...` → `application/pdf`),
/// znormalizowany do lowercase.
fn base_mime(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase()
}

/// OOXML (xlsx/docx/pptx) to ZIP-y; gdy klient wyśle generyczny mime
/// (`application/zip`/`octet-stream`), rozpoznajemy konkretny format po
/// zawartości archiwum (obecność `xl/`, `word/`, `ppt/`). To magic-bytes na
/// poziomie struktury ZIP — pewniejsze niż sam mime, który bywa zgubiony.
fn ooxml_kind_from_zip(bytes: &[u8]) -> Option<SourceKind> {
    // Szybki filtr: ZIP zaczyna się od "PK\x03\x04" (albo pustego/spanned wariantu).
    if bytes.len() < 4 || &bytes[0..2] != b"PK" {
        return None;
    }
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut has_xl = false;
    let mut has_word = false;
    let mut has_ppt = false;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name();
        if name.starts_with("xl/") {
            has_xl = true;
        } else if name.starts_with("word/") {
            has_word = true;
        } else if name.starts_with("ppt/") {
            has_ppt = true;
        }
    }
    if has_xl {
        Some(SourceKind::Xlsx)
    } else if has_word {
        Some(SourceKind::Docx)
    } else if has_ppt {
        Some(SourceKind::Pptx)
    } else {
        None
    }
}

/// Klasyfikuje plik po mime, z fallbackiem na magic-bytes. Mime jest pierwszym
/// źródłem (tani, zwykle poprawny); gdy mime jest generyczny/nieznany, a bajty
/// wyglądają jak OOXML- ZIP albo PDF, dociągamy klasę z zawartości. Nieznane →
/// `Unknown` (router kieruje na port `unknown`, nie ingestuje jako śmieci UTF-8).
pub fn classify_source(mime: &str, bytes: &[u8]) -> SourceKind {
    let base = base_mime(mime);

    if base == "application/pdf" {
        return SourceKind::Pdf;
    }
    if base.starts_with("image/") {
        return SourceKind::Image;
    }
    if base == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        || base == "application/vnd.ms-excel"
    {
        return SourceKind::Xlsx;
    }
    if base == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        || base == "application/msword"
    {
        return SourceKind::Docx;
    }
    if base == "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        || base == "application/vnd.ms-powerpoint"
    {
        return SourceKind::Pptx;
    }
    if base.starts_with("text/") || base == "application/json" {
        return SourceKind::Text;
    }

    // Magic-bytes fallback dla zgubionego/generycznego mime.
    if bytes.starts_with(b"%PDF-") {
        return SourceKind::Pdf;
    }
    if let Some(kind) = ooxml_kind_from_zip(bytes) {
        return kind;
    }

    SourceKind::Unknown
}

/// Escapuje treść komórki tabeli markdown: `|` rozwala kolumny, znaki nowej
/// linii — wiersz; oba zamieniamy na bezpieczne odpowiedniki.
fn md_table_cell_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\r', " ")
        .replace('\n', "<br>")
        .trim()
        .to_string()
}

/// Formatuje liczbę z calamine bez notacji naukowej tam, gdzie to rozsądne.
fn xlsx_format_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// Zamienia pojedynczą komórkę calamine na tekst (liczby przez parser, nie OCR).
fn xlsx_cell_to_string(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => xlsx_format_float(*f),
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(d) => xlsx_format_float(d.as_f64()),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERR({e:?})"),
    }
}

/// Renderuje wiersze (pierwszy = nagłówek) jako tabelę markdown GFM. Puste
/// kolumny w nagłówku dostają zastępcze id `colN`, by GFM był poprawny.
fn render_gfm_table(out: &mut String, rows: &[Vec<String>], max_cols: usize) {
    if rows.is_empty() || max_cols == 0 {
        return;
    }
    let header = &rows[0];
    let header_cells: Vec<String> = (0..max_cols)
        .map(|i| {
            let c = header.get(i).map(|s| s.as_str()).unwrap_or("");
            let esc = md_table_cell_escape(c);
            if esc.is_empty() {
                format!("col{}", i + 1)
            } else {
                esc
            }
        })
        .collect();

    out.push('|');
    for h in &header_cells {
        out.push(' ');
        out.push_str(h);
        out.push_str(" |");
    }
    out.push('\n');
    out.push('|');
    for _ in 0..max_cols {
        out.push_str(" --- |");
    }
    out.push('\n');

    for row in &rows[1..] {
        out.push('|');
        for i in 0..max_cols {
            let c = row.get(i).map(|s| s.as_str()).unwrap_or("");
            out.push(' ');
            out.push_str(&md_table_cell_escape(c));
            out.push_str(" |");
        }
        out.push('\n');
    }
    out.push('\n');
}

/// XLSX → markdown: każdy arkusz jako `## <nazwa>` + tabela GFM. Pierwszy wiersz
/// arkusza = nagłówek. Puste arkusze pomijamy. Liczby przechodzą dokładnie jako
/// komórki tabeli (parser, nie OCR) — kluczowe dla danych liczbowych.
pub fn xlsx_to_markdown(bytes: &[u8]) -> Result<String, String> {
    use calamine::{Reader, Xlsx};

    let cursor = Cursor::new(bytes.to_vec());
    let mut workbook: Xlsx<_> = Xlsx::new(cursor).map_err(|e| format!("Błąd odczytu xlsx: {e}"))?;

    let sheet_names = workbook.sheet_names().to_vec();
    let mut out = String::new();

    for name in sheet_names {
        let range = workbook
            .worksheet_range(&name)
            .map_err(|e| format!("Błąd odczytu arkusza '{name}': {e}"))?;
        if range.is_empty() {
            continue;
        }

        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut max_cols = 0usize;
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(xlsx_cell_to_string).collect();
            if cells.iter().all(|c| c.trim().is_empty()) {
                continue;
            }
            max_cols = max_cols.max(cells.len());
            rows.push(cells);
        }
        if rows.is_empty() || max_cols == 0 {
            continue;
        }

        out.push_str("## ");
        out.push_str(&name);
        out.push_str("\n\n");
        render_gfm_table(&mut out, &rows, max_cols);
    }

    if out.trim().is_empty() {
        return Err("Plik xlsx nie zawiera danych".to_string());
    }
    Ok(out)
}

/// Local-name tagu XML bez prefiksu namespace (`w:p` → `p`).
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(p) => &name[p + 1..],
        None => name,
    }
}

/// DOCX → markdown: rozpakuj `word/document.xml` i przejdź po `w:p`/`w:tbl`.
/// Akapity (mapowanie styli Heading na `#`/`##`) i tabele (pierwszy wiersz jako
/// nagłówek). Zachowuje tabele z dokumentów.
pub fn docx_to_markdown(bytes: &[u8]) -> Result<String, String> {
    use quick_xml::events::Event;
    use quick_xml::Reader as XmlReader;

    let cursor = Cursor::new(bytes.to_vec());
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Błąd odczytu docx (zip): {e}"))?;
    let mut xml = String::new();
    {
        let mut file = archive
            .by_name("word/document.xml")
            .map_err(|e| format!("Brak word/document.xml w docx: {e}"))?;
        file.read_to_string(&mut xml)
            .map_err(|e| format!("Błąd odczytu document.xml: {e}"))?;
    }

    let mut reader = XmlReader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut out = String::new();

    let mut para_text = String::new();
    let mut para_heading: u8 = 0;
    let mut in_text = false;

    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut cur_row: Vec<String> = Vec::new();
    let mut cur_cell = String::new();
    let mut in_cell = false;

    fn flush_para(out: &mut String, para_text: &mut String, para_heading: &mut u8) {
        let t = para_text.trim();
        if !t.is_empty() {
            if *para_heading >= 1 {
                let hashes = "#".repeat((*para_heading).min(6) as usize);
                out.push_str(&hashes);
                out.push(' ');
            }
            out.push_str(t);
            out.push_str("\n\n");
        }
        para_text.clear();
        *para_heading = 0;
    }

    fn flush_table(out: &mut String, rows: &mut Vec<Vec<String>>) {
        if !rows.is_empty() {
            let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            render_gfm_table(out, rows, cols);
        }
        rows.clear();
    }

    // Wyciąga poziom nagłówka ze stylu `HeadingN` w atrybucie `w:val`.
    fn heading_from_pstyle(e: &quick_xml::events::BytesStart) -> Option<u8> {
        for attr in e.attributes().flatten() {
            if local_name(attr.key.as_ref()) == b"val" {
                if let Ok(val) = attr.unescape_value() {
                    if let Some(rest) = val
                        .strip_prefix("Heading")
                        .or_else(|| val.strip_prefix("heading"))
                    {
                        if let Ok(n) = rest.trim().parse::<u8>() {
                            return Some(n.clamp(1, 6));
                        }
                    }
                }
            }
        }
        None
    }

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(format!("Błąd parsowania document.xml: {e}")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let ln = local_name(e.name().as_ref()).to_vec();
                match ln.as_slice() {
                    b"tbl" => {
                        flush_para(&mut out, &mut para_text, &mut para_heading);
                        in_table = true;
                        table_rows.clear();
                    }
                    b"tr" if in_table => cur_row.clear(),
                    b"tc" if in_table => {
                        in_cell = true;
                        cur_cell.clear();
                    }
                    b"t" => in_text = true,
                    b"pStyle" => {
                        if let Some(n) = heading_from_pstyle(&e) {
                            para_heading = n;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let ln = local_name(e.name().as_ref()).to_vec();
                match ln.as_slice() {
                    b"pStyle" => {
                        if let Some(n) = heading_from_pstyle(&e) {
                            para_heading = n;
                        }
                    }
                    b"br" | b"tab" => {
                        if in_cell {
                            cur_cell.push(' ');
                        } else {
                            para_text.push(' ');
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_text => {
                let s = t
                    .unescape()
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(t.as_ref()).into_owned());
                if in_cell {
                    cur_cell.push_str(&s);
                } else {
                    para_text.push_str(&s);
                }
            }
            Ok(Event::End(e)) => {
                let ln = local_name(e.name().as_ref()).to_vec();
                match ln.as_slice() {
                    b"t" => in_text = false,
                    b"tc" if in_table => {
                        in_cell = false;
                        cur_row.push(cur_cell.trim().to_string());
                        cur_cell.clear();
                    }
                    b"tr" if in_table => {
                        if !cur_row.is_empty() {
                            table_rows.push(std::mem::take(&mut cur_row));
                        }
                    }
                    b"tbl" => {
                        in_table = false;
                        flush_table(&mut out, &mut table_rows);
                    }
                    b"p" if !in_table => {
                        flush_para(&mut out, &mut para_text, &mut para_heading);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }
    flush_para(&mut out, &mut para_text, &mut para_heading);

    if out.trim().is_empty() {
        return Err("Dokument docx nie zawiera tekstu".to_string());
    }
    Ok(out)
}

/// PPTX → markdown: każdy slajd jako `## Slajd N` + akapity z `a:t` (DrawingML).
/// PPTX to ZIP z `ppt/slides/slideN.xml`; sortujemy slajdy po numerze N, by
/// kolejność odpowiadała prezentacji. Tekst z każdego `a:p` (akapit) trafia w
/// osobnej linii — to wystarcza do ingestu RAG (chunking i tak skleja).
pub fn pptx_to_markdown(bytes: &[u8]) -> Result<String, String> {
    use quick_xml::events::Event;
    use quick_xml::Reader as XmlReader;

    let cursor = Cursor::new(bytes.to_vec());
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Błąd odczytu pptx (zip): {e}"))?;

    // Zbierz nazwy slajdów `ppt/slides/slideN.xml` i posortuj po numerze N.
    let mut slide_names: Vec<(u32, String)> = Vec::new();
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        if let Some(rest) = name.strip_prefix("ppt/slides/slide") {
            if let Some(num) = rest.strip_suffix(".xml") {
                if let Ok(n) = num.parse::<u32>() {
                    slide_names.push((n, name));
                }
            }
        }
    }
    if slide_names.is_empty() {
        return Err("Brak slajdów (ppt/slides/slideN.xml) w pptx".to_string());
    }
    slide_names.sort_by_key(|(n, _)| *n);

    let mut out = String::new();
    for (idx, (_, name)) in slide_names.iter().enumerate() {
        let mut xml = String::new();
        {
            let mut file = archive
                .by_name(name)
                .map_err(|e| format!("Błąd odczytu {name}: {e}"))?;
            file.read_to_string(&mut xml)
                .map_err(|e| format!("Błąd odczytu {name}: {e}"))?;
        }

        let mut reader = XmlReader::from_str(&xml);
        reader.config_mut().trim_text(false);

        let mut slide_text = String::new();
        let mut para = String::new();
        let mut in_text = false;
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Err(e) => return Err(format!("Błąd parsowania {name}: {e}")),
                Ok(Event::Eof) => break,
                Ok(Event::Start(e)) => match local_name(e.name().as_ref()) {
                    b"t" => in_text = true,
                    _ => {}
                },
                Ok(Event::Text(t)) if in_text => {
                    let s = t
                        .unescape()
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(t.as_ref()).into_owned());
                    para.push_str(&s);
                }
                Ok(Event::End(e)) => match local_name(e.name().as_ref()) {
                    b"t" => in_text = false,
                    // `a:p` = akapit DrawingML — kończy linię tekstu slajdu.
                    b"p" => {
                        let line = para.trim();
                        if !line.is_empty() {
                            slide_text.push_str(line);
                            slide_text.push('\n');
                        }
                        para.clear();
                    }
                    _ => {}
                },
                _ => {}
            }
            buf.clear();
        }
        let line = para.trim();
        if !line.is_empty() {
            slide_text.push_str(line);
            slide_text.push('\n');
        }

        if !slide_text.trim().is_empty() {
            out.push_str(&format!("## Slajd {}\n\n", idx + 1));
            out.push_str(slide_text.trim_end());
            out.push_str("\n\n");
        }
    }

    if out.trim().is_empty() {
        return Err("Prezentacja pptx nie zawiera tekstu".to_string());
    }
    Ok(out)
}

/// Dzieli markdown na chunki po zdaniach/akapitach z overlap. Pojedyncze zdanie
/// dłuższe niż `chunk_size` jest twardo łamane PRZED składaniem, by żaden chunk
/// nie przekroczył limitu kontekstu embeddingu. Portowane z addona RAG.
pub fn split_into_chunks(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let sentences = split_into_sentences(text);
    if sentences.is_empty() {
        return vec![text.trim().to_string()];
    }

    let seg_max = chunk_size.saturating_sub(overlap + 1).max(1);
    let segments: Vec<String> = sentences
        .iter()
        .flat_map(|s| hard_split(s, seg_max))
        .collect();

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for sentence in &segments {
        if current.chars().count() + sentence.chars().count() <= chunk_size || current.is_empty() {
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
            current.push_str(sentence);
        } else {
            chunks.push(current.trim().to_string());
            current = overlap_tail(chunks.last().unwrap(), overlap);
            if !current.is_empty() && !current.ends_with(' ') {
                current.push(' ');
            }
            current.push_str(sentence);
        }
    }

    let tail = current.trim().to_string();
    if !tail.is_empty() {
        chunks.push(tail);
    }
    if chunks.is_empty() {
        chunks.push(text.trim().to_string());
    }
    chunks
}

/// Twardo łamie nadwymiarowy segment na kawałki <= chunk_size (granice w
/// znakach, UTF-8 safe).
fn hard_split(segment: &str, chunk_size: usize) -> Vec<String> {
    if chunk_size == 0 || segment.chars().count() <= chunk_size {
        return vec![segment.to_string()];
    }
    let chars: Vec<char> = segment.chars().collect();
    chars
        .chunks(chunk_size)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

/// Zwraca ogon poprzedniego chunka (overlap) zaczynający się od granicy słowa.
fn overlap_tail(prev: &str, overlap: usize) -> String {
    if overlap == 0 {
        return String::new();
    }
    let chars: Vec<char> = prev.chars().collect();
    if chars.len() <= overlap {
        return prev.to_string();
    }
    let start = chars.len() - overlap;
    let tail: String = chars[start..].iter().collect();
    match tail.find(' ') {
        Some(pos) => tail[pos + 1..].to_string(),
        None => tail,
    }
}

/// Rozdziela tekst na zdania po (. ! ?) i granicach akapitów (podwójny newline).
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];
        if ch == '\n' && i + 1 < len && chars[i + 1] == '\n' {
            if !current.trim().is_empty() {
                sentences.push(current.trim().to_string());
                current.clear();
            }
            i += 2;
            continue;
        }
        current.push(ch);
        if (ch == '.' || ch == '!' || ch == '?')
            && (i + 1 >= len || chars[i + 1] == ' ' || chars[i + 1] == '\n')
        {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
                current.clear();
            }
        }
        i += 1;
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }
    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_mime_basic_paths() {
        assert_eq!(classify_source("application/pdf", &[]), SourceKind::Pdf);
        assert_eq!(
            classify_source("application/pdf; charset=binary", &[]),
            SourceKind::Pdf
        );
        assert_eq!(classify_source("image/png", &[]), SourceKind::Image);
        assert_eq!(classify_source("text/plain; charset=utf-8", &[]), SourceKind::Text);
        assert_eq!(classify_source("application/json", &[]), SourceKind::Text);
        assert_eq!(
            classify_source(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                &[]
            ),
            SourceKind::Xlsx
        );
        assert_eq!(
            classify_source(
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                &[]
            ),
            SourceKind::Pptx
        );
        assert_eq!(classify_source("application/x-tar", &[]), SourceKind::Unknown);
    }

    #[test]
    fn classify_by_magic_bytes_when_mime_generic() {
        // Generyczny mime + magic %PDF- → PDF.
        assert_eq!(
            classify_source("application/octet-stream", b"%PDF-1.7\n..."),
            SourceKind::Pdf
        );
        // Nie-ZIP, nie-PDF, generyczny mime → Unknown.
        assert_eq!(
            classify_source("application/octet-stream", b"hello world"),
            SourceKind::Unknown
        );
    }

    #[test]
    fn split_into_chunks_never_exceeds_size_for_oversized_sentence() {
        let long = "a".repeat(5000);
        let chunks = split_into_chunks(&long, 1000, 100);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.chars().count() <= 1000, "chunk len {}", c.chars().count());
        }
    }

    #[test]
    fn split_into_chunks_empty_text_is_empty() {
        assert!(split_into_chunks("   \n\n  ", CHUNK_SIZE_CHARS, CHUNK_OVERLAP_CHARS).is_empty());
    }

    #[test]
    fn split_into_chunks_basic_paragraphs() {
        let text = "Pierwsze zdanie. Drugie zdanie.\n\nTrzeci akapit jest tutaj.";
        let chunks = split_into_chunks(text, 2048, 200);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("Pierwsze zdanie"));
        assert!(chunks[0].contains("Trzeci akapit"));
    }
}
