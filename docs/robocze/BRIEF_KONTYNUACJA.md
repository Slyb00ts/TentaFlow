# Kontynuacja: śledzenie zdarzeń + dokończenie RAG

Praca jest w połowie. Ten plik to Twój punkt startu — nie zaczynaj od czytania historii
rozmowy, bo jej nie masz. Zacznij tutaj, potem od źródła prawdy.

---

## 0. Co czytasz, zanim cokolwiek zrobisz

| Plik | Rola |
|---|---|
| `docs/DOKONCZENIE_RAG_I_ZDARZENIA.md` | **jedyna specyfikacja.** W razie sprzeczności z czymkolwiek — wygrywa |
| `mockups/zdarzenia-20260819/z01-przegladarka.html` | **poprzeczka wizualna.** DZIAŁAJĄCY prototyp, nie obrazek |
| `docs/robocze/POPRZECZKA_ZDARZENIA_RAG.md` | kryteria odbioru per tor, spisane PRZED zleceniem pracy |
| `docs/robocze/STAN_PETLI.md` | rejestr rund, werdyktów, incydentów i wniosków |
| `CLAUDE.md` | reguły repo — obowiązują każdego sub-agenta |

Gałąź: **`feat/zdarzenia-rag`**. **WYPCHNIĘTE NA `main` 2026-08-22** (decyzja właściciela,
`main` = `8320b3307`, 59 commitów ponad poprzedni `origin/main`). Bramka pushu została
przekroczona świadomie, mimo niespełnionych kryteriów R2b — patrz `STAN_PETLI.md`.

**Skutek dla wdrożeń:** `SCHEMA_VERSION` 23 → **24**, więc stare i nowe binarki odrzucają się
na handshake'u — węzły mesh trzeba przebudować RAZEM. Migracje tej pracy to **133/134/135**
(main zajął 132).

---

## 1. Jak pracujesz — metoda, nie sugestia

Jesteś ORKIESTRATOREM. **Nie piszesz kodu sam.** Dzielisz pracę, zlecasz, zbierasz DOWODY,
rozstrzygasz.

**Zasada nadrzędna: agent, który coś zbudował, NIGDY nie ocenia własnej pracy. Krytyk, który
widział poprzednią wersję, NIGDY nie ocenia poprawki.** Inaczej pętla mierzy „czy lepiej niż
poprzednio" zamiast „czy spełnia poprzeczkę".

Cykl na każdy kawałek:
1. wypisz poprzeczkę — kryteria sprawdzalne, nie „ma być ładne",
2. zleć wykonawcy z jawnym zakresem plików i zakresem negatywnym,
3. puść **ślepego krytyka w świeżym kontekście**: dostaje poprzeczkę, wzorzec i artefakt —
   **nic więcej**. Żadnej historii, żadnej samooceny wykonawcy, żadnej poprzedniej recenzji,
4. nie przechodzi → nowa runda z **NOWYM** krytykiem, przekazujesz dowody, nie streszczenia,
5. **to samo kryterium dwa razy z rzędu → STOP i pytanie do człowieka.** To znak, że kryterium
   jest źle napisane, a nie że wykonawca jest niezdolny. (Zadziałało raz — patrz §6.)

**Co liczy się jako dowód:** wynik `cargo test` z nazwami testów; `cargo check` z deltą ostrzeżeń
mierzoną metodą kopiuj-podmień-zmierz-przywróć (liczba bezwzględna jest bezużyteczna, zależy od
formatu wyjścia); zrzut obok zrzutu z prototypu **osobno dla każdej interakcji**; dowód mutacyjny.
Samo „zrobione" odrzucasz bez czytania.

**Każdemu krytykowi każ zadać dwa różne pytania**, bo to nie to samo:
- czy KAŻDY test coś pilnuje (mutuj i patrz, czy pada),
- czy JAKIKOLWIEK test pilnuje centralnej zmiany tego toru.
Raz zestaw 44 testów przeszedł mutację, która wywracała sens całego toru.

---

## 2. Reguły wyprowadzone z incydentów — nie do negocjacji

Trzy incydenty, wszystkie z winy orkiestratora. Wnioski wdrożone:

1. **ŻADEN agent nie wykonuje operacji git zapisującej do drzewa** — zakaz `checkout`, `restore`,
   `stash`, `reset`, `clean`, `apply`, `commit`, zakaz tworzenia i kasowania `worktree`.
   Pomiar delty ostrzeżeń: `cp` do katalogu tymczasowego i z powrotem.
   *Powód: zaleciłem krytykom „cofnij pliki do HEAD, zmierz, przywróć". Jeden wykonał to w drzewie
   głównym zamiast w swoim worktree i skasował pracę trzech ukończonych torów.*
2. **Po KAŻDEJ mutacji: przywróć z kopii `cp` i zweryfikuj, CZYTAJĄC KOD Z POWROTEM** — nie
   zbiorczo na końcu, nie samym grepem.
   *Powód: mutacje trafiły do commitów trzy razy. Jedna z nich nie miała markera (podmiana
   `ctx.origin` na `FlowOrigin::default()`), więc grep po `MUTATION` jej nie widział.*
3. **Nie commituj, gdy jakikolwiek agent jest w środku testu mutacyjnego, ani zaraz po masowym
   ubiciu agentów** — to dokładnie moment, w którym mutacje są żywe. Najpierw każ każdemu torowi
   potwierdzić stan WŁASNYCH plików.
4. **Commituj po każdym zdanym torze**, osobno per tor, nie `git add -A`.
   *Powód: przy pierwszym incydencie jedyną kopią było drzewo robocze.*
5. **Nie uruchamiaj `rustfmt` na plikach** — repo nie jest rustfmt-czyste na HEAD, formatowanie
   przepisuje setki cudzych linii i idzie za deklaracjami `mod` do plików, których nikt nie tknął.
   Nie uruchamiaj `cargo fix`.
6. **Limit sesji ubija wszystkich naraz.** Sufit równoległości narzuca limit, nie konflikt plików.
   Przy napiętym limicie **pracuj sekwencyjnie** — przerwanie kosztuje wtedy jeden tor, nie sześć.
7. **Rozdzielaj własność plików jawnie** w każdym zleceniu (co wolno ruszyć, czego nie).
8. **Weryfikacja przywrócenia musi być NIEZALEŻNA od kopii zapasowej.** Po przywróceniu sprawdź,
   że **oryginalna kotwica każdej mutacji jest obecna w pliku** (asercja na tekst źródłowy).
   `cmp` z kopią NIE wykrywa złej kopii — harness kopiujący raz na MUTACJĘ zamiast raz na PLIK
   zapisywał kopię z już nałożoną poprzednią mutacją i przechodził własną kontrolę, wypuszczając
   9 mutacji do drzewa z komunikatem „RESTORED (byte-identical)".
9. **Blokada mutacji obejmuje TYLKO okno mutacji** (weź blokadę → mutuj → jeden odfiltrowany
   test → przywróć → zwolnij). Zwykły build bez blokady. Nigdy nie mutuj przed wzięciem blokady.
   Nigdy nie uruchamiaj nieodfiltrowanego `cargo test --lib` (~45 min, wiesza się na
   `services::storage_proxy::server::tests::central_kv_proxy_writes_and_reads_on_authority`).

---

## 3. Decyzje człowieka, które już zapadły — nie pytaj ponownie

| Sprawa | Decyzja |
|---|---|
| Retencja `run_events` | **30 dni**, per organizacja |
| Widoczność osi czasu | użytkownik widzi swoje, admin wszystko |
| Pierwszy filtr w przeglądarce | **pochodzenie** (jak w prototypie) |
| Domyślny graf w Projektach | wyłączony |
| Język w kodzie | **angielski wszędzie**; zastane polskie komentarze tłumaczyć przy okazji edycji pliku, ale OSOBNYM przejściem, nie w commicie funkcjonalnym |
| Migracje 129, 130, 131 | zatwierdzone i wykonane |
| `SCHEMA_VERSION` 22 → 23 | zatwierdzone i wykonane |
| `org_id` w `run_events` | dodane (odstępstwo od §2.3, spec poprawiona) |
| Treść odpowiedzi modelu w logu | całość, **włączana per organizacja, domyślnie WYŁĄCZONA** |
| Pula odczytu dla `events.db` | dodany konstruktor `Db::with_read_pool` |
| §1.2 **G2** (R4) | **POZA ZAKRESEM.** Wymaga osobnej zgody. Nie zaczynaj z własnej inicjatywy |

### Bramki NIEZAPYTANE — zatrzymaj się przed nimi

- **migracja danych addona RAG** (§1.1 części 4–5): przeniesienie `ingest_jobs` addona do `jobs.db`.
  **Zmienia bundle hash** → przebudowany addon zostaje wyłączony do zatwierdzenia. Nie ruszaj
  `tentaflow-core/addons/` bez zgody.
- **wejście w §1.2 G2**
- **push**
- każda kolejna migracja schematu
- każda wartość, której nie ma w specyfikacji

---

## 4. Co JEST zrobione — nie rób drugi raz

| Tor | Stan | Werdykt ślepego krytyka |
|---|---|---|
| T5a komponent osi czasu (`tf-run-timeline.js`) | gotowy | **14/14 PASS** |
| T3 `ProgressEvent::FirstToken` | gotowy | **11/11 PASS**, macierz 4 mutacji |
| T1 pochodzenie (`origin`/`actor`/`correlation_id`) | gotowy, runda 2 | krytyk **NIE PRZEBIEGŁ** — do zlecenia |
| T2 `events.db` (schemat, pisarz, redakcja, outbox, retencja) | gotowy, runda 2 | runda 1 FAIL → naprawione |
| T3b mostek `ProgressEvent` → `events.db` + `metrics.rs` | gotowy | **krytyka BRAK** — do zlecenia |
| Migracje 129/130/131 + `SCHEMA_VERSION` | gotowe | runda 1 FAIL (brak zapisu) → naprawione |
| Zapis pochodzenia do `flow_executions` | gotowy | **krytyka BRAK** — do zlecenia |
| R1 kolejka `jobs.db` (§1.1 cz. 1–3) | gotowy, runda 2 | runda 1 FAIL → naprawione |
| R2a `graph_home` w `GraphManager` | gotowy, runda 3 | runda 2 FAIL → naprawione, **krytyka rundy 3 BRAK** |
| R3 jedna powłoka retrievalu | gotowy | **krytyka BRAK** — do zlecenia |

**Kryteria odbioru spełnione i udowodnione:** etap 1 (`flow_executions` odpowiada „skąd i kto"),
etap 3 (`FirstToken`), R1 (zadanie przeżywa restart i jest dokańczane), R3 (jeden flow dla addona
i czatu projektu).

---

## 5. Co zostało — kolejność

### 5a. Zaległe krytyki (najpierw, są tanie i mogą cofnąć „gotowe")

Cztery tory nie mają ślepego krytyka: **T1 runda 2**, **T3b**, **zapis do `flow_executions`**,
**R3**, **R2a runda 3**. Zleć je, zanim ruszysz nową funkcjonalność — inaczej budujesz na
niepotwierdzonym.

### 5b. Drobne usterki do domknięcia

1. **`docs/CODE_STUDIO_PLAN.md` §16.2 mówi „9 bloków", kod ma 10.** `patch_review` jest wymieniony
   w tabeli „czego tu nie ma", a §16.4 tego samego dokumentu go sankcjonuje jako opcjonalny.
   Nieaktualna jest proza §16.2. Do poprawienia: liczba, diagram, przeniesienie wiersza.
2. **`code_studio_lifecycle_e2e` pada** na `work_a_delegation_wrote_straight_to_disk_reaches_the_review`.
   Prawdopodobnie `open_patch_set` zwraca nietrwały, niezapisany zestaw łatek przy czystym drzewie
   roboczym, więc późniejsze `PatchSetGet` nie znajduje wiersza. Wymaga rozstrzygnięcia: czy
   delegacja nie zapisuje, czy `PatchSetGet` nie powinien błędować na zestawie nietrwałym.
3. **Przejście porządkowe:** §2.9 spec mówi „domyślnie 14 dni", a kod zasiewa 30 (decyzja
   człowieka — poprawić SPEC); komentarz nad plombą protokołu mówi „covers 21 of 133 variants",
   nieaktualne; tłumaczenie zastanych polskich komentarzy w dotkniętych plikach.

### 5c. Niezaczęte tory — to jest większość pozostałej pracy

**T5b — przeglądarka zdarzeń (największy kawałek).**
Komponent osi czasu istnieje i jest zaliczony. Brakuje: rodziny protokołu binarnego (JEDEN nowy
wariant `MessageBody`, dopisany na KOŃCU — kodowanie idzie po nazwie), handlera z filtrem
widoczności (nie-admin widzi WYŁĄCZNIE swoje, filtr po stronie serwera), rejestru z wirtualizacją,
inspektora, wyboru aktora z widocznym powiązaniem klucza API z użytkownikiem, modułu nawigacji
(Zarządzanie → Zdarzenia, obok Dziennika audytu), i18n w pięciu językach.
Kryteria: §2.10 specyfikacji plus T5.1–T5.15 w `POPRZECZKA_ZDARZENIA_RAG.md`.
**Krytyk wizualny musi porównać OBOK SIEBIE z prototypem, osobno dla każdej interakcji.**

**T6 — osadzenie w Code Studio + odsyłacz z audytu.**
Zakładka „Oś czasu" obok Konsoli/Plików/Zmian/Gita, ten sam komponent. Odsyłacz z wpisu
`audit_log` przez `correlation_id` — kolumna istnieje (migracja 129), ale **nikt do niej nie
pisze**. Uwaga: dodanie wariantów do `CodeStudioPayload` wymaga aktualizacji plomby (licznik +
odcisk) w TYM SAMYM commicie; plomba została niedawno naprawiona i działa.

**R2b — węzeł `graph_extract` (§1.2 G1).**
`GraphManager` ma już wariant z katalogiem (R2a). Brakuje: przeciągnięcia `graph_home` przez
`FlowRequestMeta`/`ExecutionContext` **jako osobne pole, nigdy przez `meta`**, samego węzła
`graph_extract`, wpięcia w platformowy flow ingestu za `chunk` sterowanego `graph_enabled`,
zdjęcia sztywnego `graph_enabled=false` z `ps-chat`, kasowania wkładu dokumentu do grafu.
Kryterium twarde: **wyłączony graf = ZERO dodatkowych wywołań LLM** (test z licznikiem).

**T4 — metryki jako zapytania.** `events/metrics.rs` istnieje z testami liczącymi TTFT i czas
narzędzi z zapisanych wierszy. Zostaje weryfikacja „te same liczby co zmierzone ręcznie na znanym
przebiegu" (§2.11 etap 4).

---

## 6. Inwarianty — złamanie to błąd, nie kompromis

1. **`origin`, `actor*`, scope, `vector_home`, `graph_home` NIGDY nie pochodzą z treści modelu.**
   Mintowane przez serwer PO autoryzacji. To granica bezpieczeństwa.
2. **`seq` alokowany `MAX(seq)+1` WEWNĄTRZ transakcji insertu**, z ograniczeniem unikalności.
3. **Redakcja PRZED zapisem.**
4. **`run_events` poza Sync Ledger.**
5. **Zero nowej instrumentacji czasu w adapterach** — czasy to RÓŻNICE zdarzeń.
6. **Nie fabrykujesz danych, których nie masz.** Brak wyniku to luka w logu, nie zmyślony wynik.
   Rekord w locie dostaje znacznik startu, nie zmyślony pasek.

---

## 7. Zastane usterki repo — znalezione po drodze, NIE nasze, NIE naprawione

Nie wchłaniaj ich po cichu. Jeśli którąś ruszysz, powiedz wprost, że to naprawa zastanej usterki.

1. **Kopia audytowa Code Studio nigdy nie jest drenowana** — `code_studio::audit_outbox::spawn_delivery_loop`,
   `workspace_db::spawn_idle_sweeper` i `checkpoint_all` nie mają ŻADNEGO wołającego. Zdarzenia
   security-relevant zapisują wiersz do outboxu, ale nic nie przenosi go do `audit_log`.
2. **`e2e_smoke_cbor_test` — 3 testy padają od lipca.** Addon `sdk-showcase` świadomie przestał
   renderować w `on_start`; test asertuje stare zachowanie. Czerwony test, który nie łapie błędu.
3. **`vector_gate_enforcement` — wada izolacji testów.** Wszystkie testy w pliku używają tego samego
   klucza cache'u (`verify_claim` skraca ścieżkę przez cache procesowo-globalny), więc zgoda z
   jednego testu wycieka do testów oczekujących odmowy. Pada tylko pod obciążeniem równoległym.
4. **Sześć testów `sync::`** pada wyłącznie przy równoległym pełnym przebiegu; pod
   `--test-threads=1` przechodzą. Zagłodzenie sesji mesh z realnymi timeoutami.
5. **`repository::list_flow_executions` nie ma wołającego.**
6. **`services::graph::tests` NIE wymaga już `TMPDIR`** — helper `tempdir()` celował w zaszyte
   `/mnt/e/...` z cudzej maszyny i wywracał CAŁY moduł na `PermissionDenied`, wyglądając jak
   regresja logiki grafu. Naprawione: fallback wyprowadzony z `CARGO_MANIFEST_DIR` do
   `target_shared/graph-tests`. Świadomie NIE `std::env::temp_dir()` — `/tmp` to tmpfs w RAM.
7. **`dispatch::stream::tests::lidar_local_robot_subscribe_streams_frame` pada na czystym HEAD**
   (niezgodność bajtów 20–27 ładunku, `src/dispatch/stream.rs:1297`). Znalezione przez krytyka
   pochodzenia, niezwiązane z żadnym z naszych torów.

---

## 8. Pułapki tego repo — sprawdzone, nie teoretyczne

- **Ciemny cel testowy jest aktywnie szkodliwy.** Dwa cele `test-support` nie kompilowały się od
  połowy sierpnia; w tym czasie zasiewany flow zmienił się pod nimi DWA razy i nikt nie zauważył.
  `cargo check --lib` tego nie widzi — biblioteka i binarki testowe to osobne cele.
- **`cargo check --lib` zielony nie znaczy, że repo się buduje.** Sprawdzaj też `--tests`,
  `--tests --features test-support` i `--bin tentaflow`.
- **Czerwony test bywa prawdziwym sygnałem.** Zanim „naprawisz" asercję, ustal, czy nie łapie
  realnego błędu. Zdarzyło się w obie strony: raz asercja była nieaktualna, raz łapała prawdziwą
  usterkę.
- **Sprawdzaj kontrakt wejścia, nie komentarz w teście.** Specyfikacja §1.3 odnotowuje błędną
  diagnozę rerankera — degradacja do kolejności wektorowej ISTNIEJE dla kształtu, którego ten flow
  używa. Nie „naprawiaj" tego.
- **Komentarz mijający się z prawdą jest gorszy niż jego brak.** Znaleziono kilkanaście takich,
  w tym twierdzących wprost coś przeciwnego niż kod obok.
- **Filtrowanie hunków po markerach gubi linie bez markera.** Po odsianiu sprawdź, że
  zastage'owana wersja SAMA się kompiluje.
- **Weryfikuj, co naprawdę wylądowało na zdalnej gałęzi.**

---

## 9. Kiedy kończysz

Gdy wszystkie kryteria z §2.11 specyfikacji (R1–R3; **R4 jest poza zakresem**) są spełnione i
potwierdzone przez ślepych krytyków, a interfejs w ślepym porównaniu z prototypem nie przegrywa na
żadnej z wymienionych interakcji.

Kończysz też, gdy trafisz na bramkę człowieka, przekroczysz budżet, albo to samo kryterium padnie
dwa razy z rzędu. W każdym z tych przypadków **zdaj relację z tym, co jest** — nie udawaj, że
skończone.

**Na koniec napisz, czego NIE zrobiłeś i dlaczego.** Raport bez tej sekcji jest odrzucany.
