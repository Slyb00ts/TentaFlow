# Projekty — moduł „Bezpieczeństwo” (SSDLC, SBOM, podatności, pentesty, CRA) — plan

> **Rewizja 0.4** (2026-09-29, po przeglądzie krytyka: izolacja skanerów, przypięcie adresów celów, poufność PSIRT na warstwie danych, cykl życia znalezisk, niezmienność dowodów, retencja a RODO; role przeniesione do `PROJECT_STUDIO_WORKFLOW_PLAN.md` §2). Rozbudowa istniejącej aplikacji **Projekty** (`project_studio/`)
> zamiast osobnej aplikacji (rewizja 0.1 zakładała osobne „TentaSec” — uzasadnienie zmiany w §0).
>
> Cel: w projekcie, który zespół już prowadzi (zadania, przypadki testowe, przebiegi, środowiska,
> harmonogramy, repozytoria, czat, agenci), włączany jest moduł **Bezpieczeństwo**. Moduł dokłada
> bezpieczny cykl wytwarzania dla **dowolnego** produktu, nie tylko NextApp:
> - klasyfikację zmian i modele zagrożeń,
> - bramki przy MR i tagu ze statusem odsyłanym do git,
> - SBOM i codzienne monitorowanie podatności wszystkich wspieranych wersji,
> - środowiska testowe na żądanie, DAST i pentesty,
> - obsługę podatności (PSIRT) z zegarem zgłoszeń CRA,
> - wyjątki, kalendarz obowiązków oraz paczki dowodowe dla audytora i klienta.
>
> Narzędzia działają w kontenerach z `tentaflow-containers`. LLM wykonuje pracę przygotowawczą,
> a decyzje zostają przy ludziach.
>
> Wymagania merytoryczne (proces, dokumenty, terminy, szablony) są w folderze `CyberSecurity/` repo
> NextApp (`/Users/critix/repos/dotnet/nextapp/CyberSecurity/`). Ten plan jest ich wykonaniem
> w narzędziu. Fakty o repo pochodzą z przeglądu z 2026-09-29. Pozycje **[niezweryfikowane]**
> trzeba potwierdzić przed decyzją.

---

## 0. Decyzje bazowe

1. **Kto nas reguluje.** Wobec produktów, które wytwarzamy, jesteśmy **producentem** w rozumieniu CRA
   (2024/2847). NIS2/KSC reguluje przede wszystkim **klientów** (operatorów). Do nas trafia przez
   umowy jako bezpieczeństwo łańcucha dostaw: SBOM, terminy łatania, raport z pentestu, prawo audytu.
   Wyjątek: firma, która utrzymuje instalacje klientów (zdalny serwis, hosting), może sama być
   podmiotem ważnym jako *dostawca usług zarządzanych ICT* — to wymaga oceny prawnej. Moduł jest
   projektowany **z perspektywy producenta**. Dane dla klientów NIS2 (SBOM, VEX, advisory, odpowiedzi
   na ankiety) są jego produktem ubocznym.
2. **Rozbudowa Projektów, nie nowa aplikacja.** Projekty już mają to, na czym SSDLC stoi:

   | Potrzeba SSDLC | Jest w Projektach |
   |---|---|
   | per-produkt dane i izolacja | baza per projekt (`project_db.rs`, pula LRU) |
   | członkowie i role | `project_members` |
   | repozytoria (kilka na projekt) | `sources` (`kind = 'git'`), `git_source.rs` |
   | zadania i defekty z ważnością | `tasks` (`task_type`, `severity`) |
   | testy, zestawy, przebiegi, raporty | `test_cases`, `test_runs_v3`, `reports.rs` |
   | cele testów z zatwierdzaniem i listą hostów | `environments` (`approval_status`, `host_allowlist_json`) |
   | harmonogramy i przebiegi automatyczne | `schedules`, `auto_runs.rs` |
   | przełączanie modułów per projekt | `projects.modules_json` |
   | agenci LLM z bezpiecznym zapisem wyniku | `generation.rs`, `code_assist.rs` |

   Osobna aplikacja musiałaby to połączyć odnośnikami, a użytkownik skakałby między dwoma miejscami
   przy jednej zmianie. W projekcie „bezpieczeństwo” staje się **właściwością zadania, przebiegu,
   środowiska i wydania**, a nie osobnym światem. Koszt tej decyzji — już duży moduł rośnie dalej
   (~22 tys. linii) — ograniczamy podmodułem `project_studio/security/` z własnymi tabelami,
   dispatchem i plikami frontu (§15).
3. **Projekt = produkt w rozumieniu CRA**, gdy w projekcie włączono moduł `security`. Produkt z kilku
   repozytoriów to jeden projekt z kilkoma źródłami git. Widok „Portfolio bezpieczeństwa” na liście
   projektów zbiera projekty z włączonym modułem, do których użytkownik ma dostęp.
4. **Role = funkcje w zespole z `PROJECT_STUDIO_WORKFLOW_PLAN.md` §2.** Członek ma zbiór funkcji
   (PM, Developer, Tester, Security, DevOps, Release Manager, …), a uprawnienia są sumą macierzy
   obszarów. Moduł dokłada obszary `security` i `security.confidential` oraz funkcję **Security**.
   Dawny „Security Champion” to osoba z funkcjami Developer + Security.
5. **LLM przygotowuje, człowiek decyduje.** Model klasyfikuje, streszcza, proponuje VEX, szkicuje
   zgłoszenie CRA i poprawkę. **Nigdy** sam nie zamyka podatności jako `nie_dotyczy`, nie wysyła
   zgłoszenia do ENISA, nie akceptuje wyjątku i nie otwiera bramki wydania.
6. **Narzędzia to kontenery** (`tentaflow-containers/security/`), a wyniki są normalizowane do
   **SARIF** i **CycloneDX**. Nowe narzędzie = nowy manifest. Wyniki z zewnętrznego CI wchodzą tą
   samą drogą.
7. **Skanowanie aktywne tylko w zatwierdzonym zakresie.** DAST, nmap, nuclei i narzędzia pentestowe
   atakują wyłącznie środowiska projektu z `approval_status = 'approved'`, w granicach ich
   `host_allowlist_json` — mechanizm już istnieje i właśnie do tego się nadaje. Bez tej bramki
   Projekty stałyby się narzędziem ataku na cudze systemy (§14).

## 1. Zakres

### 1.1 W zakresie

| Obszar | Co dochodzi do Projektów |
|---|---|
| Produkt | ustawienia CRA projektu: producent, kategoria z uzasadnieniem, okres wsparcia, linie wspierane |
| Zmiany | `security-impact` na zadaniu i MR (propozycja LLM + decyzja), model zagrożeń podpięty do zadania |
| Bramki | polityka bramek projektu (MR, tag), status commita i komentarz MR w git |
| SBOM | generowanie per ekosystem, scalanie, wersjonowanie per wydanie, podpis, eksport; ręczny manifest bibliotek „wklejonych” |
| Podatności | codzienne ponowne sprawdzanie SBOM wszystkich wspieranych wydań, VEX, SLA |
| SAST / sekrety / IaC | uruchamianie skanerów, deduplikacja, triage, wyciszenia z uzasadnieniem |
| Środowiska | obok dzisiejszych celów (stały URL) **środowiska uruchamiane** z szablonu, z TTL (Docker/Portainer, Jenkins, GitLab) |
| DAST i pentesty | skany jako nowy rodzaj przebiegu; zlecenia pentestu (zakres, konta czasowe, import raportu, retest) |
| PSIRT / CRA | sprawy podatności (poufne), zegar 24 h / 72 h / 14 dni, szkice zgłoszeń, advisory CSAF, powiadomienia klientów |
| Wyjątki, obowiązki, dowody | akceptacja ryzyka z wygaśnięciem, kalendarz czynności cyklicznych z eskalacją, paczki dowodowe i dokumenty CRA |

### 1.2 Poza zakresem

- własny system CI/CD — istniejący Jenkins/GitLab jest sterowany albo wysyła wyniki (§5),
- automatyczne wysyłanie zgłoszeń do ENISA SRP (v1: gotowy tekst, checklista i zapis numeru;
  API **[niezweryfikowane]**),
- portal publiczny dla klientów (później: „Trust Center” z advisory, SBOM na żądanie i `security.txt`),
- fuzzing, RASP, analiza binariów.

## 2. Stan repo — co dostajemy, czego brakuje

### 2.1 Reużywamy

| Potrzeba | Mechanizm |
|---|---|
| Uprawnienia aplikacji | `project_studio/app-manifest.toml` `[[permission]]` + `dispatch/app_gate.rs` |
| Członkostwo, role, izolacja | `project_members`, krata ról w `models.rs`, baza per projekt |
| Repozytoria i klonowanie z zaszyfrowanym tokenem i ochroną SSRF | `sources`, `git_source.rs`, `code_studio/git_broker.rs` |
| Zadania, komentarze, powiadomienia | `tasks.rs`, `task_comments`, `notifications.rs` |
| Przebiegi, artefakty, raporty | `runs.rs`, `run_artifacts`, `reports.rs`, `test-runner` |
| Cele testów, zatwierdzanie, lista hostów | `environments.rs` |
| Harmonogram | `schedules.rs`, `auto_runs.rs`, `scheduler/` |
| Kontenery narzędzi | `tentaflow-containers/*/_services/*.toml`, `deploy/docker.rs`, `services/portainer.rs`, `SandboxLimits::test_runner` |
| LLM z audytem i limitami | `compliance/ai_gateway.rs`, aliasy modeli, `provider_accounts/` |
| Agenci, zapis wyniku przez sink, niezaufane wejście jako dane | `agents/`, `generation.rs`, `code_assist.rs`, `services/coding_agent.rs` |
| Dziennik z łańcuchem skrótów | `audit/chain.rs`, `audit/verify.rs` |
| Zdarzenia | TentaBus (`api/bus_rest.rs`), `flow_engine/` |

### 2.2 Luki do zamknięcia (Faza 0)

| Luka | Rozwiązanie |
|---|---|
| Rola członka jest pojedyncza i liniowa | kolumna `project_members.capabilities` (§4) + `expires_at` dla kont czasowych |
| Brak odbioru webhooków | endpoint `POST /v1/projekty/{project}/hooks/{provider}` z weryfikacją podpisu (§5.2) + tryb odpytywania |
| Brak kontenerów jednorazowych per przebieg | F0–F2: wykonawcy na wzór `test-runner` tylko dla repozytoriów własnych (§6.2); od F3: kontener na przebieg |
| Środowisko = stały URL | nowy rodzaj „uruchamiane” z szablonem i sterownikiem (§7) |
| Brak klientów API GitLab/GitHub/Gitea/Jenkins | `project_studio/security/forge/`: status commita, komentarz MR, wyzwolenie joba, odczyt wyniku |
| Brak narzędzi bezpieczeństwa w katalogu | `tentaflow-containers/security/` (§6.1) |

## 3. Model pojęciowy

### 3.1 Zmiany w istniejących bytach

| Byt | Zmiana |
|---|---|
| `projects.modules_json` | nowy moduł `"security"` — włącza zakładki i obowiązki; wyłączenie ukrywa je, ale **nie kasuje** danych (dowody trzymamy 10 lat) |
| `project_members` | `capabilities` (zbiór, §4), `expires_at` (konto czasowe pentestera/audytora) |
| `tasks.task_type` | + `security` (poprawka podatności, model zagrożeń, retest); `confidential` (flaga — treść tylko dla `psirt`) |
| `tasks` | + `security_impact` (`''/none/low/high`), odnośniki do znaleziska / sprawy / modelu zagrożeń w `links_json` |
| `environments` | + `kind` (`static` — jak dziś / `managed` — uruchamiane), `template_id`, `driver`, `ttl`, `state` |
| `test_runs_v3` | + rodzaje przebiegu `scan` (skaner) i `dast`; wynik → znaleziska |
| `sources` (git) | + tryb integracji (webhook / odpytywanie / CI-upload), sekret webhooka w sejfie, polityka bramek per repo |

### 3.2 Nowe byty (baza projektu)

| Byt | Opis |
|---|---|
| **Linia wspierana** | `X.Y`, data końca wsparcia, gałąź łatkowa |
| **Wydanie** | `X.Y.Z`, tag, commit, digesty obrazów, SBOM, VEX, wyniki bramek, paczka dowodowa, stan |
| **Przebieg skanu** | narzędzie + wersja + baza podatności z datą, wejście, surowy wynik (CAS) |
| **Znalezisko** | znormalizowane, **odcisk** do deduplikacji, ważność, stan, lista wydań, których dotyczy |
| **Sprawa PSIRT** | rekord wg `szablony/rekord-podatnosci.md`, poufna, z zegarem CRA |
| **Wyjątek** | zakres, uzasadnienie, środki kompensujące, zatwierdzający, wygaśnięcie |
| **Model zagrożeń** | dokument + diagram (Threat Dragon JSON), wersja, powiązanie z zadaniem/MR |
| **Szablon środowiska** | z repo (`.tentaflow/environment.yml`), wersjonowany z kodem |
| **Zlecenie pentestu** | zakres, zasady, okno, testerzy, raport, retest |
| **Obowiązek** | reguła → wystąpienie z terminem, właścicielem, eskalacją (§10) |
| **Dokument** | wersjonowany, z szablonu + danych projektu, z zatwierdzeniem |
| **Propozycja LLM** | wynik modelu, model, skrót promptu, stan (oczekuje / przyjęta / odrzucona), kto zdecydował |

### 3.3 Maszyny stanów

**Znalezisko:** `nowe → w_triage → {potwierdzone | fałszywy_alarm | nie_dotyczy (VEX) | zaakceptowane (wyjątek)} → naprawione → zweryfikowane`.
- Przejścia poza `nowe → w_triage` wymagają człowieka ze zdolnością `triage`.
- Stany `fałszywy_alarm` i `nie_dotyczy` wymagają uzasadnienia.
- Znalezisko po stanie `naprawione` wraca do `nowe`, gdy odcisk pojawi się w nowszym przebiegu.
- Potwierdzone znalezisko tworzy **zadanie `security`** z terminem SLA — naprawa idzie zwykłą tablicą zadań.

**Sprawa PSIRT:** `przyjęta → triage → {nie_dotyczy | dotyczy} → poprawka → wydana → opublikowana → zamknięta`.
Flaga `aktywnie_wykorzystywana` z datą uzyskania wiedzy uruchamia **zegar CRA**: 24 h i 72 h od tej
daty oraz 14 dni od stanu `wydana`. Każdy termin to obowiązek z eskalacją.

**Wydanie:** `szkic → budowanie → bramki → {zablokowane | gotowe} → zatwierdzone → opublikowane → wspierane → poza_wsparciem`.

**Środowisko uruchamiane:** `zamówione → [oczekuje_na_zgodę] → uruchamiane → gotowe → wygasa → usunięte` (+ `błąd`).
Zgoda stosuje dzisiejszą regułę `environments.rs`: publiczny adres zatwierdza się automatycznie,
a prywatny czeka na admina. Dodatkowo zgody wymagają dane inne niż syntetyczne.

## 4. Role i uprawnienia

Model ról opisuje `PROJECT_STUDIO_WORKFLOW_PLAN.md` §2: **funkcje w zespole** (kilka na osobę)
i **macierz obszarów** egzekwowana na serwerze. Moduł bezpieczeństwa dokłada do niej obszary
i uprawnienia aplikacji. W pozostałej części tego dokumentu nazwy „zdolności” z rewizji 0.2
odpowiadają funkcjom według tabeli w §4.2.

### 4.1 Obszary modułu i uprawnienia aplikacji

| Obszar / uprawnienie | Znaczenie |
|---|---|
| obszar `security` (R/W/A) | R: pulpit, znaleziska niepoufne, wydania, SBOM; W: triage, VEX, modele zagrożeń, zlecenia skanów; A: polityka bramek, lista celów, wyjątki Medium/Low |
| obszar `security.confidential` (A) | sprawy PSIRT, zadania poufne, zegar CRA, szkice zgłoszeń i advisory — nadawany **jawnie** (nie dziedziczy go nawet administrator projektu) |
| obszar `releases` (A) | zatwierdzanie wydań |
| uprawnienie aplikacji `project_studio.security.enable` | włączenie modułu w projekcie (obowiązki prawne, retencja 10 lat) |
| uprawnienie aplikacji `project_studio.security.active_scan` | w ogóle wolno używać skanów aktywnych i narzędzi pentestowych (admin decyduje kto w firmie, projekt — gdzie) |
| uprawnienie aplikacji `project_studio.security.tools_admin` | katalog narzędzi, bazy podatności, sterowniki środowisk |

### 4.2 Funkcje a dawne „zdolności”

| Dawniej (rew. 0.2) | Teraz |
|---|---|
| `champion` | funkcja **Security** (zwykle razem z Developer): `security` W |
| `psirt` | funkcja Security + obszar `security.confidential` nadany jawnie (Product Security Officer) |
| `release` | funkcja **Release Manager**: `releases` A |
| `pentester` | funkcja Security lub Tester + uprawnienie aplikacji `active_scan`; zewnętrzny: członkostwo z `expires_at` |
| `risk_owner` | funkcja PM (wyjątki High) — ustawienie projektu, kto zatwierdza wyjątki High |
| `auditor` | funkcja **Obserwator** + odczyt dowodów (`security` R), członkostwo z `expires_at` |

### 4.3 Rozdział obowiązków (w handlerach)

- autor MR nie zatwierdza bramki review własnego MR,
- wnioskodawca wyjątku nie zatwierdza go sam,
- autor tagu zatwierdza wydanie tylko wtedy, gdy pozwala na to polityka projektu, i zawsze
  z wpisem w paczce dowodowej,
- propozycję LLM przyjmuje wyłącznie człowiek (`ActorKind::User`),
- zadanie `confidential` ma neutralny tytuł dla osób bez `security.confidential`
  („Poprawka bezpieczeństwa SEC-2026-014”), a treść jest ukryta.
- **Poufność jest egzekwowana na warstwie danych, nie w UI.** Treść spraw PSIRT i zadań poufnych
  **nie trafia** do:
  - wyszukiwania i indeksu wiedzy projektu (`knowledge.rs`, RAG) — osobny indeks tylko dla
    `security.confidential`,
  - ogólnego dziennika aktywności projektu (wpis „zadanie poufne zmienione” bez szczegółów),
  - powiadomień osób bez obszaru poufnego,
  - promptów LLM dla daily/weekly, changelogu, planu z dokumentów,
  - eksportów i raportów ogólnych.

  Test: użytkownik bez obszaru poufnego nie widzi tytułu, treści, komentarzy ani liczby
  komentarzy, ani przez wyszukiwarkę, ani przez czat projektu.

## 5. Integracja z git

### 5.1 Trzy tryby (per źródło git, można łączyć)

| Tryb | Jak działa | Kiedy |
|---|---|---|
| **A. Webhook** | serwer git wysyła zdarzenia, Projekty uruchamiają skanery w kontenerach i odsyłają status do MR | TentaFlow osiągalny z serwera git |
| **B. Odpytywanie** | co N minut `git ls-remote`, nowe commity i tagi → jak A | brak ruchu przychodzącego (NAT, strefa odseparowana) |
| **C. CI wysyła wyniki** | istniejący Jenkins/GitLab CI uruchamia narzędzia u siebie i wysyła SARIF/CycloneDX (`tentaflow-cli projekty upload` / REST z kluczem API projektu) | CI ma zostać głównym miejscem wykonania |

Tryb C pozwala zacząć bez zmiany infrastruktury zespołu — to on czyni moduł uniwersalnym.

### 5.2 Odbiór webhooków

`POST /v1/projekty/{project}/hooks/{gitlab|github|gitea}`, weryfikacja **przed** parsowaniem treści:
- GitLab: `X-Gitlab-Token` porównany w czasie stałym,
- GitHub: `X-Hub-Signature-256` (HMAC-SHA256 treści),
- Gitea: `X-Gitea-Signature` (HMAC-SHA256, hex).

Dalej: limit rozmiaru, idempotencja po identyfikatorze dostawy (`X-Gitlab-Event-UUID` /
`X-GitHub-Delivery`) zapisywanym w `webhook_deliveries` per źródło przez 7 dni — ponowne
doręczenie tego samego zdarzenia zwraca 200 bez ponownego przetwarzania — i kolejka (TentaBus). Nic nie jest
wykonywane synchronicznie. Nieznane źródło albo zły podpis → jednolite 404.

### 5.3 Zdarzenia → akcje (domyślna polityka, edytowalna per projekt)

| Zdarzenie | Akcje | Odpowiedź do git |
|---|---|---|
| MR otwarty / zaktualizowany | sekrety (zakres MR), SAST z linią bazową, SCA zmienionych manifestów, lint Dockerfile; LLM: propozycja `security-impact` + podsumowanie ryzyka; przy `high` zadanie „model zagrożeń” | status commita + jeden aktualizowany komentarz MR |
| Push do gałęzi głównej | to co MR + SBOM roboczy, oznaczenie znalezisk naprawionych | status |
| Tag `vX.Y.Z` | **pipeline wydania**: SBOM pełny, skan obrazu, środowisko uruchamiane → DAST baseline + zestaw testów E2E projektu → zniszczenie, podpis, paczka dowodowa, obowiązek „checklista wydania” | status (+ opcjonalnie release notes z sekcją bezpieczeństwa) |
| Zmiana manifestu zależności | ocena nowej zależności (licencja, utrzymanie, OpenSSF Scorecard, CVE) + propozycja LLM | komentarz |
| Codziennie | ponowne sprawdzenie SBOM wszystkich wspieranych wydań względem świeżej bazy | — (znaleziska, obowiązki, powiadomienia) |

### 5.4 Wyzwalanie zewnętrznego CI

Sterownik może **uruchomić** job zamiast skanować lokalnie: Jenkins (`buildWithParameters`
z tokenem, wynik przez `/api/json` albo wywołanie zwrotne) i GitLab (`/trigger/pipeline` ze
zmiennymi). Wynik wraca trybem C. Ta sama abstrakcja obsługuje sterowniki środowisk (§7).

## 6. Narzędzia w kontenerach

### 6.1 Kategoria `tentaflow-containers/security/`

| Manifest | Narzędzia | Wejście → wyjście | Sieć |
|---|---|---|---|
| `sec-scanner` (wykonawca) | Syft, cdxgen, CycloneDX CLI, Trivy (fs/image/config), Grype, OSV-Scanner, Semgrep CE / Opengrep, Gitleaks, Hadolint, cargo-audit | drzewo plików (ro) / obraz / SBOM → SARIF, CycloneDX | **brak** |
| `sec-dast` (wykonawca) | ZAP (plan automatyzacji), nuclei, testssl.sh, nmap | zatwierdzony cel → SARIF/JSON/HTML | tylko hosty z `host_allowlist_json` środowiska |
| `sec-workbench` (v2, interaktywny) | ZAP GUI przez noVNC, sqlmap, ffuf, jwt_tool | sesja przypisana do zlecenia pentestu | tylko cele zlecenia, zapis sesji |
| `sec-vulndb` (zadanie) | mirror OSV, Trivy DB, Grype DB | → wolumen ro | wyjście do źródeł albo import podpisanej paczki offline |
| `sec-sign` (wykonawca) | cosign | artefakt → podpis/atestacja | brak (klucz z sejfu na czas operacji) |

**Cele skanów aktywnych — przypięcie adresów (ochrona przed DNS rebinding):** dzisiejsze
`environments.rs` sprawdza adres przy zapisie i ponownie przy uruchomieniu (`:12-19`, `:189`), ale
skaner w kontenerze rozwiązuje nazwę **sam**, więc między sprawdzeniem a skanem zostaje okno
na podmianę rekordu DNS. Dlatego:
- core rozwiązuje nazwy celów **raz**, przy starcie przebiegu, sprawdza je z `host_allowlist_json`
  i klasą adresu,
- kontener `sec-dast` dostaje zapisane na sztywno mapowanie nazwa → IP (bez własnego DNS),
- ruch wychodzący kontenera jest ograniczony regułą sieci **do tej listy IP i portów** — nie do nazw,
- lista dozwolonych nie przyjmuje symboli wieloznacznych ani całych zakresów sieci; zakres wymaga
  wpisu administratora z uzasadnieniem.

Test obowiązkowy: nazwa publiczna przy zatwierdzeniu, prywatna przy uruchomieniu → odmowa.

Kontrakt wykonawcy = kontrakt `test-runner` (HTTP `POST /jobs`, `GET /jobs/{id}`, wyniki w katalogu
wyjściowym). Przebieg zapisuje wersję narzędzia i wiek bazy podatności. Polityka projektu może
blokować skan na bazie starszej niż N dni — to ważne dla instalacji odseparowanych, które dostają
bazę paczką offline.

### 6.2 Dlaczego wykonawcy, a nie kontener per przebieg (v1)

Platforma umie dziś długo żyjące serwisy. Wykonawca z `SandboxLimits` (rootfs ro, tmpfs,
`cap_drop ALL`, limity) wdraża się istniejącą drogą. Koszt: przebiegi różnych projektów dzielą
proces, dlatego:
- checkout robi core (`git_source.rs`), a kontener dostaje drzewo plików **bez** `.git/config`
  z tokenem,
- katalog roboczy jest usuwany po przebiegu,
- skanery działają w trybie bez wykonywania skryptów repo.

**Zakres v1 (decyzja po przeglądzie):** wspólny wykonawca skanuje **wyłącznie repozytoria własne
organizacji** (zaufane źródła). Katalog roboczy (tmpfs) jest osobny dla każdego przebiegu i niszczony
po nim, a kontener dostaje tylko drzewo plików — bez tokenów i bez sieci.

**Kontener na przebieg jest warunkiem wejścia fazy F3**, a nie F4, bo od F3 skanujemy MR/PR. Na
GitHubie PR bywa z forka, czyli jest kodem niezaufanym. Do tego czasu pipeline MR dla PR z forków
jest wyłączony (albo idzie wyłącznie trybem C — wynik z CI po stronie dostawcy).

### 6.3 Normalizacja i deduplikacja

- **SARIF:** `ruleId` + ścieżka + znormalizowany fragment (bez numeru linii) + narzędzie → odcisk.
- **SCA:** `purl` + CVE/GHSA/OSV → odcisk; jedna podatność w kilku wydaniach = jedno znalezisko
  z listą wydań.
- **Cykl życia przy codziennym ponownym sprawdzaniu:** codzienny przebieg porównuje **SBOM**
  wspieranych wydań (nie repozytoria) z bazą podatności — to dopasowanie w bazie danych, a nie
  ponowny skan kodu, więc koszt rośnie z liczbą komponentów, a nie z rozmiarem kodu. Znany odcisk
  aktualizuje listę wydań znaleziska. Nowy odcisk tworzy znalezisko. Odcisk, który zniknął ze
  wszystkich wspieranych wydań (np. wydanie wypadło ze wsparcia albo podbito zależność), przechodzi
  w stan **„nie wykrywane”** z datą — nie zostaje zamknięty jako „naprawione” bez człowieka,
  jeśli był potwierdzony.
- **Ważność:** z narzędzia → CVSS 4.0 (jeśli jest); EPSS i obecność w KEV to atrybuty. KEV jest
  **sugestią** „aktywnie wykorzystywanej”. Decyzję podejmuje człowiek, bo od niej startuje zegar CRA.

### 6.4 Biblioteki wklejone ręcznie

Wspólny problem produktów webowych (NextApp: 33 pliki w `wwwroot/lib`). Działanie:
1. skan wskazanego katalogu i sumy SHA-256,
2. rozpoznanie wersji: nagłówek pliku → znane skróty z rejestrów npm/CDN → propozycja LLM z dowodem,
3. wpis CycloneDX z `purl`.

Bramka MR: plik bez wpisu blokuje scalenie.

## 7. Środowiska testowe na żądanie

### 7.1 Środowisko uruchamiane (nowy `kind = 'managed'`)

Szablon w repo (`.tentaflow/environment.yml`): obrazy z tagu/MR, dane startowe **syntetyczne**,
**konta testowe o różnych rolach** (kluczowe dla testów uprawnień), TTL (domyślnie 8 h), zasoby,
sieć odseparowana. Po starcie środowisko staje się zwykłym celem testów projektu. `base_url`
i sekrety kont ustawia sterownik, a `host_allowlist_json` = hosty środowiska. Dzięki temu
istniejące przypadki testowe, zestawy i Playwright z `test-runner` działają bez zmian.

### 7.2 Sterowniki

| Sterownik | Co robi |
|---|---|
| `docker-local` | bollard na węźle mesh z etykietą `projekty-env`, sieć per środowisko (domyślny) |
| `portainer` | stos na zdalnym hoście (`services/portainer.rs`) |
| `jenkins` | job „deploy env” z parametrami; adres i konta przez wywołanie zwrotne |
| `gitlab` | pipeline z `environment:` i `on_stop`; adres ze zmiennych |
| `tentavm` (v2) | maszyna wirtualna ze snapshotu (produkty wymagające pełnego systemu) |

### 7.3 Użycie

1. **Pipeline wydania:** tag → środowisko → DAST + zestaw E2E → zniszczenie.
2. **Na żądanie** („pokaż MR !123 na żywo”) — przycisk przy MR/zadaniu, dla testera i PM-a.
3. **Pentest:** dłuższy TTL, zamrożona wersja, konta per tester.
4. **Reprodukcja PSIRT:** dotknięta wersja, widoczne tylko dla `psirt`.

## 8. Pentesty

- **Zlecenie pentestu** = zakres (źródła, wersja, środowisko, wymagania ASVS), zasady (okno,
  wyłączenia, kontakt awaryjny), rodzaj (wewnętrzny/zewnętrzny), testerzy.
- **Tester zewnętrzny:** członek projektu `tester` + `pentester` z `expires_at` = koniec okna.
- **Plan:** checklista WSTG/ASVS jako **zestaw przypadków testowych** (to już jest byt Projektów).
  LLM proponuje przypadki z opisu API (`api_spec.rs`) i modeli zagrożeń.
- **Wykonanie:** `sec-dast` (automat), `sec-workbench` (v2) albo narzędzia testera.
- **Import raportu zewnętrznego** (PDF/DOCX): LLM wyciąga znaleziska jako propozycje, a pentester
  lub PSO je przyjmuje.
- **Znaleziska** trafiają do wspólnej listy z SLA. Retest to przebieg testu. Zamknięcie wymaga retestu.
- **Wyniki:** podsumowanie wykonawcze dla klientów (z szablonu, bez szczegółów exploitacji) + raport
  pełny w dowodach (dostęp `psirt`/`auditor`).

## 9. Automatyzacja i LLM

Wszystkie wywołania idą przez `AiGateway` (audyt, limity). Treść z repo, zgłoszeń i raportów to
dane niezaufane (`code_assist.rs`). Wynik trafia przez sink z powiązaniem ustawionym przez serwer
(`generation.rs`). Polityka projektu określa, czy dane poufne (PSIRT, kod) mogą iść do dostawcy
zewnętrznego — domyślnie **tylko model lokalny**. Egzekwuje to **brama AI (`compliance/ai_gateway.rs`)**,
a nie interfejs: wywołanie z danymi oznaczonymi `security.confidential` do modelu spoza listy
modeli lokalnych jest odrzucane i zapisywane w dzienniku.

| Czynność | Wyzwalacz | Wynik LLM | Kto decyduje |
|---|---|---|---|
| Klasyfikacja `security-impact` | MR / zadanie | poziom + uzasadnienie + obszary z checklisty | autor / champion |
| Szkic modelu zagrożeń | `security-impact: high` | diagram przepływu + STRIDE + wymagania ASVS | champion |
| Przegląd MR pod kątem bezpieczeństwa | MR `high` | komentarze do linii (uprawnienia, SQL, walidacja, sekrety) | reviewer |
| Triage znalezisk | nowe znalezisko | grupowanie, prawdopodobny fałszywy alarm, **osiągalność** (czy kod woła podatną funkcję), propozycja VEX | champion |
| Poprawka zależności | podatność z wersją naprawioną | MR z podbiciem (agent kodujący) + streszczenie changelogu | developer + bramki |
| Wyjaśnienie znaleziska | na żądanie | opis + przykład poprawki w stylu repo | developer |
| Szkice zgłoszeń CRA 24/72/14 | zegar CRA | tekst pól art. 14 | PSO (wysyła ręcznie) |
| Advisory i noty wydania | wydanie z poprawką | szkic CSAF + wersja czytelna | PSO / release |
| Ocena nowej zależności | zmiana manifestu | licencja, utrzymanie, Scorecard, alternatywy | champion |
| **Ankiety bezpieczeństwa klientów** (NIS2/DORA) | import XLSX | odpowiedzi z odnośnikami do dowodów (RAG — wiedza projektu `knowledge.rs` + dowody) | PSO |
| Przegląd modelu zagrożeń | obowiązek kwartalny | zmiany od ostatniego przeglądu, które mogą go unieważnić | champion |

**Nie automatyzujemy decyzji** z odpowiedzialnością prawną: `nie_dotyczy`, wysłanie zgłoszenia CRA,
akceptacja wyjątku, zatwierdzenie wydania, publikacja advisory.

## 10. Silnik obowiązków („żeby nie zapominać”)

Reguła → wystąpienie z terminem, właścicielem (zdolność/rola w projekcie) i eskalacją (przypomnienie
→ zastępca → manager projektu). Wystąpienie może założyć **zadanie** na tablicy (np. „Ćwiczenie
procedury 24 h — Q4”), więc obowiązki są widoczne tam, gdzie zespół i tak pracuje.

| Reguła | Termin | Właściciel |
|---|---|---|
| Znalezisko potwierdzone | SLA ważności (np. Critical 7 dni, High 30, Medium 90) | przypisany developer |
| Zegar CRA | 24 h / 72 h od wiedzy o wykorzystaniu; 14 dni od wydania poprawki | `psirt`, eskalacja do drugiej osoby `psirt` |
| Potwierdzenie przyjęcia zgłoszenia | 2 dni robocze | `psirt` |
| Wyjątek wygasa | 14 dni przed | właściciel wyjątku |
| Pentest zewnętrzny > 12 miesięcy | 60 dni przed | `psirt` |
| Baza podatności przestarzała / przebieg nocny nie ruszył | natychmiast | admin narzędzi |
| `security.txt` — `Expires` | 30 dni przed | `psirt` |
| Koniec wsparcia linii | 90 dni przed (komunikat do klientów) | `release` |
| Przegląd modelu zagrożeń systemu | co kwartał | `champion` |
| Ćwiczenie procedury 24 h | co kwartał | `psirt` |
| Samoocena SAMM, przegląd polityki | co rok | `psirt` |
| Szkolenie secure coding | co rok, per członek `editor+` | każdy + manager |
| Zależność nieaktualizowana > 12 miesięcy | przegląd cotygodniowy | `champion` |
| Przegląd członków, zdolności i kont czasowych | co kwartał | manager |

Obowiązek zamyka człowiek albo spełnienie warunku (np. nowy raport pentestu zamyka „pentest > 12 mies.”).

## 11. PSIRT i CRA

- **Przyjmowanie:** `security@` (IMAP albo przekazanie), import z CI/skanów, zgłoszenie klienta
  (formularz publiczny w v2). Potwierdzenie przyjęcia z numerem sprawy, z szablonu zatwierdzonego
  przez PSO.
- **Rekord sprawy** = formularz z `rekord-podatnosci.md`. Dziennik w audycie z łańcuchem skrótów jest
  dowodem, **kiedy** firma się dowiedziała, a od tego liczą się terminy CRA.
- **ENISA SRP:** v1 generuje treść pól, prowadzi checklistę i zapisuje numer oraz godzinę wysłania.
  Wysyłkę wykonuje człowiek.
- **Klienci:** advisory CSAF 2.0 + wersja czytelna, VEX CycloneDX per wydanie, lista kontaktów
  bezpieczeństwa klientów per projekt, dziennik doręczeń.

## 12. Dowody i dokumenty

- **Paczka dowodowa wydania** (struktura z `CyberSecurity/07-dokumenty-i-dowody.md` §C) składa się
  automatycznie: SBOM, VEX, surowe wyniki, wersje narzędzi i baz, decyzje (kto, kiedy), wyjątki,
  checklista, podpisy, sumy. CAS, niezmienna, retencja 10 lat — **także po archiwizacji projektu**
  (`archive.rs` musi tego przestrzegać).
- **Niezmienność dowodów:** paczki i surowe wyniki w CAS (adres = skrót treści), rekord paczki
  tylko do dopisywania — brak operacji edycji i usuwania w API w okresie retencji. Okresowa
  weryfikacja łańcucha (`audit/verify.rs`) i sum CAS, a rozbieżność daje alarm.
- **Retencja a usuwanie i RODO:**
  - projekt z włączonym modułem **nie da się usunąć** w okresie retencji dowodów — tylko
    zarchiwizować (`archive.rs` respektuje blokadę),
  - dowody zawierają możliwie mało danych osobowych: identyfikatory kont zamiast nazwisk
    w paczkach, kontakty klientów poza paczką,
  - żądanie usunięcia danych osoby → **pseudonimizacja** jej danych w dowodach (podstawa
    przechowywania samej treści technicznej: obowiązek prawny producenta z CRA art. 13) —
    do potwierdzenia z prawnikiem **[niezweryfikowane]**.
  - mechanizm (rejestr kategorii danych, pseudonimizacja kluczem organizacji, blokady prawne
    dla spraw PSIRT, retencja per kategoria) to wspólna usługa platformy — `ORG_STRUCTURE_PLAN.md`
    §6.4; moduł deklaruje swoje kategorie (znaleziska, sprawy PSIRT, dowody, kontakty klientów).
- **Dokumenty z szablonów** `CyberSecurity/szablony/`: ocena ryzyka, tabela Zał. I, informacje dla
  użytkownika, deklaracja zgodności UE. Wersjonowane, z zatwierdzeniem.
- **Eksport audytorski:** ZIP „stan na dzień” (dokumenty + ostatnie N paczek + wyjątki + metryki SLA).
- **Metryki:** czas naprawy vs SLA, zaległe podatności we wspieranych wersjach, odsetek MR `high`
  z modelem zagrożeń, wiek wyjątków.

## 13. Ekrany (mockupy: `mockups/projekty-security-RRRRMMDD/`, kontrakt `projekty-20260723/shared/BUILD_CONTRACT.md`)

Nowa zakładka projektu **Bezpieczeństwo** (widoczna, gdy moduł włączony) z podzakładkami.
Pozostałe zmiany wchodzą do istniejących ekranów.

| ID | Ekran | Rodzaj |
|---|---|---|
| B01 | Lista projektów — kolumna/filtr „Bezpieczeństwo” (Critical/High, obowiązki po terminie) + widok Portfolio | zmiana `p01` |
| B02 | Bezpieczeństwo · Pulpit — KPI, „do decyzji” (propozycje LLM, wyjątki, wydania), oś zdarzeń | nowy |
| B03 | Bezpieczeństwo · Znaleziska — filtry, grupowanie, triage, panel propozycji VEX | nowy |
| B04 | Bezpieczeństwo · Wydania — bramki, SBOM, paczka dowodowa, zatwierdzenie | nowy |
| B05 | Bezpieczeństwo · SBOM — drzewo, licencje, komponenty ręczne, porównanie wydań | nowy |
| B06 | Bezpieczeństwo · PSIRT — sprawy (poufne), **zegar CRA** jako oś 24 h / 72 h / 14 dni, szkice, advisory | nowy |
| B07 | Bezpieczeństwo · Pentesty — zlecenia, plan (zestaw przypadków), znaleziska, retest, import | nowy |
| B08 | Bezpieczeństwo · Wyjątki i Obowiązki | nowy |
| B09 | Bezpieczeństwo · Dokumenty i dowody | nowy |
| B10 | Zadanie — pole `security-impact`, propozycja LLM, model zagrożeń, flaga poufności | zmiana `z02` |
| B11 | Środowiska — rodzaj „uruchamiane”: szablony, TTL, zamów środowisko (okno) | zmiana `t12` |
| B12 | Członkowie — zdolności bezpieczeństwa i konto czasowe | zmiana `x03` |
| B13 | Ustawienia projektu — moduł Bezpieczeństwo: dane produktu CRA, linie wspierane, tryby git, polityka bramek, LLM dla danych poufnych | zmiana `x04` |

## 14. Model zagrożeń modułu

| Zagrożenie | Środek |
|---|---|
| **Narzędzie ataku na cudze systemy** | skany aktywne tylko na środowiskach `approved` w granicach `host_allowlist_json`; uprawnienie aplikacji `active_scan` + zdolność `pentester`; audyt każdego skanu; `sec-dast` z polityką sieci |
| Kradzież tokenów git i kluczy podpisu | sejf, token nie trafia do kontenera, klucz cosign tylko w `sec-sign` na czas operacji |
| Złośliwe repozytorium | skanery bez wykonywania skryptów, brak sieci; do F3 tylko repozytoria własne, od F3 kontener na przebieg (PR z forków) |
| Wstrzyknięcie poleceń do LLM przez MR, kod, zgłoszenie, raport | treść jako dane, sink z powiązaniem serwera, brak narzędzi zapisu poza sinkiem, decyzje ludzkie |
| Wyciek danych PSIRT do członków projektu (zob. §4.3 — warstwa danych) | zdolność `psirt`, zadania `confidential` z neutralnym tytułem, filtr w `tasks.rs` i w wyszukiwaniu/wiedzy projektu (RAG nie może indeksować treści poufnej dla osób bez `psirt`) |
| Konto czasowe żyje dłużej niż zlecenie | `expires_at` sprawdzane w bramce dostępu do projektu (także dla otwartych sesji i websocketów), przypomnienie właścicielowi zlecenia 7 dni przed końcem, przedłużenie tylko przez administratora projektu z wpisem w historii; obowiązek kwartalnego przeglądu |
| Podrobione webhooki | podpis przed parsowaniem, idempotencja, jednolite 404 |
| Łańcuch dostaw samych narzędzi | obrazy `security/*` przypięte digestem, skanowane przez moduł (Projekty skanują swoje narzędzia), podpisane paczki baz offline |
| Manipulacja dowodami | CAS + audyt z łańcuchem skrótów + podpis paczki |

## 15. Model danych i kod

- **Baza główna (`projekty.db`):** `project_members` + `capabilities TEXT NOT NULL DEFAULT '[]'`,
  `expires_at TEXT`. Nowe uprawnienia w manifeście.
- **Baza projektu:** migracja dokłada tabele modułu z prefiksem `sec_`: `sec_support_lines`,
  `sec_releases`, `sec_scan_runs`, `sec_findings`, `sec_finding_releases`, `sec_finding_events`,
  `sec_vex`, `sec_exceptions`, `sec_threat_models`, `sec_env_templates`, `sec_pentests`,
  `sec_psirt_cases`, `sec_psirt_clock`, `sec_obligation_rules`, `sec_obligations`, `sec_documents`,
  `sec_llm_proposals`, `sec_customer_contacts`. Zmiany w `tasks`, `environments`, `test_runs_v3`,
  `sources` jak w §3.1.
- **CAS:** surowe wyniki, SBOM, paczki dowodowe, raporty pentestów.
- **Kod:** `project_studio/security/` (`mod.rs`, `findings.rs`, `sbom.rs`, `releases.rs`, `psirt.rs`,
  `obligations.rs`, `pentests.rs`, `envs_managed.rs`, `forge/`, `hooks.rs`, `llm.rs`, `evidence.rs`).
  Osobny `dispatch/project_studio_security.rs`, każde wywołanie przez bramkę roli + zdolności.
  Front: `www/js/modules/projekty/security-*.js`, i18n w 5 językach.

## 16. Fazy

| Faza | Zakres | Wartość |
|---|---|---|
| **F0 — fundament** | zdolności i `expires_at`, moduł `security` w `modules_json`, webhooki + odpytywanie, klienci forge, manifesty `sec-scanner` i `sec-vulndb` | szkielet |
| **F1 — SBOM i podatności** | linie wspierane, wydania, SBOM, codzienne sprawdzanie, znaleziska SCA → zadania `security`, VEX, triage, komponenty ręczne, tryb C | CRA Zał. I cz. II pkt 1–2; odpowiedź na „dajcie SBOM” |
| **F2 — PSIRT i obowiązki** | sprawy PSIRT, zadania poufne, zegar CRA, szkice zgłoszeń, advisory, obowiązki, wyjątki | art. 14 CRA; nic nie ginie |
| **F3 — bramki i LLM** (warunek wejścia: kontener na przebieg) | pipeline MR/tagu, SAST/sekrety/IaC, statusy do git, propozycje LLM, paczka dowodowa | pełny SSDLC z dowodami |
| **F4 — środowiska i DAST** | środowiska uruchamiane i sterowniki, `sec-dast` z przypiętymi adresami celów | testy na żywym produkcie przed wydaniem |
| **F5 — pentesty i dokumenty** | zlecenia pentestów, konta czasowe, import raportów, workbench, dokumenty z szablonów, eksport audytorski, ankiety klientów | komplet dokumentacji technicznej CRA |

Pierwszy projekt przeprowadzany przez moduł to **sama TentaFlow** (Rust, `Cargo.lock`,
`docs/cargo-dependencies-audit.md`). Drugi to **NextApp** (.NET + npm + biblioteki ręczne + Docker).
Dwa różne ekosystemy od początku pilnują uniwersalności.

## 17. Otwarte pytania

1. ~~Dostawcy LLM~~ — **rozstrzygnięte:** domyślnie modele lokalne, zewnętrzni niezablokowani; model wybiera konfiguracja agenta. Otwarte jedynie: czy dla spraw `security.confidential` wymuszać model lokalny polityką projektu.
2. ~~Serwery git~~ — **rozstrzygnięte:** GitLab i GitHub obowiązkowo od pierwszej fazy; Gitea później.
3. Czy Jenkins faktycznie buduje wydania (w NextApp `jenkinsfile` to szablon) — priorytet sterownika Jenkins vs GitLab?
4. Czy klienci mają dostawać SBOM/VEX/advisory przez portal (Trust Center), czy wystarczy eksport?
5. Czy moduł ma być dostępny w edycji slim, czy tylko full (kontenery narzędzi ważą kilka GB)?
6. Retencja 10 lat a usuwanie projektu: czy „usuń projekt” przy włączonym module ma być zablokowane, czy ma zostawiać archiwum dowodów?
