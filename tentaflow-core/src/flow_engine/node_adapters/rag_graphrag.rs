// =============================================================================
// Plik: flow_engine/node_adapters/rag_graphrag.rs
// Opis: Hop grafowy retrievalu RAG (GraphRAG, E3.2). Dwa adaptery wpięte w ciało
//       pętli multi-hop (retrieval_round), obok hopu wektorowego:
//         * `rag_graph_seed`  — z biezacego pod-pytania (`meta.rag_current_query`)
//           identyfikuje encje zapytania (podejscie (a): normalizacja + n-gramy,
//           lustro `normalize_entity_name` z ingestu E3.0) i zapisuje je jako
//           seedy PPR do `meta.graph_seeds` = [{id, weight}]. PAYLOAD przechodzi
//           bez zmian, zeby embeddings/vector dalej szukaly po tekscie pytania.
//         * `rag_graph_facts` — seeduje graf (`ctx.graph` PPR po seedach z meta),
//           dla TOP encji wyciaga FAKTY (neighbors: encja--rel-->encja), formatuje
//           je jako tekst grafowy z provenance i FUZUJE z kontekstem wektorowym w
//           payloadzie (sekcja „Fakty z grafu wiedzy:") + zapisuje
//           `meta.rag_graph_facts`. Graf jest BEST-EFFORT: brak feature `graph`,
//           brak kolekcji `kg`, brak seedow/encji w grafie => czysty pass-through
//           (degradacja do samego retrievalu wektorowego, RAG dalej dziala).
//       Capy (anti-DoS): liczba seedow, top_n PPR, liczba faktow — lustrzone do
//       host-side capow z `graph_search` (ten sam `GraphComputeGuard`).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

/// Kolekcja grafowa aktywnego widoku MemGraphRAG. MUSI byc identyczna z kolekcja, do
/// ktorej pisze ingest addona RAG (`KG_COLLECTION="kg_active"` w addons/rag/src/lib.rs
/// i `[[graph_collection]]` w manifescie) — inaczej retrieval czyta pusty graf.
const KG_COLLECTION: &str = "kg_active";

/// Meta-klucz: biezace pod-pytanie hopu (ustawiane przez `rag_query_seed`).
const META_CURRENT_QUERY: &str = "rag_current_query";
/// Meta-klucz: seedy grafu wyliczone z encji zapytania = `[{id, weight}]`.
pub const META_GRAPH_SEEDS: &str = "graph_seeds";
/// Meta-klucz: sformatowany tekst faktow grafowych (fuzowany do kontekstu LLM).
/// Po E3.2 niesie fakty ZAKUMULOWANE przez wszystkie hopy (lustro pasazy w
/// `rag_accumulated`), nie tylko ostatni hop — `rag_finalize` go fuzuje.
pub const META_GRAPH_FACTS: &str = "rag_graph_facts";
/// Meta-klucz: zakumulowane fakty grafu `[{source, rel, target}]` (strukturalne)
/// przez wszystkie iteracje petli multi-hop. Dedup po (source, rel, target),
/// twardy cap `MAX_ACCUMULATED_FACTS` chroni rozmiar kontekstu przez iteracje.
pub const META_GRAPH_FACTS_ACCUMULATED: &str = "rag_graph_facts_accumulated";

/// Twardy cap calkowitej liczby ZAKUMULOWANYCH faktow grafu przez wszystkie hopy
/// petli. Bez niego fakty rosly by liniowo z liczba iteracji (eksplozja
/// kontekstu). Wyzszy niz `MAX_GRAPH_FACTS` (cap pojedynczego hopu), bo zbiera z
/// wielu hopow, ale nadal ograniczony.
pub const MAX_ACCUMULATED_FACTS: usize = 40;

/// Twardy cap liczby seedow grafu z jednego zapytania (anti-DoS — wektor
/// personalizacji PPR nie moze rosnac bez ograniczen). Lustro `MAX_PPR_SEEDS`
/// z `graph_search`, ale nizszy: zapytanie uzytkownika to garstka encji.
pub const MAX_GRAPH_SEEDS: usize = 16;

/// Minimalna dlugosc tokenu encji (w znakach). Krotsze tokeny (spojniki, „a",
/// „w") sa szumem — nie seeduja grafu.
const MIN_TOKEN_CHARS: usize = 3;

/// Maksymalna liczba slow w kandydacie n-gramowym (uni/bi/tri-gram). Encje w
/// grafie to zwykle 1–3 slowa (imie+nazwisko, nazwa firmy).
const MAX_NGRAM_WORDS: usize = 3;

/// Twardy cap liczby TOP encji z PPR, dla ktorych wyciagamy fakty (neighbors).
/// Kazda encja to osobny trawers — ograniczamy wachlarz.
pub const MAX_GRAPH_ENTITIES: u32 = 8;

/// Twardy cap calkowitej liczby faktow grafowych wlozonych do kontekstu LLM
/// (kontrola rozmiaru kontekstu). Lustro ducha `MAX_ACCUMULATED` dla pasazy.
pub const MAX_GRAPH_FACTS: usize = 30;

/// Liczba sasiadow pobieranych per encja (przed globalnym capem faktow).
const NEIGHBORS_PER_ENTITY: u32 = 12;

/// Stopwordy PL/EN — nie seeduja grafu (czyste szumowe tokeny). Lista celowo
/// krotka: tylko najczestsze spojniki/przyimki/zaimki pytajne, ktore nigdy nie
/// sa encja. Reszta filtrowana przez `MIN_TOKEN_CHARS`.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "was", "were", "who", "what", "which", "with", "from", "this", "that",
    "kto", "co", "jaki", "jaka", "jakie", "czy", "oraz", "lub", "ale", "dla", "jest", "byl",
    "byla", "bylo", "ktory", "ktora", "ktore", "tego", "tej", "nad", "pod", "przy",
];

// =============================================================================
// Czysta logika identyfikacji encji zapytania (podejscie (a))
// =============================================================================

/// Normalizuje token/fraze tak SAMO jak ingest E3.0 (`normalize_entity_name`):
/// collapse bialych znakow + lowercase. Dzieki temu seed pasuje 1:1 do `node_id`
/// w grafie (id wezla = znormalizowana nazwa encji).
fn normalize_phrase(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Czy znormalizowany token jest sensownym kandydatem na encje (nie stopword,
/// nie za krotki, zawiera litere/cyfre). Interpunkcja na brzegach jest zdejmowana
/// przez `tokenize`, wiec tu sprawdzamy juz oczyszczony token.
fn is_candidate_token(tok: &str) -> bool {
    if tok.chars().count() < MIN_TOKEN_CHARS {
        return false;
    }
    if STOPWORDS.contains(&tok) {
        return false;
    }
    tok.chars().any(|c| c.is_alphanumeric())
}

/// Tnie zapytanie na tokeny: po bialych znakach, zdejmujac interpunkcje z brzegow
/// (kropki, przecinki, cudzyslowy, nawiasy) i lowercase. Zachowuje wewnetrzne
/// myslniki/apostrofy (np. „e-mail", „d'Arc").
fn tokenize(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Identyfikuje encje zapytania jako znormalizowane kandydaty-seedy (podejscie
/// (a): n-gramy 1..=`MAX_NGRAM_WORDS` z oczyszczonych tokenow). Kazdy seed niesie
/// wage: dluzsze n-gramy (bardziej specyficzna fraza) dostaja wyzsza wage, bo
/// trafienie wielowyrazowej encji jest mocniejszym sygnalem niz pojedyncze slowo.
///
/// Nieznane seedy sa NIESZKODLIWE: backend PPR pomija id spoza grafu
/// (`GraphManager::ppr` filtruje przez `id_index`), wiec generujemy kandydatow
/// hojnie, a graf sam zostawia tylko realne encje. Wynik jest zdeduplikowany
/// (po id, wyzsza waga wygrywa) i capniety do `MAX_GRAPH_SEEDS`.
pub fn identify_query_entities(query: &str) -> Vec<(String, f64)> {
    use std::collections::HashMap;

    let tokens = tokenize(query);
    let mut by_id: HashMap<String, f64> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    // n-gramy od najdluzszych (najbardziej specyficznych) do najkrotszych —
    // dzieki temu przy dedupie pierwsza (wyzsza waga) ustala kolejnosc.
    for n in (1..=MAX_NGRAM_WORDS).rev() {
        if tokens.len() < n {
            continue;
        }
        for window in tokens.windows(n) {
            // Pojedynczy token musi byc sensowny; dla n>1 dopuszczamy fraze,
            // o ile co najmniej jeden czlon nie jest stopwordem/za krotki — fraza
            // „uniwersytet w princeton" jest wartosciowa mimo „w" w srodku.
            let usable = if n == 1 {
                is_candidate_token(&window[0])
            } else {
                window.iter().any(|t| is_candidate_token(t))
            };
            if !usable {
                continue;
            }
            let id = normalize_phrase(&window.join(" "));
            if id.is_empty() {
                continue;
            }
            // Waga rosnie z dlugoscia frazy: 1-gram=1.0, 2-gram=2.0, 3-gram=3.0.
            let weight = n as f64;
            match by_id.get(&id) {
                Some(prev) if *prev >= weight => {}
                Some(_) => {
                    by_id.insert(id, weight);
                }
                None => {
                    order.push(id.clone());
                    by_id.insert(id, weight);
                }
            }
        }
    }

    let mut seeds: Vec<(String, f64)> = order
        .into_iter()
        .map(|id| {
            let w = by_id.get(&id).copied().unwrap_or(1.0);
            (id, w)
        })
        .collect();
    // Najmocniejsze (najdluzsze frazy) najpierw, potem cap.
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
    seeds.truncate(MAX_GRAPH_SEEDS);
    seeds
}

/// Serializuje seedy do JSON `[{id, weight}]` (ksztalt zgodny z `GraphSeed` /
/// wejsciem `graph_search` op=ppr), zeby meta niosla je miedzy wezlami.
fn seeds_to_json(seeds: &[(String, f64)]) -> Value {
    Value::Array(
        seeds
            .iter()
            .map(|(id, w)| serde_json::json!({ "id": id, "weight": w }))
            .collect(),
    )
}

// =============================================================================
// Czysta logika faktow grafowych -> tekst + fuzja z kontekstem wektorowym
// =============================================================================

/// Pojedynczy fakt grafowy: `(zrodlo, relacja, cel)` znormalizowanych encji.
/// Czysta struktura — formatowanie i cap testowane bez backendu grafu.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphFact {
    pub source: String,
    pub rel: String,
    pub target: String,
}

/// Formatuje liste faktow jako blok tekstowy dla LLM. Kazdy fakt w czytelnej
/// formie „zrodlo — relacja → cel". Pusta lista => pusty string (brak sekcji w
/// kontekscie). Lista jest capniety do `MAX_GRAPH_FACTS` PRZED formatowaniem.
pub fn format_graph_facts(facts: &[GraphFact]) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for f in facts.iter().take(MAX_GRAPH_FACTS) {
        s.push_str(&format!("- {} — {} → {}\n", f.source, f.rel, f.target));
    }
    s
}

/// Serializuje fakty do JSON `[{source, rel, target}]` — strukturalna postac
/// trzymana w `meta.rag_graph_facts_accumulated` (zeby kolejne hopy mogly je
/// scalac i deduplikowac, a nie tylko skleic tekst).
fn facts_to_json(facts: &[GraphFact]) -> Value {
    Value::Array(
        facts
            .iter()
            .map(|f| {
                serde_json::json!({
                    "source": f.source,
                    "rel": f.rel,
                    "target": f.target,
                })
            })
            .collect(),
    )
}

/// Odczytuje zakumulowane fakty z meta (`[{source, rel, target}]`). Zly ksztalt /
/// brak klucza => pusta lista.
fn facts_from_json(value: Option<&Value>) -> Vec<GraphFact> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let source = f.get("source").and_then(|v| v.as_str())?;
                    let rel = f.get("rel").and_then(|v| v.as_str())?;
                    let target = f.get("target").and_then(|v| v.as_str())?;
                    Some(GraphFact {
                        source: source.to_string(),
                        rel: rel.to_string(),
                        target: target.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Scala fakty z biezacego hopu z dotad zakumulowanymi: kolejnosc zachowana
/// (najpierw stare, potem nowe), dedup po (source, rel, target), twardy cap
/// `MAX_ACCUMULATED_FACTS`. Czysta funkcja — fundament inwariantu, ze finalny
/// kontekst widzi fakty ze WSZYSTKICH hopow (lustro `merge_accumulated` pasazy).
pub fn merge_accumulated_facts(existing: &[GraphFact], incoming: &[GraphFact]) -> Vec<GraphFact> {
    use std::collections::HashSet;
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut merged: Vec<GraphFact> = Vec::with_capacity(existing.len() + incoming.len());
    for f in existing.iter().chain(incoming.iter()) {
        let key = (f.source.clone(), f.rel.clone(), f.target.clone());
        if seen.insert(key) {
            merged.push(f.clone());
        }
        if merged.len() >= MAX_ACCUMULATED_FACTS {
            break;
        }
    }
    merged
}

/// Fuzja kontekstu LLM: dokleja sekcje „Fakty z grafu wiedzy:" do istniejacego
/// kontekstu wektorowego (pasaze). Pusty tekst faktow => zwraca kontekst bez
/// zmian (degradacja). To rdzen GraphRAG: sedzia i finalny LLM widza OBA zrodla
/// (pasaze wektorowe + fakty grafowe) w jednym payloadzie.
pub fn fuse_context(vector_context: &str, graph_facts_text: &str) -> String {
    if graph_facts_text.trim().is_empty() {
        return vector_context.to_string();
    }
    let mut s = String::with_capacity(vector_context.len() + graph_facts_text.len() + 64);
    s.push_str(vector_context);
    if !vector_context.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("\nFakty z grafu wiedzy:\n");
    s.push_str(graph_facts_text);
    s
}

/// Wyciaga seedy `[(id, weight)]` z `meta.graph_seeds`. Brak / zly ksztalt =>
/// pusta lista (degradacja). Wagi nie sa dzis przekazywane do backendu PPR
/// (`GraphManager::ppr` bierze rownowazone id), ale czytamy je dla zgodnosci
/// wejscia z `GraphSeed`.
///
/// Cap `MAX_GRAPH_SEEDS` egzekwowany TUTAJ, a nie tylko w `rag_graph_seed`:
/// `meta.graph_seeds` mogla zostac zapisana w innym flow albo zmutowana po
/// seedowaniu, a to wlasnie ta funkcja karmi kosztowny PPR. Cap musi byc tam,
/// gdzie odpala PPR — inaczej wektor personalizacji moze rosnac bez ograniczen.
fn seeds_from_meta(envelope: &FlowEnvelope) -> Vec<String> {
    envelope
        .meta
        .get(META_GRAPH_SEEDS)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    s.get("id")
                        .and_then(|v| v.as_str())
                        .filter(|x| !x.is_empty())
                        .map(str::to_string)
                })
                .take(MAX_GRAPH_SEEDS)
                .collect()
        })
        .unwrap_or_default()
}

// =============================================================================
// rag_graph_seed — identyfikacja encji zapytania -> seedy PPR w meta
// =============================================================================

pub struct RagGraphSeedNodeAdapter;

impl RagGraphSeedNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RagGraphSeedNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for RagGraphSeedNodeAdapter {
    fn node_type(&self) -> &str {
        "rag_graph_seed"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // Payload przechodzi BEZ ZMIAN (tekst pytania) — kolejny wezel to
        // embeddings, ktory potrzebuje tekstu. Stad port Text jak `rag_query_seed`.
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("rag_graph_seed: brak krawedzi wejsciowej"))?;
        let mut out: FlowEnvelope = (*input.envelope).clone();

        // Zrodlo pytania: biezace pod-pytanie hopu (meta), w pierwszym hopie =
        // payload Text (po `rag_query_seed`). Brak pytania => brak seedow
        // (degradacja: graf zostaje pominiety), NIE blad — RAG ma dzialac dalej.
        let query = out
            .meta
            .get(META_CURRENT_QUERY)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| out.payload.as_text().map(|s| s.to_string()))
            .unwrap_or_default();

        let seeds = identify_query_entities(&query);
        out.meta
            .insert(META_GRAPH_SEEDS.to_string(), seeds_to_json(&seeds));
        // Payload nietkniety — embeddings dostaje ten sam tekst pytania.
        Ok(out)
    }
}

// =============================================================================
// rag_graph_facts — PPR po seedach -> neighbors top encji -> fakty -> fuzja
// =============================================================================

pub struct RagGraphFactsNodeAdapter;

impl RagGraphFactsNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Buduje fakty grafowe z `ctx.graph` (PPR seedow z meta -> top encje ->
    /// neighbors). Best-effort: kazdy blad backendu (brak kolekcji `kg`, busy
    /// guard) degraduje do pustej listy faktow zamiast wywracac hop. Dostepne
    /// tylko pod feature `graph` — bez niego `ctx.graph` nie istnieje.
    #[cfg(feature = "graph")]
    fn collect_facts(envelope: &FlowEnvelope, ctx: &ExecutionContext) -> Vec<GraphFact> {
        use crate::services::graph::{GraphComputeGuard, NeighborDir};
        use crate::services::org::DEFAULT_ORG_ID;

        let seeds = seeds_from_meta(envelope);
        if seeds.is_empty() {
            return Vec::new();
        }
        // Tozsamosc instancji wymagana do scope'u (org, addon, kolekcja). Brak
        // addon_id => graf pominiety (jak `graph_search`, ale tu best-effort:
        // degradacja, nie blad — GraphRAG jest opcjonalnym wzbogaceniem).
        let Some(addon) = ctx.addon_id.as_deref() else {
            return Vec::new();
        };
        let org = ctx
            .org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string());

        // Cap wspolbieznosci: TEN SAM guard co host-fn / `graph_search` — hop
        // grafowy RAG nie obchodzi capa DoS. Saturacja => brak faktow (degradacja).
        let _compute = match GraphComputeGuard::acquire(addon) {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };

        // Krok 1: PPR personalizowany na seedach -> top encje powiazane w grafie.
        let ranked = match ctx.graph.ppr(
            &org,
            addon,
            KG_COLLECTION,
            &seeds,
            MAX_GRAPH_ENTITIES,
            0.85,
            20,
        ) {
            Ok(r) => r,
            // Brak kolekcji `kg` / inny blad => brak faktow (degradacja).
            Err(_) => return Vec::new(),
        };
        if ranked.is_empty() {
            return Vec::new();
        }

        // Krok 2: dla TOP encji wyciagnij FAKTY (krawedzie out: encja--rel-->cel).
        // Globalny cap MAX_GRAPH_FACTS chroni rozmiar kontekstu niezaleznie od
        // liczby encji. Dedup po (source, rel, target) — ta sama krawedz moze
        // wyjsc z dwoch top encji.
        use std::collections::HashSet;
        let mut facts: Vec<GraphFact> = Vec::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for (entity_id, _score) in ranked.iter() {
            if facts.len() >= MAX_GRAPH_FACTS {
                break;
            }
            let neighbors = match ctx.graph.neighbors(
                &org,
                addon,
                KG_COLLECTION,
                entity_id,
                NeighborDir::Out,
                None,
                NEIGHBORS_PER_ENTITY,
            ) {
                Ok(n) => n,
                Err(_) => continue,
            };
            for (target, rel, _weight) in neighbors {
                if facts.len() >= MAX_GRAPH_FACTS {
                    break;
                }
                let key = (entity_id.clone(), rel.clone(), target.clone());
                if seen.insert(key) {
                    facts.push(GraphFact {
                        source: entity_id.clone(),
                        rel,
                        target,
                    });
                }
            }
        }
        facts
    }

    /// Wariant bez feature `graph`: graf nie istnieje w buildzie => zero faktow.
    /// GraphRAG degraduje do czystego retrievalu wektorowego.
    #[cfg(not(feature = "graph"))]
    fn collect_facts(_envelope: &FlowEnvelope, _ctx: &ExecutionContext) -> Vec<GraphFact> {
        Vec::new()
    }
}

impl Default for RagGraphFactsNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for RagGraphFactsNodeAdapter {
    fn node_type(&self) -> &str {
        "rag_graph_facts"
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // Wyjscie to kontekst tekstowy (pasaze + fakty) dla sedziego LLM.
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("rag_graph_facts: brak krawedzi wejsciowej"))?;
        let mut out: FlowEnvelope = (*input.envelope).clone();

        // Wejsciowy payload = kontekst wektorowy zbudowany przez `rag_accumulate`
        // (pytanie + zakumulowane pasaze). Fakty grafowe dokleimy do niego.
        let vector_context = out.payload.as_text().unwrap_or_default().to_string();

        // Fakty TEGO hopu, scalone z zakumulowanymi z poprzednich iteracji
        // (dedup po (source, rel, target), cap MAX_ACCUMULATED_FACTS). Lustro
        // akumulacji pasazy w `rag_accumulate`: sedzia i finalny LLM widza fakty
        // ze WSZYSTKICH hopow, nie tylko ostatniego.
        let hop_facts = Self::collect_facts(&out, ctx);
        let existing = facts_from_json(out.meta.get(META_GRAPH_FACTS_ACCUMULATED));
        let accumulated = merge_accumulated_facts(&existing, &hop_facts);

        let facts_text = format_graph_facts(&accumulated);

        out.meta.insert(
            META_GRAPH_FACTS_ACCUMULATED.to_string(),
            facts_to_json(&accumulated),
        );
        // META_GRAPH_FACTS niesie tekst zakumulowanych faktow — `rag_finalize`
        // czyta wlasnie ten klucz, wiec finalny LLM dostaje pelny zbior.
        out.meta.insert(
            META_GRAPH_FACTS.to_string(),
            Value::String(facts_text.clone()),
        );
        out.payload = FlowValue::Text(fuse_context(&vector_context, &facts_text));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(node_type: &str) -> FlowNode {
        FlowNode {
            id: format!("{node_type}-1"),
            node_type: node_type.into(),
            config: Value::Null,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "prev".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    // --- identify_query_entities (czysta identyfikacja encji) ---------------

    #[test]
    fn identify_normalizes_like_ingest() {
        // Lustro normalize_entity_name: lowercase + collapse bialych znakow.
        let seeds = identify_query_entities("Ada   Lovelace");
        let ids: Vec<&str> = seeds.iter().map(|(id, _)| id.as_str()).collect();
        // 2-gram „ada lovelace" musi byc kandydatem (znormalizowany jak w grafie).
        assert!(ids.contains(&"ada lovelace"), "ids: {ids:?}");
        // Pojedyncze tokeny tez (unigramy).
        assert!(ids.contains(&"ada") && ids.contains(&"lovelace"), "ids: {ids:?}");
    }

    #[test]
    fn identify_longer_ngrams_weigh_more() {
        let seeds = identify_query_entities("Albert Einstein");
        // 2-gram „albert einstein" ma wyzsza wage niz unigramy -> jest pierwszy.
        assert_eq!(seeds[0].0, "albert einstein");
        assert!(seeds[0].1 > 1.0, "fraza wielowyrazowa ma wage > 1: {:?}", seeds[0]);
    }

    #[test]
    fn identify_drops_stopwords_and_short_tokens() {
        let seeds = identify_query_entities("kto byl prezesem w IBM");
        let ids: Vec<&str> = seeds.iter().map(|(id, _)| id.as_str()).collect();
        // „kto", „byl", „w" to stopwordy / za krotkie — nie sa unigramami-seedami.
        assert!(!ids.contains(&"kto"), "stopword 'kto' nie jest seedem: {ids:?}");
        assert!(!ids.contains(&"byl"), "stopword 'byl' nie jest seedem: {ids:?}");
        assert!(!ids.contains(&"w"), "za krotki 'w' nie jest seedem: {ids:?}");
        // „ibm" (3 znaki, nie stopword) JEST seedem.
        assert!(ids.contains(&"ibm"), "'ibm' ma byc seedem: {ids:?}");
    }

    #[test]
    fn identify_strips_punctuation() {
        let seeds = identify_query_entities("Princeton, University.");
        let ids: Vec<&str> = seeds.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"princeton"), "interpunkcja zdjeta: {ids:?}");
        assert!(ids.contains(&"university"), "ids: {ids:?}");
    }

    #[test]
    fn identify_caps_seed_count() {
        // Dlugie zapytanie z wieloma unikalnymi tokenami -> liczba seedow capnieta.
        let words: Vec<String> = (0..50).map(|i| format!("token{i:03}")).collect();
        let seeds = identify_query_entities(&words.join(" "));
        assert!(
            seeds.len() <= MAX_GRAPH_SEEDS,
            "liczba seedow capnieta do {MAX_GRAPH_SEEDS}, bylo {}",
            seeds.len()
        );
    }

    #[test]
    fn identify_empty_query_yields_no_seeds() {
        assert!(identify_query_entities("").is_empty());
        assert!(identify_query_entities("   ").is_empty());
        // Samo stopwordy/za krotkie -> brak seedow (degradacja w hopie).
        assert!(identify_query_entities("a w i").is_empty());
    }

    // --- format_graph_facts + cap ------------------------------------------

    #[test]
    fn format_facts_renders_edges() {
        let facts = vec![
            GraphFact {
                source: "einstein".into(),
                rel: "pracowal_w".into(),
                target: "princeton".into(),
            },
            GraphFact {
                source: "einstein".into(),
                rel: "urodzil_sie_w".into(),
                target: "ulm".into(),
            },
        ];
        let text = format_graph_facts(&facts);
        assert!(text.contains("einstein — pracowal_w → princeton"), "tekst: {text}");
        assert!(text.contains("einstein — urodzil_sie_w → ulm"), "tekst: {text}");
    }

    #[test]
    fn format_facts_empty_is_empty_string() {
        assert_eq!(format_graph_facts(&[]), "");
    }

    #[test]
    fn format_facts_caps_count() {
        let facts: Vec<GraphFact> = (0..(MAX_GRAPH_FACTS + 20))
            .map(|i| GraphFact {
                source: "s".into(),
                rel: "r".into(),
                target: format!("t{i}"),
            })
            .collect();
        let text = format_graph_facts(&facts);
        let lines = text.lines().count();
        assert_eq!(lines, MAX_GRAPH_FACTS, "liczba faktow capnieta, bylo {lines} linii");
    }

    // --- fuse_context (fuzja pasaze + fakty) -------------------------------

    #[test]
    fn fuse_appends_graph_section() {
        let vec_ctx = "Pytanie: Q\n\nKontekst (zakumulowane pasaże):\n[0] (doc=d1) pasaz\n";
        let facts = "- einstein — pracowal_w → princeton\n";
        let fused = fuse_context(vec_ctx, facts);
        // Oba zrodla obecne: pasaze (wektor) + sekcja faktow (graf).
        assert!(fused.contains("pasaz"), "pasaze zachowane: {fused}");
        assert!(fused.contains("Fakty z grafu wiedzy:"), "naglowek faktow: {fused}");
        assert!(fused.contains("einstein — pracowal_w → princeton"), "fakt: {fused}");
    }

    #[test]
    fn fuse_no_facts_returns_vector_context_unchanged() {
        // Degradacja: brak faktow -> kontekst wektorowy bez zmian (zadnej sekcji).
        let vec_ctx = "Pytanie: Q\n\npasaze...\n";
        assert_eq!(fuse_context(vec_ctx, ""), vec_ctx);
        assert_eq!(fuse_context(vec_ctx, "   "), vec_ctx);
        assert!(!fuse_context(vec_ctx, "").contains("Fakty z grafu wiedzy"));
    }

    // --- rag_graph_seed (wezel) --------------------------------------------

    #[tokio::test]
    async fn graph_seed_writes_seeds_to_meta_and_keeps_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Albert Einstein".into());
        env.meta
            .insert(META_CURRENT_QUERY.into(), json!("Albert Einstein"));
        let out = RagGraphSeedNodeAdapter::new()
            .execute(&node("rag_graph_seed"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        // Payload nietkniety (embeddings dostaje tekst pytania).
        assert_eq!(out.payload.as_text(), Some("Albert Einstein"));
        let seeds = out
            .meta
            .get(META_GRAPH_SEEDS)
            .and_then(|v| v.as_array())
            .expect("meta.graph_seeds ustawione");
        let ids: Vec<&str> = seeds
            .iter()
            .filter_map(|s| s.get("id").and_then(|v| v.as_str()))
            .collect();
        assert!(ids.contains(&"albert einstein"), "ids: {ids:?}");
        // Kazdy seed niesie wage (ksztalt GraphSeed).
        assert!(seeds.iter().all(|s| s.get("weight").and_then(|w| w.as_f64()).is_some()));
    }

    #[tokio::test]
    async fn graph_seed_no_entities_degrades_to_empty_seeds() {
        // Zapytanie bez encji (same stopwordy) -> pusta lista seedow w meta,
        // NIE blad. Hop grafowy zostanie pominiety (degradacja do wektora).
        let mut env = FlowEnvelope::empty();
        env.meta.insert(META_CURRENT_QUERY.into(), json!("kto co czy"));
        let out = RagGraphSeedNodeAdapter::new()
            .execute(&node("rag_graph_seed"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        let seeds = out
            .meta
            .get(META_GRAPH_SEEDS)
            .and_then(|v| v.as_array())
            .unwrap();
        assert!(seeds.is_empty(), "brak encji -> brak seedow: {seeds:?}");
    }

    // --- rag_graph_facts (wezel) -------------------------------------------

    #[tokio::test]
    async fn graph_facts_no_seeds_passes_context_through() {
        // Brak seedow (meta.graph_seeds puste) -> degradacja: payload (kontekst
        // wektorowy) przechodzi bez zmian, brak sekcji faktow.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Pytanie: Q\n\npasaze...\n".into());
        env.meta.insert(META_GRAPH_SEEDS.into(), json!([]));
        let out = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        let ctx_text = out.payload.as_text().unwrap();
        assert_eq!(ctx_text, "Pytanie: Q\n\npasaze...\n", "kontekst bez zmian");
        assert!(!ctx_text.contains("Fakty z grafu wiedzy"), "brak sekcji faktow");
        // meta.rag_graph_facts ustawione na pusty string (spojny stan).
        assert_eq!(
            out.meta.get(META_GRAPH_FACTS).and_then(|v| v.as_str()),
            Some("")
        );
    }

    /// Pod feature `graph`: hop grafowy seeduje PPR realnymi encjami zapytania,
    /// wyciaga fakty (neighbors) i FUZUJE je z kontekstem wektorowym. To
    /// integracja calego slice'u E3.2 na zywym `GraphManager`.
    #[cfg(feature = "graph")]
    #[tokio::test]
    async fn graph_facts_fuses_real_facts_under_feature() {
        use crate::flow_engine::node_adapter::test_support::stub_graph;
        let g = stub_graph();
        let mut ctx = stub_ctx();
        ctx.addon_id = Some("inst-a".into());
        ctx.org_id = Some("org-1".into());
        ctx.graph = g.clone();

        // Seeduj graf wiedzy: einstein --pracowal_w--> princeton (kolekcja kg).
        for id in ["einstein", "princeton"] {
            g.upsert_node_with_quota("org-1", "inst-a", KG_COLLECTION, id, "Entity", "{}", "null")
                .unwrap();
        }
        g.upsert_edge_with_quota(
            "org-1", "inst-a", KG_COLLECTION, "einstein", "pracowal_w", "princeton", 1.0, "{}",
            "null",
        )
        .unwrap();

        // rag_graph_seed identyfikuje „einstein" z pytania; rag_graph_facts
        // seeduje PPR i wyciaga fakt.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Pytanie: gdzie pracowal Einstein?\n\npasaz wektorowy\n".into());
        env.meta
            .insert(META_CURRENT_QUERY.into(), json!("gdzie pracowal Einstein"));
        let seeded = RagGraphSeedNodeAdapter::new()
            .execute(&node("rag_graph_seed"), &[input(env)], &ctx)
            .await
            .unwrap();
        let out = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(seeded)], &ctx)
            .await
            .unwrap();

        let ctx_text = out.payload.as_text().unwrap();
        // Fuzja: pasaz wektorowy ZOSTAJE + fakt grafowy dolaczony.
        assert!(ctx_text.contains("pasaz wektorowy"), "pasaz wektorowy zachowany: {ctx_text}");
        assert!(ctx_text.contains("Fakty z grafu wiedzy:"), "sekcja faktow: {ctx_text}");
        assert!(
            ctx_text.contains("einstein — pracowal_w → princeton"),
            "fakt grafowy: {ctx_text}"
        );
    }

    /// Pod feature `graph`: seedy bez odpowiednika w grafie (zapytanie o encje
    /// spoza KG) -> PPR pusty -> degradacja do kontekstu wektorowego. Graf NIE
    /// jest pusty (ma encje z krawedziami), wiec gdyby PPR degenerowal do
    /// globalnego rankingu (bug 1), faktyte encji wyciekly by jako szum — test
    /// pilnuje, ze przy samych nieznanych seedach wynik jest PUSTY.
    #[cfg(feature = "graph")]
    #[tokio::test]
    async fn graph_facts_unknown_entities_degrade_under_feature() {
        use crate::flow_engine::node_adapter::test_support::stub_graph;
        let g = stub_graph();
        let mut ctx = stub_ctx();
        ctx.addon_id = Some("inst-a".into());
        ctx.org_id = Some("org-1".into());
        ctx.graph = g.clone();
        // Kolekcja kg ma encje Z KRAWEDZIA (tesla --wynalazl--> radio), ale ZADEN
        // seed zapytania jej nie dotyka. Bez fixu bug 1: uniform PageRank wskazal
        // by „tesla", a jej fakt wyciekl by do kontekstu (szum).
        for id in ["tesla", "radio"] {
            g.upsert_node_with_quota("org-1", "inst-a", KG_COLLECTION, id, "Entity", "{}", "null")
                .unwrap();
        }
        g.upsert_edge_with_quota(
            "org-1", "inst-a", KG_COLLECTION, "tesla", "wynalazl", "radio", 1.0, "{}", "null",
        )
        .unwrap();

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("kontekst wektorowy\n".into());
        env.meta.insert(
            META_GRAPH_SEEDS.into(),
            json!([{"id": "nieznana encja", "weight": 1.0}]),
        );
        let out = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(env)], &ctx)
            .await
            .unwrap();
        let ctx_text = out.payload.as_text().unwrap();
        // PPR bez trafien -> brak faktow -> kontekst wektorowy bez zmian. Fakt
        // globalnej encji NIE wyciekl.
        assert_eq!(ctx_text, "kontekst wektorowy\n");
        assert!(!ctx_text.contains("Fakty z grafu wiedzy"));
        assert!(!ctx_text.contains("tesla"), "globalna encja nie wyciekla: {ctx_text}");
    }

    // --- seeds_from_meta cap (bug 2) ---------------------------------------

    #[test]
    fn seeds_from_meta_caps_seed_count() {
        // meta.graph_seeds z wieksza liczba seedow niz MAX_GRAPH_SEEDS (np. po
        // mutacji w innym flow) musi byc przyciete PRZED PPR.
        let seeds_json: Vec<Value> = (0..(MAX_GRAPH_SEEDS + 10))
            .map(|i| json!({"id": format!("e{i}"), "weight": 1.0}))
            .collect();
        let mut env = FlowEnvelope::empty();
        env.meta.insert(META_GRAPH_SEEDS.into(), Value::Array(seeds_json));
        let seeds = seeds_from_meta(&env);
        assert_eq!(
            seeds.len(),
            MAX_GRAPH_SEEDS,
            "liczba seedow przycieta do {MAX_GRAPH_SEEDS}, bylo {}",
            seeds.len()
        );
        // Zachowana kolejnosc wejscia (pierwsze MAX_GRAPH_SEEDS).
        assert_eq!(seeds[0], "e0");
        assert_eq!(seeds[MAX_GRAPH_SEEDS - 1], format!("e{}", MAX_GRAPH_SEEDS - 1));
    }

    // --- merge_accumulated_facts (bug 3: dedup + cap przez hopy) -----------

    fn fact(source: &str, rel: &str, target: &str) -> GraphFact {
        GraphFact {
            source: source.into(),
            rel: rel.into(),
            target: target.into(),
        }
    }

    #[test]
    fn merge_facts_accumulates_across_hops() {
        // Hop 1 dal jeden fakt, hop 2 dolozyl drugi -> finalny zbior ma OBA.
        let hop1 = vec![fact("einstein", "pracowal_w", "princeton")];
        let hop2 = vec![fact("einstein", "urodzil_sie_w", "ulm")];
        let merged = merge_accumulated_facts(&hop1, &hop2);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0], fact("einstein", "pracowal_w", "princeton"));
        assert_eq!(merged[1], fact("einstein", "urodzil_sie_w", "ulm"));
    }

    #[test]
    fn merge_facts_dedups_by_triple() {
        // Ten sam fakt (source, rel, target) z dwoch hopow -> raz w wyniku.
        let existing = vec![fact("a", "r", "b")];
        let incoming = vec![fact("a", "r", "b"), fact("a", "r", "c")];
        let merged = merge_accumulated_facts(&existing, &incoming);
        assert_eq!(merged.len(), 2, "duplikat (a,r,b) zdeduplikowany: {merged:?}");
        assert_eq!(merged[0], fact("a", "r", "b"));
        assert_eq!(merged[1], fact("a", "r", "c"));
    }

    #[test]
    fn merge_facts_caps_total() {
        // Akumulacja przez wiele hopow nie moze rosnac bez ograniczen -> cap.
        let incoming: Vec<GraphFact> = (0..(MAX_ACCUMULATED_FACTS + 15))
            .map(|i| fact("s", "r", &format!("t{i}")))
            .collect();
        let merged = merge_accumulated_facts(&[], &incoming);
        assert_eq!(
            merged.len(),
            MAX_ACCUMULATED_FACTS,
            "cap calkowitej liczby zakumulowanych faktow"
        );
    }

    #[test]
    fn facts_json_roundtrip() {
        // Serializacja do meta i odczyt musza dawac ten sam zbior faktow.
        let facts = vec![
            fact("einstein", "pracowal_w", "princeton"),
            fact("tesla", "wynalazl", "radio"),
        ];
        let json = facts_to_json(&facts);
        let back = facts_from_json(Some(&json));
        assert_eq!(back, facts);
        // Brak klucza / zly ksztalt -> pusto.
        assert!(facts_from_json(None).is_empty());
        assert!(facts_from_json(Some(&json!("not array"))).is_empty());
    }

    /// Pod feature `graph`: dwa kolejne hopy `rag_graph_facts` z ROZNYMI seedami
    /// (kazdy trafia w inna encje z innym faktem) akumuluja oba fakty w
    /// `meta.rag_graph_facts_accumulated` i oba widnieja w kontekscie drugiego
    /// hopu (bug 3: final widzi fakty ze WSZYSTKICH hopow, nie tylko ostatniego).
    #[cfg(feature = "graph")]
    #[tokio::test]
    async fn graph_facts_accumulate_between_hops_under_feature() {
        use crate::flow_engine::node_adapter::test_support::stub_graph;
        let g = stub_graph();
        let mut ctx = stub_ctx();
        ctx.addon_id = Some("inst-a".into());
        ctx.org_id = Some("org-1".into());
        ctx.graph = g.clone();

        for id in ["einstein", "princeton", "tesla", "radio"] {
            g.upsert_node_with_quota("org-1", "inst-a", KG_COLLECTION, id, "Entity", "{}", "null")
                .unwrap();
        }
        g.upsert_edge_with_quota(
            "org-1", "inst-a", KG_COLLECTION, "einstein", "pracowal_w", "princeton", 1.0, "{}",
            "null",
        )
        .unwrap();
        g.upsert_edge_with_quota(
            "org-1", "inst-a", KG_COLLECTION, "tesla", "wynalazl", "radio", 1.0, "{}", "null",
        )
        .unwrap();

        // Hop 1: seed „einstein" -> fakt pracowal_w.
        let mut env1 = FlowEnvelope::empty();
        env1.payload = FlowValue::Text("kontekst hop1\n".into());
        env1.meta
            .insert(META_GRAPH_SEEDS.into(), json!([{"id": "einstein", "weight": 1.0}]));
        let after1 = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(env1)], &ctx)
            .await
            .unwrap();

        // Hop 2: zmieniony seed na „tesla" -> nowy fakt wynalazl. Akumulator
        // (meta.rag_graph_facts_accumulated) przechodzi z hopu 1 do hopu 2.
        let mut env2: FlowEnvelope = after1;
        env2.payload = FlowValue::Text("kontekst hop2\n".into());
        env2.meta
            .insert(META_GRAPH_SEEDS.into(), json!([{"id": "tesla", "weight": 1.0}]));
        let after2 = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(env2)], &ctx)
            .await
            .unwrap();

        let ctx_text = after2.payload.as_text().unwrap();
        // Kontekst hopu 2 niesie OBA fakty (zakumulowane), nie tylko biezacy.
        assert!(
            ctx_text.contains("einstein — pracowal_w → princeton"),
            "fakt z hopu 1 zachowany: {ctx_text}"
        );
        assert!(
            ctx_text.contains("tesla — wynalazl → radio"),
            "fakt z hopu 2 dodany: {ctx_text}"
        );
        // Strukturalny akumulator ma oba fakty (dedup po trojce).
        let acc = facts_from_json(after2.meta.get(META_GRAPH_FACTS_ACCUMULATED));
        assert_eq!(acc.len(), 2, "akumulator ma fakty z obu hopow: {acc:?}");
    }
}
