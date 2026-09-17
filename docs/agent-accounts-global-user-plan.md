# Konta agentów CLI: globalne i użytkownika, przypisanie w Agents, automatyczna obecność na nodach

Data: 2026-09-17. Plan następczy do [agent-accounts-code-studio-plan.md](agent-accounts-code-studio-plan.md).
Stan wyjściowy ustalono czytaniem kodu, bez uruchamiania; numery linii są wskazówką, nie dowodem.
Mockupy wszystkich ekranów: `mockups/agent-accounts-20260917/index.html` (A01–A04, N01, U01, G01, C01, C02).

Zasady nadrzędne ustalone z właścicielem:

- **Wszystko z GUI.** Żadna czynność (dodanie konta, logowanie, klucz API, dostęp, runtime na nodzie) nie
  wymaga protokołu „z ręki", pliku ani CLI.
- **Logowanie = prawdziwe logowanie aplikacji.** CLI działa w tle na nodzie, jego link wraca do GUI, użytkownik
  otwiera go w nowym oknie, a kod wraca przez GUI do aplikacji (Claude) albo GUI pokazuje kod urządzenia
  (Codex/Grok/Muse). Powstały token/plik poświadczenia jest tym, co TentaFlow przechowuje i przenosi między
  nodami — identycznie dla konta globalnego i konta użytkownika.
- **Bez blokady konta.** Wiele równoległych instancji na jednym koncie, jak przy zwykłej pracy z CLI.
- **Czego agent/adapter nie umie — dorabiamy**, nie wycinamy funkcji.
- **Istniejące ekrany zostają.** Agents (`mockups/agenci-20260822`) i konsola Code Studio
  (`mockups/code-studio-20260814`) są wiążące; konta to wyłącznie dopisana sekcja „Silnik agenta / Konto" w
  „Model i generowanie", chip konta przy sub-agencie oraz istniejący wzorzec „agent pyta" dla połączenia konta.

## 1. Stan obecny — jak dziś działają konta

Nie ma pojęć „konto globalne" i „konto użytkownika". Są dwa niezależne mechanizmy:

| | Klucz API organizacji | Konto agenta (login/subskrypcja) |
|---|---|---|
| Gdzie | `code_agent_credentials` w `code_studio.db`, klucz `(org, node, engine)` | pliki w `keys/coding-agents/accounts/<uuid>/` + wiersz `services` (`transport=agent_rpc`) |
| Silniki | tylko codex, claude-code | wszystkie cztery |
| Kto konfiguruje | admin, wyłącznie protokołem — GUI nie istnieje | admin: wdrożenie serwisu z katalogu na konkretnym nodzie + logowanie |
| Co może użytkownik | nic; klucz jest używany niejawnie, gdy sesja nie wskazała konta | używać konta, jeśli admin nadał grant (`coding_agent_account_grants`); własne sesje, brak wglądu w cudze |
| Między nodami | nigdy | ręczne „Przenieś konto" (admin, codex/claude, źródło kasuje kopię) |

Konsekwencje:

- Konto = deployment serwisu na jednym nodzie. Użytkownik nie może sam dodać ani zalogować swojego konta
  (`auth.start` wymaga admina, `coding_agent.rs:291`).
- Jedno konto = jedna aktywna sesja (`account_busy`, bridge `main.rs:1336`). Konto współdzielone przez zespół
  blokuje się na pierwszym użytkowniku.
- Wybór konta następuje przy otwarciu sesji Code Studio (`session.agent_service_id`) i **zastępuje cały harness**
  płaskim flow z jednym `delegate_cli` (`dispatch/code_studio.rs:2994`). Agent z ekranu Agents ma tylko pole
  `model`; nie da się mu przypisać silnika CLI ani konta.
- Code Studio widzi wyłącznie konta z lokalnej tabeli `services` noda workspace'u (`require_local`).
- Token odświeżony przez CLI nie jest adoptowany — konto przechodzi w „zaloguj ponownie" (`main.rs:1720`).

## 2. Model docelowy

### 2.1 Konto a serwis

Z punktu widzenia użytkownika nic się nie zmienia: konta nadal żyją w Services, jako zakładka „Konta agentów".
Różnica jest wyłącznie w danych: wiersz `services` jest lokalny dla jednego noda i nie jest synchronizowany,
więc konto zapisane w nim nie może „być" na kilku nodach. Dlatego tożsamość konta przechodzi do własnej,
synchronizowanej tabeli, a `services` opisuje już tylko runtime zainstalowany na danym nodzie.

Rozdzielić to, co dziś zlepia wiersz `services`:

- **Runtime silnika na nodzie** — instalacja CLI + bridge. Zostaje w `services`/katalogu, instalowany raz na node
  (admin albo automatycznie przy pierwszym użyciu). Nie niesie tożsamości.
- **Konto** — nowa encja `provider_accounts` w głównej bazie, synchronizowana:
  `id (uuid), org_id, engine_id, scope ('global'|'user'), owner_user_id (NULL dla global), display_name,
  auth_kind ('subscription'|'api_key'), provider_subject, state, credential_revision, home_node_id, created_by`.
  Unikalność `(owner_user_id, engine_id, provider_subject)`; konto użytkownika ma `is_default` per `(user, engine)`.
- **Poświadczenie** — `provider_account_credentials(account_id, revision, material_enc, fingerprint, written_by_node)`.
  Jedno źródło prawdy dla obu dzisiejszych mechanizmów: klucz API organizacji staje się kontem
  `scope=global, auth_kind=api_key`. Tabela `code_agent_credentials` i logika „vault row → OrgCredential"
  znikają w tym samym etapie (bez ścieżki równoległej).
- **Granty** — `provider_account_grants(account_id, subject_kind 'user'|'group'|'org', subject_id)`; dotyczą
  tylko kont globalnych. Konto użytkownika jest używalne wyłącznie przez właściciela — bez wyjątku dla admina.
- Bridge jest uruchamiany przez Core **na żądanie per (konto, node)**, nie jako deployment. Katalog
  `accounts/<uuid>/` i blokada `account.lock` zostają.

### 2.2 Kto co robi

- **Administrator**: Services → *Konta agentów* (A01) → dodaje konto globalne oknem logowania A02
  (link → kod → weryfikacja tożsamości). „Klucz API" to tylko drugi, opcjonalny sposób w tym samym kreatorze —
  pole na klucz zamiast logowania, dla rozliczenia za tokeny; dziś istnieje wyłącznie w protokole, bez GUI.
  Nadaje dostęp (A04), widzi sesje i nody (A03), instaluje runtime (N01).
- **Użytkownik**: Profil → *Moje konta agentów* → „Połącz Claude Code / Codex / Grok / Muse". Logowanie kodem
  urządzenia wykonuje się na nodzie zdolnym do sandboxa (bieżącym albo wskazanym przez mesh, gdy bieżący to
  np. telefon). Może odłączyć konto i wybrać domyślne (U01). Drugi punkt wejścia: pierwsze użycie agenta
  z `mode=user` w Code Studio — karta w czacie otwiera to samo okno logowania, a wstrzymana tura wznawia się
  sama po weryfikacji konta (C01). Admin widzi, że konto istnieje, nie widzi materiału i nie
  może go użyć.

### 2.3 Przypisanie w Agents

Agent dostaje pole `runtime_json` (append-only, `serde(default)`, synchronizowane z tabelą `agents`):

```json
{"kind":"llm"}
{"kind":"cli","engine":"claude-code","model":"…","account":{"mode":"global","account_id":"<uuid>"}}
{"kind":"cli","engine":"claude-code","model":"…","account":{"mode":"user"}}
```

- `mode=global` — zapis agenta wymaga admina i sprawdza, że konto jest globalne, tego silnika i aktywne.
- `mode=user` — konto rozstrzyga się **przy każdym uruchomieniu** z `AgentPrincipal.user_id` przebiegu
  (dzieci dziedziczą principal, więc podagent działa na koncie tego, kto prowadzi sesję, nie autora agenta).
- Jedna funkcja `resolve_run_account(agent, principal, node)` — jedyne miejsce decyzji, używane przez
  `delegate_cli`, czat `/v1` i konsolę. Zwraca konto albo typowaną odmowę:
  `user_account_missing{engine}` (czat pokazuje CTA „Połącz konto"), `account_grant_denied`,
  `account_relogin_required`, `node_runtime_unavailable`. **Nigdy nie podmienia trybu**: brak konta użytkownika
  nie przełącza na globalne ani na klucz API.
- Harness agenta `kind=cli` to istniejący blok `delegate_cli`; jego `service_id` z konfiguracji znika, konto i
  runtime przychodzą z rozstrzygnięcia. `session.agent_service_id` i syntetyczne flow z
  `resolve_harness_flow` zostają usunięte — sesja zawsze idzie przez przypięty Code Harness, a to agenci w nim
  są `llm` albo `cli`.
- UI Agents: obok wyboru modelu segment „Model / Aplikacja CLI"; dla CLI: silnik, model, „Konto globalne
  (lista)" / „Konto użytkownika prowadzącego sesję". Komponenty `tf-*`, i18n w pięciu językach.

### 2.4 Automatyczna obecność konta na nodzie

Wyzwalacz: `resolve_run_account` na nodzie N stwierdza, że lokalna materializacja nie istnieje albo ma
starszą rewizję → `ensure_account_on_node(account, N)`.

Transport — Sync Ledger, nie kopiowanie katalogów:

- `provider_accounts` i granty: zwykłe zasoby platformowe w `sync/core_registry.rs`.
- `provider_account_credentials`: osobny typ zasobu, przeszyfrowywany kluczem odbiorcy tym samym mechanizmem co
  `apply_shared_setting_secret`; Sync Policy ogranicza odbiorców do nodów z flagą „runtime agentów" (nie do
  telefonów i nie do wszystkich). Dzięki temu konfiguracja jest jednorazowa, a node działa także, gdy node
  logowania jest offline.
- Na nodzie materiał trafia do `accounts/<uuid>/` dopiero przy użyciu; bridge kopiuje go do prywatnego
  profilu instancji jak dziś.

Rotacja tokenów (najtrudniejsza część, różna per silnik):

| Silnik | Materiał | Strategia |
|---|---|---|
| claude-code | `setup-token` (statyczny, wstrzykiwany env) | replikacja wprost; brak rotacji |
| codex | `auth.json` z refresh tokenem | **jeden odświeżający**: tylko `home_node_id` odświeża (proaktywnie, poza sesją); nody-satelity dostają nowe rewizje. Jeśli CLI na satelicie mimo to odświeży, zgłasza nowy materiał z rewizją bazową — CAS u właściciela, przyjęcie tylko przy zgodnym `provider_subject` |
| grok-build | `auth.json` + hak `auth_provider_command` | broker: CLI pyta Core o token, refresh token nie opuszcza vaultu |
| muse-code | `config/muse/auth.json` | jak codex, po teście kontraktowym |

`home_node_id` przenosi się automatycznie (istniejący protokół `account_move`, bez kasowania replik), gdy
właściciel jest nieosiągalny dłużej niż próg, a inny node ma bieżącą rewizję. Dzisiejsza kwarantanna
(`credential-review-required`) zostaje wyłącznie dla zmiany tożsamości albo przegranego CAS.

### 2.5 Współbieżność i izolacja w jednym katalogu

- **Blokada `account_busy` znika.** Dziś istnieje tylko dlatego, że każda sesja dostaje własną KOPIĘ pliku
  poświadczenia, a po sesji bridge porównuje kopię z oryginałem — dwie sesje naraz dałyby dwie rozbieżne kopie.
  Docelowo wszystkie instancje konta na nodzie współdzielą jeden katalog poświadczenia (tak jak zwykłe CLI
  współdzieli `~/.codex/auth.json` między oknami), a prywatne per instancja zostają historia, cache, tmp i HOME.
  Bridge obsługuje wiele sesji jednocześnie; opcjonalny limit w GUI (A03) domyślnie „bez limitu".
  Zmianę pliku poświadczenia bridge wykrywa obserwacją katalogu, weryfikuje tożsamość i publikuje nową rewizję.
- Każdy przebieg CLI ma prywatny profil `accounts/<account>/instances/<uuid>` — HOME, config, tmp, env z
  allowlisty. Agent na koncie globalnym i agent na koncie użytkownika w tym samym worktree to dwa procesy w
  dwóch sandboxach; polityka sandboxa daje odczyt wyłącznie własnego profilu i workspace'u, nigdy
  `accounts/*` innego konta.
- `vendor_session_id` wiązany z `(account_id, user_id, agent_id, workspace)`; wznowienie z innym kontem jest
  odmową, nie migracją rozmowy.
- Wspólne są tylko pliki projektu (w tym `.claude/`, `.codex/`, `CLAUDE.md`) — to konfiguracja projektu, nie
  poświadczenia. Bridge odrzuca projektowe ustawienia zmieniające źródło uwierzytelnienia (apiKeyHelper,
  auth_provider_command, env w settings).
- Zapis do jednego worktree: jedna tura pisząca naraz (blokada tury w `delegate_cli`), recenzent/tester
  czytają równolegle.
- Metryki i audyt: `model_metrics_rollup` i `audit_log` dostają `account_id`; zużycie konta globalnego jest
  rozliczane per użytkownik.

Granica, której nie da się obejść: kod uruchomiony w sesji może odczytać token swojego CLI. Dla konta
globalnego oznacza to, że każdy uprawniony użytkownik może wynieść token organizacji. Dlatego konto globalne
typu `api_key` idzie przez istniejący adapter z ticketem (klucz nie wchodzi do procesu), a globalna
subskrypcja jest oznaczona w GUI jako „tylko dla zaufanych".

## 3. Warunki wstępne (bez nich „na każdym nodzie" nie działa)

1. **Linux**: sieć w sandboxie. `bwrap` zostaje z `--unshare-net`; proxy egress wystawione jako gniazdo unix
   zamontowane do sandboxa + forwarder `127.0.0.1:port → UDS` uruchamiany wewnątrz (ten sam plik bridge'a).
   Usunąć bramkę `with_proxy` (`process_sandbox.rs:182`). Testy integracyjne bwrap (dziś są tylko macOS).
2. **Sonda gotowości** sprawdza `runtime.status` z rzeczywistą próbą sandboxa, nie samo `/health`.
3. **Bridge w archiwach release** (dziś kompilowany u użytkownika przez `cargo`). Silniki widoczne także w
   edycji slim — to nie jest lokalna inferencja.
4. **Grok/Muse na Linuxie**: przypiąć artefakty linux x86_64/aarch64 z SHA256 w `managed_cli.rs`.
5. **Windows**: bez backendu sandboxa node pozostaje „tylko zdalnie"; manifesty przestają deklarować
   `windows`, dopóki AppContainer nie przejdzie kontraktu. macOS bez sesji GUI: jawny komunikat w node pickerze.
6. Czat `/v1`: obsługa zdarzeń `grok`/`muse` w `execute_chat`, koniec tury tylko z obserwowanego zdarzenia.

## 4. Etapy

| # | Zakres | Warunek zakończenia |
|---|---|---|
| 0 | Pomiary na prawdziwych kontach: rotacja refresh tokenu codex/muse przy dwóch kopiach, czas życia access tokenu, `auth_provider_command` groka, równoległe sesje | Tabela 2.4 potwierdzona albo poprawiona; bez tego etap 3 nie startuje |
| 1 | Warunki wstępne 1–4 i 6 | codex i claude-code wykonują turę na Linuxie w sandboxie z siecią; deploy na nodzie bez sandboxa kończy się błędem |
| 2 | `provider_accounts` + granty + poświadczenia; migracja istniejących kont i `code_agent_credentials`; bridge na żądanie; self-service logowania użytkownika; GUI „Konta agentów" i „Moje konta agentów" | Użytkownik bez roli admina łączy własne konto; admin nie może go użyć; dwa konta tego samego silnika nie widzą się nawzajem |
| 3 | Synchronizacja kont i poświadczeń, `ensure_account_on_node`, single-refresher/CAS/broker, automatyczna zmiana `home_node_id` | Login na A, sesja na B bez żadnej akcji; refresh na B widoczny na A; A offline nie blokuje B; dwa nody naraz nie unieważniają tokenu |
| 4 | `agents.runtime_json`, `resolve_run_account`, UI Agents, usunięcie `agent_service_id` i syntetycznego flow, limity współbieżności | Dwóch użytkowników w tym samym workspace z agentem `mode=user` działa na własnych kontach; agent `mode=global` obok, w tym samym katalogu; test negatywny: brak konta użytkownika nie używa globalnego |
| 5 | Metryki/audyt per konto, dokumentacja operacyjna, e2e Playwright ścieżki kont (dziś brak) | Analytics pokazuje zużycie per konto i użytkownik |

Kontrakty do zachowania: warianty protokołu tylko dopisywane (`SCHEMA_VERSION` rośnie — wszystkie nody
przebudować razem), `agents` i nowe tabele z migracją i wpisem w `core_registry`, brak shimów: stare
`coding_agent_account_grants`, `code_agent_credentials`, „Przenieś konto" w GUI i `agent_service_id` znikają w
etapie, który je zastępuje.

## 5. Decyzje do potwierdzenia

1. **Replikacja tokenów subskrypcji przez Sync Ledger** odwraca wcześniejszą decyzję „jeden właściciel +
   transfer". Rekomendacja: tak, ale tylko do nodów z flagą runtime agentów i z jednym odświeżającym.
   Alternatywa: pobieranie na żądanie od noda-właściciela (mniej kopii, ale właściciel offline = brak pracy).
2. **Konto globalne — kto może używać**: rekomendacja granty (user/grupa/cała organizacja), a przypisanie
   konta do agenta nie omija grantu. Alternatywa: samo przypisanie w Agents wystarcza.
3. **Windows**: rekomendacja — wycofać deklarację z manifestów do czasu backendu sandboxa.
