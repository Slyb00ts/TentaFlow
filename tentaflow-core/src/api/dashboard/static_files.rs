// =============================================================================
// Plik: api/dashboard/static_files.rs
// Opis: Serwowanie plikow statycznych wbudowanych w binarie z katalogu www/.
//       Pliki sa generowane przez build.rs (rerun-if-changed=www) co gwarantuje
//       automatyczna rekompilacje po zmianie jakiegokolwiek pliku dashboardu.
// =============================================================================

// Wygenerowany przez build.rs — mapa sciezka -> (content_type, bytes).
// Nazwa pliku pozostala historyczna po usunieciu wwwroot/.
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

include!(concat!(env!("OUT_DIR"), "/wwwroot_embed.rs"));

// Wygenerowany przez build.rs — zbiorczy SHA-256 calego frontu. Serwer wysyla go
// w MetaSchemaVersionAck; front porownuje z wlasnym (zbakowanym w
// asset-manifest.js) i przy roznicy proponuje reload (nieaktualny front po
// aktualizacji backendu/addonu).
include!(concat!(env!("OUT_DIR"), "/asset_build_hash.rs"));

/// The asset hash that describes the bytes this process actually serves.
///
/// In `TENTAFLOW_WWW_DIR` mode the front is served from disk while the constant
/// baked at compile time describes the tree as it was at build time, so the two
/// drift apart on the first frontend edit and the dashboard's update prompt can
/// never be satisfied by a reload. Read it from the same generated file the
/// browser loads, so both sides always compare the same value.
pub fn asset_build_hash() -> String {
    if let Ok(dir) = std::env::var("TENTAFLOW_WWW_DIR") {
        if !dir.is_empty() {
            let manifest =
                std::path::Path::new(&dir).join("js/generated/asset-manifest.js");
            if let Ok(text) = std::fs::read_to_string(&manifest) {
                if let Some(hash) = parse_manifest_hash(&text) {
                    return hash;
                }
            }
        }
    }
    ASSET_BUILD_HASH.to_string()
}

/// Extracts the value of `export const ASSET_BUILD_HASH = "<hex>";`.
fn parse_manifest_hash(text: &str) -> Option<String> {
    let tail = text.split_once("ASSET_BUILD_HASH")?.1;
    let tail = tail.split_once('"')?.1;
    let (hash, _) = tail.split_once('"')?;
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(hash.to_string())
}

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
/// file is absent (caller falls back to the embedded copy). ETag = per-file
/// SHA-256 (16 hex) liczony w locie — dev sciezka tez wspiera warunkowe GET.
fn serve_from_disk(dir: &str, rel: &str) -> Option<(u16, &'static str, Bytes, String)> {
    let full = std::path::Path::new(dir).join(rel);
    std::fs::read(&full).ok().map(|bytes| {
        let etag = disk_etag(&bytes);
        (200u16, disk_mime(rel), Bytes::from(bytes), etag)
    })
}

/// SHA-256 (16 hex) dla sciezki dyskowej (dev). Embedded pliki maja etag
/// wyliczony w build.rs, wiec runtime hashuje tylko przy TENTAFLOW_WWW_DIR.
fn disk_etag(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// Resolves (status, content_type, body, etag) for an HTTP path. Embedded files are
/// served as `&'static [u8]` without a copy.
/// Pliki sa wbudowane w binarie — zero zaleznosci od systemu plikow. `etag`
/// jest pusty dla odpowiedzi bez cache (403/404).
fn resolve(path: &str) -> (u16, &'static str, Bytes, String) {
    // Normalizuj sciezke — domyslnie index.html
    let clean_path = match path {
        "/" | "" => "index.html",
        p => p.trim_start_matches('/'),
    };

    // Zdekoduj URL-encoded znaki przed sprawdzeniem path traversal
    let decoded = urlencoding::decode(clean_path).unwrap_or_default();

    // Zabezpiecz przed path traversal (surowy i zdekodowany)
    if clean_path.contains("..") || decoded.contains("..") || decoded.contains('\0') {
        return (403, "text/plain", Bytes::from_static(b"Forbidden"), String::new());
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

    if let Some((content_type, data, etag)) = wwwroot_lookup(clean_path) {
        return (200, content_type, Bytes::from_static(data), etag.to_string());
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
        return (404, "text/plain", Bytes::from_static(b"Not Found"), String::new());
    }

    if let Some((content_type, data, etag)) = wwwroot_lookup("index.html") {
        (200, content_type, Bytes::from_static(data), etag.to_string())
    } else {
        (404, "text/plain", Bytes::from_static(b"Not Found"), String::new())
    }
}

/// What the HTTP layer needs to answer a static request.
pub struct StaticAsset {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Bytes,
    /// Empty for responses that must not be cached (403/404).
    pub etag: String,
    pub gzipped: bool,
}

/// Bodies below this size are not worth the CPU or the extra header bytes.
const MIN_COMPRESSIBLE_BYTES: usize = 1024;

/// Compressed bodies by content ETag. The ETag is a hash of the bytes, so one
/// entry serves every path with that content; in disk mode an edited file gets a
/// new ETag, hence the cap.
static GZIP_CACHE: OnceLock<Mutex<HashMap<String, Bytes>>> = OnceLock::new();
const GZIP_CACHE_MAX_ENTRIES: usize = 1024;

pub fn is_compressible(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || ["javascript", "json", "xml", "svg", "wasm"]
            .iter()
            .any(|kind| content_type.contains(kind))
}

/// True when `Accept-Encoding` lists gzip with a non-zero quality.
pub fn accepts_gzip(header: &str) -> bool {
    header.split(',').any(|item| {
        let mut parts = item.split(';');
        let coding = parts.next().unwrap_or("").trim();
        if !coding.eq_ignore_ascii_case("gzip") && coding != "*" {
            return false;
        }
        parts
            .filter_map(|p| p.trim().strip_prefix("q="))
            .all(|q| q.trim().parse::<f32>().map_or(true, |q| q > 0.0))
    })
}

fn gzip_bytes(body: &[u8]) -> Option<Bytes> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::with_capacity(body.len() / 3), Compression::best());
    encoder.write_all(body).ok()?;
    let packed = encoder.finish().ok()?;
    (packed.len() < body.len()).then(|| Bytes::from(packed))
}

/// Static file for `path`, gzip-compressed when the client accepts it and the
/// type benefits. Compression runs once per content on the blocking pool.
pub async fn serve(path: &str, accepts_gzip: bool) -> StaticAsset {
    let (status, content_type, body, etag) = resolve(path);
    let mut asset = StaticAsset { status, content_type, body, etag, gzipped: false };
    if !accepts_gzip
        || status != 200
        || asset.etag.is_empty()
        || asset.body.len() < MIN_COMPRESSIBLE_BYTES
        || !is_compressible(content_type)
    {
        return asset;
    }
    let cache = GZIP_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = cache.lock().ok().and_then(|map| map.get(&asset.etag).cloned());
    let packed = match cached {
        Some(packed) => Some(packed),
        None => {
            let source = asset.body.clone();
            let packed = tokio::task::spawn_blocking(move || gzip_bytes(&source))
                .await
                .ok()
                .flatten();
            if let (Some(packed), Ok(mut map)) = (packed.as_ref(), cache.lock()) {
                if map.len() >= GZIP_CACHE_MAX_ENTRIES {
                    map.clear();
                }
                map.insert(asset.etag.clone(), packed.clone());
            }
            packed
        }
    };
    if let Some(packed) = packed {
        asset.body = packed;
        asset.gzipped = true;
    }
    asset
}

#[cfg(test)]
mod compression_tests {
    use super::*;

    #[test]
    fn accept_encoding_negotiation() {
        assert!(accepts_gzip("gzip"));
        assert!(accepts_gzip("br, gzip;q=0.5"));
        assert!(accepts_gzip("*"));
        assert!(!accepts_gzip("br, deflate"));
        assert!(!accepts_gzip("gzip;q=0"));
        assert!(!accepts_gzip(""));
    }

    #[test]
    fn only_text_like_types_are_compressed() {
        assert!(is_compressible("text/javascript; charset=utf-8"));
        assert!(is_compressible("application/wasm"));
        assert!(is_compressible("image/svg+xml"));
        assert!(!is_compressible("image/png"));
        assert!(!is_compressible("font/woff2"));
    }

    #[test]
    fn gzip_round_trips_and_skips_incompressible_input() {
        use std::io::Read;
        let text = "tentaflow ".repeat(500);
        let packed = gzip_bytes(text.as_bytes()).expect("repetitive text shrinks");
        let mut unpacked = String::new();
        flate2::read::GzDecoder::new(&packed[..]).read_to_string(&mut unpacked).unwrap();
        assert_eq!(unpacked, text);
        assert!(gzip_bytes(b"x").is_none());
    }
}
