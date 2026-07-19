// =============================================================================
// File: protocol/document.rs — document/blob store host-function ABI payloads
// Purpose: single source of truth for the CBOR request/response structs of the
// `document_put_v1`, `document_get_v1`, `document_delete_v1` and
// `document_list_v1` host functions (RAG E1.3 — per-instance blob store for
// addon file uploads). Shared verbatim by the core host (decode input / encode
// output) and the addon SDK (encode input / decode output) so the wire format
// cannot drift.
//
// Chunking: surowe pliki (PDF/obraz) bywają > limit payloadu CBOR. Bajty kawałka
// NIE jadą w CBOR — wchodzą osobnym ptr/len przez `read_guest_bytes`. CBOR niesie
// tylko metadane kawałka (doc_id, mime, chunk_index, total_chunks) i — w wyjściu
// get — `total_chunks`. Maps używają kluczy całkowitych przez `#[cbor(map)]`.
// =============================================================================

use minicbor::{Decode, Encode};

// -----------------------------------------------------------------------------
// document_put_v1
// -----------------------------------------------------------------------------

/// Input metadanych dla `document_put_v1`. Sam bajty kawałka idą osobnym
/// ptr/len (nie w CBOR), żeby duży plik nie rozsadził sufitu payloadu CBOR.
///
/// `doc_id` pusty na PIERWSZYM kawałku (`chunk_index == 0`) → host wygeneruje
/// nowy identyfikator i zwróci go w `DocumentPutOutput.doc_id`; addon MUSI go
/// podać na kolejnych kawałkach. `total_chunks >= 1`; finalizacja zapisu
/// następuje gdy `chunk_index == total_chunks - 1`. `mime` brany z pierwszego
/// kawałka.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentPutInput {
    #[n(0)]
    pub doc_id: String,
    #[n(1)]
    pub mime: String,
    #[n(2)]
    pub chunk_index: u32,
    #[n(3)]
    pub total_chunks: u32,
}

/// Output `document_put_v1`. `doc_id` to identyfikator dokumentu (wygenerowany
/// na pierwszym kawałku albo echo podanego). `finalized=true` dopiero po
/// ostatnim kawałku — wtedy `size_bytes`/`sha256` opisują kompletny plik;
/// dla kawałków pośrednich `finalized=false`, a `size_bytes`/`sha256` zerowe.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentPutOutput {
    #[n(0)]
    pub doc_id: String,
    #[n(1)]
    pub finalized: bool,
    #[n(2)]
    pub size_bytes: u64,
    #[n(3)]
    pub sha256: String,
}

// -----------------------------------------------------------------------------
// document_get_v1
// -----------------------------------------------------------------------------

/// Input `document_get_v1`. Czyta dokument po `doc_id` kawałkami: `chunk_index`
/// wskazuje który kawałek odczytać. Rozmiar kawałka wyznacza host (stały, patrz
/// `DOCUMENT_CHUNK_BYTES`); addon iteruje `chunk_index` od 0 do
/// `total_chunks - 1` zwróconego w wyjściu.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentGetInput {
    #[n(0)]
    pub doc_id: String,
    #[n(1)]
    pub chunk_index: u32,
}

/// Output metadanych `document_get_v1`. Bajty kawałka jadą osobnym buforem
/// (out_ptr), a tutaj wracają tylko metadane: `total_chunks` (ile kawałków ma
/// cały plik), `chunk_len` (długość TEGO kawałka), `mime` i `size_bytes`
/// (rozmiar całego dokumentu). Sam payload bajtowy jest zapisany do bufora
/// guest pod `out_ptr` przed metadanymi (host robi to w jednym wywołaniu — patrz
/// opis ABI w document.rs).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentGetMeta {
    #[n(0)]
    pub total_chunks: u32,
    #[n(1)]
    pub chunk_len: u32,
    #[n(2)]
    pub mime: String,
    #[n(3)]
    pub size_bytes: u64,
}

// -----------------------------------------------------------------------------
// document_delete_v1
// -----------------------------------------------------------------------------

/// Input `document_delete_v1`. Kasuje plik + wpis rejestru po `doc_id`.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentDeleteInput {
    #[n(0)]
    pub doc_id: String,
}

/// Output `document_delete_v1`. `removed=true` gdy dokument istniał i został
/// usunięty; `false` gdy `doc_id` nie istniał (delete idempotentny).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentDeleteOutput {
    #[n(0)]
    pub doc_id: String,
    #[n(1)]
    pub removed: bool,
}

// -----------------------------------------------------------------------------
// document_list_v1
// -----------------------------------------------------------------------------

/// Input `document_list_v1`. Brak pól — listuje wszystkie dokumenty instancji.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentListInput {}

/// Pozycja w wyniku `document_list_v1` — metadane jednego dokumentu instancji.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentMeta {
    #[n(0)]
    pub doc_id: String,
    #[n(1)]
    pub mime: String,
    #[n(2)]
    pub size_bytes: u64,
    #[n(3)]
    pub sha256: String,
    #[n(4)]
    pub created_at: String,
    /// Zaufany marker kanału uploadu (np. `audio_capture` z renderera
    /// AudioCapture); pusty/None dla zwykłych uploadów i put-ów addonu.
    #[n(5)]
    pub source: Option<String>,
}

/// Output `document_list_v1` — lista dokumentów scoped do (org, instancja).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct DocumentListOutput {
    #[n(0)]
    pub documents: Vec<DocumentMeta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T)
    where
        T: Encode<()> + for<'b> Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut buf = Vec::new();
        minicbor::encode(value, &mut buf).unwrap();
        let decoded: T = minicbor::decode(&buf).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn roundtrip_put_input() {
        roundtrip(&DocumentPutInput {
            doc_id: String::new(),
            mime: "application/pdf".into(),
            chunk_index: 0,
            total_chunks: 3,
        });
        roundtrip(&DocumentPutInput {
            doc_id: "doc-abc".into(),
            mime: "application/pdf".into(),
            chunk_index: 2,
            total_chunks: 3,
        });
    }

    #[test]
    fn roundtrip_put_output() {
        roundtrip(&DocumentPutOutput {
            doc_id: "doc-abc".into(),
            finalized: true,
            size_bytes: 1_500_000,
            sha256: "deadbeef".into(),
        });
        roundtrip(&DocumentPutOutput {
            doc_id: "doc-abc".into(),
            finalized: false,
            size_bytes: 0,
            sha256: String::new(),
        });
    }

    #[test]
    fn roundtrip_get_meta() {
        roundtrip(&DocumentGetMeta {
            total_chunks: 3,
            chunk_len: 65536,
            mime: "application/pdf".into(),
            size_bytes: 1_500_000,
        });
    }

    #[test]
    fn roundtrip_delete() {
        roundtrip(&DocumentDeleteInput {
            doc_id: "doc-abc".into(),
        });
        roundtrip(&DocumentDeleteOutput {
            doc_id: "doc-abc".into(),
            removed: true,
        });
    }

    #[test]
    fn roundtrip_list() {
        roundtrip(&DocumentListInput {});
        roundtrip(&DocumentListOutput {
            documents: vec![DocumentMeta {
                doc_id: "doc-abc".into(),
                mime: "application/pdf".into(),
                size_bytes: 1_500_000,
                sha256: "deadbeef".into(),
                created_at: "2026-06-21T10:00:00Z".into(),
                source: Some("audio_capture".into()),
            }],
        });
    }
}
