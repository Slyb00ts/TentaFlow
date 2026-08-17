// ===== Plik: services/graph/ppr.rs — Personalized PageRank w Rust nad CSR =====
//
// Cozo ma wbudowany PageRank, ale BEZ wektora personalizacji (potwierdzone w
// źródle spike'a). MemGraphRAG §4.3.3 wymaga PPR z `P_init` skupionym na seed-
// subgrafie, więc liczymy go tu: bounded power-iteration nad CSR wyeksportowanym
// z Cozo (`backend::export_edges`). Funkcja jest REALNA (nie stub) — pełne
// wpięcie host-fn `graph_ppr_v1` przychodzi w slice B1.
//
// Model: na każdym kroku masa `damping` jest rozprowadzana po out-krawędziach
// PROPORCJONALNIE DO WAGI krawędzi (graf faktów jest ważony — poprawka codex
// pkt 7), a `(1 - damping)` teleportuje z powrotem na wektor personalizacji
// (seedy). Węzły bez out-krawędzi (dangling) oddają swoją masę z powrotem do
// teleportacji (rozłożoną wg wektora personalizacji), żeby zachować sumę
// prawdopodobieństwa. Węzeł, którego wszystkie out-krawędzie mają wagę 0, jest
// traktowany jak dangling (brak kierunku rozkładu).

use super::csr::Csr;

/// Wynik PPR: id węzła + wynik, posortowane malejąco po wyniku.
pub type PprScores = Vec<(String, f64)>;

/// Personalized PageRank power-iteration nad CSR.
///
/// `seeds` to WAŻONE indeksy węzłów `(idx, weight)` stanowiące wektor
/// personalizacji (rozkład teleportacji). Wagi seedów sterują rozkładem
/// teleportu (MemGraphRAG §6.2, R6): kotwica o wyższej wadze przyciąga więcej
/// masy, więc silniej kształtuje ranking. Reguły budowy wektora:
///   * indeksy poza zakresem `< n` są ignorowane,
///   * powtórzone indeksy są deduplikowane przez SUMOWANIE wag (ten sam węzeł
///     podany dwa razy = jedna kotwica o połączonej wadze, nie podwójny teleport),
///   * wagi `<= 0` (oraz NaN) są odfiltrowane — kotwica bez dodatniej masy nie
///     personalizuje teleportu,
///   * po czyszczeniu wagi są normalizowane do sumy 1 (zachowanie sumy
///     prawdopodobieństwa).
/// Gdy po czyszczeniu NIE zostaje żadna ważna kotwica (pusto, same nieznane
/// indeksy, same niedodatnie wagi) => jednostajna teleportacja po wszystkich
/// węzłach (degeneruje do zwykłego PageRanku). `damping` w (0,1) (typowo 0.85),
/// `iters` to liczba iteracji power-iteration.
///
/// UWAGA: ta funkcja jest niskopoziomowa i celowo NIE rozróżnia „seedów nie
/// podano" od „podano same nieznane id" — z jej perspektywy oba to pusty wektor
/// → uniform. Rozróżnienie semantyczne (retrieval z jawnymi kotwicami: same
/// nieznane seedy → PUSTY wynik, nie uniform) egzekwuje `GraphManager::ppr`
/// PRZED wywołaniem tej funkcji.
///
/// Zwraca pełny wektor `(id, score)` posortowany malejąco. Pruning top-N robi
/// warstwa wyżej (host-fn / retrieval).
pub fn personalized_pagerank(
    csr: &Csr,
    seeds: &[(usize, f64)],
    damping: f64,
    iters: usize,
) -> PprScores {
    let n = csr.node_count();
    if n == 0 {
        return Vec::new();
    }
    let damping = damping.clamp(0.0, 1.0);

    // Wektor personalizacji (teleportacja) z WAG seedów. Dedup po indeksie
    // SUMUJE wagi (ten sam węzeł = jedna kotwica), wagi niedodatnie/NaN odpadają,
    // a sumę normalizujemy do 1. Brak ważnych kotwic => rozkład jednostajny.
    let mut teleport = vec![0.0f64; n];
    let mut weight_sum = 0.0f64;
    for &(s, w) in seeds {
        if s >= n || !(w > 0.0) {
            continue;
        }
        teleport[s] += w;
        weight_sum += w;
    }
    if weight_sum > 0.0 {
        let inv = 1.0 / weight_sum;
        teleport.iter_mut().for_each(|t| *t *= inv);
    } else {
        let uniform = 1.0 / n as f64;
        teleport.iter_mut().for_each(|t| *t = uniform);
    }

    // Start od rozkładu teleportacji.
    let mut p = teleport.clone();

    for _ in 0..iters {
        let mut next = vec![0.0f64; n];
        // Masa z węzłów dangling (brak out-krawędzi lub zerowa suma wag) —
        // rozdzielana wg teleportacji.
        let mut dangling_mass = 0.0f64;

        for (u, &pu) in p.iter().enumerate() {
            let neighbors = csr.neighbors(u);
            let weights = csr.neighbor_weights(u);
            let wsum = csr.weighted_out_degree(u);
            if neighbors.is_empty() || wsum <= 0.0 {
                dangling_mass += pu;
                continue;
            }
            let mass = damping * pu;
            for (idx, &v) in neighbors.iter().enumerate() {
                next[v] += mass * (weights[idx] / wsum);
            }
        }

        let teleport_factor = (1.0 - damping) + damping * dangling_mass;
        for (ni, &t) in next.iter_mut().zip(teleport.iter()) {
            *ni += teleport_factor * t;
        }
        p = next;
    }

    let mut scored: PprScores = csr.ids.iter().cloned().zip(p).collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored
}
