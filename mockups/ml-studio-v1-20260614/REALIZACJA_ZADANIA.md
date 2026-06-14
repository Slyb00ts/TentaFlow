# ML Studio — realizacja: mapowanie mockup → zadanie (v2: moduł RDZENIA)

ML Studio to **natywny moduł rdzenia TentaFlow** (NIE addon WASM). Wzór: `src/scheduler/`, `src/compliance/`. Mockupy w tym katalogu = specyfikacja wizualna. Zasada: każdy commit produkcyjny (zero zaślepek). Każdy element weryfikowany przez codex (zgodność z mockupem + błędy).

## Architektura (decyzje)

- **Backend:** moduł `tentaflow-core/src/ml_studio/` (mod.rs, models.rs, repository.rs, db.rs, training.rs, evaluation.rs). Bezpośrednio używa core (SqlitePool, flow_engine, services), bez addon ABI.
- **Protokół:** `MessageBody::MlStudioBody(MlStudioPayload)` w `tentaflow-protocol/src/message_body.rs` (wzór: `SchedulerPayload`). Binarny CBOR przez `/wt/api`+`/ws/api`. NIE REST.
- **Handlery:** `tentaflow-core/src/dispatch/ml_studio.rs`, makra `#[handler(variant=...)] #[policy(UserSession)] #[observed]`.
- **UI:** `tentaflow-core/www/js/modules/ml-studio.js` (wzór: `scheduler.js`), komponenty `tf-*`, klient protokołu binarnego. Rejestracja w nawigacji: `www/js/app.js` (registry/ADMIN_NAV) + router.
- **SQLite — OSOBNY PLIK (wymóg użytkownika):** dedykowany `.runtime/data/ml_studio.db` z własnym poolem + własnym runnerem migracji (`src/ml_studio/db.rs`). To nowy precedens (platforma standardowo trzyma wszystko w `tentaflow.db`). Konsekwencja: `owner_user_id`/`org_id` to **referencje na poziomie aplikacji** (id z core), NIE SQL FK do `user_accounts`/`organizations` (te są w `tentaflow.db`). Tożsamość/RBAC z `HandlerContext` (sesja core).
- **Userzy z core:** zero tabeli `users` w `ml_studio.db`; aktor z `HandlerContext`, autoryzacja `#[policy(UserSession)]`.

## Backend infra (moduły/serwisy core — bramkują slajdy UI)

| ID | Zakres | Realizacja w core | Bramkuje |
|---|---|---|---|
| **B1** | Konektory danych + profilowanie (xlsx/csv/parquet/db) | serwis/moduł core (`calamine`/`csv`/`parquet`/`sqlx`) + walidacja/limity/vault/SSRF | t-dane, f-dane, c-dane, rag01 |
| **B2** | Metrics collector time-series + stream | `ml_studio/metrics.rs` (osobny SQLite/ring) + WS przez istniejący event/WS | treningi live |
| **B3** | Rejestr modeli (wersje/metryki/deploy/rollback) | `ml_studio/registry.rs`, deploy przez containers | m07, *-eksport, ewaluacje |
| **B4** | Pipeline eksportu (GGUF/ONNX/NVFP4/MLX/TensorRT/CoreML) + gating | krok bundla ml-training + `ml_studio/export.rs` | *-eksport |
| **B5** | Bundle Python `tentaflow-containers/ml-training/` (SFT/LoRA/QLoRA/DoRA+DPO, AutoGluon/PyOD, diffusers, whisper) | container + service_request | wszystkie treningi |

## Nowe komponenty tf-* (www/js/components + wariant UiComponent w tentaflow-ui-schema jeśli renderowane ze schematu)

| ID | Komponent | Dla |
|---|---|---|
| **C1** | `tf-leaderboard` | t03, f03, d04 |
| **C2** | `tf-metrics-live` (WS krzywe) | f02, *-trening |
| **C3** | `tf-annotate-canvas` | r02 |
| **C4** | `tf-quant-matrix` | *-eksport |

## Pionowe plastry UI (mockup → zadanie)

| Slajd | Mockupy | Zależności | Opis |
|---|---|---|---|
| **S0** Fundament | (shell) | — | Moduł `src/ml_studio/` + osobny `ml_studio.db` (pool+migracje) + `MlStudioPayload` w protokole + `dispatch/ml_studio.rs` + `www/js/modules/ml-studio.js` + pozycja „ML Studio" w nawigacji. Migracje: projects, datasets, schemas, annotations, training_runs, models, metrics_history, lookup_dicts, service_models (owner_user_id/org_id jako TEXT, app-level). |
| **S1** Projekty + router | p00, p01 | S0 | Handlery ProjectsList/Create/Detail; UI lista (karty/filtry/Nowy/KPI) + kreator nazwa→typ (router 6 typów). |
| **S2** Tabular/Anomalie | t-dane..t04 | B1,B2,B5,C1,B3 | Profil danych, cel+auto-klasy, AutoML/anomalie (flow→B5), leaderboard, ewaluacja+deploy. |
| **S3** Rozpoznawanie | a-dane,r01,r02,a-trening,a-ewaluacja,a-eksport | B1,B2,B3,B4,B5,C2,C3,C4 | Import zdjęć, schemat, anotacja+pre-label, trening vision, mAP/confusion, eksport. |
| **S4** FT LLM | f00,f-dane,f01,f02,f03 | B1,B2,B4,B5,C2,C4 | Model bazowy, dane SFT/DPO, metoda dwie osie, trening live, eksport+benchmark. |
| **S5** RAG | rag01..rag04 | B1, vector_* | Korpus, chunking+embedding, indeks HNSW, eval+playground+deploy. |
| **S6** Destylacja | d01..d04 | B2,B4,B5,C1 | Nauczyciel+uczeń, prompty, generacja+trening ucznia, porównanie+eksport. |
| **S7** FT vision/audio | c01..c-eksport | B1,B2,B4,B5,C2,C4 | Modalność, dane, metoda dwie osie, trening (WER itp.), eksport. |
| **S8** Zasoby+Rejestr+Przegląd | zasoby,m07,przeglad-projektu | B3 | Biblioteka zasobów (capability matrix, słowniki, źródła, dostępy), rejestr modeli, przegląd projektu. |

## Kolejność realizacji

1. **S0** fundament (moduł+osobny db+protokół+UI shell+nawigacja) — działa: pusty moduł widoczny w dashboardzie.
2. **S1** Projekty+router (tylko `ml_studio.db`, end-to-end).
3. **C1–C4** komponenty tf-* (gdy pierwszy plaster ich wymaga).
4. **B5+B1+B2** bundle + konektory + metrics.
5. **S4** FT LLM end-to-end (wzór Guard).
6. **B3,B4** rejestr+eksport.
7. **S2, S5, S8**.
8. **S3, S6, S7**.

## Pętla weryfikacji (codex)

Po każdym elemencie: `codex exec` sprawdza (a) zgodność z mockupem (sekcje/pola/przepływ/pochodzenie wartości), (b) błędy/kompilacja/bezpieczeństwo (handler, SQL, SSRF, brak zaślepek, zgodność z wzorcem scheduler/compliance). PASS = done. Kompilacja: `cargo check` (NIE release-LTO; dysk bywa pełny).
