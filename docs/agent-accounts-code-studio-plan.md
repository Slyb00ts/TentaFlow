# Konta dostawców i aplikacje agentowe w TentaFlow

Data analizy: 2026-09-06. Dokument zachowuje analizę stanu **sprzed implementacji** oraz docelowy plan. Sekcja 2 opisuje wcześniejszy kod, a nie obecny diff.

Zaimplementowano osobne profile kont, granty użytkowników, sandbox procesowy, lokalne katalogi, czat jako domyślny widok i adaptery Grok Build/Muse Code. Testy cyklu życia, odtworzenia gatewaya po awarii oraz przerwanego przeniesienia konta między dwoma rzeczywistymi procesami mesh przeszły. Wyniki wykonanych prób i ich ograniczenia są w [process-sandbox-validation.md](process-sandbox-validation.md) oraz [provider-cli-contracts.md](provider-cli-contracts.md). Nie zakończono logowania na prawdziwych kontach i nie wykonano płatnych zapytań.

Zakres pierwszego wdrożenia jest węższy od docelowego planu: zarządzanie kontami i przypinanie katalogu hosta wymaga administratora; macOS ma testowany sandbox, Linux i Windows wymagają osobnej walidacji. Mobilność oznacza jawne przeniesienie nieaktywnego konta Codex/Claude pomiędzy zaufanymi nodami, bez kopiowania projektów i historii oraz bez równoczesnego działania dwóch kopii. Integracja Antigravity jest zablokowana przez niepotwierdzoną izolację systemowego keyringu. Przenoszenie kont Grok/Muse, automatyczny refresh ze sprawdzaniem tożsamości oraz wariant Docker nie są deklarowane jako gotowe.

Grant konta w obecnej implementacji oznacza zaufaną delegację poświadczenia: wykonywany kod może odczytać poświadczenie wybranego CLI. Nie uzyskano izolacji tego tokenu od użytkownika sesji. Cofnięcie dostępu zatrzymuje sesje TentaFlow, ale nie unieważnia tokenów u dostawcy; panel grantów jawnie opisuje tę granicę.

## 1. Rekomendacja i zakres

Wprowadzić kategorię **Aplikacje agentowe** w katalogu oraz widok **Konta i dostęp** w zarządzaniu serwisami. „Subskrypcja” to rodzaj rozliczania konta, nie rodzaj silnika ani deploymentu.

Rozdzielić pięć pojęć:

| Pojęcie | Przykład | Odpowiedzialność |
|---|---|---|
| Dostawca | OpenAI, Anthropic, Google, xAI, Meta | Tożsamość zewnętrzna, uprawnienia produktowe, uwierzytelnianie |
| Aplikacja/silnik | Codex CLI, Claude Code, Antigravity CLI, Grok Build, Muse Code | Program, wersja, protokół integracji, możliwości |
| Konto | „Codex prywatny Anny”, „Codex firma X” | Właściciel, sposób logowania, dostęp, limity, poświadczenia |
| Instalacja/serwis | Codex  na nodzie A | Dostępność runtime, wersja, stan instalacji, możliwości wykonawcze |
| Instancja/sesja | Konto Anny + projekt P + sesja S na nodzie B | Proces, katalog, historia, wybrane konto, budżet, czas życia |

Jedno konto może mieć dostęp do kilku aplikacji, jeśli faktycznie pozwala na to dostawca. Kilka kont tego samego dostawcy to kilka rekordów z własnymi identyfikatorami. Kilka sesji na jednym koncie nie oznacza dodatkowego limitu u dostawcy.

Założenie do oceny użytkownika: konta osobiste są domyślnie prywatne; konta/miejsca organizacji mają jawne przypisania. Uprawnienie w TentaFlow nie tworzy automatycznie miejsca ani uprawnienia produktowego u dostawcy. Projekt zarządza użyciem istniejących kont, nie zakupem i fakturowaniem subskrypcji.

Zakres obejmuje wszystkie warstwy wymagane do działania wielu kont i pracy w odpowiednim projekcie. To więcej niż kosmetyczna zmiana katalogu. Wykorzystujemy istniejący supervisor, bridge, vault, PEP, sesje, worktree i mesh; nie budujemy osobnego systemu deploymentu, drugiego schedulera ani nowego transportu sieciowego. Poszczególne etapy mają kończyć się kompletnym, sprawdzalnym zachowaniem.

## 2. Co działa obecnie i gdzie

### 2.1 Dodawanie serwisów z GUI

Obecny przepływ:

```text
Services → New service → Catalog → wybór noda/targetu
  → manifest silnika → engine-deploy-wizard
  → serviceManifestDeployRequest (protokół binarny)
  → dispatch/handlers.rs → services/deploy → supervisor
  → rekordy deployment/services → modele i stan w GUI
```

- [services.js](../tentaflow-core/www/js/modules/services.js), okolice 108: przycisk nowego serwisu otwiera katalog.
- [catalog.js](../tentaflow-core/www/js/modules/catalog.js), okolice 90–115: target jest wybierany przed silnikiem; zakładki to TentaFlow, NIM, IBM, External.
- [manifest-store.js](../tentaflow-core/www/js/modules/catalog/manifest-store.js): katalog czyta generowany rejestr; `provider` steruje zakładką, `category` kategorią funkcjonalną.
- [engine-deploy-wizard.js](../tentaflow-core/www/js/modules/catalog/engine-deploy-wizard.js), 564: rzeczywista lista dostawców obsługujących wybór subskrypcji zawiera tylko `openai`. Gemini ma ślady UI/opisów, ale funkcja nie włącza mu tego trybu.
- [dispatch/handlers.rs](../tentaflow-core/src/dispatch/handlers.rs), 4286 i 4444: handler uruchamia deployment, odbiera wynik OAuth i szyfruje poświadczenia przed zapisaniem konfiguracji. Wskazana w starszym AGENTS.md ścieżka `api/dashboard/api_services_manifest.rs` nie istnieje w tym checkoutcie.

Cloud API, subskrypcja OpenAI używana jako backend modelu oraz pełna aplikacja CLI to już teraz różne ścieżki wykonania. Nie należy utożsamiać ich tylko dlatego, że wszystkie mogą zwracać tekst czatu.

### 2.2 Codex i Claude Code jako serwisy

Istnieją manifesty [codex.toml](../tentaflow-containers/agents/_services/codex.toml) oraz [claude-code.toml](../tentaflow-containers/agents/_services/claude-code.toml): `category=agents`, `provider=external`, Docker i native `managed-cli`. Kategoria `agents` obejmuje również inne typy agentów, np. teams-bot; sama zmiana nazwy całej kategorii byłaby zbyt szeroka.

- [services/deploy/binary.rs](../tentaflow-core/src/services/deploy/binary.rs), 104: instalacja CLI współdzielona według silnika i wersji; jest to sensowne dla niezmiennych plików programu.
- Ten sam plik, 172: **mutowalny stan** trafia do `keys/coding-agents/<engine_id>`. Brak `account_id`, `service_id` i `instance_id`.
- Native ustawia `CODEX_HOME` dla Codexa, lecz nie ustawia analogicznego `CLAUDE_CONFIG_DIR` dla Claude Code.
- [services/deploy/docker.rs](../tentaflow-core/src/services/deploy/docker.rs), 1796: kontenery tego samego silnika bindują ten sam katalog stanu hosta do `/data`.
- [agents/Dockerfile](../tentaflow-containers/agents/Dockerfile): `HOME=/data`, `CODEX_HOME=/data/codex`, `CLAUDE_CONFIG_DIR=/data/claude`, workspace w `/workspace`. Nie ma deklaracji `USER`.
- [coding-agent.js](../tentaflow-core/www/js/modules/coding-agent.js), 28: po deploymentcie GUI szuka pierwszego pasującego silnika i noda. Przy wielu wdrożeniach nie ma gwarancji, że otworzy właściwe konto/nowy deployment.

Wniosek o wysokiej pewności z kodu: **wiele rekordów services nie daje dziś izolacji wielu kont**. Wspólny katalog obejmuje także `sessions.json`, cache i rejestr procesów bridge. Ryzyko dotyczy loginu, historii i zarządzania procesami. Native i Docker używają przy tym różnych podścieżek na same dane uwierzytelnienia, więc nie zapewniają również jednolitej migracji konta.

### 2.3 Logowanie i uprawnienia

- [dispatch/handlers.rs](../tentaflow-core/src/dispatch/handlers.rs), 11372: `ServiceAgentRequest` wymaga sesji użytkownika; rozpoczęcie logowania i zapis/zamknięcie sesji `auth-*` wymagają admina.
- W tym handlerze nie ma sprawdzenia właściciela zwykłej sesji CLI ani grantu do konkretnego konta przed `sessions.list`, `session.events`, `session.turn`, `session.close`. Bridge również nie przechowuje właściciela konta/sesji.
- [mesh/command_executor.rs](../tentaflow-core/src/mesh/command_executor.rs), 1307: `handle_agent_rpc` dostaje serwis, operację i JSON; nie dostaje tożsamości użytkownika jako odpowiednika assertion z Code Studio. Zaufany peer nie identyfikuje sam w sobie osoby.
- [services/access.js](../tentaflow-core/www/js/modules/services/access.js) dotyczy widoczności modeli/aliasów oraz konsumentów dodatków. Nie jest kompletnym ACL kont użytkowników.
- [code_studio/db.rs](../tentaflow-core/src/code_studio/db.rs), 63: vault ma klucz główny `(org_id, node_id, engine_id)`. [vault.rs](../tentaflow-core/src/code_studio/vault.rs), 633: kolejne zapisanie klucza robi upsert. To pojedyncze poświadczenie silnika w danym zakresie, nie lista kont.

To ustalenia statyczne, nie wykonany test przejęcia sesji. Pokazują brak egzekwowania izolacji na analizowanej ścieżce RPC. Nowy model dostępu musi objąć odczyt zdarzeń i historii równie mocno jak uruchamianie procesu.

### 2.4 Ścieżki Code Studio i katalog projektu

- [code_studio/paths.rs](../tentaflow-core/src/code_studio/paths.rs): reference repo i worktree sesji są rozdzielone; sesja ma `code-studio/<workspace_id>/worktrees/<session_id>`.
- [provisioning.rs](../tentaflow-core/src/code_studio/provisioning.rs), 178: obecne repo to `empty` albo klon `git`. To nie jest ogólny mechanizm podłączenia wskazanego istniejącego katalogu hosta.
- [delegate_cli.rs](../tentaflow-core/src/flow_engine/node_adapters/delegate_cli.rs), 1092: delegacja wybiera bridge po `service_id` i bierze worktree sesji.
- [cli_bridge.rs](../tentaflow-core/src/code_studio/cli_bridge.rs), 500: `open` przekazuje worktree w `session.create`.
- [bridge/main.rs](../tentaflow-containers/agents/native/coding-agent-bridge/src/main.rs), 986, 1249 i 1458: bridge waliduje katalog pod skonfigurowanym rootem i uruchamia Codex/Claude z `current_dir(workspace)`; Codex dostaje dodatkowo `cwd` przy tworzeniu wątku.

**Nie brakuje samego ustawienia cwd.** Brakuje jednolitego powiązania konta, runtime i projektu oraz mapowania ścieżek host–kontener. Delegacja przekazuje ścieżkę hosta, a Docker montuje workspace jako `/workspace`; w analizowanym łańcuchu nie ma translacji. Native ogranicza root do wartości z deploymentu lub cwd procesu TentaFlow. Worktree Code Studio może być poza tym rootem. Kreator nie ustawia `workspace_root` w analizowanym formularzu.

Oddzielna ścieżka [services/coding_agent.rs](../tentaflow-core/src/services/coding_agent.rs), 268 (`execute_chat`), bierze workspace z konfiguracji serwisu lub `.`. Nie ma w argumentach workspace/sesji Code Studio. Nie wolno traktować zwykłego wybrania modelu `codex/...` w czacie jako równoważnego uruchomieniu agenta w otwartym projekcie. Obecna ścieżka AgentRpc w runtime zwraca też SSE dopiero po wyniku blokującym; natywny strumień zdarzeń bridge jest właściwszą podstawą czatu agentowego.

`current_dir` i sprawdzenie prefiksu ścieżki nie stanowią sandboxa. Bridge uruchamia programy bezpośrednio, a `.envs(...)` dokłada środowisko do dziedziczonego. Dla izolacji wielu użytkowników potrzebne są uprawnienia OS/kontenery i kontrola środowiska, nie tylko osobne nazwy katalogów.

### 2.5 Wybór sposobu płacenia i stan logowania

[cli_adapter.rs](../tentaflow-core/src/code_studio/cli_adapter.rs), 296: `resolve_delegation_auth` wybiera klucz organizacji z vault; przy jego braku pyta o login CLI na nodzie. Użytkownik nie wskazuje konta. [delegate_cli.rs](../tentaflow-core/src/flow_engine/node_adapters/delegate_cli.rs), 911: dla `ProviderLogin` zwraca puste env i args, zachowując login bridge.

Tryb API ma wartościowe zabezpieczenia: ticket ograniczony do uruchomienia, wstrzykiwanie sekretu poza CLI, limity, PEP i audyt. Należy zachować te mechanizmy. Nie należy wymuszać ich modelu proxy na loginie subskrypcyjnym, jeśli dostawca takiego sposobu nie wspiera.

Bridge wywołuje `require_authenticated(state.provider)` przed zastosowaniem env sesji. To dodatkowe sprzężenie: nawet wybór API/ticket może zależeć od loginu bridge. Trzeba sprawdzać wybrane poświadczenie uruchomienia, nie globalny stan programu.

Jest też osobny backend [codex.rs](../tentaflow-core/src/services/backend/codex.rs) i [codex_oauth.rs](../tentaflow-core/src/services/backend/codex_oauth.rs), który implementuje logowanie i dostęp do backendu subskrypcyjnego OpenAI bez CLI. Odświeżone tokeny w analizowanych funkcjach trafiają do cache w pamięci; nie widać zapisu rotowanego refresh tokenu z powrotem do trwałego magazynu. To ryzyko ponownego logowania po restarcie, nie zaobserwowany incydent. Nowy system nie może utrzymywać dwóch niezależnych właścicieli odświeżania tego samego konta.

### 2.6 Dlaczego wejście do projektu pokazuje ustawienia

[code-studio.js](../tentaflow-core/www/js/modules/code-studio.js), 624–625: `open` i `settings` wywołują `goto(workspaceId, null)`. Ten route otwiera sesje oraz konfigurację workspace. [code-studio-session.js](../tentaflow-core/www/js/modules/code-studio-session.js), 418: sama otwarta sesja już wybiera `konsola`, czyli główny czat.

## 3. Dostawcy — zweryfikowany stan

| Dostawca | Produkt i integracja | Wniosek dla planu |
|---|---|---|
| OpenAI | Codex CLI/app-server; logowanie ChatGPT lub API. Dokumentacja opisuje cache plikowy/keyring oraz przenoszenie pliku auth na headless host/Docker. | Oddzielne konto, wybrana metoda logowania, proces app-server w kontekście projektu. Przenoszenie tylko obsługiwanego materiału uwierzytelnienia. [Authentication](https://learn.chatgpt.com/docs/auth) |
| Anthropic | Claude Code obsługuje login subskrypcyjny, API i metody organizacyjne. Dokumentacja opisuje `CLAUDE_CONFIG_DIR`, osobne wpisy Keychain, plik credentials na Linux i `claude setup-token`. | Adapter ma rozróżniać pełny login i token do automatyzacji; nie wszystkie funkcje mają tę samą dostępność. Osobny profil również w native. [Authentication](https://code.claude.com/docs/en/authentication) |
| Google | **Antigravity CLI**, polecenie `agy`, oraz SDK. CLI ma tryb headless, wejście/wyjście stream-json i kontynuowanie konkretnej rozmowy. | To aplikacja wskazana jako „agy”. Adapter stream-json lub SDK po teście zgodności; nie uruchamiać pełnego IDE jako koniecznego elementu backendu. [Getting started](https://antigravity.google/docs/cli/getting-started), [Headless](https://antigravity.google/docs/cli/headless/) |
| xAI | **Grok Build**, polecenie `grok`; headless, jawny `cwd`, sesje i ACP (`grok agent stdio`). | Adapter ACP jest naturalnym punktem integracji. [Overview](https://docs.x.ai/build/overview), [Headless/ACP](https://docs.x.ai/build/cli/headless-scripting) |
| Meta | **Muse Code**, polecenie `muse`, oficjalna aplikacja terminalowa z subskrypcjami; osobno Meta Model API i modele Muse Spark. | Dodać pełny adapter aplikacji Muse Code oraz odrębną integrację API. [Muse Code](https://developer.meta.com/ai/products/muse-code/), [Dokumentacja](https://dev.meta.ai/docs/overview/) |

Google opisuje logowanie przez systemowy keyring i flow dla SSH. Obsługuje też oddzielny tryb Gemini API key wymagający ustawienia `modelProvider`; sam klucz w env nie wystarcza. Dokumentacja nie ustanawia ogólnego kontraktu eksportu loginu między dowolnymi systemami. **Mobilność subskrypcji Antigravity wymaga osobnego testu; nie można obiecać jej na podstawie kopiowania katalogu.** [Installation & Auth](https://antigravity.google/docs/cli/install/)

Grok dokumentuje browser/device-code, API key oraz `auth_provider_command`. To potencjalny punkt podłączenia zarządzanego źródła tokenów, ale jego zachowanie trzeba sprawdzić w przypiętej wersji; istnieją też polityki organizacyjne ograniczające rodzaj logowania. [Enterprise deployments](https://docs.x.ai/build/enterprise)

Gemini CLI nadal ma własną dokumentację, lecz zawiera komunikat o zastąpieniu go przez Antigravity CLI dla użytkowników free/Google One. Nie projektować nowej integracji Google wyłącznie wokół Gemini CLI bez uwzględnienia tej zmiany. [Gemini authentication](https://geminicli.com/docs/get-started/authentication/)

Muse Code ma login przeglądarkowy lub klucz; `META_API_KEY` ma pierwszeństwo przed zapisanym kluczem i loginem. Konta Meta Managed Account wymagają API key. Runner musi więc czyścić dziedziczone źródła uwierzytelniania i sprawdzać efektywny tryb konta. [Authentication and billing](https://dev.meta.ai/docs/muse-code/auth)

Subskrypcja Muse Code dotyczy specjalnego poświadczenia podłączanego przez onboarding CLI, przeznaczonego tylko do Muse Code. Dodatkowe klucze API są rozliczane pay-as-you-go. Nie kierować tego poświadczenia do ogólnego backendu Meta API ani nie zastępować go zwykłym kluczem po osiągnięciu limitu. Dostępność korzyści zależy od regionu. [Subscriptions](https://dev.meta.ai/docs/muse-code/subscriptions)

Punkt integracji Muse: `muse exec --json` (JSONL), wznowienie przez `exec --session-id`, eksport sesji; CLI odrzuca niezgodny workspace bez jawnego override. Dokumentacja ostrzega, że hooks i MCP działają poza wewnętrznym sandboxem, a umiejętności mogą być wykrywane z osobistych katalogów innych agentów. Potrzebna zewnętrzna izolacja całego runtime i prywatne katalogi. Nie włączać automatycznie bypass approvals. Obsługę interaktywnych approvals oraz przenoszenie browser credential trzeba potwierdzić testem kontraktowym; strona konfiguracji podczas odczytu zgłaszała niedostępność. [Extending and automating](https://dev.meta.ai/docs/muse-code/extending)

Ograniczenie researchu: Google i DuckDuckGo zwróciły CAPTCHA, Bing zwracał wyniki niepasujące do części zapytań. Nazwy i możliwości powyżej oparto na odczytanych stronach oficjalnych, nie na tych wynikach. Dokumentacja Muse Code została odnaleziona przez oficjalny link podany przez użytkownika.

## 4. Docelowe GUI

### Katalog

Przyciski/kategorie rozróżniają **Modele i silniki**, **Dostawcy API**, **Aplikacje agentowe**, **Infrastruktura**. Rozdział dotyczy rodzaju integracji; vendor jest osobnym filtrem. Zachować manifest jako źródło prawdy, rozszerzając jego istniejące typy i walidację o rodzaj integracji oraz możliwości auth/runtime. Nie dodawać kolejnych list `['codex', 'claude-code', ...]` w funkcjach UI i backendu.

Dla aplikacji agentowych kroki: aplikacja → dodaj/wybierz konto → uprawnienia → dostępne nody i sposób wykonania. Instalacja runtime może być współdzielona, konto nie musi być na stałe przypisane do pierwszego wybranego noda.

### Konta i dostęp

Wiersz konta pokazuje nazwę, dostawcę, zweryfikowaną tożsamość/workspace dostawcy, właściciela, metodę logowania, status, dopuszczone nody, aktywne instancje i dostępne limity. Plan/limit, którego dostawca nie raportuje, ma stan „brak danych”.

Akcje: dodaj konto, zaloguj/odnów, nadaj dostęp, odbierz dostęp, wstrzymaj nowe uruchomienia, zakończ aktywne instancje, usuń konto. Użytkownik może zarządzać własnym kontem bez globalnej roli admina; instalowanie runtime na nodzie pozostaje osobnym uprawnieniem.

Rozdzielić uprawnienia: używanie konta, zarządzanie grantami, odnowienie loginu, oglądanie zużycia oraz odczyt konkretnej sesji. Grant użycia konta nie daje dostępu do cudzej historii. Docelowe ukrycie tokenu przed użytkownikiem wymaga wspieranego przez dostawcę brokera uwierzytelniania i izolacji narzędzi od procesu CLI; obecna implementacja tego nie zapewnia i wymaga zaufania do użytkownika. Zmiana właściciela loginu nie może odbywać się niezauważalnie przez „zaloguj ponownie”. Sprawdzić zewnętrzną tożsamość po logowaniu i refreshu.

### Code Studio

Kliknięcie projektu otwiera czat. Wznowić ostatnią dostępną sesję bieżącego użytkownika w tym workspace; jeśli nie ma sesji, pokazać pusty czat z wyborem aplikacji/konta. Tworzenie sesji idempotentne, bez rozpoczęcia płatnego uruchomienia przy samym wejściu do widoku. Nie wznawiać automatycznie cudzej sesji współdzielonego projektu.

Ustawienia mają osobny route/przycisk. Bezpośredni link do sesji nadal wskazuje tę sesję po sprawdzeniu uprawnień. W pasku czatu: aplikacja, konto, model, node, katalog roboczy i stan. Zmiana konta otwiera nową instancję i wątek dostawcy; przeniesienie treści rozmowy jest oddzielną, jawną operacją.

Całość z istniejącymi `tf-*` komponentami i i18n. Rozszerzać brakujące możliwości komponentów w miejscu ich definicji.

## 5. Model danych i jawny wybór konta

Rozszerzyć istniejące repozytoria, bez równoległego przechowywania tego samego sekretu w kilku mechanizmach.

- `provider_accounts`: globalny UUID, organizacja/właściciel, provider, nazwa, zweryfikowany subject i workspace dostawcy, rodzaj auth, stan, revision, wskazanie właściciela poświadczeń. Nazwa/e-mail są etykietą, nie identyfikatorem. Oddzielnie zapisać sposób rozliczania i dopuszczone zastosowania poświadczenia: `api_key` nie oznacza automatycznie pay-as-you-go ani prawa użycia w dowolnej aplikacji (przypadek Muse Code).
- `provider_account_grants`: account, użytkownik/grupa, uprawnienie, opcjonalny workspace/node, ważność, rewizja cofnięcia. Wykorzystać istniejące tożsamości i PEP.
- Rozszerzyć/restrukturyzować `code_agent_credentials` jako magazyn wersjonowanych poświadczeń wskazanych przez account UUID. Unikalność `(org,node,engine)` nie może zastępować konta. Wspólny vault dla API i kont aplikacji, z typowanym rodzajem materiału.
- Rozszerzyć `cli_instances`: account UUID, actor user UUID, execution node, workspace/session, provider session ID, credential lease, runtime version/digest, efektywny auth mode, stan uruchomienia.
- Stan sesji/workspace przechowuje preferencję konta per użytkownik. Istniejąca tabela instalacji/services nadal opisuje runtime; UUID instalacji albo para node+lokalny service ID rozwiązuje kolizje numerycznych ID między nodami.
- Trwałe dzierżawy konta z numerem generacji i idempotency key. To stan koordynacji sekretu i procesu, nie nowa kolejka workflow.

Każdy start wymaga jawnego `account_id`. Serwer rozwiązuje konto, granty i możliwości aplikacji; nie wybiera innego konta ani API na podstawie obecności zmiennych środowiskowych lub wyczerpanego limitu. Przy kilku kontach użytkownik wybiera np. „Codex prywatny” albo „Codex firma X”. Domyślne konto to zapisana preferencja, zawsze ponownie autoryzowana.

## 6. Izolacja: konto, proces i pliki

```text
GUI użytkownika
  │ konto + workspace + sesja + idempotency key
  ▼
Core: tożsamość → grant konta ∩ dostęp workspace ∩ polityka noda
  │ podpisane żądanie wykonania, bez sekretu w GUI
  ▼
Właściciel poświadczeń ── dzierżawa / dozwolony materiał ──▶ Runner na nodzie projektu
                                                              │
                                                  prywatny profil instancji
                                                  jawne cwd / mount / sandbox
                                                              │
                                                              ▼
                                                     oficjalny proces CLI
                                                              │
                                                   zdarzenia → sesja → GUI
```

Trzy osobne obszary:

1. **Instalacja programu**: engine/version/platform, współdzielona tylko do odczytu.
2. **Poświadczenia konta**: vault poza projektem, dostęp kontrolowany; jeden autorytatywny właściciel wersji.
3. **Profil instancji**: historia, cache, ustawienia i pliki robocze prywatne dla użytkownika/sesji/instancji. Nie kopiować całego `~/.codex`, `~/.claude`, `~/.grok` ani systemowego keyringu.

Runner buduje środowisko z allowlisty, zamiast dziedziczyć wszystkie credential/provider/profile/proxy variables procesu TentaFlow. Ustawia prawdziwe katalogi domowe/konfiguracyjne dziecka zgodnie z adapterem i platformą, prywatny temp oraz ograniczone PATH. Sprawdza także ustawienia projektowe, pluginy, hooks i helpery, które mogłyby zmienić źródło uwierzytelnienia lub uruchomić obcy kod. Sekret nie trafia do repo, logów, obrazu, historii terminala ani eksportu workspace.

Osobne foldery pod jednym UID nie chronią przed wzajemnym odczytem. Dla współdzielonego noda wymagany sandbox OS albo kontener/VM z odrębną tożsamością i montowaniem wyłącznie workspace sesji. Dla trybu native opisać i przetestować rzeczywistą granicę izolacji każdej platformy; brak mechanizmu izolacji wyklucza uruchomienie w trybie wieloużytkownikowym.

Sam agent posiadający login subskrypcji może mieć dostęp do tego loginu. Nie obiecywać ochrony przed wyciekiem tego samego tokenu do narzędzi wykonujących się z tymi samymi prawami. Gdzie protokół pozwala, trzymać refresh token w brokerze, a wykonanie narzędzi oddzielić od procesu posiadającego sekret. W przeciwnym razie zakresem zaufania jest całe środowisko sesji, a nie tylko binarka CLI.

## 7. Jednorazowe logowanie i przenoszenie między nodami

„Zaloguj raz” oznacza ponowne używanie i odświeżanie autoryzacji aż do wygaśnięcia, cofnięcia lub wymogu ponownego logowania dostawcy. Nie da się uczciwie zagwarantować logowania raz na zawsze.

Każdy adapter deklaruje sprawdzony sposób mobilności:

- `brokered`: udokumentowany helper/token hook pozwala przekazywać ograniczony materiał, a refresh pozostaje u właściciela.
- `portable`: oficjalny format/cache może być materializowany prywatnie na docelowym nodzie, wraz z kontrolowanym zapisem nowej wersji.
- `node_bound`: logowanie związane z keyringiem/urządzeniem nie ma potwierdzonego sposobu eksportu. UI pokazuje wymagane logowanie na tym nodzie; konto nie udaje przenośnego.

To właściwości różnych integracji, nie automatyczne przełączanie zachowania po błędzie. Nie kopiować refresh tokenów do wszystkich nodów przez CRDT/gossip ani nie replikować katalogów domowych przez współdzielony volume.

Przepływ przenoszenia:

1. Autoryzować użytkownika, konto, docelowy node, workspace i metodę wykonania.
2. Przyznać dzierżawę z TTL, generacją, credential revision oraz idempotency key. Powtórzenie requestu zwraca tę samą instancję.
3. Uruchomić prywatny runtime na nodzie projektu. Materiał przekazać dedykowaną, uwierzytelnioną ścieżką mesh; zaszyfrować dla docelowego odbiorcy. Globalnie publikować tylko dozwolone metadane.
4. Jeżeli CLI odświeża plik tokenu, przejąć nową wersję atomowo z kontrolą oczekiwanej rewizji. Stary writer nie może nadpisać nowej wersji.
5. Przed przeniesieniem zakończyć/quiesce stare procesy, zapisać najnowsze auth i wspierany stan sesji; następnie przekazać wykonanie. Żywy proces nie migruje sam przez sieć.
6. Po zakończeniu wyczyścić prywatne materializacje. Historia projektu zostaje według polityki retencji, niezależnie od poświadczeń.

Początkowo jeden aktywny writer/lease na konto, dopóki adapter nie przejdzie testu współbieżnego refreshu. Różne konta działają równolegle. Współbieżność wielu sesji jednego konta można włączyć dla adapterów ze sprawdzonym brokerowaniem lub poprawnym modelem refreshu; limit dostawcy jest wspólny i kontrolowany na poziomie konta.

**Podział sieci:** fencing token blokuje stare zapisy w TentaFlow, lecz nie unieważnia tokenu już wydanego przez dostawcę. Odłączony runner nadal może mieć dostęp do internetu. Runner musi sam kończyć wykonywanie po utracie dzierżawy; przy niepotwierdzonym zatrzymaniu nie uruchamiać drugiego writera. Pełne odebranie skopiowanego credential wymaga mechanizmu dostawcy, nie samego ACL TentaFlow.

**Awaria właściciela:** bez dostępnego najnowszego credential nie wydawać nowych dzierżaw. Backup szyfrowany zawiera również materiał potrzebny do odszyfrowania w kontrolowanym recovery; samo skopiowanie ciphertext między nodami z różnymi settings keys nie wystarcza. Automatyczny failover tylko z autorytatywną rewizją i skutecznym fencingiem. Nie dokładamy systemu konsensusu w pierwszym etapie.

Mesh wykorzystuje istniejące CBOR i [assertion.rs](../tentaflow-core/src/code_studio/assertion.rs): rozszerzyć wzorzec o konto/instancję/operację, audience noda, expiry, nonce, revisions i powiązanie treści żądania. Odbiorca sprawdza aktualne granty. Podpis peer identity bez uprawnień użytkownika nie wystarcza.

## 8. Właściwy katalog i współpraca nad projektem

Wymagany niezmiennik: **edytor, terminal, przegląd zmian i aplikacja agentowa pracują na tych samych plikach danej sesji**.

- Dla projektu zarządzanego przez Code Studio zachować istniejący worktree per sesja. To nie przypadkowy katalog: jego zawartość musi być tym, co pokazuje edytor. Pokazywać w UI zarówno projekt, jak i efektywny katalog/branch sesji.
- Wymagany jest tryb pracy bezpośrednio w istniejącym katalogu użytkownika, bez klonowania, przenoszenia ani zastępowania go worktree. Rejestracja katalogu jest osobnym uprawnieniem administratora noda; użytkownik wybiera przyznany zasób po identyfikatorze. Obsłużyć katalogi Git i non-Git; funkcje Git dostępne tylko tam, gdzie istnieje repozytorium. Szczegóły w sekcji 8.1.
- Dla wspólnego katalogu z zapisem: jeden writer albo wspólna sesja współpracy z jasnym właścicielem. Dwie niezależne sesje z równoległym zapisem wymagają osobnych worktree. Izolacja kont nie rozwiązuje konfliktów plików.
- Docker: backend rozwiązuje `host_session_root → /workspace`, ustawia cwd `/workspace` i przekłada ścieżki zdarzeń z powrotem do przestrzeni projektu. Nie wysyła ścieżki hosta jako cwd do kontenera.
- Native: cwd = zweryfikowany root sesji. Kontrola symlinków, traversal i podmiany katalogu między walidacją a otwarciem oparta na istniejących helperach filesystem/sandbox, nie wyłącznie stringowym `starts_with`.
- Node wykonania ma dostęp do konkretnej wersji plików. Przeniesienie konta nie przenosi niezatwierdzonych plików. Zmiana noda projektu wymaga osobnego checkpointu i transferu plików/stanu; nie można mapować tej samej ścieżki tekstowej na przypadkowy katalog innego hosta.
- Provider session ID jest powiązany z kontem, actor, workspace, CLI version i instancją. Nie używać ogólnego „continue latest” z katalogu. Resume/fork między kontami jest zabronione bez jawnego utworzenia nowej rozmowy.

### 8.1 Istniejące katalogi: administrator przyznaje zasób, runner egzekwuje dostęp

Wymaganie użytkownika: kontynuowanie pracy np. w `/Users/critix/repos/rust/TentaFlow` na Macu albo `/mnt/d/repos/TentaFlow` na konkretnym nodzie Linux/WSL. Te ścieżki wskazują faktyczne pliki, także z niezatwierdzonymi zmianami. TentaFlow nie tworzy ich kopii jako warunku otwarcia projektu.

**Dwa niezależne zabezpieczenia:**

1. Administrator noda rejestruje dokładny katalog i nadaje użytkownikom dostęp do tego zasobu. Wspólny UID usługi nie pozwala ustalić właściciela na podstawie samego `stat`; autorytatywne jest przypisanie administratora. Sam użytkownik może zgłosić ścieżkę do zatwierdzenia, ale takie zgłoszenie nie daje mu przeglądania hosta ani informacji o cudzych plikach.
2. Cały runtime użytkownika (CLI, terminal, testy, language servers, hooks i MCP) działa w egzekwowanej izolacji. Sam whitelist w GUI/API nie zatrzyma `cat /cudzy/katalog/plik` uruchomionego przez shell. W [sandbox.rs](../tentaflow-core/src/code_studio/sandbox.rs), okolice 58 i 1020, `trusted_native` jawnie nie daje izolacji OS. Ten tryb nie może obsługiwać odseparowanych użytkowników współdzielonego noda.

Proponowany zasób `workspace_locations`: UUID, node UUID, kanoniczna ścieżka hosta, etykieta, tożsamość katalogu/filesystemu gdzie dostępna, stan, revision, kto zatwierdził. Osobne granty lokalizacji wskazują użytkownika/grupę oraz read/write/execute. Rozszerzyć istniejący model workspace o rodzaj storage `managed` albo `attached` i referencję do lokalizacji; nie zmieniać znaczenia istniejących ścieżek managed repo.

Przepływ GUI:

```text
Administrator noda → Udostępnij katalog
  node: Mac Critix
  katalog: /Users/critix/repos/rust/TentaFlow
  etykieta: TentaFlow — istniejące repo
  dostęp: Critix, odczyt + zapis + wykonywanie
                         │
                         ▼
Użytkownik → Code Studio → Otwórz istniejący katalog
  wybiera tylko przyznane lokalizacje
  wybiera konto agenta → otwiera czat
```

Użytkownik może sam przełączać się między już przyznanymi lokalizacjami. Nie potrzebuje zgody admina przy każdym wejściu. Nie może zmienić fizycznej ścieżki zasobu ani rozszerzyć grantu; nawet właściciel workspace nie nabywa przez to prawa udostępniania kolejnych katalogów hosta. Zmiana katalogu tworzy nowy kontekst sesji; nie podmienia mountu pod działającym agentem.

Domyślnie administrator udostępnia dokładny projekt. Opcjonalny grant nadrzędnego katalogu daje dostęp do całej jego zawartości, więc nie wolno obiecywać ukrycia cudzych projektów poniżej tego grantu. Katalogów użytkowników nie rozdzielać tylko filtrami listy plików wewnątrz wspólnego mountu.

Każde API plików, wyszukiwania, indeksu, Git, terminala, artefaktów i agenta rozwiązuje workspace/location ID oraz sprawdza grant bieżącego użytkownika. Żądanie nie wskazuje arbitralnego host cwd. Zdalny node sprawdza grant i revision lokalizacji, nie tylko podpis/zaufanie peerowi. Cache, indeks i eksport nie mogą ujawnić zawartości po odebraniu dostępu.

Przykład wykonania: dokładny katalog hosta bind-mounted read-write jako `/workspace` w prywatnym kontenerze sesji. To te same pliki, nie kopia: edycja w zewnętrznym IDE jest widoczna w Code Studio, a zapis agenta w istniejącym repo. Mapowanie UID/GID ma zachować możliwość zapisu użytkownika hosta; nie uruchamiać `chown -R` na repo. Montować wyłącznie przyznany katalog, prywatny profil i jawnie dozwolone zasoby. Bez całego `/Users`, `/mnt/d`, `/`, host PID namespace, uprzywilejowania, socketu Docker i interfejsów sterujących TentaFlow. Prywatny proces nie dostaje prawa wywoływania hostowych operacji Core z uprawnieniami serwisu.

Tożsamość katalogu i mount trzeba weryfikować przy otwarciu. Zmiana symlinku, rename/podmiana rootu, niedostępny dysk lub podmontowanie innego filesystemu blokują start do ponownej walidacji. Nie tworzyć pustego katalogu w miejscu odłączonego dysku. Rozwiązywać ścieżki względem otwartego rootu przy użyciu istniejących bezpiecznych helperów; uwzględnić hardlinki i zagnieżdżone mounty, które mogą udostępniać dane spoza projektu. Sama kanonizacja przed późniejszą operacją nie usuwa wyścigu TOCTOU. Pliki współdzielone hardlinkiem z cudzym zasobem wymagają odrzucenia lub jawnego zakresu wspólnego dostępu.

Git w attached repo wymaga osobnego kontraktu: zachować oryginalne `.git`, branch, index i niezacommitowane zmiany; nie automatycznie init/checkout/reset/clean. Broker Git nie może wykonywać projektowych hooks/helperów jako nieograniczony użytkownik hosta. Istniejąca zasada ukrywania metadanych Git w managed worktree nie może zostać bez analizy przeniesiona na attached repo. Git worktree/submodule z metadanymi poza przyznanym rootem wymaga jawnego udostępnienia niezbędnego zasobu albo odmowy operacji Git, nigdy dostępu do dowolnego katalogu nadrzędnego.

Współbieżność: jedna sesja zapisująca TentaFlow na fizyczną lokalizację, również gdy została zarejestrowana pod kilkoma aliasami; nakładające się rooty wykrywać i blokować przy konfliktującym zapisie. Praca równoległa w oddzielnym worktree jest opcją użytkownika, nie przymusowym przeniesieniem. Zewnętrzne IDE nie respektuje blokady TentaFlow: watcher i porównanie wersji pliku przed zapisem chronią operacje edytora/API, ale arbitralne komendy shell mogą nadal ścigać się z edytorem. Nie obiecywać pełnej transakcyjności przy jednoczesnym zewnętrznym zapisie; wykrywać zmiany i nie nadpisywać ich automatycznym przywracaniem snapshotu.

Cofnięcie grantu zatrzymuje aktywne instancje/terminale, odcina API i usuwa mounty. **Odłączenie lub usunięcie workspace attached nigdy nie usuwa katalogu użytkownika.** Sprzątać tylko dane zarządzane przez TentaFlow. Zewnętrzny katalog pozostaje przypisany do konkretnego noda; przeniesienie konta nie oznacza przeniesienia katalogu.

Testy dodatkowe: próba otwarcia cudzego location ID i surowej ścieżki, `cd ..` i absolutne ścieżki z terminala/agenta, symlink/hardlink/root swap, zagnieżdżone mounty, obcy index/search cache, grant revoked podczas działania, dwa aliasy tego samego repo, równoczesne IDE, brak dysku `/mnt/d`, błędne UID plików i usunięcie workspace bez naruszenia plików hosta. Test na katalogu non-Git oraz repo z dirty index i plikami untracked.

### 8.2 Lekki runtime wbudowany w produkt, bez obowiązkowego Dockera

Preferencja użytkownika: minimalny narzut i rozmiar, uruchomienie oficjalnego CLI z istniejącym katalogiem, obsługa Linux/macOS/Windows, zarządzanie przez TentaFlow. Rekomendowany pierwszy kierunek: **sandbox procesowy z backendem właściwym dla OS**, a nie VM dla każdej sesji. Wspólny kontrakt bezpieczeństwa i GUI, różna implementacja. Nie obiecywać identycznych możliwości ani działania bez przygotowania systemu na każdej platformie.

| Mechanizm | Ustalenie ze źródła | Zastosowanie w TentaFlow |
|---|---|---|
| Bubblewrap | Linux, prywatne namespaces, wybiórcze mounty, kontrola procesów/IPC/network; politykę definiuje caller. | Kandydat podstawowy na Linux: minimalne read-only runtime/toolchain i dokładny projekt read-write; bez daemona Docker i obrazu pełnej dystrybucji jako konieczności. [Źródło](https://github.com/containers/bubblewrap) |
| Landlock | Linux, ograniczanie uprawnień procesów również bez uprawnień administratora. | Dodatkowa warstwa ograniczeń; sam nie zastępuje prywatnego widoku filesystemu, izolacji IPC/procesów i polityki sieci. Sprawdzać wymagane możliwości kernela. [Źródło](https://landlock.io/) |
| Anthropic Sandbox Runtime (SRT) | Biblioteka/CLI, research preview. README opisuje Seatbelt/sandbox-exec na macOS, bubblewrap na Linux oraz dedykowanego użytkownika i WFP/ACL na Windows. Domyślnie odczyt filesystemu jest szeroki; zapis ograniczony. | Pierwszy gotowy kandydat do testu porównawczego, nie automatycznie gotowa izolacja tenantów. Przypiąć wersję i zweryfikować implementację/dystrybucję wszystkich trzech backendów. Wspólny windowsowy użytkownik `srt-sandbox` nie gwarantuje sam izolacji sesji, jeśli jego ACL sumują dostęp do kilku projektów. [Źródło](https://github.com/anthropic-experimental/sandbox-runtime) |
| AppContainer | Windows: kontrola zasobów plików, sieci, procesów i tożsamości aplikacji. | Kandydat native Windows: osobna tożsamość sandboxa, granty katalogów, ograniczone tokeny i lifecycle przez Job Object. Wymaga prototypu zgodności CLI, shell i kompilatorów; Job Object sam nie ogranicza filesystemu. [Źródło](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation) |
| Firecracker | Mikro-VM korzystające z Linux KVM, z własnym guest kernel/rootfs. | Opcja silniejszej granicy na Linux, nie jeden natywny runtime na trzy OS. [Źródło](https://github.com/firecracker-microvm/firecracker) |
| libkrun | Biblioteka z C API, napisana w Rust; Linux/KVM i macOS ARM64/HVF. README wymaga izolacji VMM po stronie hosta; sam virtio-fs nie ogranicza dostępu do wskazanego katalogu. | Kandydat do osobnego eksperymentu VM, nie gotowe rozwiązanie Windows ani bezpieczne mountowanie katalogu bez host sandboxa. Gałąź 2.0 ma rozwijane API; prototyp opierać na stabilnym wydaniu. [Źródło](https://github.com/containers/libkrun) |

Na macOS sprawdzić Seatbelt w przypiętym runtime jako kandydat do native process sandbox. To ograniczenia dostępu do rzeczywistej ścieżki, nie linuxowy bind mount; `cwd` może pozostać `/Users/critix/repos/rust/TentaFlow`. Zweryfikować ograniczenia API i wspierane wersje macOS, procesy potomne, IPC/Keychain/Apple Events oraz dostęp do konta CLI. Nie włączać opcji umożliwiających uruchomienie aplikacji poza sandboxem. Na Linux `/workspace` może być prywatnym mountem tych samych plików. Windows ma trzeci model dostępu do ścieżek. UI pokazuje rzeczywistą ścieżkę i nie zakłada `/workspace` na wszystkich OS.

Zmiana rekomendacji nie oznacza powrotu do `trusted_native`: ten tryb nadal nie zapewnia izolacji. Istniejące `sandbox.rs` i `exec/{unix,windows}.rs` należy rozszerzyć w miejscu o egzekwowane backendy. Nie implementować od zera własnego hypervisora ani samodzielnie projektowanego mechanizmu bezpieczeństwa kernela. Wybrać utrzymywane komponenty, spiąć je z istniejącym PEP, grantami lokalizacji i gatewayem. Jeśli runtime nie spełnia wymaganego profilu, odmówić startu; brak automatycznego uruchomienia bez izolacji.

„Wbudowane w TentaFlow” oznacza z punktu widzenia użytkownika brak osobnej obsługi Dockera: instalator dostarcza sprawdzone helpery/runtime, TentaFlow uruchamia je i zarządza ich życiem. Nie musi to oznaczać jednej statycznej binarki. SRT wnosi zależności Node/TypeScript i helpery OS, bubblewrap osobny mały program; trzeba zmierzyć koszt dystrybucji i ocenić licencje. Windows może wymagać jednorazowej konfiguracji kont/ACL/WFP przez admina. Nie uruchamiać całego Core z podwyższonymi prawami tylko dla sandboxa; ewentualny broker administracyjny ma wąskie, autoryzowane operacje.

Sandbox procesowy korzysta z systemu hosta i nie uruchamia guest kernel. Oczekiwany mniejszy koszt startu/pamięci niż VM to hipoteza do pomiaru w naszym workloadzie, nie benchmark. Mikro-VM wymaga także RAM gościa, obrazu systemu, toolchaina i ścieżki udostępniania plików; mała binarka VMM nie jest rozmiarem całego środowiska. Linuxowy guest na Macu nie wykona natywnego builda Xcode/MLX, więc nie może być przezroczystym zamiennikiem native dla wszystkich projektów.

Rozszerzenie etapu 0: prototyp z prawdziwym CLI na wszystkich OS, test odczytu/zapisu cudzych katalogów, IPC i procesów, terminala/PTY, child sandbox nesting, auth/refresh oraz istniejącego projektu. Porównać cold/warm startup, narzut pamięci i rozmiar instalacji bez pamięci samego CLI, operacje na wielu plikach, watcher, build i cleanup. Włączyć do produktu dopiero backend przechodzący wspólny kontrakt. Mikro-VM pozostaje oddzielnym profilem mocniejszej izolacji, bez obietnicy jednolitej implementacji Windows/macOS/Linux.

## 9. Plan wykonania

### Przyjęty kierunek i wynik pierwszej próby (2026-09-06)

Użytkownik wybrał sandbox procesowy. Brak dostępnej maszyny Windows nie blokuje prac nad kontraktem i backendem macOS; Windows pozostaje niezweryfikowany i nie może deklarować gotowej izolacji. Linux również wymaga osobnego wykonania testów. Nie należy pytać ponownie o wybór między Dockerem a sandboxem procesowym.

Wykonano próbę SRT 0.0.75 na macOS 26.6.2 ARM64, z rzeczywistym Codex CLI 0.153.4 (`--version`, bez logowania). Powtarzalny skrypt: `scripts/test-process-sandbox.py`. Szczegóły i ograniczenia: [process-sandbox-validation.md](process-sandbox-validation.md). Wynik: 16 z 18 kontroli zakończonych powodzeniem, w tym kontrole uruchomienia; dwa wymagania izolacji nie zostały spełnione: odczyt i zapis przez istniejący hardlink w dozwolonym projekcie. Cały test zwraca błąd. To próba komponentu, nie integracja runtime z Code Studio ani potwierdzenie bezpieczeństwa wieloużytkownikowego.

Kolejność realizacji runnera:

1. Zdefiniować dopuszczenie istniejącego katalogu: admin nadaje grant do konkretnej lokalizacji noda, backend waliduje tożsamość katalogu oraz aliasy. Wykrycie plików z wieloma dowiązaniami oznacza odmowę podłączenia do czasu jawnego rozwiązania problemu; nie usuwać ani nie przepisywać plików użytkownika automatycznie. Sam skan przed startem nie zamyka wyścigu z procesem hosta, więc nie jest wystarczającym dowodem izolacji.
2. Spiąć egzekwowaną politykę z istniejącymi `SandboxManager`/`SandboxLease` i planami exec oraz PTY. Ten sam kontrakt musi objąć oddzielną ścieżkę CLI bridge i wszystkie jego procesy potomne. Nie udostępniać trybu jako bezpiecznego, jeśli jedna ścieżka omija policy.
3. Przypinać lease do aktora, konta, sesji i przyznanego katalogu; tworzyć prywatny profil i minimalne środowisko. Instalacje CLI udostępniać tylko do odczytu. Polityki toolchaina nie mogą udostępniać współdzielonych, zapisywalnych katalogów z sekretami.
4. Sprawdzić realne logowanie, refresh, PTY, narzędzia, anulowanie całego drzewa procesów oraz dwie równoległe sesje. Dopiero po zamknięciu testów izolacji włączyć backend macOS w produkcie. Brak sprawnego backendu oznacza odmowę uruchomienia, bez przejścia do `trusted_native`.
5. Wykonać tę samą macierz na Linux i później na dostępnej maszynie Windows; dostępność backendu raportować per node. Nie uznawać kompilacji ani mocków Windows za weryfikację granicy bezpieczeństwa.

| Etap | Zmiana | Warunek zakończenia |
|---|---|---|
| 0. Kontrakty integracji | Przypiąć wersje Codex/Claude/agy/grok/muse; zbadać login, status tożsamości, config isolation, refresh, stream, resume, cancel, approvals, cwd, platformy i Docker. Rozbieżność wersji manifestów i notatek Phase 0B rozstrzygnąć na realnych binarkach. | Macierz udokumentowanych i zmierzonych możliwości. Brak potwierdzonego eksportu auth oznaczony jako ograniczenie. |
| 1. Konta i autoryzacja | Migracje kont/grantów/credential revision; jawny account ID; actor i assertion na każdej operacji; odczyt sesji tylko dla uprawnionych; authenticated bridge IPC. | Dwa konta Codex nie nadpisują się; użytkownik nie może odczytać ani prowadzić cudzej sesji. |
| 2. Runner i izolacja | Prywatne profile/UID lub sandbox; env allowlist; rozdzielenie instalacji i stanu; jawne mapowanie cwd; auth.status dla wybranego konta. | Dwa różne konta działają równocześnie na jednym nodzie i w odrębnych kontenerach bez współdzielenia loginu, historii i procesu. |
| 3. Mobilność kont | Właściciel credential, lease/generacje, atomowy refresh, transfer/recovery, cleanup, cofnięcie dostępu. | To samo przenośne konto przechodzi A → B → Docker bez nowego loginu w warunkach testowych; awaria/split-brain nie tworzy drugiego writera. |
| 4. Katalog i zarządzanie | Aplikacje agentowe, Konta i dostęp, wiele kont, ownership, granty, statusy, zużycie. Wizard dostaje konkretny ID wdrożenia, nie wyszukuje pierwszego po engine. | Cały scenariusz dodania własnego/drugiego konta i nadania dostępu wykonalny z GUI. |
| 5. Code Studio | Chat-first, osobne ustawienia, selektor konta, trwałe przypisanie instancji, rzeczywisty stream; istniejący worktree i jawny tryb katalogu lokalnego. | Otwórz projekt → czat; agent widzi i zmienia dokładnie pliki pokazane w edytorze. Test native, Docker i mesh. |
| 6. Nowi dostawcy | Dodać kompletne adaptery Antigravity, Grok Build i Muse Code, właściwe manifesty/instalatory/platformy; oddzielnie Meta Model API po walidacji jego schematu. | Każda udostępniona integracja przechodzi ten sam zestaw kontraktowy; GUI pokazuje rzeczywistą dostępność mobilności i funkcji. |

Etap 0 rozpoczyna research wszystkich dostawców, aby ograniczenia agy/Grok/Muse wpłynęły na wspólny model przed implementacją. Etap 6 nie może być samym dodaniem kafelków.

Build/release obejmuje natywne instalatory i obrazy bridge dla deklarowanych OS/architektur, kontrolowane aktualizacje z przypiętych wersji/digestów, wymagane zależności (obecnie npm dla części CLI), generację manifestów i wasm glue. Nie aktualizować binarki podczas działającej sesji; po aktualizacji ponownie sprawdzić kontrakt. Nie ręcznie edytować wygenerowanego JS manifestu.

### Migracja istniejących danych

Zatrzymać aktywne procesy bridge przed migracją mutowalnych profili. Utworzyć explicit account z istniejącego loginu tylko wtedy, gdy da się ustalić tożsamość; nie zgadywać właściciela na podstawie noda. Jeśli nie da się go ustalić, zachować zaszyfrowany materiał i oznaczyć konto jako wymagające przypisania, bez nowych grantów.

Kilka services wskazujących ten sam profil nie staje się kilkoma kontami. Wspólny stan importować raz, mapowania services zachować osobno. Przenieść aktywne dane atomowo z dziennikiem migracji; kopię do recovery traktować jak sekret. Runtime po migracji korzysta wyłącznie z nowego modelu: usunąć wybór konta po engine i globalnym loginie, zaktualizować callerów, testy i schematy w tym samym etapie. Stare i nowe formaty nie działają równolegle jako fallback.

Osobno rozstrzygnąć istniejący bezpośredni backend subskrypcyjny OpenAI: zachować produktową możliwość tylko na zweryfikowanym kontrakcie, z tym samym account vault i koordynacją refresh; jeśli nie ma wspieranego kontraktu, przeprowadzić jawną migrację do CLI. Nie utrzymywać drugiej ukrytej kopii tego samego loginu.

## 10. Testy, awarie i wydajność

```text
GUI: dodaj konto / wybierz / otwórz projekt
  ├─ UI: konkretny deployment, chat-first, brak cudzych sesji
  ▼
Core: identity + account grant + workspace grant
  ├─ negatywne: obce UUID, usunięty grant, replay, zły audience
  ▼
Account lease / refresh / state persistence
  ├─ restart, równoległy refresh, stary writer, partition, owner offline
  ▼
Runner: profil + env + filesystem + cwd
  ├─ native, Docker, mesh, symlink, mount, obce HOME/klucze/procesy
  ▼
Prawdziwe CLI: login → turn → tools → stream → resume → cancel
  └─ plik kontrolny w projekcie; brak zapisu/odczytu w sąsiedniej sesji
```

Obowiązkowa macierz:

1. Dwa konta tego samego użytkownika i dwa konta różnych użytkowników; native–native, Docker–Docker i native–Docker.
2. Ten sam provider account dodany ponownie: wykrycie tej samej tożsamości/entitlement, brak sztucznego mnożenia limitów.
3. Zwykły user nie odczytuje cudzych login events, historii, modeli zależnych od konta ani sesji; nie odpowiada na cudze approvals.
4. Zmiana konta nie przenosi provider session ID; współdzielenie projektu nie współdzieli domyślnie loginów.
5. CLI z celowo ustawionym obcym API key/HOME/profile w środowisku rodzica nadal uruchamia wyłącznie jawnie wybrane konto.
6. Konto loguje się na A, przechodzi na B i Docker; refresh, restart oraz powrót na A zachowują najnowszą wersję. Osobny test Keychain macOS vs Linux credentials.
7. Dwa żądania startu, duplikaty mesh, zerwane połączenie, owner offline, utrata dzierżawy, crash przy zapisie tokenu, pełny dysk.
8. Cofnięcie grantu zatrzymuje nowe uruchomienia i egzekwuje ustaloną politykę istniejących; brak możliwości kontynuacji przez bezpośredni bridge port.
9. Agent odczytuje plik kontrolny i zapisuje zmianę widoczną w edytorze; host/container mapowanie działa w obie strony, symlink nie ucieka poza grant.
10. Dwie sesje jednego projektu mają osobne worktree; jawny shared-directory mode blokuje równoległego writera. Transfer noda obejmuje niezatwierdzone pliki lub odrzuca zmianę noda.
11. Widok projektu otwiera własny czat, settings route nie tworzy sesji; wejście/reload nie uruchamia płatnego zapytania. Brak konta/provisioning/offline pokazuje stan w powierzchni czatu.
12. Instalacja/upgrade przypiętych CLI, drift protokołu, timeout, cancel zabijający potomków, wznowienie po crashu, odtworzenie migracji.
13. Muse Code: credential subskrypcji nie trafia do ogólnego API ani innego harnessu; dodatkowy API key nie zastępuje wybranego loginu. Hooks/MCP/foreign skills nie omijają izolacji zewnętrznego runnera. Niezerowy wynik testów projektu nie jest utożsamiany z samym kodem zakończenia procesu agenta.

Wykorzystać istniejące testy `code_studio_lifecycle_e2e`, `code_harness_flow_e2e`, `critic_code_studio`, scenariusze Playwright `code-studio-harness`, `code-studio-delegation`, `code-studio-projects` i testy bridge. Dodać testy rzeczywistych binarek na dedykowanych kontach testowych; mock protokołu nie dowodzi zachowania keyringu, refreshu i izolacji OS.

Wydajność: wspólne read-only instalacje CLI, procesy tworzone na żądanie, limity liczby instancji/PTY i kolejka na zajętym koncie, TTL tylko dla bezpiecznych cache. Cache modeli/usage kluczowany kontem i credential revision, nie samym engine. Zachować brak płatnego model discovery przy otwarciu GUI. Ograniczyć rozmiar i retencję zdarzeń oraz dodać backpressure strumieni; ACL rewidować przy cofnięciu grantów. Mierzyć czas startu sesji, czas wznowienia, liczbę procesów/osieroconych procesów i błędy refresh bez logowania sekretów. Nie podawać fikcyjnych procentów wykorzystania subskrypcji.

## 11. Decyzje produktowe i przyjęte założenia

Rekomendowany komplet do dalszych prac, z przyjętym przez użytkownika kierunkiem sandboxa procesowego:

1. „Aplikacje agentowe” + „Konta i dostęp”; subskrypcja jako metoda rozliczania.
2. Konto osobiste prywatne; granty organizacji jawne i niezależne od dostępu do historii projektu.
3. Jeden autorytatywny właściciel credential, kontrolowany transfer zamiast globalnej synchronizacji tokenów.
4. Równoległość między różnymi kontami od początku; równoległość jednego konta zgodna z przetestowanymi możliwościami adaptera.
5. Worktree jako standard izolowanych sesji; jawne podłączenie istniejącego katalogu dla pracy bezpośredniej, z kontrolą równoczesnego zapisu.
6. Chat-first jako wejście do projektu; ustawienia otwierane jawnie.

Alternatywa „jedno konto = jeden stale działający kontener” jest prostsza operacyjnie, ale nie rozwiązuje izolacji historii wielu sesji ani pracy w wielu projektach bez dodatkowego runtime. Alternatywa „jeden wspólny HOME i przełączanie loginu” koliduje wprost z wymaganiem niezależnych kont. Centralne uruchamianie całego CLI obok vault wymagałoby dodatkowego zdalnego systemu plików/narzędzi; na pierwszą implementację wybrać uruchomienie na nodzie projektu i mobilność ograniczoną kontraktem dostawcy.

Status przeglądu: analiza i plan gotowe z ograniczeniami. Izolacja wieloużytkownikowa, przenośność loginów Antigravity/Muse i odporność refreshu nie są jeszcze potwierdzone testem wykonawczym. Ten dokument nie stanowi deklaracji, że istniejący produkt ma te właściwości.
