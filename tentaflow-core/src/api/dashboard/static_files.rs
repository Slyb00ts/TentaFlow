// =============================================================================
// Plik: api/dashboard/static_files.rs
// Opis: Serwowanie plikow statycznych wbudowanych w binarie z katalogu www/.
//       Pliki sa generowane przez build.rs (rerun-if-changed=www) co gwarantuje
//       automatyczna rekompilacje po zmianie jakiegokolwiek pliku dashboardu.
// =============================================================================

// Wygenerowany przez build.rs — mapa sciezka -> (content_type, bytes).
// Nazwa pliku pozostala historyczna po usunieciu wwwroot/.
include!(concat!(env!("OUT_DIR"), "/wwwroot_embed.rs"));

// Wygenerowany przez build.rs — zbiorczy SHA-256 calego frontu. Serwer wysyla go
// w MetaSchemaVersionAck; front porownuje z wlasnym (zbakowanym w
// asset-manifest.js) i przy roznicy proponuje reload (nieaktualny front po
// aktualizacji backendu/addonu).
include!(concat!(env!("OUT_DIR"), "/asset_build_hash.rs"));

/// Content-type from a file extension, for the disk dev path (the embedded map
/// carries its own types).
fn disk_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// Reads `<dir>/<rel>` from disk for the TENTAFLOW_WWW_DIR dev path. `None` if the
/// file is absent (caller falls back to the embedded copy).
fn serve_from_disk(dir: &str, rel: &str) -> Option<(u16, &'static str, Vec<u8>)> {
    let full = std::path::Path::new(dir).join(rel);
    std::fs::read(&full).ok().map(|bytes| (200u16, disk_mime(rel), bytes))
}

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

    // Dev affordance: when TENTAFLOW_WWW_DIR points at a www/ source tree, serve files
    // from disk per-request so frontend edits show on a browser refresh WITHOUT a
    // rebuild. The embedded copy stays the production path (env unset → zero overhead).
    if let Ok(dir) = std::env::var("TENTAFLOW_WWW_DIR") {
        if !dir.is_empty() {
            if let Some(resp) = serve_from_disk(&dir, clean_path) {
                return resp;
            }
        }
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
