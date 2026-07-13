// =============================================================================
// Plik: flow_engine/node_adapters/page_branch.rs
// Opis: Wspólne narzędzia gałęzi PDF (cardinality 1:1 bez fan-out). Strony PDF
//       z `pdf_rasterize` jadą jako JEDEN envelope `Json{pages:[blob_refs]}`;
//       węzły `*_pages` (vision_parse_pages / page_detect_pages / ocr_pages)
//       iterują po tej liście i emitują wzbogaconą listę
//       `Json{pages:[{index,markdown|blocks}]}` zgodną z `document_merge`.
//       Ten moduł parsuje wejściowe blob-refy stron — jeden kontrakt dla
//       wszystkich węzłów batch.
// =============================================================================

use anyhow::{anyhow, Result};

use crate::flow_engine::blob_store::BlobRef;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};

/// Jedna strona PDF jako blob-ref + jej indeks (kolejność czytania). Indeks
/// niesiemy dalej w wyjściowych `pages[].index`, żeby `document_merge` (i
/// ewentualny fan-in z kilku gałęzi) potrafił dopasować strony.
pub(crate) struct PageBlob {
    pub index: u64,
    pub blob_ref: BlobRef,
}

/// Parsuje payload `Json{pages:[{index,blob_id,sha256,size_bytes,mime}]}`
/// (kształt z `pdf_rasterize`) na listę `PageBlob`. Strony bez `blob_id` to
/// błąd kształtu — nie da się ich pobrać. Mime domyślnie `image/png` (tak
/// renderuje rasteryzer); rozmiar/sha opcjonalne dla pobrania, ale wymagamy ich
/// obecności tylko jeśli były (defensywnie ufamy producentowi z tego samego
/// flow).
pub(crate) fn parse_page_blobs(envelope: &FlowEnvelope) -> Result<Vec<PageBlob>> {
    let obj = match &envelope.payload {
        FlowValue::Json(v) => v,
        other => {
            return Err(anyhow!(
                "page-branch: payload musi być Json{{pages:[...]}}, dostał {}",
                other.kind()
            ))
        }
    };
    let items = obj
        .get("pages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("page-branch: payload Json bez 'pages' (tablica)"))?;
    if items.is_empty() {
        return Err(anyhow!("page-branch: pusta lista 'pages'"));
    }

    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(|v| v.as_u64())
            .unwrap_or(i as u64);
        let id = item
            .get("blob_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("page-branch: strona[{i}] brak 'blob_id'"))?
            .to_string();
        let mime = item
            .get("mime")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("image/png")
            .to_string();
        let sha256 = item
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let size_bytes = item.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push(PageBlob {
            index,
            blob_ref: BlobRef {
                id,
                size_bytes,
                mime,
                sha256,
            },
        });
    }
    Ok(out)
}
