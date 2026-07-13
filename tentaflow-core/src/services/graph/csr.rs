// ===== Plik: services/graph/csr.rs — reprezentacja CSR grafu (snapshot z Cozo) =====
//
// Compressed-Sparse-Row dla skierowanego grafu krawędzi wyeksportowanego z
// kolekcji Cozo. `offsets[u]..offsets[u+1]` to przedział out-krawędzi węzła `u`
// w `targets`/`weights`. `ids[u]` mapuje indeks CSR z powrotem na id węzła
// (String). Wejście dla PPR liczonego w Rust (`ppr.rs`).
//
// Każda krawędź niesie wagę (`weights`, równolegle do `targets`). PPR rozkłada
// masę proporcjonalnie do wagi, więc projekt zakłada wagi faktów (poprawka codex
// pkt 7) — eksport z Cozo MUSI te wagi przenieść.

use std::collections::HashMap;

/// Snapshot grafu w formacie CSR. `offsets` ma długość `n+1`; `targets` i
/// `weights` mają długość = liczba krawędzi i są indeksowane spójnie.
#[derive(Debug, Clone, PartialEq)]
pub struct Csr {
    /// Mapowanie indeks -> id węzła (kolejność stabilna: posortowane po id).
    pub ids: Vec<String>,
    /// Offsety CSR (długość `ids.len() + 1`).
    pub offsets: Vec<usize>,
    /// Cele krawędzi (indeksy węzłów), długość = liczba krawędzi.
    pub targets: Vec<usize>,
    /// Wagi krawędzi (równoległe do `targets`), długość = liczba krawędzi.
    pub weights: Vec<f64>,
}

impl Csr {
    /// Liczba węzłów.
    pub fn node_count(&self) -> usize {
        self.ids.len()
    }

    /// Liczba krawędzi.
    pub fn edge_count(&self) -> usize {
        self.targets.len()
    }

    /// Indeks węzła po jego id (lub `None`, gdy nieobecny).
    pub fn index_of(&self, id: &str) -> Option<usize> {
        // Budujemy lokalną mapę tylko gdy potrzeba — przy małych seedach to
        // tańsze niż trzymać HashMap w strukturze. Dla wielu lookupów użyj
        // `id_index()`.
        self.ids.iter().position(|x| x == id)
    }

    /// Mapa id -> indeks dla szybkich, wielokrotnych lookupów (np. seedy PPR).
    pub fn id_index(&self) -> HashMap<&str, usize> {
        self.ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect()
    }

    /// Out-stopień węzła `u`.
    pub fn out_degree(&self, u: usize) -> usize {
        self.offsets[u + 1] - self.offsets[u]
    }

    /// Cele out-krawędzi węzła `u`.
    pub fn neighbors(&self, u: usize) -> &[usize] {
        &self.targets[self.offsets[u]..self.offsets[u + 1]]
    }

    /// Wagi out-krawędzi węzła `u` (równoległe do `neighbors(u)`).
    pub fn neighbor_weights(&self, u: usize) -> &[f64] {
        &self.weights[self.offsets[u]..self.offsets[u + 1]]
    }

    /// Suma wag out-krawędzi węzła `u` (mianownik przy rozkładzie ważonym).
    pub fn weighted_out_degree(&self, u: usize) -> f64 {
        self.neighbor_weights(u).iter().sum()
    }

    /// Pełny stopień (out + in) każdego węzła, indeksowany jak `ids`. Używany do
    /// kary log-degree przy P_init (MemGraphRAG §6.2): węzeł-hub (wysoka łączność)
    /// jest słabszą kotwicą personalizacji PPR, bo prawie wszystko jest z nim
    /// powiązane (mała selektywność). Liczymy nieważony stopień strukturalny:
    /// out = długość przedziału CSR, in = liczba krawędzi celujących w węzeł
    /// (pojedynczy przebieg po `targets`). Zwraca wektor długości `node_count`.
    pub fn total_degrees(&self) -> Vec<usize> {
        let n = self.node_count();
        let mut deg = vec![0usize; n];
        for u in 0..n {
            deg[u] += self.out_degree(u);
        }
        for &t in &self.targets {
            deg[t] += 1;
        }
        deg
    }

    /// Buduje CSR z listy id węzłów i ważonych krawędzi `(src_id, dst_id, weight)`.
    /// Krawędzie wskazujące na nieznane id (spoza `ids`) są pomijane — eksport z
    /// Cozo nie powinien ich produkować, ale to chroni przed niespójnym
    /// snapshotem. Wagi <= 0 są podnoszone do 0 (krawędź nie przenosi masy, ale
    /// nadal liczy się do `edge_count` snapshotu).
    pub fn from_edges(ids: Vec<String>, triples: &[(String, String, f64)]) -> Csr {
        let n = ids.len();
        let index: HashMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();

        let mut degree = vec![0usize; n];
        let mut edges: Vec<(usize, usize, f64)> = Vec::with_capacity(triples.len());
        for (s, d, w) in triples {
            let (Some(&su), Some(&du)) = (index.get(s.as_str()), index.get(d.as_str())) else {
                continue;
            };
            degree[su] += 1;
            edges.push((su, du, w.max(0.0)));
        }

        let mut offsets = vec![0usize; n + 1];
        for i in 0..n {
            offsets[i + 1] = offsets[i] + degree[i];
        }
        let mut cursor = offsets.clone();
        let mut targets = vec![0usize; edges.len()];
        let mut weights = vec![0.0f64; edges.len()];
        for (su, du, w) in edges {
            let pos = cursor[su];
            targets[pos] = du;
            weights[pos] = w;
            cursor[su] += 1;
        }

        Csr {
            ids,
            offsets,
            targets,
            weights,
        }
    }
}
