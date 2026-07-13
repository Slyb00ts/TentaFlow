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
/// Meta-klucz (toggle opcjonalnego grafu): czy fuzja grafowa jest wlaczona dla tej
/// instancji RAG. Addon RAG ZAWSZE wysyla te flage przy `ask` (host-fn allowlist).
/// `false` => wezly grafowe robia NO-OP (degradacja do czysto wektorowego retrievalu).
/// BRAK klucza => legacy/inny caller bez toggle => zachowanie dotychczasowe (graf ON),
/// inaczej zlamalibysmy flowy uruchamiane bez addona RAG.
pub const META_GRAPH_ENABLED: &str = "graph_enabled";

/// Czy hop grafowy ma sie wykonac wg meta. Bramka toggle opcjonalnego grafu:
/// jawne `graph_enabled=false` => pomin graf; brak klucza => legacy ON. JEDNO
/// zrodlo prawdy dla `rag_graph_seed`/`rag_graph_facts`.
fn graph_enabled_in_meta(envelope: &FlowEnvelope) -> bool {
    match envelope
        .meta
        .get(META_GRAPH_ENABLED)
        .and_then(|v| v.as_bool())
    {
        Some(enabled) => enabled,
        None => true,
    }
}
/// Meta-klucz (MemGraphRAG D5): mapa aktywnych aliasow encji `[{alias, canonical}]`, wstrzykiwana
/// przez addon RAG do flow.meta (host-fn allowlist). `rag_graph_seed` uzywa jej do alias-rewrite
/// seedow PPR (alias->canonical). Brak klucza => brak rewrite (degradacja).
pub const META_ENTITY_ALIASES: &str = "entity_aliases";
/// Meta-klucz (MemGraphRAG §4.3.2, eq. 19 — Information Density): mapa rzadkosci encji
/// `[{id, density}]`, gdzie `density` ∈ [0,1] to znormalizowane IDF encji w korpusie
/// (0 = encja pospolita, 1 = unikalna/rzadka). Wstrzykiwana przez addon RAG (df liczone
/// z `graph_artifacts`). `rag_graph_facts` skaluje nia P_init relevance: seed potwierdzony
/// w pasazu, ale RZADKI, jest mocniejsza kotwica niz seed pospolity. Brak klucza => czysty
/// `RELEVANCE_BOOST` (degradacja jak dotychczas).
pub const META_ENTITY_DENSITY: &str = "entity_density";
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

/// Pula KANDYDATOW na seedy zbierana z `meta.graph_seeds` PRZED P_init. Wieksza
/// niz `MAX_GRAPH_SEEDS`: cap do `MAX_GRAPH_SEEDS` zapada dopiero PO przewazeniu
/// (log-degree + relevance) w `ppr_with_p_init`, wiec kandydat poza pierwszymi 16
/// leksykalnie — ale z wysoka waga finalna — musi miec szanse trafic do PPR. Mimo
/// to ograniczamy pule (anti-DoS), zeby koszt przewazenia/sortu nie rosl bez granic.
pub const MAX_GRAPH_SEED_CANDIDATES: usize = 64;

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

/// Mnoznik wagi seeda potwierdzonego w top-pasazach wektorowych (P_init relevance,
/// MemGraphRAG §6.2). > 1, ale umiarkowany: pasaz to silny sygnal, lecz nie ma
/// zdominowac kary log-degree ani struktury grafu. Encja zarazem rzadka (niski
/// stopien) i obecna w pasazach wektorowych jest najsilniejsza kotwica.
const RELEVANCE_BOOST: f64 = 2.0;

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
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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
/// (po id, wyzsza waga wygrywa). Producent NIE capuje do `MAX_GRAPH_SEEDS` (16),
/// tylko do `MAX_GRAPH_SEED_CANDIDATES` (64) — meta niesie pelna pule kandydatow,
/// a finalny cap do `MAX_GRAPH_SEEDS` zapada DOPIERO w `ppr_with_p_init` PO pelnym
/// P_init (relevance + log-degree). Capowanie tutaj do 16 zabilo by pule: kandydat
/// poza pierwszymi 16 leksykalnie nigdy nie dostalby przewazenia. Cap do 64 to
/// granica anti-DoS na rozmiar payloadu meta (bounded).
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
    // Najmocniejsze (najdluzsze frazy) najpierw; cap tylko do puli kandydatow
    // (anti-DoS). Finalny cap do MAX_GRAPH_SEEDS zapada PO przewazeniu w PPR.
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
    seeds.truncate(MAX_GRAPH_SEED_CANDIDATES);
    seeds
}

/// D5 alias-rewrite (R5): przepisuje id seedow przez mape aliasow `[{alias, canonical}]` z meta.
/// Seed pasujacy do `alias` (po znormalizowanym id — alias-id z ingestu jest juz znormalizowany,
/// jak seed) staje sie `canonical`. Gdy dwa seedy zmapuja na ten sam canonical, scalamy je biorac
/// WIEKSZA wage (najsilniejszy sygnal personalizacji PPR). Czysta funkcja — testowalna bez hosta.
/// Mapa to TYLKO ulatwienie retrievalu; brak/zly ksztalt => seedy bez zmian (degradacja).
fn rewrite_seeds_with_aliases(
    seeds: Vec<(String, f64)>,
    aliases: Option<&Value>,
) -> Vec<(String, f64)> {
    use std::collections::HashMap;
    let map: HashMap<String, String> = aliases
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let a = e.get("alias").and_then(|v| v.as_str())?;
                    let c = e.get("canonical").and_then(|v| v.as_str())?;
                    if a.is_empty() || c.is_empty() {
                        return None;
                    }
                    Some((a.to_string(), c.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    if map.is_empty() {
        return seeds;
    }

    // Zachowaj kolejnosc pierwszego pojawienia (stabilnosc), scalaj duplikaty po wiekszej wadze.
    let mut order: Vec<String> = Vec::with_capacity(seeds.len());
    let mut by_id: HashMap<String, f64> = HashMap::with_capacity(seeds.len());
    for (id, w) in seeds {
        let canonical = map.get(&id).cloned().unwrap_or(id);
        match by_id.get(&canonical) {
            Some(prev) if *prev >= w => {}
            Some(_) => {
                by_id.insert(canonical, w);
            }
            None => {
                order.push(canonical.clone());
                by_id.insert(canonical, w);
            }
        }
    }
    order
        .into_iter()
        .map(|id| {
            let w = by_id.get(&id).copied().unwrap_or(1.0);
            (id, w)
        })
        .collect()
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

/// Wyciaga KANDYDATOW na seedy `[(id, weight)]` z `meta.graph_seeds`. Brak / zly
/// ksztalt => pusta lista (degradacja). Wagi to BAZA P_init (`base × relevance`
/// dolozy `collect_facts`), ktora plynie do `ppr_with_p_init` jako wektor
/// personalizacji.
///
/// Tu NIE capujemy do `MAX_GRAPH_SEEDS` — cap zapada PO przewazeniu w
/// `ppr_with_p_init`, inaczej kotwica poza pierwszymi 16 leksykalnie (ale z
/// wysoka waga finalna) nigdy nie trafilaby do PPR. Ograniczamy jedynie pule do
/// `MAX_GRAPH_SEED_CANDIDATES` (anti-DoS): `meta.graph_seeds` mogla zostac
/// zapisana w innym flow albo zmutowana po seedowaniu, wiec rozmiar musi byc
/// zwiazany tam, gdzie odpala kosztowny PPR.
fn seeds_from_meta(envelope: &FlowEnvelope) -> Vec<(String, f64)> {
    envelope
        .meta
        .get(META_GRAPH_SEEDS)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let id = s
                        .get("id")
                        .and_then(|v| v.as_str())
                        .filter(|x| !x.is_empty())?;
                    // Brak wagi => 1.0 (neutralna kotwica) — zgodnie z `GraphSeed`.
                    let w = s.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);
                    Some((id.to_string(), w))
                })
                .take(MAX_GRAPH_SEED_CANDIDATES)
                .collect()
        })
        .unwrap_or_default()
}

/// Dokleja sygnal P_init RELEVANCE (MemGraphRAG §6.2) do wag kandydatow PRZED
/// PPR. To jedyny sygnal P_init liczony adapter-side, bo zalezy od pasazy
/// wektorowych (`passage_text`, juz w payloadzie po `rag_accumulate`) — kary
/// log-degree i cap robi `ppr_with_p_init` nad tym samym CSR co PPR.
///
/// Encja, ktorej znormalizowana nazwa WSPOLWYSTEPUJE w tekscie top-pasazy
/// wektorowych, dostaje mnoznik `RELEVANCE_BOOST` — kotwica potwierdzona przez
/// retrieval wektorowy jest mocniejsza (fuzja warstw M_pas i M_fac). `id` jest
/// juz znormalizowany (lowercase, collapse) jak ingest, wiec szukamy go wprost w
/// zlowercase'owanym tekscie. Pusty tekst => brak boostu (degradacja).
///
/// Information Density (MemGraphRAG eq. 19): boost potwierdzonej kotwicy jest
/// dodatkowo skalowany rzadkoscia encji `density(id)` ∈ [0,1] (znormalizowane IDF
/// z `meta.entity_density`). Mnoznik = `RELEVANCE_BOOST × (1 + density)`: encja
/// pospolita (density≈0) -> czysty `RELEVANCE_BOOST`, encja unikalna (density≈1)
/// -> do `2 × RELEVANCE_BOOST`. Tak jak w papierze, rzadkie/informacyjne encje w
/// pasazu sa silniejszym dowodem niz generyczne. Brak wpisu w mapie => density 0
/// (degradacja do dawnego, plaskiego boostu). Mnoznik pozostaje OGRANICZONY (max
/// 2×), zeby nie zdominowac kary log-degree ani struktury grafu.
///
/// Wagi <= 0 nie powstaja (mnoznik dodatni). Kolejnosc i liczba kandydatow bez
/// zmian — to tylko PRZEWAZENIE istniejacych kotwic.
fn apply_relevance_boost(
    seeds: Vec<(String, f64)>,
    passage_text: &str,
    density: &std::collections::HashMap<String, f64>,
) -> Vec<(String, f64)> {
    let haystack = passage_text.to_lowercase();
    if haystack.is_empty() {
        return seeds;
    }
    seeds
        .into_iter()
        .map(|(id, mut w)| {
            if id.len() >= MIN_TOKEN_CHARS && haystack.contains(&id) {
                let d = density.get(&id).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                w *= RELEVANCE_BOOST * (1.0 + d);
            }
            (id, w)
        })
        .collect()
}

/// Odczytuje mape rzadkosci encji `meta.entity_density = [{id, density}]` do
/// `HashMap<id, density∈[0,1]>` (Information Density, eq. 19). Wpisy bez `id`/
/// `density`, puste id lub niefinite/poza-zakresem density sa pomijane. Brak
/// klucza / zly ksztalt => pusta mapa (apply_relevance_boost degraduje do plaskiego
/// boostu). Czysta funkcja — testowalna bez hosta.
fn density_from_meta(envelope: &FlowEnvelope) -> std::collections::HashMap<String, f64> {
    envelope
        .meta
        .get(META_ENTITY_DENSITY)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let id = e
                        .get("id")
                        .and_then(|v| v.as_str())
                        .filter(|x| !x.is_empty())?;
                    let d = e.get("density").and_then(|v| v.as_f64())?;
                    if !d.is_finite() {
                        return None;
                    }
                    Some((id.to_string(), d.clamp(0.0, 1.0)))
                })
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

        // Bramka toggle opcjonalnego grafu: przy `graph_enabled=false` adapter jest
        // NO-OP — przepuszcza envelope bez seedow PPR (zero zapytan do grafu), dokladnie
        // jak istniejaca degradacja „brak pytania/encji". Retrieval degraduje do czysto
        // wektorowego (rerank + LLM). Bez tej bramki query nadal fuzowalby istniejacy graf.
        if !graph_enabled_in_meta(&out) {
            return Ok(out);
        }

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
        // D5 alias-rewrite (R5, TYLKO retrieval-side): przepisz alias->canonical na seedach,
        // by zapytanie o alias ("einstein") trafilo w kanoniczny wezel PPR ("albert einstein").
        // Mapa aliasow przychodzi z addona RAG przez flow.meta (host-fn allowlist); brak => no-op.
        let seeds = rewrite_seeds_with_aliases(seeds, out.meta.get(META_ENTITY_ALIASES));
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

        // P_init structure-aware (MemGraphRAG §6.2): boost relevance liczymy tu
        // (zalezy od pasazy wektorowych w payloadzie), a kare log-degree, cap
        // liczby kotwic i PPR robi `ppr_with_p_init` nad JEDNYM snapshotem CSR —
        // inaczej stopnie i ranking mogłyby byc z roznych snapshotow, a kandydat
        // capniety przed przewazeniem nigdy nie zostalby rozwazony.
        let passage_text = envelope.payload.as_text().unwrap_or_default();
        let density = density_from_meta(envelope);
        let seeds = apply_relevance_boost(seeds, passage_text, &density);

        // Krok 1: PPR z P_init na seedach -> top encje powiazane w grafie. Cap do
        // MAX_GRAPH_SEEDS zapada wewnatrz, PO przewazeniu base × relevance × log-degree.
        // damping=0.5 (MemGraphRAG eq. 20, λ=0.5): wysoki restart trzyma propagacje w
        // lokalnym sasiedztwie seedow i ogranicza semantic drift na multi-hop (papier
        // celowo wybiera 0.5, nie klasyczne 0.85 PageRanku).
        let ranked = match ctx.graph.ppr_with_p_init(
            &org,
            addon,
            KG_COLLECTION,
            &seeds,
            MAX_GRAPH_SEEDS,
            MAX_GRAPH_ENTITIES,
            0.5,
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

        // Bramka toggle opcjonalnego grafu: przy `graph_enabled=false` adapter jest
        // NO-OP — przepuszcza envelope z nietknietym kontekstem wektorowym, bez PPR i
        // bez fuzji faktow grafowych (zero zapytan do grafu). Identyczne jak degradacja
        // „brak seedow/grafu", gdzie `collect_facts` zwraca pusta liste i nic nie doklejamy.
        if !graph_enabled_in_meta(&out) {
            return Ok(out);
        }

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
        assert!(
            ids.contains(&"ada") && ids.contains(&"lovelace"),
            "ids: {ids:?}"
        );
    }

    #[test]
    fn identify_longer_ngrams_weigh_more() {
        let seeds = identify_query_entities("Albert Einstein");
        // 2-gram „albert einstein" ma wyzsza wage niz unigramy -> jest pierwszy.
        assert_eq!(seeds[0].0, "albert einstein");
        assert!(
            seeds[0].1 > 1.0,
            "fraza wielowyrazowa ma wage > 1: {:?}",
            seeds[0]
        );
    }

    #[test]
    fn identify_drops_stopwords_and_short_tokens() {
        let seeds = identify_query_entities("kto byl prezesem w IBM");
        let ids: Vec<&str> = seeds.iter().map(|(id, _)| id.as_str()).collect();
        // „kto", „byl", „w" to stopwordy / za krotkie — nie sa unigramami-seedami.
        assert!(
            !ids.contains(&"kto"),
            "stopword 'kto' nie jest seedem: {ids:?}"
        );
        assert!(
            !ids.contains(&"byl"),
            "stopword 'byl' nie jest seedem: {ids:?}"
        );
        assert!(
            !ids.contains(&"w"),
            "za krotki 'w' nie jest seedem: {ids:?}"
        );
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
    fn identify_caps_seed_count_to_candidate_pool() {
        // Producent NIE capuje do MAX_GRAPH_SEEDS (16) — niesie pelna pule do
        // MAX_GRAPH_SEED_CANDIDATES (64), zeby finalny cap zapadl PO przewazeniu
        // w PPR. Dlugie zapytanie z wieloma unikalnymi tokenami: liczba kandydatow
        // capnieta do puli (anti-DoS), ale MOZE przekroczyc 16.
        let words: Vec<String> = (0..120).map(|i| format!("token{i:03}")).collect();
        let seeds = identify_query_entities(&words.join(" "));
        assert!(
            seeds.len() <= MAX_GRAPH_SEED_CANDIDATES,
            "liczba kandydatow capnieta do {MAX_GRAPH_SEED_CANDIDATES}, bylo {}",
            seeds.len()
        );
        assert!(
            seeds.len() > MAX_GRAPH_SEEDS,
            "pula kandydatow przekracza MAX_GRAPH_SEEDS (16) — pula NIE jest martwa, bylo {}",
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

    // --- D5 alias-rewrite seedow (R5, retrieval-side) ----------------------

    #[test]
    fn alias_rewrite_maps_alias_to_canonical() {
        // Seed "einstein" (alias) -> "albert einstein" (canonical) wg mapy z meta.
        let seeds = vec![("einstein".to_string(), 2.0), ("physics".to_string(), 1.0)];
        let aliases = json!([{ "alias": "einstein", "canonical": "albert einstein" }]);
        let out = rewrite_seeds_with_aliases(seeds, Some(&aliases));
        let ids: Vec<&str> = out.iter().map(|(id, _)| id.as_str()).collect();
        assert!(
            ids.contains(&"albert einstein"),
            "alias przepisany na canonical: {ids:?}"
        );
        assert!(!ids.contains(&"einstein"), "alias zniknal: {ids:?}");
        assert!(ids.contains(&"physics"), "nie-alias bez zmian: {ids:?}");
    }

    #[test]
    fn alias_rewrite_merges_duplicate_canonical_keeping_max_weight() {
        // Dwa aliasy ("usa", "us") tej samej encji -> jeden canonical "united states", waga = MAX.
        let seeds = vec![("usa".to_string(), 1.0), ("us".to_string(), 3.0)];
        let aliases = json!([
            { "alias": "usa", "canonical": "united states" },
            { "alias": "us", "canonical": "united states" }
        ]);
        let out = rewrite_seeds_with_aliases(seeds, Some(&aliases));
        assert_eq!(out.len(), 1, "duplikaty canonical scalone: {out:?}");
        assert_eq!(out[0].0, "united states");
        assert_eq!(
            out[0].1, 3.0,
            "scalona waga = MAX (najsilniejszy sygnal PPR)"
        );
    }

    #[test]
    fn alias_rewrite_no_map_is_noop() {
        let seeds = vec![("einstein".to_string(), 2.0)];
        // Brak mapy / pusta / zly ksztalt => seedy bez zmian (degradacja).
        assert_eq!(rewrite_seeds_with_aliases(seeds.clone(), None), seeds);
        assert_eq!(
            rewrite_seeds_with_aliases(seeds.clone(), Some(&json!([]))),
            seeds
        );
        assert_eq!(
            rewrite_seeds_with_aliases(seeds.clone(), Some(&json!("garbage"))),
            seeds
        );
    }

    #[test]
    fn rag_graph_seed_applies_alias_rewrite_from_meta() {
        // E2E adaptera: meta.entity_aliases -> seedy w meta.graph_seeds przepisane na canonical.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("einstein".to_string());
        env.meta.insert(
            META_ENTITY_ALIASES.to_string(),
            json!([{ "alias": "einstein", "canonical": "albert einstein" }]),
        );
        let ctx = stub_ctx();
        let adapter = RagGraphSeedNodeAdapter::new();
        let out = tokio_test_block(adapter.execute(&node("rag_graph_seed"), &[input(env)], &ctx))
            .unwrap();
        let seeds = out
            .meta
            .get(META_GRAPH_SEEDS)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let ids: Vec<String> = seeds
            .iter()
            .filter_map(|s| s.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        assert!(
            ids.iter().any(|i| i == "albert einstein"),
            "seed przepisany na canonical: {ids:?}"
        );
        assert!(
            !ids.iter().any(|i| i == "einstein"),
            "alias zniknal z seedow: {ids:?}"
        );
    }

    /// Mini-runtime do odpalenia jednego async execute w tescie synchronicznym.
    fn tokio_test_block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
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
        assert!(
            text.contains("einstein — pracowal_w → princeton"),
            "tekst: {text}"
        );
        assert!(
            text.contains("einstein — urodzil_sie_w → ulm"),
            "tekst: {text}"
        );
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
        assert_eq!(
            lines, MAX_GRAPH_FACTS,
            "liczba faktow capnieta, bylo {lines} linii"
        );
    }

    // --- fuse_context (fuzja pasaze + fakty) -------------------------------

    #[test]
    fn fuse_appends_graph_section() {
        let vec_ctx = "Pytanie: Q\n\nKontekst (zakumulowane pasaże):\n[0] (doc=d1) pasaz\n";
        let facts = "- einstein — pracowal_w → princeton\n";
        let fused = fuse_context(vec_ctx, facts);
        // Oba zrodla obecne: pasaze (wektor) + sekcja faktow (graf).
        assert!(fused.contains("pasaz"), "pasaze zachowane: {fused}");
        assert!(
            fused.contains("Fakty z grafu wiedzy:"),
            "naglowek faktow: {fused}"
        );
        assert!(
            fused.contains("einstein — pracowal_w → princeton"),
            "fakt: {fused}"
        );
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
        assert!(seeds
            .iter()
            .all(|s| s.get("weight").and_then(|w| w.as_f64()).is_some()));
    }

    #[tokio::test]
    async fn graph_seed_no_entities_degrades_to_empty_seeds() {
        // Zapytanie bez encji (same stopwordy) -> pusta lista seedow w meta,
        // NIE blad. Hop grafowy zostanie pominiety (degradacja do wektora).
        let mut env = FlowEnvelope::empty();
        env.meta
            .insert(META_CURRENT_QUERY.into(), json!("kto co czy"));
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
        assert!(
            !ctx_text.contains("Fakty z grafu wiedzy"),
            "brak sekcji faktow"
        );
        // meta.rag_graph_facts ustawione na pusty string (spojny stan).
        assert_eq!(
            out.meta.get(META_GRAPH_FACTS).and_then(|v| v.as_str()),
            Some("")
        );
    }

    // --- toggle opcjonalnego grafu (graph_enabled w meta) ------------------

    #[tokio::test]
    async fn graph_seed_noop_when_graph_disabled() {
        // graph_enabled=false -> NO-OP: payload bez zmian, ZADNYCH seedow w meta
        // (zero zapytan do grafu). Degradacja do czysto wektorowego retrievalu.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Albert Einstein".into());
        env.meta
            .insert(META_CURRENT_QUERY.into(), json!("Albert Einstein"));
        env.meta.insert(META_GRAPH_ENABLED.into(), json!(false));
        let out = RagGraphSeedNodeAdapter::new()
            .execute(&node("rag_graph_seed"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("Albert Einstein"));
        // Klucz seedow w ogole nie powstaje — adapter wyszedl przed identyfikacja encji.
        assert!(
            out.meta.get(META_GRAPH_SEEDS).is_none(),
            "OFF nie zapisuje seedow: {:?}",
            out.meta.get(META_GRAPH_SEEDS)
        );
    }

    #[tokio::test]
    async fn graph_seed_runs_when_flag_absent_legacy() {
        // Brak flagi (legacy/inny caller bez toggle) -> zachowanie dotychczasowe:
        // seedy sa wyliczane normalnie. Inaczej zlamalibysmy flowy bez addona RAG.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Albert Einstein".into());
        env.meta
            .insert(META_CURRENT_QUERY.into(), json!("Albert Einstein"));
        let out = RagGraphSeedNodeAdapter::new()
            .execute(&node("rag_graph_seed"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert!(
            out.meta
                .get(META_GRAPH_SEEDS)
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty()),
            "brak flagi => graf ON => seedy wyliczone"
        );
    }

    #[tokio::test]
    async fn graph_facts_noop_when_graph_disabled() {
        // graph_enabled=false -> NO-OP: kontekst wektorowy przechodzi bez zmian,
        // brak sekcji faktow, brak meta faktow (zero zapytan do grafu). Identyczna
        // degradacja wektorowa jak przy braku seedow, ale bez wchodzenia w PPR.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("Pytanie: Q\n\npasaze...\n".into());
        // Nawet z obecnymi seedami adapter ma byc NO-OP przy OFF.
        env.meta.insert(
            META_GRAPH_SEEDS.into(),
            json!([{"id": "einstein", "weight": 1.0}]),
        );
        env.meta.insert(META_GRAPH_ENABLED.into(), json!(false));
        let out = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(env)], &stub_ctx())
            .await
            .unwrap();
        let ctx_text = out.payload.as_text().unwrap();
        assert_eq!(ctx_text, "Pytanie: Q\n\npasaze...\n", "kontekst bez zmian");
        assert!(
            !ctx_text.contains("Fakty z grafu wiedzy"),
            "brak sekcji faktow"
        );
        // Adapter wyszedl przed fuzja -> nie zapisuje meta faktow.
        assert!(
            out.meta.get(META_GRAPH_FACTS).is_none(),
            "OFF nie zapisuje faktow"
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
            "org-1",
            "inst-a",
            KG_COLLECTION,
            "einstein",
            "pracowal_w",
            "princeton",
            1.0,
            "{}",
            "null",
        )
        .unwrap();

        // rag_graph_seed identyfikuje „einstein" z pytania; rag_graph_facts
        // seeduje PPR i wyciaga fakt.
        let mut env = FlowEnvelope::empty();
        env.payload =
            FlowValue::Text("Pytanie: gdzie pracowal Einstein?\n\npasaz wektorowy\n".into());
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
        assert!(
            ctx_text.contains("pasaz wektorowy"),
            "pasaz wektorowy zachowany: {ctx_text}"
        );
        assert!(
            ctx_text.contains("Fakty z grafu wiedzy:"),
            "sekcja faktow: {ctx_text}"
        );
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
            "org-1",
            "inst-a",
            KG_COLLECTION,
            "tesla",
            "wynalazl",
            "radio",
            1.0,
            "{}",
            "null",
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
        assert!(
            !ctx_text.contains("tesla"),
            "globalna encja nie wyciekla: {ctx_text}"
        );
    }

    // --- seeds_from_meta pula kandydatow (bug 2) ---------------------------

    #[test]
    fn seeds_from_meta_caps_candidate_pool() {
        // meta.graph_seeds z wieksza liczba seedow niz MAX_GRAPH_SEED_CANDIDATES
        // (np. po mutacji w innym flow) musi byc przyciete do puli kandydatow.
        // NIE do MAX_GRAPH_SEEDS — finalny cap zapada PO przewazeniu w
        // `ppr_with_p_init`, zeby kandydat poza pierwszymi 16 mial szanse.
        let seeds_json: Vec<Value> = (0..(MAX_GRAPH_SEED_CANDIDATES + 10))
            .map(|i| json!({"id": format!("e{i}"), "weight": 1.0}))
            .collect();
        let mut env = FlowEnvelope::empty();
        env.meta
            .insert(META_GRAPH_SEEDS.into(), Value::Array(seeds_json));
        let seeds = seeds_from_meta(&env);
        assert_eq!(
            seeds.len(),
            MAX_GRAPH_SEED_CANDIDATES,
            "pula kandydatow przycieta do {MAX_GRAPH_SEED_CANDIDATES}, bylo {}",
            seeds.len()
        );
        // Wieksza niz finalny cap — P_init dostaje pelna pule do przewazenia.
        assert!(seeds.len() > MAX_GRAPH_SEEDS);
        assert_eq!(seeds[0].0, "e0");
    }

    #[test]
    fn seeds_from_meta_carries_weight() {
        // Waga z meta MUSI przeplynac (R6) — nie jest gubiona. Brak wagi => 1.0.
        let mut env = FlowEnvelope::empty();
        env.meta.insert(
            META_GRAPH_SEEDS.into(),
            json!([{"id": "a", "weight": 3.5}, {"id": "b"}]),
        );
        let seeds = seeds_from_meta(&env);
        assert_eq!(seeds[0], ("a".to_string(), 3.5));
        assert_eq!(seeds[1], ("b".to_string(), 1.0), "brak wagi => 1.0");
    }

    // --- P_init relevance (boost adapter-side) -----------------------------
    // Kara log-degree i cap-po-przewazeniu testowane sa w `services::graph`
    // (`ppr_with_p_init`), bo licza sie nad CSR. Tu tylko boost relevance.

    #[test]
    fn p_init_relevance_boosts_entities_in_passages() {
        // Encja obecna w top-pasazach wektorowych dostaje boost relevance.
        let passages = "Albert Einstein opracowal teorie wzglednosci.";
        let out = apply_relevance_boost(
            vec![
                ("albert einstein".into(), 1.0),
                ("isaac newton".into(), 1.0),
            ],
            passages,
            &std::collections::HashMap::new(),
        );
        let w = |id: &str| out.iter().find(|(x, _)| x == id).map(|(_, w)| *w).unwrap();
        assert!(
            w("albert einstein") > w("isaac newton"),
            "encja w pasazach mocniejsza: {} vs {}",
            w("albert einstein"),
            w("isaac newton")
        );
        // Brak mapy density => plaski RELEVANCE_BOOST (degradacja jak dawniej).
        assert_eq!(w("albert einstein"), RELEVANCE_BOOST);
    }

    #[test]
    fn p_init_information_density_scales_boost_by_rarity() {
        // Dwie encje w pasazu: rzadka (density=1.0) dostaje 2× RELEVANCE_BOOST,
        // pospolita (density=0.0) tylko 1× — eq. 19 information density.
        let passages = "Osimertinib leczy raka pluc. Pacjent woli herbate.";
        let mut density = std::collections::HashMap::new();
        density.insert("osimertinib".to_string(), 1.0); // unikalna nazwa leku
        density.insert("pacjent".to_string(), 0.0); // generyczne slowo
        let out = apply_relevance_boost(
            vec![("osimertinib".into(), 1.0), ("pacjent".into(), 1.0)],
            passages,
            &density,
        );
        let w = |id: &str| out.iter().find(|(x, _)| x == id).map(|(_, w)| *w).unwrap();
        assert_eq!(
            w("osimertinib"),
            RELEVANCE_BOOST * 2.0,
            "rzadka encja: 2× boost"
        );
        assert_eq!(w("pacjent"), RELEVANCE_BOOST, "pospolita encja: 1× boost");
        assert!(
            w("osimertinib") > w("pacjent"),
            "rzadka kotwica mocniejsza niz pospolita"
        );
    }

    #[test]
    fn density_from_meta_parses_and_clamps() {
        let mut env = FlowEnvelope::empty();
        env.meta.insert(
            META_ENTITY_DENSITY.into(),
            json!([
                { "id": "osimertinib", "density": 0.9 },
                { "id": "pacjent", "density": 0.0 },
                { "id": "przepelnione", "density": 5.0 },   // clamp do 1.0
                { "id": "", "density": 0.5 },                 // puste id -> pominiete
                { "id": "brak_density" },                     // brak density -> pominiete
                { "id": "nan", "density": null }              // null -> pominiete
            ]),
        );
        let map = density_from_meta(&env);
        assert_eq!(map.get("osimertinib"), Some(&0.9));
        assert_eq!(map.get("pacjent"), Some(&0.0));
        assert_eq!(
            map.get("przepelnione"),
            Some(&1.0),
            "density > 1 clamp do 1"
        );
        assert!(!map.contains_key(""), "puste id pominiete");
        assert!(!map.contains_key("brak_density"));
        assert!(!map.contains_key("nan"));
    }

    #[test]
    fn density_from_meta_absent_is_empty() {
        let env = FlowEnvelope::empty();
        assert!(
            density_from_meta(&env).is_empty(),
            "brak klucza => pusta mapa"
        );
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
        assert_eq!(
            merged.len(),
            2,
            "duplikat (a,r,b) zdeduplikowany: {merged:?}"
        );
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
            "org-1",
            "inst-a",
            KG_COLLECTION,
            "einstein",
            "pracowal_w",
            "princeton",
            1.0,
            "{}",
            "null",
        )
        .unwrap();
        g.upsert_edge_with_quota(
            "org-1",
            "inst-a",
            KG_COLLECTION,
            "tesla",
            "wynalazl",
            "radio",
            1.0,
            "{}",
            "null",
        )
        .unwrap();

        // Hop 1: seed „einstein" -> fakt pracowal_w.
        let mut env1 = FlowEnvelope::empty();
        env1.payload = FlowValue::Text("kontekst hop1\n".into());
        env1.meta.insert(
            META_GRAPH_SEEDS.into(),
            json!([{"id": "einstein", "weight": 1.0}]),
        );
        let after1 = RagGraphFactsNodeAdapter::new()
            .execute(&node("rag_graph_facts"), &[input(env1)], &ctx)
            .await
            .unwrap();

        // Hop 2: zmieniony seed na „tesla" -> nowy fakt wynalazl. Akumulator
        // (meta.rag_graph_facts_accumulated) przechodzi z hopu 1 do hopu 2.
        let mut env2: FlowEnvelope = after1;
        env2.payload = FlowValue::Text("kontekst hop2\n".into());
        env2.meta.insert(
            META_GRAPH_SEEDS.into(),
            json!([{"id": "tesla", "weight": 1.0}]),
        );
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
