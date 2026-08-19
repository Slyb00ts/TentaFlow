# Poprzeczka odbioru — śledzenie zdarzeń + dokończenie RAG

Dokument roboczy orkiestratora. **Nie jest źródłem prawdy** — źródłem jest
`docs/DOKONCZENIE_RAG_I_ZDARZENIA.md`. Tu spisane są kryteria SPRAWDZALNE, wzorce odniesienia
i sposób dowodzenia dla każdego toru, ustalone PRZED zleceniem pracy.

Zasada nadrzędna: **wykonawca nie ocenia własnej pracy; krytyk, który widział poprzednią wersję,
nie ocenia poprawki.** Werdykt bez dowodu = brak werdyktu. Fail-closed: czego nie da się
zweryfikować, jest NIESPEŁNIONE.

## Decyzje człowieka (zapadłe 2026-08-19)

| # | Pytanie spec §5 | Decyzja |
|---|---|---|
| 1 | Pierwszy filtr | **pochodzenie** (chipy `origin`), aktor w menu z wyszukiwarką — jak prototyp |
| 2 | Retencja `run_events` | **30 dni**, konfigurowalne |
| 3 | Widoczność osi czasu | **użytkownik widzi swoje, admin wszystko** |
| 4 | Domyślny graf w Projektach | **wyłączony** (rozstrzygnięte w spec §1.2 G1 pkt 3) |
| 5 | §1.2 G2 / R4 | **poza tą pętlą** — wymaga osobnej zgody, pytanie wraca po G1 |

Sprostowanie do źródła prawdy: G2 nie oznacza wspólnej bazy dla addona i Projektów. Mechanizm
wspólny (kod w core), dane osobne per scope — addon pisze u siebie, projekt do `<project>/graph/`,
dokładnie jak `vector_home` dla wektorów.

## Stan faktyczny, który koryguje specyfikację

Ustalenia z rekonesansu, sprzeczne z brzmieniem spec — **wygrywa kod**:

1. **`ProgressSink` JUŻ ISTNIEJE** — `flow_engine/dispatchers/progress.rs:99-109`, wraz z
   `ProgressEvent` (17 wariantów), brokerem (`progress_broker.rs`) i konsumentem
   (`dispatch/run_events.rs::to_wire`). Spec §2.6 mówi „nowy" — **nie wolno tworzyć drugiego**.
   T3 = dodanie wariantu `FirstToken` + emisja + pisarz jako drugi subskrybent.
2. **`ToolCallStarted`/`ToolCallFinished` mają `call_id`** — `progress.rs:43-49`, emisja
   `node_adapters/tool_exec.rs:793` i `:834`. Zgodne ze spec.
3. **`correlation_id` NIE istnieje w `audit_log` ani `flow_executions`.** Istnieje jako `u64`
   transportowy (`dispatch/mod.rs:77`) i jako `TEXT` w `compliance_ai_events`
   (`migrations.rs:6933`). `audit_log` ma `request_id TEXT`. Odsyłacz z §2.10 pkt 3 wymaga
   **migracji bazy głównej** → bramka człowieka.
4. **`actor_user_id` jest rozwiązywalny tylko dla `key_type='user'`.** `Principal`
   (`auth/acl.rs:40-44`) rozróżnia `User`/`Group`/`ApiKey`, ale jest **gubiony na granicy
   handlera** — do `FlowRequestMeta` dochodzi tylko `UserContext`. Przeciągnięcie `Principal`
   to część T1.
5. **Wzorce z `code_studio` są martwe** — `audit_outbox::spawn_delivery_loop`,
   `workspace_db::spawn_idle_sweeper` i `workspace_db::checkpoint_all` **nie mają wołających**.
   Kopiujemy kształt, ale pętle `events.db` MUSZĄ być wpięte w `main.rs`.
6. **Istnieje `UnifiedTimeline`** — `www/js/modules/profile-timeline.js:358`: Canvas2D, wiele
   pasm, zoom kółkiem, pan przeciągnięciem, zaznaczanie zakresu, hit-testing przez offscreen
   colour-key. Oś czasu ma powstać z tego wzorca, nie od zera.
7. **`MessageBody` ma 311 wariantów**, kodowanych **po NAZWIE** (dowód: golden testy
   `project_studio.rs:2323`, `code_studio.rs:1874`). Budżet 256 z komentarzy jest nieaktualny.
   Nowa rodzina = JEDEN wariant dopisany na końcu.
8. **`#[policy(...)]` jest jedyną realną bramką uprawnień.** Wpis w `ADMIN_NAV` (`app.js:106`)
   tylko ukrywa pozycję.

---

## T1 — `origin` + `actor*` w `FlowRequestMeta`

**Wzorzec odniesienia:** `vector_home` — `dispatcher.rs:134` (pole struktury, NIGDY klucz w
`meta`), `dispatcher.rs:259` (kopia do `ExecutionContext`), `node_adapter.rs:185`.
Uzasadnienie w kodzie: `node_adapter.rs:176-184`.

| # | Kryterium | Dowód |
|---|---|---|
| T1.1 | `FlowOrigin` i `ActorKind` to **enumy**, nie `String` | `cargo check`; literówka `"code_stdio"` nie kompiluje się |
| T1.2 | Pola są na `FlowRequestMeta` i `ExecutionContext`, **nie** w `envelope.meta` | `grep -n 'meta\["origin"\]\|meta\["actor' ` zwraca 0 trafień |
| T1.3 | Każdy z 14 produkcyjnych punktów konstrukcji stempluje wartość świadomie | tabela plik:linia → `origin`, 1 test per punkt wejścia |
| T1.4 | **Wartość podana przez model NIE trafia do `origin`/`actor*`/scope** | test: envelope z `meta["origin"]="admin"` → `ctx.origin` pozostaje wartością serwera |
| T1.5 | `actor_user_id` = `Some(user)` **tylko** dla klucza `key_type='user'`; `group`/`general` → `None`, a `actor_id` nadal identyfikuje klucz | test per wariant `Principal` |
| T1.6 | Sub-agent dziedziczy po rodzicu (`subagent_reactor.rs:82` dziś nie dziedziczy NIC) | test: run dziecka ma `origin`/`actor*` rodzica |
| T1.7 | `correlation_id: Option<String>` na `FlowRequestMeta`, stemplowany po obu stronach | test: wpis `audit_log` i wiersz `run_events` z tego samego przebiegu mają tę samą wartość |
| T1.8 | Zero ostrzeżeń `cargo check` w dotkniętych plikach | `cargo check --lib 2>&1 \| grep -c warning` = baseline |

**Zakres negatywny:** nie dotykać logiki dispatchu, ACL, ani `compliance_ai_events`.

---

## T2 — `events.db`

**Wzorce:** pisarz `code_studio/events.rs:709-782` (verbatim kształt), pula
`code_studio/workspace_db.rs:65-216`, side-DB init `project_studio/db.rs:55-118`, redakcja
`code_studio/redact.rs`, outbox `code_studio/audit_outbox.rs`, retencja
`agents/retention_purge.rs:31-104`.

| # | Kryterium | Dowód |
|---|---|---|
| T2.1 | Schemat dokładnie jak spec §2.3 (kolumny, 4 indeksy, `ux_run_events_idem` częściowy, `PRAGMA auto_vacuum=INCREMENTAL`) | test odczytujący `sqlite_master` |
| T2.2 | `seq` = `MAX(seq)+1` **wewnątrz** transakcji insertu | przegląd kodu + test |
| T2.3 | Dwa równoległe zapisy → **głośny błąd**, nie przeplot | test z dwoma wątkami; asercja na `PRIMARY KEY` violation lub serializację, NIE na cichy sukces obu z tym samym `seq` |
| T2.4 | Powtórka pod tym samym `idempotency_key` → `duplicate: true`, zero nowych wierszy | test |
| T2.5 | **Redakcja PRZED zapisem** | test: zdarzenie z `Authorization: Bearer sk-...` w payloadzie → na dysku `[redacted]`; asercja czyta plik/kolumnę, nie strukturę w pamięci |
| T2.6 | Wiersz outboxu powstaje w **tej samej transakcji** co zdarzenie security-relevant | test: wymuszony błąd insertu outboxu ⇒ zdarzenia też nie ma |
| T2.7 | Dostarczanie at-least-once: audyt w bazie głównej PIERWSZY, `delivered_at` DRUGI | przegląd kodu + test |
| T2.8 | Retencja **30 dni**, konfigurowalna, sweeper **wpięty w `main.rs`** | test + `grep` na wołającego |
| T2.9 | `run_events` poza Sync Ledger | brak wpisu w `sync/core_registry.rs`; test |
| T2.10 | Pula, sweeper idle i `checkpoint_all` mają **realnych wołających** (nie martwy kod jak w `code_studio`) | `grep` na wołających w `main.rs` |

**Bramka:** `events.db` to osobny plik z własną tabelą wersji — **nie** migracja bazy głównej.
Migracja bazy głównej dotyczy tylko `audit_log.correlation_id` i uprawnień → osobne pytanie.

---

## T3 — `FirstToken`

| # | Kryterium | Dowód |
|---|---|---|
| T3.1 | Wariant dodany do ISTNIEJĄCEGO `ProgressEvent` (`progress.rs`), zero nowych sinków | `grep -c 'trait ProgressSink'` = 1 |
| T3.2 | Emisja przy **pierwszym niepustym delcie**, nie pierwszym chunku | test ze strumieniem `["", "", "a"]` → dokładnie 1 emisja, po `"a"` |
| T3.3 | Emisja per **krok**, nie per przebieg (pętla harnessu woła `stream_llm_member` raz na iterację) | test z 2 iteracjami → 2 emisje |
| T3.4 | Pokryte OBIE pętle konsumujące delty: `stream_llm_member` (`executor.rs:1187`) i `finalize_streaming_flow` (`executor.rs:2164`) | test per ścieżka |
| T3.5 | **Zero nowych liczników czasu w adapterach** | `git diff --stat flow_engine/node_adapters/` — brak `Instant::now`/`elapsed`/`duration_ms` w dodanych liniach |
| T3.6 | `match` w `dispatch/run_events.rs::to_wire` wyczerpany, lista `kind` w `message_body.rs:3650` uzupełniona | `cargo check` |

---

## T4 — Metryki jako zapytania

| # | Kryterium | Dowód |
|---|---|---|
| T4.1 | Czas narzędzia parowany **po `call_id`**, nie po nazwie | test: dwa równoległe wywołania tego samego narzędzia o różnych czasach → dwa poprawne wyniki |
| T4.2 | Niezamknięte wywołanie **odrzucone** przy `turn_end`, nie liczone jako 0 ani jako nieskończoność | test |
| T4.3 | TTFT = `request_started` → `first_token` w tym samym kroku | test na znanym przebiegu |
| T4.4 | Dekodowanie = `first_token` → `assistant_message` | test |
| T4.5 | Liczby zgodne z ręcznym pomiarem na znanym przebiegu | fixture z ustalonymi znacznikami czasu; asercje na konkretne liczby |
| T4.6 | Zero kolumn `duration_ms` zapisywanych przez adaptery — czasy są RÓŻNICAMI | przegląd |

---

## T5 — Przeglądarka

**Wzorzec wizualny:** `mockups/zdarzenia-20260819/z01-przegladarka.html` (działający prototyp),
`z02-inspektor.html`, `z03-code-studio.html`.
**Wzorzec techniczny:** `UnifiedTimeline` (`profile-timeline.js:358`), `lib/virtual-list.js`,
`.tf-toolbar` (`controls.css:1233`), `tf-table`, `tf-chip`, `tf-segmented`, `tf-searchbox`.

Oś czasu pojawia się w 3 miejscach (przeglądarka, Code Studio, przebieg agenta) ⇒ zgodnie z
regułą 7 `CLAUDE.md` musi być **komponentem `tf-*`**, nie kodem w module.

| # | Kryterium (każde sprawdzane OSOBNO, zrzut obok zrzutu) | Dowód |
|---|---|---|
| T5.1 | Zoom kółkiem **kotwiczy się w punkcie kursora** — punkt pod kursorem nie przesuwa się | zrzut przed/po dla kursora przy lewej krawędzi i przy prawej |
| T5.2 | Przeciągnięcie lewym = zaznaczenie zakresu, po puszczeniu widok = zaznaczenie | zrzut z widoczną ramką zaznaczenia + zrzut po |
| T5.3 | **Prawy przycisk = pan**, menu kontekstowe zablokowane | zrzut w trakcie |
| T5.4 | Dwuklik = pełny zakres | zrzut |
| T5.5 | **Minimapa** pod osią: całość + prostokąt okna widoku; klik przesuwa okno | zrzut przy oddaleniu i przybliżeniu |
| T5.6 | Sprzężenie **w obie strony**: najechanie na wiersz podświetla pasmo ORAZ najechanie na pasmo podświetla wiersz, z przewinięciem do widoku | dwa osobne zrzuty |
| T5.7 | **Dwie skale** jako JAWNY przełącznik: „czas rzeczywisty" / „równe odstępy"; nigdy automat | zrzut obu; kod bez automatycznego przełączenia |
| T5.8 | Pasmo modelu **rozcięte na TTFT i dekodowanie** (kreskowanie / pełne) | zrzut z legendą |
| T5.9 | Granice tur jako **pionowe linie** z etykietą T1..Tn | zrzut |
| T5.10 | Trzy tory: model / wiadomości / narzędzia, z etykietami | zrzut |
| T5.11 | **Klucz API pokazuje powiązanie** z użytkownikiem w liście aktorów I w inspektorze; brak powiązania jawny („klucz serwisowy"), nie puste pole | dwa zrzuty: klucz powiązany i niepowiązany |
| T5.12 | **Bez fabrykowania czasu**: rekord w locie ma znacznik startu, nie zmyślony pasek | zrzut rekordu bez `tool_result` |
| T5.13 | Zero surowych `<button>`/`<input>`/`<select>` | `grep -nE '<(button\|input\|select)[ >]' www/js/modules/zdarzenia.js www/js/components/tf-run-timeline.js` = 0 |
| T5.14 | i18n w pięciu językach, liczba mnoga przez `{count\|f1\|f2\|f3}` | test parzystości kluczy + `grep` na sklejanie |
| T5.15 | Nie-admin widzi **wyłącznie swoje** przebiegi — filtr po stronie serwera, nie klienta | test handlera: sesja usera B nie dostaje wierszy usera A |

---

## T6 — Osadzenie w Code Studio + odsyłacz z audytu

**Wzorzec:** `code-studio-session.js:39-67` (`DOCK_CATEGORIES`/`PHONE_VIEWS`/`VIEW_MAP`),
`:521-527` (stage pane), `:745-749` (`mountExternalPanes`), kontrakt panelu
`code-studio-panes.js:9-25`, CSS `code-studio.css:1155-1193`.

| # | Kryterium | Dowód |
|---|---|---|
| T6.1 | Zakładka „Oś czasu" obok Konsoli/Plików/Zmian/Gita, ten sam komponent co przeglądarka | zrzut obok `z03-code-studio.html` |
| T6.2 | Zakres zawężony do sesji, bez osobnego kodu osi | przegląd: jeden `tf-run-timeline` |
| T6.3 | Z wpisu `audit_log` **jedno kliknięcie** do miejsca w osi | zrzut przed/po |
| T6.4 | Pinowane testy nazw `code_studio.rs:1767-1862` zaktualizowane w TYM SAMYM commicie | `cargo test code_studio` |

---

## T7/R1 — trwała kolejka `jobs.db`

**Wzorzec:** claim `project_studio/runs.rs:429-455` (`UPDATE … RETURNING`), side-DB
`project_studio/db.rs:55`, brama `services/ingest_gate.rs`, anulowanie
`services/cancel_registry.rs`.

| # | Kryterium | Dowód |
|---|---|---|
| R1.1 | `<data>/jobs.db` osobny od `events.db` (rotacja loga nie zabiera zadań) | dwa pliki, dwie tabele wersji |
| R1.2 | `claim` to **JEDEN** atomowy `UPDATE … RETURNING` | test: dwa wątki, każdy dostaje inne zadanie, żadne nie dwa razy |
| R1.3 | Zadanie ingestu w Projektach **przeżywa restart procesu** i jest dokańczane | test: enqueue → symulacja restartu → drenaż kończy |
| R1.4 | Osierocone zadanie po zabiciu procesu jest zamykane przy starcie | test `reconcile_orphans` |
| R1.5 | Addon i Projekty przechodzą przez **tę samą** kolejkę | przegląd: `ingest_drain` to cienkie wywołanie host-fn |
| R1.6 | Kolejność limitów zachowana: reclaim addona (2400 s) > `max_runtime` schedulera (1800 s) | test lub asercja stałych |

**Bramka:** migracja `ingest_jobs` addona **zmienia bundle hash** → osobne pytanie przed zmianą.

---

## T7/R2 — `graph_home` + węzeł `graph_extract` (G1)

**Wzorzec:** `NamespaceManager::get_or_create_at` (`services/vector/namespace.rs:681`) i jego
semantyka z `:672-680` — **istniejąca kolekcja ignoruje `custom_dir`, wygrywa zapisana ścieżka**;
`validate_custom_dir` (`namespace.rs:170`). Dziś `GraphManager::file_path_for`
(`services/graph/collection.rs:186`) jest bezwarunkowe.

| # | Kryterium | Dowód |
|---|---|---|
| R2.1 | `graph_home` jako **osobne pole** na `FlowRequestMeta`/`ExecutionContext`, nigdy w `meta` | `grep` = 0 |
| R2.2 | Kolekcja NIEISTNIEJĄCA powstaje w katalogu wołającego; dla ISTNIEJĄCEJ wygrywa zapisana ścieżka | dwa testy |
| R2.3 | Ścieżka walidowana (absolutna, bez `..`) — odpowiednik `validate_custom_dir` | test |
| R2.4 | Węzeł `graph_extract` zarejestrowany, przechodzi R1–R8, ma wpis w palecie `flow_node_templates` | `cargo test`; walidacja flow |
| R2.5 | Zapisuje provenance (`chunk_id`, `doc_id`, wersja ekstraktora) | test |
| R2.6 | Projekt z włączonym grafem ma **niepuste `kg_active` we WŁASNYM katalogu** | test integracyjny |
| R2.7 | **Wyłączony graf = ZERO dodatkowych wywołań LLM** | test z licznikiem wywołań: `graph_enabled=false` ⇒ licznik 0 |
| R2.8 | Usunięcie dokumentu usuwa jego węzły i krawędzie (refcount po `graph_artifacts`) | test |
| R2.9 | `ps-chat` przestaje wysyłać `graph_enabled=false` na sztywno (`stream_handlers.rs:1959`) | przegląd + test |
| R2.10 | Domyślnie **wyłączony** | test |

---

## T7/R3 — jedna powłoka retrievalu

| # | Kryterium | Dowód |
|---|---|---|
| R3.1 | `output` w `query` obsługuje oba tryby (streaming / blok z cytatami) sterowane `meta` | test per tryb |
| R3.2 | `answer` bierze model z `envelope.meta` z fallbackiem na `rag-llm` | test per przypadek |
| R3.3 | To samo pytanie z addona i z czatu projektu przechodzi **tym samym flow** | test |
| R3.4 | Czat **nadal streamuje** i **nadal używa modelu projektu** | test |
| R3.5 | Kontrakt rerankera nietknięty — `retrieval-round` karmi go kształtem vector-hits, dla którego istnieje degradacja | przegląd `reranker.rs:166-268`; NIE „naprawiać" |

---

## Co liczy się jako dowód (przypomnienie dla krytyków)

- **Kod:** `cargo test` z NAZWAMI testów i wynikiem; `cargo check` bez ostrzeżeń w dotkniętych plikach.
- **UI:** zrzut obok zrzutu z prototypu, **osobno dla każdej interakcji**. „Wygląda dobrze" = brak dowodu.
- **Metryki:** te same liczby, co zmierzone ręcznie na znanym przebiegu.
- **Bezpieczeństwo:** test pokazujący, że wartość podana przez model NIE trafia do `origin`/scope.

Samo „zrobione" od wykonawcy odrzucamy bez czytania.
