# Ujednolicenie RAG: Projekty (Project Studio) ↔ addon RAG

Wersja 2.1 — po recenzji weryfikującej tezy w kodzie oraz po decyzji właściciela o **platformowych
aliasach i flowach RAG** (§0). Sekcje oznaczone **[korekta]** zmieniły się względem 1.0; §3.A, §3.B,
§3.C i §4 były w 1.0 błędne lub przeszacowane.

## 0. Decyzja kierunkowa: RAG jest częścią platformy, nie addona

Aliasy `rag-*` oraz flowy `ingest`, `query` i `retrieval-round` są dostarczane z TentaFlow **od
startu, bez zainstalowanego addona**. Addon RAG przestaje być właścicielem tych zasobów i staje się
ich konsumentem — zostają przy nim rzeczy faktycznie addonowe: schemat SQLite z migracjami, logika
grafowa (MemGraphRAG), narzędzia, deklaracje `[[vector_namespace]]` i `[[graph_collection]]`, UI.

Ta decyzja usuwa inwersję zależności, przez którą wbudowany moduł core opierał się na opcjonalnym
addonie, i **naprawia §3.B u źródła**. Ma trzy zweryfikowane konsekwencje mechaniczne — każda jest
warunkiem koniecznym, nie szczegółem wykonania:

1. **Aliasy platformowe muszą mieć `visibility = "public"`.** Dziś są `private` i działają wyłącznie
   dlatego, że `compute_uses_alias_status_within_tx` (`db/repository.rs:1749`) skraca przez
   `owner_match` — addon konsumuje własny alias. Gdy właścicielem przestanie być addon, `private`
   wpada w gałąź `_ => "denied"`, a `rag-embeddings` i `rag-parse` mają `required = true`, więc
   **install addona zostanie odrzucony** (`addon/mod.rs:1116-1122`). `public` → `auto_granted`.
2. **Addon musi USUNĄĆ `[[alias]]` dla tych aliasów** (zostawiając `[[uses_alias]]`). Inaczej
   `materialize_addon_aliases` (`lifecycle.rs:854`) rejestruje je z `owner_type="addon"`, co przy
   core-owym właścicielu wywala install guardem własności (`repository.rs:3216-3221`). Usunięcie
   `[[alias]]` jest zarazem tym, co realnie naprawia §3.B: `deactivate_aliases_owned_by_addon`
   działa po `model_alias_owners`, więc alias bez addonowego właściciela nie jest deaktywowany przy
   zatrzymaniu addona.
3. **`query` musi wołać `retrieval-round` przez `body_flow_id` (stały UUID seeda), nie
   `body_flow_engine_id`.** Ten drugi wymaga `ctx.addon_id` i składa nazwę `{addon}:{local}`
   (`loop_block.rs:134`), czego flow platformowy nie ma. **To ta sama zmiana, która odblokowuje
   Etap 5 dla Projektów** — blokada opisana wcześniej znika przy okazji, a nie jako osobna praca.

Uwaga o wiązaniu: seed tworzy aliasy **niezwiązane** (pusty target, zaparkowane) — dokładnie jak
dziś robi to manifest (`suggested_default = ""`), więc admin nadal podpina model ręcznie. Zmienia
się tylko to, że jego wybór przestaje znikać przy zatrzymaniu addona.

Cel: **jedna implementacja każdego mechanizmu**, używana przez oba konsumenty. Dane pozostają
rozdzielone (projekt pisze do swojego scope, addon do swojego); wspólny jest KOD i FLOW.
Mostkowanie „agent sięga po wiedzę z projektu albo z addona" jest osobnym tematem i rozwiązuje się
flowem po tej unifikacji — ten plan go nie realizuje, ale go umożliwia.

## 1. Granica: co ujednolicamy, czego NIE

**Ujednolicamy (kod + flow):** parsowanie dokumentu, chunking, embedding, zapis wektorów,
cleanup/dedup, kolejkę zadań ingestu, postęp i anulowanie, retrieval wektorowy.

**NIE ujednolicamy (i to jest poprawne):**
- **Dane.** Scope zostaje rozdzielony: addon pisze pod swój `instance_id`, projekt pod
  `ps-<project_id>` w katalogu projektu. Żaden etap tego nie zmienia.
- **ACL.** Projekty egzekwują członkostwo per użytkownik (`knowledge::require_member`, jednolity
  NotFound). Addon ma izolację per instancja + uprawnienia manifestu. Dwa różne modele autoryzacji
  dla dwóch różnych bytów — nie duplikacja, tylko odmienna semantyka.
- **Ekstrakcja grafu.** Patrz §6 (D3).

## 2. Inwentarz duplikacji (stan faktyczny)

| Mechanizm | Projekty | Addon RAG | Ocena |
|---|---|---|---|
| Ekstrakcja dokumentu | `ingest.rs` woła wprost `docx_to_markdown`/`xlsx`/`pptx`/`extract_pdf_text` | flow: `document_router` → `word/excel/pptx/text_extract`, `pdf_rasterize`, `vision_parse` | duplikacja + luka (Projekty nie umieją skanów) |
| Chunking prozy | `split_into_chunks` ze **stałymi** | node `chunk` — te same stałe, **konfigurowalne** | wspólna funkcja, rozjazd granic |
| **Chunkowanie kodu** **[nowe]** | `chunk_code` (`ingest.rs:1026`) — cięcie po liniach, `location` jako zakres linii | **brak odpowiednika** | funkcja PS bez pokrycia we flow → §5 Etap 3c |
| **Źródła URL** **[nowe]** | `WorkPayload::Url` → `web_research::reader::read_url` (`ingest.rs:1106`) | **brak** — trigger flow-ingestu przyjmuje wyłącznie blob | jw. |
| Embedding | `embed_texts` → `Router::execute_embeddings` | node `embed_chunks` | dwie ścieżki do tego samego aliasu |
| Zapis wektorów | `store_chunks` | node `store` | pełna duplikacja |
| **Schemat pól wektora** **[nowe]** | 6 pól: `doc_id, chunk_index, text, source_id, path, location` (`ingest.rs:538-566`) | 3 pola + opcjonalny `collection_id` (`store.rs:258-281`) | **niekompatybilne** → blokada Etapu 3 |
| `ref_id_for` (FNV-1a) | `ingest.rs:521` | `store.rs:121` | kopia dosłowna |
| Cleanup-then-reingest | `delete_doc_vectors`, wieloprzebiegowy | `store.rs`, jednoprzebiegowy | duplikacja; różnica węższa niż sądzono → §3.A |
| Kolejka / postęp / cancel | cancel registry + `log_bus` + semafor 2 | `ingest_jobs` + `ingest_drain` ze Schedulera | dwa niezależne systemy zadań |
| Skip po content-hash | jest dla drzew kodu (`diff_tree`), brak dla uploadu pliku | migracja `011_document_content_hash` | luka wąska → §3.C |
| Retrieval | `knowledge::search` — jeden strzał k-NN | `retrieval_round`: seed → embed → vector → reranker → akumulacja → sędzia, w pętli | duplikacja intencji, różna jakość |
| **`normalize_entity_name`** **[nowe]** | — | `addons/rag/src/lib.rs:2113` + **lustro w core** `node_adapters/rag_graphrag.rs:6-8` | duplikacja wewnątrz warstwy grafowej |

## 3. Wady wynikające z duplikacji

### 3.A Cleanup wektorów **[korekta — teza z 1.0 była przeszacowana]**
W 1.0 twierdziłem, że `store.rs` wycieka wektory, bo czyści jednym przebiegiem, a PS wieloma.
To było za mocne. `store_chunks_blocking` woła `delete_doc_vectors(..., Some(vectors[0]))`
(`ingest.rs:687`) — **tę samą realną sondę co `store.rs:294`**, co komentarz PS wprost przyznaje
(„store.rs pattern"). Wieloprzebiegowość ratuje wyłącznie ścieżkę `delete_file_vectors`, która
sonduje wektorem zerowym (`ingest.rs:589`).

Realna ekspozycja jest wąska: deterministyczny `ref_id` nadpisuje chunki `0..n-1`, więc orphany
powstają tylko gdy liczba chunków **spadnie**, a dedup po `content_hash` w addonie blokuje zwykły
re-upload. Zostaje requeue zawieszonego joba przy niedeterministycznym parsie (gałąź vision).

Premisa o przybliżonym recall filtrowanego ANN w zvec **pozostaje niezweryfikowana** — filtr trafia
do silnika C++, którego źródeł nie ma w repo. Nie jest to więc argument za Etapem 1; argumentem
jest sama duplikacja `ref_id_for` i ciała zapisu.

### 3.B Projekty i Code Studio padają, gdy addon RAG się zatrzyma **[korekta — poważniejsze niż w 1.0]**
W 1.0 napisałem, że alias `rag-embeddings` znika po odinstalowaniu addona. Defekt jest inny i
szerszy. Alias faktycznie nie jest seedowany przez core (jedyny seeder to `seed_camera_cv_aliases`,
`db/seed.rs:1260`, lista wyłącznie `tentavision-*`). Ale uninstall wiersza **nie kasuje** —
`deactivate_aliases_owned_by_addon` (`addon/mod.rs:3306`) ustawia `is_active=0`, i leci to również
**gdy zniknie ostatnia instancja addona** (`addon/mod.rs:2338-2340`). Resolver wymaga `is_active=1`
i niepustego targetu, a `ingest.rs:499-503` nie ma fallbacku → twardy błąd.

Czyli: **zwykłe zatrzymanie addona RAG wyłącza bazę wiedzy Projektów.** Dotyczy to także
`rag-parse`, `rag-llm`, `rag-reranker`, a przez `RouterEmbedder::embed` → `ingest::embed_texts`
(`code_studio/index.rs:126-129`) również indeksu Code Studio.

Naprawa nie polega na doseedowaniu wiersza: `INSERT OR IGNORE` bez wiersza w `model_alias_owners`
i tak odda własność pierwszemu installowi addona (`db/repository.rs:3194-3226`), a deaktywacja
wróci. **Rozwiązanie przyjęte w §0** — alias przestaje mieć addonowego właściciela — usuwa przyczynę,
a nie objaw. Wykonanie: Etap 0c.

### 3.C Skip po content-hash **[korekta — luka 3× węższa niż w 1.0]**
Dla drzew kodu Projekty mają dedup: `diff_tree` porównuje `(path, sha256)` (`ingest.rs:1001`) i do
re-embeddingu idą tylko `added`+`changed` (`repository.rs:1420`). Luka dotyczy **wyłącznie uploadu
pojedynczego pliku**.

### 3.D Projekty nie ingestują skanów i obrazów — ale naprawa nie jest darmowa **[korekta]**
`VISION_REQUIRED_MSG` → status `skipped`, podczas gdy flow ma `pdf_rasterize` + `vision_parse_pages`.
W 1.0 napisałem, że Etap 3 usuwa tę lukę „bez pisania nowego kodu". Nieprawda: ścieżka vision
zależy od aliasów `rag-parse`/`rag-ocr`, które są addonowe i podlegają temu samemu wyłączeniu co
§3.B. Bez Etapu 0c jest to zamiana jednej luki na warunkową zależność.

## 4. Architektura docelowa: jeden scope, jeden flow **[korekta — brakuje trzech rzeczy, nie jednej]**

Fundament istnieje: nody `store`/`vector` kluczują po `(org_id, ctx.addon_id, namespace)`, Projekty
**już** używają `ps-<project_id>` (`ingest::vector_scope`), a `get_or_create_at` istnieje właśnie
dla nie-addonowych właścicieli — przy istniejącym wierszu wygrywa zapisany `file_path`, więc
otwarcie z generycznego noda trafia we właściwy katalog.

W 1.0 twierdziłem, że brakuje **jednej** rzeczy (`vector_home`). Brakuje trzech:

1. **Przeciągnięcia `custom_dir` przez funkcje quota-upsert.** Nody nie wołają `get_or_create`
   bezpośrednio: `store.rs:349` woła `upsert_batch_with_quota`, `vector.rs:383` —
   `upsert_with_quota`, a namespace powstaje **wewnątrz** nich (`namespace.rs:838`, `:932`).
   Dlatego pre-create w `ingest.rs:696` nie jest hackiem do usunięcia, tylko konsekwencją API.
2. **`vector_home` w `ExecutionContext` / `FlowRequestMeta` / `IngestRequest`** — jako osobne pole,
   **nigdy** przez `options`/`envelope.meta`. `execute_ingest` wlewa `request.options` wprost do
   `env.meta` (`executor.rs:4025-4029`), a `meta` jest zapisywalne przez blok addona WASM. Ścieżka
   tworzenia pliku indeksu nie może być sterowana przez addon — tym bardziej że
   `get_or_create_at` **nie waliduje `custom_dir`** (zwykły `dir.join`, `namespace.rs:759-761`).
   Do zrobienia razem: walidacja containment ścieżki.
3. **Rozszerzalnego schematu pól w nodzie `store`.** Node zapisuje `doc_id/chunk_index/text`
   (+`collection_id`), a `knowledge::search` czyta `source_id/path/location` i buduje z nich
   `KnowledgeHit` → cytaty w `ps-chat`, `core.project_search` i KbSearch. Bez tego Etap 3 cicho
   wyzeruje metadane cytatów.

## 5. Etapy

### Etap 0 — fundament scope'u **[WYKONANE: 0a, 0b, 0c, 0d]**
- **0a.** `custom_dir` przez `upsert_with_quota` + `upsert_batch_with_quota`; walidacja containment
  w `get_or_create_at`.
- **0b.** `vector_home` w `ExecutionContext`, `FlowRequestMeta`, `IngestRequest` (osobne pole).
  Nody wybierają wariant `_at` gdy `Some`.
- **0c. WYKONANE.** Aliasy platformowe wg §0: seed `rag-embeddings`, `rag-parse`, `rag-llm`, `rag-reranker`,
  `rag-ocr`, `rag-page-elements`, `rag-table-structure`, `rag-graphic-elements` w core z
  `visibility="public"` i pustym targetem; usunięcie odpowiadających `[[alias]]` z manifestu addona
  (zostają `[[uses_alias]]`). Migracja istniejących instalacji: wiersze `model_alias_owners` o
  `owner_type='addon'` przepisywane są na `manual`, inaczej deaktywacja przy stopie zostaje mimo
  nowego seeda. **To jedyny etap naprawiający defekt, który psuje działający system dziś** — poszedł
  przed resztą planu.

  **Uściślenie z wykonania:** właścicielem platformowym jest `manual` (bez `owner_id`), a nie brak
  wiersza. `aliases_owned_by_addon` filtruje po `owner_type='addon'`, więc deaktywacja i tak nie
  zachodzi, ale wpis `manual` dokłada ochronę: guard własności odrzuca przejście `manual → addon`,
  więc obcy addon deklarujący `[[alias]] rag-*` dostanie głośny błąd instalacji zamiast po cichu
  przejąć alias platformy. Brak wiersza tej ochrony nie daje.

  Doszła też reaktywacja: alias związany przez admina, a zgaszony przez zatrzymanie addona, po
  zdjęciu właściciela nie miałby już kogo włączyć — seed przywraca go przez
  `set_model_alias_active_audited_within_tx` (z kontrolą łańcucha aliasów), tolerując błąd, żeby
  nie wywalić startu.
- **0d.** Usunięcie pre-create z `ingest.rs` (dopiero gdy 0a+0b działają).

**Weryfikacja:** istniejący namespace addonowy otwiera się z niezmienionej ścieżki; nowy projektowy
powstaje pod `<data>/projects/<id>/vectors/`; zatrzymanie addona RAG nie psuje ingestu w Projektach.

### Etap 1 — jedna implementacja prymitywów wektorowych **[WYKONANE]**
Moduł `services/vector/doc_vectors.rs`: `ref_id_for`, `delete_doc_vectors` (wariant
wieloprzebiegowy), wspólne ciało zapisu z rollbackiem. `store.rs` i `ingest.rs` stają się cienkimi
wywołaniami.

**Uzasadnienie [korekta]:** motywem jest usunięcie kopii, nie naprawa wycieku (§3.A). Zapowiadany w
1.0 „test, który dziś musi failować" prawdopodobnie przejdzie — `store.rs` ma już
`reingest_removes_orphan_chunks`.

### Etap 2 — flowy platformowe: `ingest`, `query`, `retrieval-round` **[WYKONANE: 2a, 2b, 2c]**
Idzie **po 0c** (flow platformowy nie może zależeć od aliasów addonowych). W 1.0/2.0 etap dotyczył
samego ingestu; wg §0 obejmuje wszystkie trzy flowy naraz — i tak muszą jechać razem, bo `query`
woła `retrieval-round`.

Wszystkie trzy `flows/*.json` → seed core jako flowy systemowe (stałe UUID, `is_system=1`,
odświeżane przy starcie). Addon przestaje wozić kopie i traci `[[engine_flow]]`;
`ingest_document`/`ingest_drain` oraz narzędzie `ask` dispatchują pod stałe nazwy platformowe
zamiast czytać `engine_flow_model:<id>` z KV instancji.

`query` przechodzi z `body_flow_engine_id` na `body_flow_id` ze stałym UUID `retrieval-round` (§0.3).

**[nowe] Do dopisania w wykonaniu:**
- Krok czyszczący: `upgrade_core` (`lifecycle.rs:1907`) rejestruje tylko flowy z NOWEGO manifestu i
  **nie sprząta usuniętych** — zostanie osierocony wiersz flow, binding i KV `engine_flow_model:ingest`.
- To **nie jest** wzorzec `ps-chat`: `ps-chat` ma `service_type=NULL`, brak published-name i jest
  dispatchowany po stałym `flow_id`. Etap 2 wprowadza pierwszy system-flow adresowany po
  published-name. Dodatkowo `flows` jest synchronizowane, a sync koercuje `is_system` do 0.
- Powierzchnia ataku: po zmianie na globalną published-name flow-ingest będzie odpalalny przez każdy
  addon z `document.read`. Zapis i tak ląduje w jego własnym namespace (`store.rs:52-59`), więc to
  nie wyciek danych, ale zmiana warta odnotowania.
- Kolejność `[[engine_flow]]` przestaje mieć znaczenie, bo znika cała sekcja — ale legacy KV
  `engine_flow_model` (nazwa pierwszego flow) zostaje w bazie i trzeba je wyczyścić razem z
  `engine_flow_model:<id>`.

### Etap 3 — Projekty na wspólnym flow **[3a, 3b WYKONANE; 3c: zostaje natywnie]**
- **3a.** Rozszerzenie schematu pól noda `store` o `source_id/path/location` (§4.3). Bez tego reszta
  etapu wyzeruje cytaty.
- **3b.** Przełączenie ścieżki proza/office/PDF na `execute_ingest`.
- **3c. ROZSTRZYGNIĘTE — zostają natywne.** `chunk_code` (zakres linii jako `location`) i
  `WorkPayload::Url` (trigger flow-ingestu przyjmuje wyłącznie blob binarny) nie mają odpowiednika
  we flow, a chunkowanie kodu to inna semantyka niż chunking prozy. Wszystko inne idzie flow.

**[nowe] Koszty do zaakceptowania lub zaadresowania:**
- `execute_ingest` bierze `document_bytes: Vec<u8>` w całości do RAM, robi `blobs.put` i `delete` po
  flow — podwójny zapis wobec istniejącego `files/<sha256>`.
- Jeden run flow = wiersz w `flow_executions`; sync drzewa kodu (do 5000 plików) = do 5000 wierszy
  audytu.
- `ingest_request_to_initial_envelope` buduje świeży `FlowRequestMeta` (`executor.rs:4030`):
  nowy `CancellationToken`, `progress_sink: None`, `deadline: None`. Anulowanie zejdzie z
  granularności „między batchami embeddingów" (`EMBED_BATCH=16`) do „między plikami", a postęp
  wewnątrz pliku zniknie. To trzeba przeprowadzić przez `IngestRequest`, nie tracić.

**Weryfikacja:** ten sam plik zingestowany do projektu i do kolekcji addona daje identyczne granice
chunków i identyczną liczbę wektorów **oraz** niepuste `source_name`/`file_path`/`location` w
cytatach `ps-chat`.

### Etap 4 — jedna kolejka zadań ingestu
Usługa core: kolejka + postęp + cancel + limit współbieżności. Projekty i `ingest_drain` stają się
jej klientami.

### Etap 5 — jeden retrieval **[nowa blokada]**
`retrieval_round` i `query` jako flowy systemowe; `project_knowledge` zostaje wyłącznie bramką
(członkostwo + scope + delegacja), join nazw źródeł zostaje po stronie PS.

**Blokada zdjęta w Etapie 2** (§0.3): przejście `query` na `body_flow_id` ze stałym UUID rozwiązuje
to samo dla Projektów — `loop_block.rs:134` nie jest już na ścieżce, bo `ps-<id>:retrieval-round`
nigdy nie powstaje.

**[nowe]** `retrieval_round` ma wpięte nody grafowe (`rag_graph_seed`, `rag_graph_facts`) i
`output_fields` bez `source_id/path/location`. Dla scope projektowego nody grafowe zdegradują się do
pass-through (brak kolekcji `kg_active`) — to działa, ale znaczy, że asymetria grafu wchodzi do
planu tylnymi drzwiami i musi być tu jawnie opisana. Dochodzi zależność od `rag-reranker`/`rag-llm`,
czyli §3.B ponownie.

## 6. Decyzje

**D1 — konfigurowalność chunkingu w Projektach.** **Rekomendacja:** stałe w V1; zmiana chunkingu
unieważnia wektory i wymusza re-ingest.

**D2 — nazwa aliasu. ROZSTRZYGNIĘTE (§0).** Nazwy zostają, właścicielem staje się platforma.
**[korekta]** Code Studio nie ma własnego wiązania — deleguje do `project_studio::ingest::embed_texts`,
więc dziedziczy los Projektów; po §0 przestaje to być zależnością od addona.

**D3 — warstwa grafowa** **[korekta argumentacji, decyzja bez zmian]**
Potwierdzone: `extract_chunk_graph` żyje wyłącznie w wasm addona (`lib.rs:4378`, wołane z `:904`),
flow ingestu nie ma żadnego noda grafowego, graf powstaje **po** flow, czytając teksty chunków z
powrotem z `passages`.

Ale teza z 1.0 („graf nie jest zduplikowany") jest częściowo fałszywa: **strona retrievalowa jest
już w core** (`rag_graphrag.rs`, `graph_search.rs`, `GraphManager` w `ExecutionContext`,
oba nody wpięte w `retrieval_round`), a `normalize_entity_name` jest zaimplementowany dwa razy, ze
stałą `kg_active` powtórzoną w trzech miejscach z komentarzem „MUSI byc identyczna".

**Rekomendacja bez zmian:** nie wciągać ekstrakcji MemGraphRAG do zakresu — to projekt wagi całego
RAG_ETAP3. Ale zawęzić tezę do „ekstrakcja grafu nie jest zduplikowana" i przyjąć, że Etap 5 i tak
dotknie strony retrievalowej.

## 7. Poza zakresem

- Most „agent czyta z projektu albo z addona" — po unifikacji kwestia doboru scope we flow.
- Migracja danych. Żaden etap nie przenosi wektorów. Wyjątek: po Etapie 1 osierocone wektory z
  wcześniejszych re-ingestów (wąski przypadek z §3.A) wymagają jednorazowego przebiegu, jeśli
  uznamy to za istotne.
- Zmiany w protokole wire Project Studio (limit 248/256 wariantów) — plan nie dodaje wariantów.
- **[nowe]** `archive.rs` czyta `EMBEDDINGS_ALIAS` przy decyzji „przenieś wektory verbatim vs
  re-indeksuj" (`archive.rs:502,1496`) — Etap 0c dotyka tej ścieżki, wymaga sprawdzenia przy wykonaniu.
- **[nowe]** Dziedziczenie `flow_depth` w `ingest_invoke.rs:168`: tworzy świeży `ExecutionContext` z
  pustym `flow_stack`, mimo że `ExecutionContext::new_with_flow_depth` istnieje — guard rekursji
  (`MAX_FLOW_DEPTH=3`) się resetuje. Etap 3 dokłada drugiego callera do tej ścieżki. Osobne zadanie.

---

## 8. Zadanie NASTĘPNE (po zakończeniu unifikacji RAG i Projektów)

Zapisane na wniosek właściciela — **do zrobienia po Etapach 0–5**, nie w ich trakcie.

**Agent musi mieć sterowanie głębokością rozumowania i temperaturą.**

1. **Ile ma myśleć.** Po wybraniu modelu dla agenta trzeba móc ustawić poziom rozumowania.
   Poziomy NIE są stałą listą: bywa `low`/`medium`/`high`, czasem dochodzi `xhigh` albo `max`, a
   część modeli nie ma tego wcale. Zestaw musi **wynikać z modelu**, tak jak porty LLM w Flow
   Builderze wynikają z modalności — wzorzec jest już zbudowany
   (`www/js/modules/flows-builder/model-modalities.js`: katalog jest źródłem prawdy, a model
   nieznany katalogowi nie jest traktowany jak niezdolny, tylko zostawiony bez ograniczenia).
2. **Temperatura per agent.** Raz chcemy działanie bardziej twórcze, raz zachowawcze. Dziś agent
   nie ma tego pola.

Stan zastany (sprawdzony, nie zakładany):
- `AgentParams` ma slot `params_json`, ale we wszystkich miejscach zapisu jest to `"{}"` — pole
  jest zarezerwowane i nieużywane. To naturalne miejsce na oba ustawienia, bez migracji kolumn.
- W core **nie ma dziś pojęcia `reasoning_effort`** w żadnej postaci. Katalog modeli niesie
  modalności, ale nie poziomy rozumowania — więc dojdzie do tego nowe pole katalogowe i jego
  wypełnienie per backend (llama.cpp wystawia `--reasoning`/`--reasoning-format`
  i `--chat-template-kwargs`; dla API zewnętrznych poziomy są własnością dostawcy).
- `LlmRequest` ma już `temperature`, więc strona wykonawcza dla punktu 2 istnieje — brakuje
  wyłącznie ustawienia na agencie i przekazania go do żądania.

Do rozstrzygnięcia przy podejmowaniu zadania: co robić, gdy agent ma ustawiony poziom, a model
zostanie przepięty na taki, który go nie wspiera (zignorować, zdegradować do najbliższego, czy
zablokować zapis).

---

## 9. Zadanie po §8: obserwowalność wzorowana na DeepSeek Harness

Pełna analiza: [`ANALIZA_DEEPSEEK_HARNESS.md`](ANALIZA_DEEPSEEK_HARNESS.md). Tu tylko to, co z niej
wchodzi do kolejki — **po** unifikacji RAG i po §8.

Sedno: u nich **nie ma instrumentacji czasu**. Jest jeden append-only log zdarzeń z ciągłym `seq`
i czasem, a `toolMs`, `ttftMs`, `decodeMs`, `llmMs` to **foldy po parach zdarzeń**. Z tego samego
logu wynikają za darmo wznowienie, fork, wyszukiwanie i replay.

Kolejność wg stosunku wartości do kosztu:

1. **Log zdarzeń przebiegu z ciągłym `seq` i czasem** jako źródło prawdy. Dziś `flow_executions` +
   `TraceStep` to „wiersz na przebieg" — nie da się z tego policzyć ani TTFT, ani czasu jednego
   narzędzia. `flow_executions` zostaje jako indeks.
2. **Metryki jako zapytania po logu, zero liczników w `node_adapters/*`.**
3. **Rejestr składania promptu** zamiast promptu wklejonego w config węzła `llm` w seedzie — z
   jawną kolejnością narzędzi i głośnym błędem przy złej konfiguracji, nie cichym przy generacji.
4. **`timeout_ms` w deklaracji narzędzia + jeden wrapper egzekwujący.** Idle-timeout w
   `code_studio/exec` rozwiązał ten sam problem punktowo.
5. **Odrzucanie niezamkniętych wywołań narzędzi przy zamknięciu tury** w liczeniu czasu — inaczej
   pierwsza awaria zatruwa statystyki na stałe.
6. **Cykl życia notatek decyzyjnych** (`proposed`/`implemented`/`rejected`/`archived` × klasa,
   zakodowane w ścieżce). Mamy `docs/*.md` bez statusu i klasyfikacji.
7. **`Model Experience` / wpływ na cache KV w opisie każdego bloku flow** — u nas miejscem na to
   jest opis węzła w palecie.

Świadomie NIE kopiujemy: telemetrii bez reguł redakcji (ich kontrakt to at-most-once, a reguły
należą do wdrożenia — Compliance Core wymaga więcej) ani Code Mode jako alternatywy dla function
callingu; to osobna decyzja produktowa.
