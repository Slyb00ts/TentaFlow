# Uniwersalne metadane wektorowe i filtr AST (v1)

Warstwa, dzięki której addon zapisuje typowane metadane przy wektorach i filtruje
wyniki k-NN **po naszemu** — niezależnie od tego, który backend (wbudowany
**zvec** czy zewnętrzny **Milvus**) obsługuje dany namespace. Addon nigdy nie
pisze składni konkretnego silnika; core tłumaczy uniwersalny model na natywną
formę wybranego backendu.

Model jest jednym źródłem prawdy współdzielonym przez hosta (Rust), Rust addon
SDK oraz przyszłe SDK w Pythonie i C#. Typy są kodowane w **CBOR (minicbor)** ze
stałymi tagami całkowitoliczbowymi — ten sam kształt bajtów odczyta każdy język.

---

## 1. Przegląd

- **Namespace** = jedna kolekcja wektorów (jeden plik/katalog w zvec albo jedna
  kolekcja w Milvus). Addon deklaruje go w manifeście; core wybiera backend
  per-addon (GUI admina, klucze `__vector_backend` w `addon_config`).
- **Metadane** = typowane kolumny skalarne przypięte do wektora (np. `source`,
  `score`, `lang`). Schemat deklaruje manifest (`[[vector_namespace]].fields`).
- **Filtr** = strukturalne drzewo (AST) nad polami metadanych. Core renderuje je
  do wyrażenia natywnego backendu (zvec albo Milvus).
- **Output fields** = lista nazw pól metadanych zwracanych przy każdym hicie.

```
addon (Rust/Python/C#) --CBOR--> core --tłumaczy--> zvec  ALBO  Milvus
   buduje Filter AST              filter::to_zvec / to_milvus
```

---

## 2. Deklaracja w manifeście

Pola metadanych deklaruje się w bloku `[[vector_namespace]]`:

```toml
[[vector_namespace]]
name       = "documents"
dimensions = 768
distance   = "cosine"       # cosine | euclidean | dot
data_class = "B"            # klasa RODO: A | B | C
# gate     = "d4-historical"  # opcjonalny gate polityki

  [[vector_namespace.fields]]
  name    = "source"
  type    = "str"           # str | int | float | bool
  indexed = true            # zbuduj indeks skalarny (do filtrowania)

  [[vector_namespace.fields]]
  name    = "score"
  type    = "int"

  [[vector_namespace.fields]]
  name    = "fresh"
  type    = "bool"
```

Dla wyszukiwania **hybrydowego** (dense + sparse, typowe w RAG) ustaw flagę
`sparse`:

```toml
[[vector_namespace]]
name       = "documents"
dimensions = 768
distance   = "cosine"
data_class = "B"
sparse     = true          # kolekcja dostaje dodatkowe pole sparse
```

Flaga jest ustalana przy tworzeniu namespace (utrwalana w kolumnie
`addon_vector_namespaces.sparse`, migracja v53) i nie podlega reconciliation —
zmiana `sparse` dla istniejącego namespace wymaga jego odtworzenia. Addon sam
dostarcza wektor sparse (np. BM25/SPLADE) — core nie tokenizuje, tak jak nie
liczy embeddingu dense.

Reguły:

- `type` ∈ `{str, int, float, bool}` (walidowane przy instalacji — literówka
  przerywa instalację).
- `indexed` domyślnie `false`. Dla pól używanych w filtrach ustaw `true`.
- Schemat jest utrwalany w kolumnie `addon_vector_namespaces.fields_json`
  (migracja v52) przy pierwszym użyciu namespace. Od tego momentu jest
  autorytatywny — patrz [§7 Reconciliation](#7-reconciliation).

---

## 3. Model typów i kontrakt CBOR

Definicje: `tentaflow-sdk-spec/src/protocol/vector_query.rs`. Wszystkie enumy
kodują wariant jako **tag całkowitoliczbowy** (`#[n(N)]`); struktury z
`#[cbor(map)]` kodują pola jako mapę o kluczach całkowitych. Te tagi są **częścią
kontraktu wire** — implementacja w innym języku MUSI używać tych samych liczb.

### 3.1 FieldType — typ pola (enum, tag = wariant)

| Tag | Wariant |
|-----|---------|
| 0   | `Str`   |
| 1   | `Int`   |
| 2   | `Float` |
| 3   | `Bool`  |

### 3.2 FieldValue — wartość pola (enum z jednym polem `#[n(0)]`)

| Tag | Wariant | Ładunek `#[n(0)]` |
|-----|---------|-------------------|
| 0   | `Str`   | `tstr`            |
| 1   | `Int`   | `i64`             |
| 2   | `Float` | `f64`             |
| 3   | `Bool`  | `bool`            |

### 3.3 Field — nazwane pole (`#[cbor(map)]`)

| Klucz | Pole    | Typ          |
|-------|---------|--------------|
| 0     | `name`  | `tstr`       |
| 1     | `value` | `FieldValue` |

### 3.4 FieldSpec — deklaracja pola w schemacie (`#[cbor(map)]`)

| Klucz | Pole         | Typ         |
|-------|--------------|-------------|
| 0     | `name`       | `tstr`      |
| 1     | `field_type` | `FieldType` |
| 2     | `indexed`    | `bool`      |

> `FieldSpec` jest po stronie hosta/manifestu — addon zwykle go nie wysyła
> (schemat pochodzi z manifestu). Jest tu dla kompletności kontraktu.

### 3.5 Filter — drzewo filtra (enum)

Warianty porównań to `(field_name: tstr, value: FieldValue)`, czyli pola
`#[n(0)]` i `#[n(1)]`. Operatory logiczne biorą listę/box pod `#[n(0)]`.

| Tag | Wariant | Ładunek |
|-----|---------|---------|
| 0   | `Eq`    | `(tstr, FieldValue)` — `field = value` |
| 1   | `Ne`    | `(tstr, FieldValue)` — `field != value` |
| 2   | `Gt`    | `(tstr, FieldValue)` — `field > value` |
| 3   | `Gte`   | `(tstr, FieldValue)` — `field >= value` |
| 4   | `Lt`    | `(tstr, FieldValue)` — `field < value` |
| 5   | `Lte`   | `(tstr, FieldValue)` — `field <= value` |
| 6   | `In`    | `(tstr, array<FieldValue>)` — `field in [...]` |
| 7   | `And`   | `array<Filter>` |
| 8   | `Or`    | `array<Filter>` |
| 9   | `Not`   | `Filter` (jeden, zagnieżdżony) |

`And`/`Or` z pustą listą oraz `In` z pustą listą są **odrzucane** przez core
(`InvalidFilter`).

### 3.6 SparseVector — wektor rzadki (`#[cbor(map)]`)

| Klucz | Pole      | Typ          |
|-------|-----------|--------------|
| 0     | `indices` | `array<u32>` |
| 1     | `values`  | `array<f32>` |

`indices` i `values` muszą mieć równą długość. `indices[i]` to id termu/wymiaru,
`values[i]` jego waga.

### 3.7 Fusion — strategia fuzji hybrydowej (enum)

| Tag | Wariant    | Ładunek |
|-----|------------|---------|
| 0   | `Rrf`      | `u32` — stała rankingowa (60 = konwencja) |
| 1   | `Weighted` | `(f32, f32)` — `(waga_dense, waga_sparse)` |

`Rrf` (Reciprocal Rank Fusion) to domyślna, odporna strategia — nie wymaga
strojenia. `Weighted` sumuje znormalizowane score z wagami.

---

## 4. ABI host-funkcji wektorowych

Ładunki: `tentaflow-sdk-spec/src/protocol/vector.rs`. Wektor jedzie po wire jako
base64(little-endian f32) w polu `*_b64`. Nowe pola są `Option` — brak klucza w
mapie dekoduje się do `None` (kompatybilność wstecz: starszy addon bez metadanych
działa bez zmian).

### 4.1 `vector_upsert_v1` — wejście `VectorUpsertInput`

| Klucz | Pole         | Typ                  |
|-------|--------------|----------------------|
| 0     | `namespace`  | `tstr`               |
| 1     | `ref_id`     | `u64`                |
| 2     | `vector_b64` | `tstr`               |
| 3     | `fields`     | `Option<array<Field>>` |
| 4     | `sparse`     | `Option<SparseVector>` (tylko gdy namespace ma `sparse = true`) |

### 4.1a `vector_hybrid_search_v1` — wejście `VectorHybridSearchInput`

| Klucz | Pole            | Typ                   |
|-------|-----------------|-----------------------|
| 0     | `namespace`     | `tstr`                |
| 1     | `dense_b64`     | `tstr`                |
| 2     | `sparse`        | `SparseVector`        |
| 3     | `k`             | `u32`                 |
| 4     | `gate_claim_id` | `Option<tstr>`        |
| 5     | `filter`        | `Option<Filter>`      |
| 6     | `output_fields` | `Option<array<tstr>>` |
| 7     | `fusion`        | `Option<Fusion>` (brak = RRF 60) |

Wyjście to ten sam `VectorSearchOutput` co przy `vector_search_v1`.

### 4.2 `vector_search_v1` — wejście `VectorSearchInput`

| Klucz | Pole            | Typ                     |
|-------|-----------------|-------------------------|
| 0     | `namespace`     | `tstr`                  |
| 1     | `query_b64`     | `tstr`                  |
| 2     | `k`             | `u32`                   |
| 3     | `gate_claim_id` | `Option<tstr>`          |
| 4     | `filter`        | `Option<Filter>`        |
| 5     | `output_fields` | `Option<array<tstr>>`   |

### 4.3 `vector_search_v1` — hit `VectorSearchHit`

| Klucz | Pole     | Typ                    |
|-------|----------|------------------------|
| 0     | `ref_id` | `u64`                  |
| 1     | `score`  | `f32`                  |
| 2     | `fields` | `Option<array<Field>>` |

`fields` w hicie wraca tylko dla pól wymienionych w `output_fields`.

---

## 5. Użycie w Rust (addon SDK)

Re-eksporty w `tentaflow_addon_sdk::prelude`: `VectorField`, `VectorFieldValue`,
`VectorFieldType`, `VectorFilter` (aliasy typów z sdk-spec).

```rust
use tentaflow_addon_sdk::prelude::*;

// Upsert wektora z metadanymi (nazwy + typy muszą zgadzać się z manifestem).
let fields = [
    VectorField { name: "source".into(), value: VectorFieldValue::Str("inbox".into()) },
    VectorField { name: "score".into(),  value: VectorFieldValue::Int(42) },
];
let count = vector_upsert("documents", 1001, &embedding, &fields)?;

// Filtr: source = 'inbox' AND score >= 10
let filter = VectorFilter::And(vec![
    VectorFilter::Eq("source".into(), VectorFieldValue::Str("inbox".into())),
    VectorFilter::Gte("score".into(), VectorFieldValue::Int(10)),
]);

// Top-10 z filtrem; zwróć pola "source" i "score" przy każdym hicie.
let hits = vector_search(
    "documents",
    &query,
    10,
    None,                       // gate_claim_id
    Some(&filter),
    &["source", "score"],
)?;

for h in hits {
    // h.ref_id, h.score, h.fields: Vec<VectorField>
}
```

Brak metadanych / brak filtra: `vector_upsert(ns, id, &v, &[])`,
`vector_search(ns, &q, k, None, None, &[])`.

Hybryda (dense + sparse) — namespace z `sparse = true`:

```rust
use tentaflow_addon_sdk::prelude::*;

// Zapis: dense embedding + sparse (np. BM25/SPLADE policzony przez addon).
let sparse = SparseVector { indices: vec![100, 305, 7012], values: vec![0.9, 1.4, 0.6] };
vector_upsert_sparse("documents", 1001, &embedding, &fields, Some(&sparse))?;

// Zapytanie hybrydowe: dense query + sparse query, fuzja RRF (None = RRF 60).
let q_sparse = SparseVector { indices: vec![305], values: vec![1.0] };
let hits = vector_hybrid_search(
    "documents",
    &query_embedding,
    &q_sparse,
    10,
    None,                                  // gate_claim_id
    Some(&VectorFilter::Eq("lang".into(), VectorFieldValue::Str("pl".into()))),
    &["source"],
    Some(VectorFusion::Weighted(0.6, 0.4)), // albo None => RRF
)?;
```

---

## 6. Użycie w Python i C# (kontrakt CBOR)

Nie ma jeszcze dedykowanego addon SDK dla Pythona/C#. Do czasu jego powstania
ładunki buduje się bezpośrednio jako CBOR według tagów z [§3](#3-model-typów-i-kontrakt-cbor)
i [§4](#4-abi-host-funkcji-wektorowych). Poniżej wzorce 1:1 z modelem Rusta.

### 6.1 Python (biblioteka `cbor2`)

```python
import cbor2, struct

def vec_b64(values):  # little-endian f32 -> bytes (potem base64 jak w ABI)
    import base64
    return base64.b64encode(struct.pack(f"<{len(values)}f", *values)).decode()

# FieldValue: {tag: [payload]} — enum minicbor = 1-elementowa mapa {wariant: [pola]}
def fv_str(s):   return {0: [s]}
def fv_int(i):   return {1: [i]}
def fv_float(f): return {2: [f]}
def fv_bool(b):  return {3: [b]}

# Field: mapa {0: name, 1: value}
def field(name, value): return {0: name, 1: value}

# Filter: {tag: [pola]}
def f_eq(name, value):  return {0: [name, value]}
def f_gte(name, value): return {3: [name, value]}
def f_in(name, values): return {6: [name, values]}
def f_and(parts):       return {7: [parts]}
def f_or(parts):        return {8: [parts]}
def f_not(inner):       return {9: [inner]}

# VectorUpsertInput {0:namespace, 1:ref_id, 2:vector_b64, 3:fields?}
upsert = {
    0: "documents",
    1: 1001,
    2: vec_b64(embedding),
    3: [field("source", fv_str("inbox")), field("score", fv_int(42))],
}
payload = cbor2.dumps(upsert)

# VectorSearchInput {0:ns,1:query_b64,2:k,3:gate?,4:filter?,5:output_fields?}
search = {
    0: "documents",
    1: vec_b64(query),
    2: 10,
    4: f_and([f_eq("source", fv_str("inbox")), f_gte("score", fv_int(10))]),
    5: ["source", "score"],
}
```

> Enum minicbor koduje się jako jednoelementowa mapa `{tag: [pola_wariantu]}`.
> Wariant bez pól (gdyby istniał) to `{tag: []}`. Klucze opcjonalne (`gate`,
> `filter`, `output_fields`, `fields`) po prostu pomija się, gdy `None`.

Hybryda w Pythonie — `SparseVector` to mapa `{0: indices, 1: values}`, `Fusion`
to enum `{tag: [...]}`:

```python
def sparse_vec(indices, values): return {0: indices, 1: values}
def fusion_rrf(k):               return {0: [k]}
def fusion_weighted(d, s):       return {1: [d, s]}

# VectorUpsertInput z polem sparse (klucz 4)
upsert = {0: "documents", 1: 1001, 2: vec_b64(embedding),
          4: sparse_vec([100, 305], [0.9, 1.4])}

# VectorHybridSearchInput {0:ns,1:dense_b64,2:sparse,3:k,4:gate?,5:filter?,6:output?,7:fusion?}
hybrid = {0: "documents", 1: vec_b64(query), 2: sparse_vec([305], [1.0]),
          3: 10, 7: fusion_rrf(60)}
payload = cbor2.dumps(hybrid)
```

### 6.2 C# (biblioteka `PeterO.Cbor` lub `System.Formats.Cbor`)

```csharp
using PeterO.Cbor;

// FieldValue {tag: [payload]}
CBORObject FvStr(string s)  => CBORObject.NewMap().Add(0, CBORObject.NewArray().Add(s));
CBORObject FvInt(long i)    => CBORObject.NewMap().Add(1, CBORObject.NewArray().Add(i));

// Field {0:name, 1:value}
CBORObject Field(string name, CBORObject value) =>
    CBORObject.NewMap().Add(0, name).Add(1, value);

// Filter {tag: [args]}
CBORObject FEq(string n, CBORObject v) =>
    CBORObject.NewMap().Add(0, CBORObject.NewArray().Add(n).Add(v));
CBORObject FGte(string n, CBORObject v) =>
    CBORObject.NewMap().Add(3, CBORObject.NewArray().Add(n).Add(v));
CBORObject FAnd(params CBORObject[] parts) {
    var arr = CBORObject.NewArray();
    foreach (var p in parts) arr.Add(p);
    return CBORObject.NewMap().Add(7, CBORObject.NewArray().Add(arr));
}

// VectorUpsertInput
var upsert = CBORObject.NewMap()
    .Add(0, "documents")
    .Add(1, 1001)
    .Add(2, VecB64(embedding))
    .Add(3, CBORObject.NewArray()
        .Add(Field("source", FvStr("inbox")))
        .Add(Field("score",  FvInt(42))));
byte[] payload = upsert.EncodeToBytes();
```

Hit dekoduje się odwrotnie: `VectorSearchHit` to mapa `{0:ref_id, 1:score,
2:fields?}`, gdzie `fields` to lista `Field`.

---

## 7. Reconciliation (zmiana schematu przy aktualizacji addona)

Gdy nowy manifest deklaruje inny zestaw `fields`, `lifecycle::upgrade` uruchamia
reconciliation dla każdego namespace, który ma już wiersz w DB (dla każdej org,
w której addon jest zainstalowany). Schemat zapisany w `fields_json` jest
autorytatywny — samo ponowne otwarcie namespace z innym zestawem pól **nie**
przekształca kolekcji; robi to dopiero jawny krok reconciliation.

Zachowanie zależne od backendu (`VectorBackend::reconcile_fields`):

- **zvec (wbudowany, domyślny)** — DDL online w zvec działa tylko dla typów
  numerycznych, więc uniwersalną ścieżką jest **pełny rebuild kolekcji**:
  odczyt wszystkich dokumentów (wektor + metadane) → utworzenie nowej kolekcji z
  docelowym schematem → ponowny zapis (usunięte pola pomijane, dodane pozostają
  `NULL`) → atomowy swap katalogu. Działa dla każdego typu i każdej zmiany
  (dodanie / usunięcie / zmiana typu). Identyczny schemat = no-op.
- **Milvus (zewnętrzny)** — dodanie pola jest online (`add_collection_field`,
  pole musi być `nullable`). Usunięcie pola ani zmiana typu **nie są** możliwe
  online w Milvus → reconciliation zwraca jasny błąd (admin musi odtworzyć
  kolekcję migracją). `upgrade` loguje ostrzeżenie i kończy się sukcesem; niespójny
  schemat ujawni się później jako czytelny błąd przy filtrze/zapisie.

`reconcile_namespace` zwraca `ReconcileReport { added, dropped }` do audytu.

---

## 8. Translacja do backendów i bezpieczeństwo

`services/vector/filter.rs` renderuje `Filter` do wyrażenia natywnego. Jedyna
różnica składniowa między backendami to operator równości:

| AST          | zvec            | Milvus           |
|--------------|-----------------|------------------|
| `Eq`         | `name = v`      | `name == v`      |
| `Ne`         | `name != v`     | `name != v`      |
| `Gt/Gte/Lt/Lte` | `name > v` …  | `name > v` …     |
| `In`         | `name in [..]`  | `name in [..]`   |
| `And/Or`     | `(a) and (b)`   | `(a) and (b)`    |
| `Not`        | `not (a)`       | `not (a)`        |

Bezpieczeństwo (wspólne dla obu backendów):

- **Nazwy pól** muszą pasować do `^[A-Za-z_][A-Za-z0-9_]{0,63}$` — blokuje
  wstrzyknięcie operatorów/cudzysłowów przez spreparowaną nazwę pola.
- **Literały tekstowe** są escapowane (`\` → `\\`, `'` → `\'`).
- **Float** musi być skończony (NaN/∞ → `InvalidFilter`).
- Puste grupy `And`/`Or` oraz pusta lista `In` → `InvalidFilter`.

Dzięki temu, że addon buduje **strukturalne drzewo** zamiast pisać składnię
filtra, te zabezpieczenia są wymuszane centralnie w core, a różnice składniowe
backendów są niewidoczne dla addona.
