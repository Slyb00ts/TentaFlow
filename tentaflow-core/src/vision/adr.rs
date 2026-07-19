// =============================================================================
// Plik: vision/adr.rs
// Opis: Lookup tablic ADR (pomarańczowa plansza „kemler/UN") — mapuje odczytany
//       numer UN na kanoniczny wpis z `adr-list.json` (kemler + UN + opis
//       ładunku). Sam OCR robi ogólny silnik PP-OCRv5 (patrz `onnx_ocr.rs`);
//       ten moduł tylko dopasowuje jego linie do listy dozwolonych pozycji.
// =============================================================================

use serde::Deserialize;
use tracing::warn;

use crate::paths;

/// Kształt `adr-list.json` — lista dozwolonych pozycji ADR.
#[derive(Debug, Deserialize)]
struct ListaAdr {
    pary: Vec<ParaAdr>,
}

/// Pojedyncza pozycja ADR: górny wiersz planszy (kemler), dolny wiersz (numer
/// UN) oraz opis ładunku (do prezentacji we froncie).
#[derive(Debug, Deserialize)]
struct ParaAdr {
    kemler: String,
    un: String,
    opis: String,
}

/// Maksymalna odległość edycyjna dolnego wiersza (numeru UN) do wpisu z listy,
/// dopuszczana przy snapie. Powyżej niej odczyt uznajemy za zbyt niepewny i
/// zwracamy `None` (bez zgadywania). UN to 4 cyfry, które OCR czyta najpewniej,
/// dlatego próg jest ciasny.
const MAX_ODLEGLOSC_UN: usize = 1;

/// Wczytuje listę dozwolonych pozycji ADR z `<vision_models_dir>/adr-list.json`
/// (katalog `.runtime/`, gitignorowany). W kodzie źródłowym NIE ma żadnej listy
/// wbudowanej — gdy pliku brak, jest pusty lub niepoprawny, zwracamy pustą listę,
/// a [`snap_adr`] nie zwróci wtedy żadnego dopasowania (ADR nie jest pokazywany).
/// Każdy wpis niesie kemler, numer UN oraz opis ładunku (do prezentacji).
fn wczytaj_liste_adr() -> Vec<(String, String, String)> {
    let sciezka = paths::vision_models_dir().join("adr-list.json");
    match std::fs::read(&sciezka) {
        Ok(bytes) => match serde_json::from_slice::<ListaAdr>(&bytes) {
            Ok(lista) => lista
                .pary
                .into_iter()
                .map(|p| (p.kemler, p.un, p.opis))
                .collect(),
            Err(e) => {
                warn!(
                    "[adr] {} istnieje, ale nie udało się go sparsować ({e}) — lista ADR pusta",
                    sciezka.display()
                );
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Z listy linii OCR (posortowanych góra→dół) wyłuskuje numer UN (dolna 3-4
/// cyfrowa grupa) i snapuje do `adr-list.json`. Górny wiersz (kemler) bierzemy
/// z trafionego wpisu listy — OCR górnego rzędu bywa mylony, więc mu nie ufamy.
/// Zwraca `"<kemler>/<UN> <opis>"` albo `None`, gdy żadna linia nie ma sensownej
/// 3-4 cyfrowej grupy lub odczyt nie da się dopasować do listy.
pub fn snap_adr_from_lines(lines: &[String]) -> Option<String> {
    // Kandydat na UN to NAJNIŻSZA (ostatnia w kolejności góra→dół) linia, w
    // której same cyfry ASCII dają grupę 3-4 znaków. `snap_adr` i tak dopasowuje
    // po numerze UN, więc górny wiersz (kemler) jest pomijany.
    let un = lines
        .iter()
        .rev()
        .filter_map(|line| {
            let cyfry: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
            (3..=4).contains(&cyfry.len()).then_some(cyfry)
        })
        .next()?;
    snap_adr(&un)
}

/// Dociąga surowy odczyt OCR do najbliższej znanej pozycji ADR. Ponieważ numery
/// UN są w liście unikalne i rozłączne, a OCR czyta dolny wiersz (4 cyfry)
/// najpewniej, dopasowujemy GŁÓWNIE po UN: wybieramy wpis o minimalnej
/// odległości Levenshteina UN do odczytu dolnego wiersza. Górny wiersz (kemler)
/// bywa mylony (np. „301" zamiast „30"), więc kemler bierzemy z TRAFIONEGO wpisu,
/// nie z OCR. Gdy najlepsze dopasowanie przekracza [`MAX_ODLEGLOSC_UN`] — zwraca
/// `None`. Zwraca `"<kemler>/<un> <opis>"` z listy (opis po separatorze-spacji,
/// żeby front mógł go odczepić bez żadnych danych wbudowanych po swojej stronie).
/// Gdy lista pusta (brak pliku `adr-list.json`) — zawsze `None`.
pub fn snap_adr(dolny: &str) -> Option<String> {
    if dolny.is_empty() {
        return None;
    }
    let lista = wczytaj_liste_adr();
    if lista.is_empty() {
        return None;
    }
    let mut najlepsza: Option<(usize, &(String, String, String))> = None;
    for para in &lista {
        let dist = levenshtein(dolny, &para.1);
        let lepsza = match najlepsza {
            Some((d, _)) => dist < d,
            None => true,
        };
        if lepsza {
            najlepsza = Some((dist, para));
        }
    }
    match najlepsza {
        Some((dist, (kemler, un, opis))) if dist <= MAX_ODLEGLOSC_UN => {
            if opis.is_empty() {
                Some(format!("{kemler}/{un}"))
            } else {
                Some(format!("{kemler}/{un} {opis}"))
            }
        }
        _ => None,
    }
}

/// Odległość edycyjna Levenshteina (wstawienia/usunięcia/podmiany) między dwoma
/// napisami — na bajtach ASCII, bo numery ADR to wyłącznie cyfry.
fn levenshtein(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // Jednowierszowa tablica DP (poprzedni wiersz odległości).
    let mut poprzedni: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut lewo_gora = poprzedni[0];
        poprzedni[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let koszt = usize::from(ca != cb);
            let nowa = (poprzedni[j + 1] + 1)
                .min(poprzedni[j] + 1)
                .min(lewo_gora + koszt);
            lewo_gora = poprzedni[j + 1];
            poprzedni[j + 1] = nowa;
        }
    }
    poprzedni[b.len()]
}
