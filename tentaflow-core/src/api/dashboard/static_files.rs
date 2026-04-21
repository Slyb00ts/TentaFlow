// =============================================================================
// Plik: api/dashboard/static_files.rs
// Opis: Serwowanie plikow statycznych wbudowanych w binarie z katalogu wwwroot/.
//       Pliki sa generowane przez build.rs (rerun-if-changed=wwwroot) co gwarantuje
//       automatyczna rekompilacje po zmianie jakiegokolwiek pliku www.
// =============================================================================

// Wygenerowany przez build.rs — mapa sciezka -> (content_type, bytes)
include!(concat!(env!("OUT_DIR"), "/wwwroot_embed.rs"));

/// Zwraca (status, content_type, body_bytes) dla podanej sciezki HTTP.
/// Pliki sa wbudowane w binarie — zero zaleznosci od systemu plikow.
pub fn serve(path: &str) -> (u16, &'static str, Vec<u8>) {
    // Normalizuj sciezke — domyslnie index.html
    let clean_path = match path {
        "/" | "" => "index.html",
        p => p.trim_start_matches('/'),
    };

    // Zdekoduj URL-encoded znaki przed sprawdzeniem path traversal
    let decoded = urlencoding::decode(clean_path).unwrap_or_default();

    // Zabezpiecz przed path traversal (surowy i zdekodowany)
    if clean_path.contains("..") || decoded.contains("..") || decoded.contains('\0') {
        return (403, "text/plain", b"Forbidden".to_vec());
    }

    if let Some((content_type, data)) = wwwroot_lookup(clean_path) {
        return (200, content_type, data.to_vec());
    }

    // SPA fallback tylko dla sciezek-routes (bez rozszerzenia lub .html).
    // Dla assetow (.js, .css, .wasm, .png itd.) zwracamy 404, zeby przegladarka
    // nie dostala HTML pod zadanie modulu JS (co lami MIME checking ES modules).
    let is_asset = clean_path
        .rsplit('/')
        .next()
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, ext)| !ext.eq_ignore_ascii_case("html"))
        .unwrap_or(false);

    if is_asset {
        return (404, "text/plain", b"Not Found".to_vec());
    }

    if let Some((content_type, data)) = wwwroot_lookup("index.html") {
        (200, content_type, data.to_vec())
    } else {
        (404, "text/plain", b"Not Found".to_vec())
    }
}
