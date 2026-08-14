# Code Studio — plan wdrożenia

> **Rewizja 1.7** (2026-08-14). Wbudowany moduł Core (NIE addon), na wzór ML Studio i Projektów:
> środowisko pracy nad kodem — repozytoria, edytor, terminal, git — napędzane własnym,
> wieloagentowym harnessem, którego każdy krok jest widoczny jako blok w Flow Builderze.
> Codex i Claude Code są w nim **jednym z agentów**, a nie osobnym światem.
>
> Odniesienia `plik:linia` pochodzą z audytu repo z 2026-08-14.

### Zmiany 1.6 → 1.7

**`trusted_native` jest trybem domyślnym.** Decyzja właściciela produktu, spójna z pierwotnym
wyborem („natywnie domyślnie, kontener opcjonalnie"). Kreator preselekcjonuje `trusted_native`,
a `container` jest świadomym wyborem mocniejszej izolacji. Ponieważ domyślną wartością jest tryb
**bez izolacji**, wybór nigdy nie może być niewidoczny: każde założenie workspace emituje zdarzenie
audytowe z rozstrzygniętym trybem, a UI trwale oznacza workspace natywny (§19). Zastępuje wpis 10
z rewizji 1.6, który wymagał jawnego podania trybu.

### Zmiany 1.5 → 1.6

**Decyzja: tryb natywny to `trusted_native`, bez zarządzania kontami systemu operacyjnego.**
Rewizja 1.5 próbowała odtworzyć w nim właściwości kontenera przez dedykowanych użytkowników OS
(`cs-rw-*`, `cs-ro-*`). To wymagałoby uprzywilejowanego launchera tworzącego konta i ACL, principali
per sesja i recovery osieroconych kont — dużego podsystemu w zamian za izolację i tak słabszą niż
kontener. Właściciel produktu tego nie chciał i ma rację: tryb natywny ma być **prostym, zaufanym
wykonaniem lokalnym**, a nie namiastką sandboxa. Kto potrzebuje izolacji, wybiera kontener.

| # | Zmiana | Sekcja |
|---|---|---|
| 1 | Usunięci użytkownicy `cs-rw-*`/`cs-ro-*` i cała implikowana przez nich architektura (uprzywilejowany launcher, principale per sesja, mapowanie UID/SID, sprzątanie kont). Tryb natywny biegnie jako użytkownik usługi TentaFlow | §7.1, §7.2 |
| 2 | Tryb nazwany wprost `trusted_native` — **nie obiecuje izolacji od hosta**. `ro` i brak `fs_write` dotyczą narzędzi Core, nie syscalli uruchomionej komendy | §7.1 |
| 3 | `egress_enforcement` jako jawna wartość: `namespace` \| `firewall` \| `unrestricted`. Przy `unrestricted` **nie obiecujemy filtrowania ani audytu hostów** — zamiast „allowlisty doradczej", która sugerowała nieistniejącą kontrolę | §5.1, §7.6, §17.3 |
| 4 | **Koniec automatycznej degradacji `cow` → `rw`.** Domyślnie fail-closed: kopia przez reflink/clone, a gdy niemożliwa — odmowa. Praca na prawdziwym worktree wymaga osobnej, interaktywnej zgody `profile_degrade_rw` | §7.2, §9.3 |
| 5 | **Broker nigdy nie ufa plikowi `.git` z worktree** — używa własnej mapy `session_id → git_dir + work_tree` i jawnych `--git-dir`/`--work-tree`. Dotyczy obu trybów | §11.1 |
| 6 | Serwer **odrzuca** `native + autonomous` i `native + local_only` bez `firewall`; ukrycie opcji w UI to nie walidacja | §9.5, §18 |
| 7 | Zmiana gałęzi docelowej w trakcie merge'a **unieważnia próbę** — nowy merge, nowe testy, nowy review; nie samo odświeżenie `expected_old` | §11.6, §16.3 |
| 8 | Nowy krok `finalize_merge`: po rozwiązaniu konfliktu commit powstaje **z zaakceptowanych blobów**, przed `update_target_ref` | §11.6, §16.2, §16.4 |
| 9 | Naprawione niespójności: threat model nie zakłada już, że każda sesja to kontener; sekcja agentów nie obiecuje, że tester „fizycznie" nie zapisze; Faza 0B bez ADR „kontener jedynym trybem"; tabela CAS nie wiąże commitu ze stanem worktree | §2.2, §13.2, §15, §23 |
| 10 | ~~Tryb wykonania jest wyborem jawnym, bez wartości domyślnej~~ — **ZASTĄPIONE w 1.7**: `trusted_native` jest domyślny, a przed cichym wyborem chroni zdarzenie audytowe i trwałe oznaczenie w UI | §5.1, §19 |

### Zmiany 1.4 → 1.5

**Decyzja produktowa: tryb natywny zostaje w V1.** Rewizja 1.3 usunęła go, powołując się na to, że
egzekucja uprawnień opiera się na profilach montowań. Właściciel produktu podtrzymał pierwotny wybór,
więc tryb wraca — z jawnie opisanym, węższym zakresem gwarancji (§7.1). Reszta architektury bez zmian.

> Wpisy 1, 3, 4 i 5 poniżej dotyczą kont OS (`cs-rw-*`/`cs-ro-*`) i **zostały zastąpione w 1.6**
> przez `trusted_native` bez zarządzania kontami. Blok pozostaje dla historii decyzji.

| # | Zmiana | Sekcja |
|---|---|---|
| 1 | `exec_mode` (`container`/`native`) wraca do rejestru; kontener pozostaje **domyślny** | §5.1, §0 |
| 2 | Nowa tabela gwarancji per tryb — jedno miejsce, w którym widać, co jest egzekwowane, a co doradcze | §7.1 |
| 3 | Natywnie procesy agentów biegną jako **dedykowany użytkownik OS**, a `repo/` (metadane git), vault, `workspace.db` i inne workspace'y są dla niego zamknięte uprawnieniami | §7.1, §7.3 |
| 4 | Natywny `ro` realizowany drugim użytkownikiem OS; `cow` emulowany kopią worktree poniżej progu, powyżej **degraduje do `rw` z ostrzeżeniem** | §7.2 |
| 5 | Natywnie polityka egress jest **doradcza**, chyba że węzeł ma regułę zapory po właścicielu procesu (Linux); `local_only` jest wtedy **niedostępna** | §7.6, §17.3 |
| 6 | `autonomous` pozostaje niedostępny natywnie; `git_push`/`git_merge`/commit bez zmian (broker i tak jest poza procesem agenta) | §9.5 |
| 7 | UI oznacza workspace natywny trwałym ostrzeżeniem i ukrywa opcje, których w nim nie da się dotrzymać | §19 |

### Zmiany 1.3 → 1.4

| # | Zmiana | Sekcja |
|---|---|---|
| 1 | **Worktree integracyjny powstaje jako `--detach` na `expected_old`**, a wynik merge'a dodatkowo pod prywatnym refem `refs/code-studio/integration/<op_id>`. Utworzenie go „na `target_branch`" przesuwało referencję docelową już przy `merge`, przed testami i akceptacją | §11.6 |
| 2 | **Merge przechodzi przez testy i review**: `merge_integration → tester + reviewer → patch_review → update_target_ref`. Graf szedł wprost do `update_target_ref`, wbrew własnemu opisowi | §16.2 |
| 3 | **Worktree integracyjny nie jest usuwany po konflikcie ani odrzuceniu** — stan `held`, przejmuje go run rewizji. Usunięcie tylko po zatwierdzonym `update-ref`, porzuceniu przez użytkownika albo zamknięciu sesji | §5.3, §11.6 |
| 4 | Recovery ponawia **wyłącznie** przy `idempotent = 1`; niepotwierdzony `exec`, `push`, `merge` idzie do `unknown` niezależnie od precondition | §13.1 |
| 5 | Commit **nie wymaga**, by worktree nadal trzymał zaakceptowany hash — sprawdza artefakt, `base_commit` i CAS referencji. Późniejsze zmiany worktree stają się kolejnym patch setem | §11.5 |
| 6 | `rename` przy budowie commitu usuwa `old_path` **i** dodaje nową ścieżkę | §11.5 |
| 7 | `session_runs.trigger` o `agent_spawn` i `cli_delegate`; `cli_instances.vendor_session_id` nullowalne (nieznane w stanie `starting`) | §5.3 |
| 8 | Sandbox z `lease_id` — dwa równoległe procesy o tym samym profilu są możliwe; unikalność tylko dla współdzielonych, nie-`ephemeral` | §5.3, §7.2 |
| 9 | `repo_kind='empty'` tworzy **początkowy pusty commit** w S4, więc `base_commit` jest zawsze obecny | §6 |
| 10 | Vault adaptera: jawne mapowanie `(org_id, node_id, engine_id) → poświadczenie` + capability `cli_delegate` | §5.2, §7.5, §9.2 |
| 11 | **Pełne utwardzenie configu git obowiązuje od pierwszego `clone` w Fazie 1**, nie od Fazy 2 | §23 |
| 12 | `jti` **trwałe** (przeżywa restart), a asercja dla operacji mutujących związana z `op_id` i digestem — dziennik operacji jest drugą linią obrony | §12.1 |
| 13 | Artefakty przechowują **kanoniczne, zredagowane dane strukturalne**, nie surowe argv i nie surowe wyjście — „pełne argv w artefakcie" przeczyło zasadzie redakcji i własnemu testowi | §7.8, §13.4 |

### Zmiany 1.2 → 1.3

| # | Zmiana | Sekcja |
|---|---|---|
| 1 | ~~Tryb natywny usunięty z V1~~ — **WYCOFANE w 1.5**: właściciel produktu podtrzymał tryb natywny, który wraca z jawnie węższym zakresem gwarancji (§7.1). Wpis zostaje dla historii decyzji | §7.1 |
| 2 | **Wstrzykiwanie poświadczenia CLI przeprojektowane.** Proxy `CONNECT` nie widzi wnętrza TLS, więc nie może podmienić nagłówka — mechanizm z 1.2 był niewykonalny. Zamiast tego **adapter dostawcy**: lokalny reverse proxy terminujący TLS, na który CLI jest przekierowane przez nadpisanie base URL, plus **krótkotrwały ticket** związany z sesją, runem, instancją, modelem, metodą i budżetem. Faza 0B jest twardym **go/no-go per CLI** — brak mechanizmu = integracja wyłączona, żadnego „słabszego fallbacku" | §7.5, §17.3 |
| 3 | **Agent modelowy nie ma `git_commit` ani `git_stage`.** Commit wykonuje wyłącznie koordynator po zaakceptowanym patch secie, a broker buduje go **z zaakceptowanych blobów przez tymczasowy indeks** — commitowane jest dokładnie to, co przeszło review, a nie stan worktree w chwili commitu | §10, §11.5, §15 |
| 4 | **Token shimu związany z aktorem, runem i zbiorem capability**, nie z samą sesją | §7.3, §11.1 |
| 5 | `session_operations`: typowany `op_kind`, trwałe wejście, precondition/postcondition, OID-y git; `op_id` z krotki pochodzenia obsługującej też terminal, UI, shim i bloki flow | §13.1 |
| 6 | CAS rozdziela **niezmienny** `patch_base_blob_sha` od **ruchomego** `expected_current_blob_sha`; objęte także `delete`, `rename` i kolejne edycje tego samego pliku | §13.2 |
| 7 | Profil sandboxa rozbity na **niezależne** `mount_access` (`ro`/`cow`/`rw`) i `network_access` (`none`/`gateway`) | §5.3, §7.2 |
| 8 | **Tester dostaje jednorazowy overlay COW** — realne buildy zapisują `target/`, `bin/obj`, `.gradle`, `node_modules`, a zmiany są odrzucane po teście | §7.2, §15 |
| 9 | **Merge na worktree integracyjnym**: scalenie → testy → review → atomowy `update-ref(new, expected_old)` na gałęzi docelowej | §11.6 |
| 10 | Ścieżka „Popraw/Podziel plan" ma domknięty wynik: **najwyżej jeden obrót w runie**, kolejny kończy run jako `revision_requested` | §16.2, §16.3 |
| 11 | `SessionAssertion`: Ed25519 z `kid`, rotacja z oknem nakładania, wystawca związany z uwierzytelnionym peerem kanału, **uczciwe SLA cofania uprawnień** (natychmiast tylko dla zmian na owner node) | §12.1 |
| 12 | Mirror audytu przez **trwały outbox** z ponawianiem; argv **redagowane** przed zapisem (może zawierać tokeny) | §13.4 |

### Historia wcześniejszych rewizji (skrót)

**1.0 → 1.1**: owner-node coordinator; kontener per sesja; filesystem na uchwytach katalogów;
sekrety node-local; przepisany model runtime; rozdzielone permission i review; capability zamiast
nazw narzędzi; twardy profil git; threat model, maszyny stanów, macierz RBAC, saga, kwoty, egress,
observability. **1.1 → 1.2**: acykliczny graf harnessu (pętla = nowy run); jawny `spawn`
implementera; profile wykonania jako egzekucja OS; `.git` poza sandboxem; cały git w brokerze;
bramka egress; `SessionAssertion`; idempotencja efektów; poprawiony CAS; `mandatory_interactive`.

---

## 0. Decyzje bazowe

| Obszar | Decyzja |
|---|---|
| Mózg | Własny harness na flow engine. Codex/Claude Code jako **agenci typu CLI** |
| Agenci | Role (planowanie / orkiestracja / kodowanie / wyszukiwanie / przegląd / testy) na istniejącej tabeli `agents`; każdy z własnym modelem, capability i skillami |
| Orkiestracja | Hybryda: jawne `spawn`/`await_subagents` **oraz** `core.agent_spawn` |
| Flow | Jeden flow „Code Harness", edytowalny, **acykliczny**; iteracja = nowy run w tej samej sesji |
| Wykonanie | **`trusted_native` domyślnie** (zaufane wykonanie lokalne, bez obietnicy izolacji od hosta); `container` jako świadomy wybór pełnej izolacji — §7.1 |
| Koordynator | **Owner node** — pliki, procesy, zgody, CLI, broker git, adapter dostawcy, zapis zdarzeń |
| Git | W całości w brokerze na owner node; sandbox dostaje shim RPC z tokenem związanym z aktorem i runem |
| Commit | Wykonuje **tylko koordynator**, z blobów zaakceptowanych w review; żaden agent nie ma `git_commit` |
| Sieć sandboxa | Brak trasy domyślnej; wyjście przez bramkę egress; ruch do dostawcy przez adapter terminujący TLS |
| Węzeł / katalog | Użytkownik wybiera **węzeł**; katalog zakłada **system** |
| Mesh | Transport od Fazy 4; model danych i granice od Fazy 1 |
| Zgody | Capability + tryby autonomii + allowlista; `git_push`/`git_merge`/`secret_manage` zawsze interaktywne |
| Repozytoria | Pełny cykl + push + merge przez worktree integracyjny; SSH i LAN pod profilem §11; worktree per sesja |
| UI | Pełne IDE: drzewo, edytor, diff, terminal, git, oś czasu agenta |
| Workspace | Byt pierwszej klasy; prywatny domyślnie; role owner/editor/viewer; link N:M do projektu |
| Projekty | Workspace jako źródło kodu dla testów F3 — po commicie |
| Wyszukiwanie | Grep od Fazy 2; indeks semantyczny od Fazy 7 |
| Konta CLI | Jedno konto organizacji na węzeł; materiał **nigdy** w sandboxie — adapter + ticket |
| Trwałość | Sesja trwała, praca w tle, wznowienie po restarcie; idempotencja **efektów** z pre/postcondition |
| Platformy | Linux, macOS, Windows. Kontener wymaga runtime'u (na Windows WSL2/Hyper-V); węzeł bez niego obsługuje wyłącznie tryb natywny |
| MCP | Poza zakresem — uniwersalny addon MCP |

---

## 1. Stan faktyczny

### 1.1 Integracja Codex / Claude Code

```
Dashboard: Usługi → „Coding agents"   www/js/modules/services.js:723-848 + modules/coding-agent.js
      │ ApiBinary.action('serviceAgentRequest', {serviceId, nodeId, operation, payloadJson})
      ▼
Core dispatch  dispatch/handlers.rs:10838-10880
      │ usługa na innym węźle → MeshCommandType::AgentRpc (tentaflow-protocol/src/mesh.rs:612)
      ▼                          → mesh/command_executor.rs:1153-1184
Core proxy  services/coding_agent.rs:63-162   (8 operacji, endpoint loopback-only :82, timeout 65 s)
      ▼ HTTP
Bridge  tentaflow-containers/agents/native/coding-agent-bridge/src/main.rs (876 linii)
      ├── Codex       → `codex app-server`, JSON-RPC po stdio (:522-618)
      └── Claude Code → PTY 40×120 + skanowanie ekranu (portable-pty + vt100) (:620-706)
```

Instalacja jako `native_managed_cli`: `npm install` pinowanej wersji do
`<cache>/coding-agents/<engine>/<version>`, bridge budowany lokalnie (`services/deploy/binary.rs:108-207`).

| | Codex | Claude Code |
|---|---|---|
| Transport | JSON-RPC | wpisywanie tekstu do PTY, czytanie ekranu |
| Sesja | `threadId` od CLI | własny UUID przez `--session-id` |
| Zdarzenia | strukturalny JSON | surowe bajty terminala |
| Lista modeli | `model/list` | uruchomienie CLI, `/model`, OCR ekranu (`parse_claude_models`, :755) |
| Gotowość | natychmiast | napisy `"for shortcuts"` / `"bypass permissions on"` (:692) |
| Trust prompt | n/d | auto-`\r` na `"Quick safety check"` (:686) |

Stan: `sessions.json` (metadane), zdarzenia **w RAM**, ring 10 000 z ucinaniem po 1 000 (:799-810).
Konta: jedno na węzeł na engine (`keys/coding-agents/<engine>`, dla Codex `CODEX_HOME`), logowanie
tylko dla admina (`services.js:733`). Jeden `workspace_root` na usługę (bridge:812-824).
Integracja z platformą: żadna — `AgentRpc` wykluczony z routingu inferencji
(`services/runtime/transport_client.rs:156`, `services/handles_cache.rs:310`).

### 1.2 Trzy defekty do natychmiastowej naprawy

**D1 — dziesiątki sesji w historii Claude Code.** `services/supervisor.rs:886`
(`sync_coding_agent_models`, `INTERVAL = 300 s`, z pętli w :412 i :750) co 5 minut woła `models.list`,
a bridge (`main.rs:308-344`) uruchamia na to pełny `claude --session-id <świeży UUID>` w PTY.
~12 sesji/h. To samo przy otwarciu okna „Sesje" (`services.js:781`) i w ścieżce logowania
(`coding-agent.js:42`, `:104`).

**D2 — wyciek procesów.** `TerminalRuntime` to zmienna lokalna; `_child` nie jest ubijany ani reapowany.

**D3 — zatwierdzenia Codeksa wiszą.** Bridge startuje wątek z `approvalPolicy:"on-request"` (:583),
ale nie ma ścieżki odpowiedzi na request server→client (:554).

### 1.3 Co wykorzystujemy bez przepisywania

Harness agentowy jest **jednym flow z pętlą inline** (`db/seed.rs:1315-1400`):
`trigger → conversation_history → agent_context → [region agent_turn: compact_context → llm(tools)
→ tool_exec ─loop_back→] → persist_turn → output(stream)`.
Mechanika: `flow_engine/types.rs:87-92`, `:166-179`, R11 `flow_engine/validation.rs:659-760`.

Bloki dziedziczone 1:1: `ask_user` (≤ 4 opcje + własna odpowiedź, `node_adapters/ask_user.rs:69-80`),
`spawn`, `await_subagents`, `subagent_status`, `on_subagent_complete`, `condition`, `combine`,
`compact_context`, `persist_turn`, `output`.

Tabela `agents` (`db/migrations.rs:1891-1911`) ma per agent: `model`, `tools_json`, `skills_json`,
`params_json`, `max_iterations`, `timeout_secs`, `max_subagents`, `max_spawn_depth`, `flow_id`.
Allowlista w `agents/catalog.rs`; nieznany `core.*` odrzucany. Builtiny dziś: `core.skill_view`,
`core.agent_spawn|wait|list|cancel`, `core.ask_user`, `core.project_search`,
`core.project_list_sources`, `core.project_case_save` (`agents/builtins.rs:65-73`) — **brak narzędzi
plikowych i wykonawczych**.

Runtime: `AgentRunManager`, `InteractionRegistry`, `ProgressEvent`
(`flow_engine/dispatchers/progress.rs:19-83`), `tf-agent-activity`.

Wzorce: Projekty (rejestr + per-projekt DB za LRU-poolem 16, `project_studio/project_db.rs`;
sub-enum `message_body.rs:8066`; uprawnienia `migrations.rs:3881-3897`; kafelek `apps-home.js:17`;
ekran `www/js/app.js:66,521`; strumienie `stream_handlers.rs:1824,2122`), `ml_link.rs`,
`SandboxLimits` (`deploy/docker.rs:28-66`), `NamespaceManager::get_or_create_at`
(`services/vector/namespace.rs:654`), autoryzacja aktora (`mesh/command_executor.rs:113`, `:1315`).

### 1.4 Ograniczenia rdzenia, które kształtują plan

| Fakt w kodzie | Konsekwencja |
|---|---|
| Region pętli ma **wyłącznie stop strukturalny** — `last_assistant_has_tool_calls` (`executor.rs:763`); `LoopRegion` niesie tylko `max_iterations` i `final_pass` (`cache.rs:52-65`, `:418-431`) | Region bez węzła `llm` kończy się po jednej iteracji. Pętla deterministyczna jest dziś **niewyrażalna** w grafie (§16.1) |
| R11 + toposort Kahna: krawędź inna niż `loop_back` liczy się do in-degree (`validation.rs:665-760`) | Powrót z późniejszego etapu to cykl → graf odrzucony |
| `flow_versions` + `FlowVersionList/Get/Restore` **istnieją** (`migrations.rs:6034`, `handlers.rs:1546`, `:1610`) | Do dopisania tylko factory restore, pin wersji przez sesję, zakres wersji aktywnej |
| `AllowForRun` jest **per run** (`agents/interaction.rs:152-159`) | Grant sesyjny to nowy, trwały byt (§9.1) |
| `tf-code-editor`: 7 języków (`tf-code-editor.js:15`) | Bez Rust/C#/HTML/CSS/shell/TOML „IDE" jest obietnicą na wyrost (§19) |

---

## 2. Threat model i granice zaufania

### 2.1 Aktorzy

| Aktor | Zaufanie | Zagrożenie |
|---|---|---|
| Właściciel workspace | uwierzytelniony, autoryzowany do swojego workspace | eskalacja na cudzy workspace, eskalacja na host |
| Członek (editor/viewer) | jw., w zakresie roli | wyjście poza rolę |
| Administrator org | zarządza, **nie czyta treści** bez zdarzenia audytowego | ciche czytanie cudzego kodu |
| Model (LLM) | **niezaufany** | narzędzie poza zakresem, eksfiltracja, zapis poza worktree, commit z pominięciem review |
| Treść repozytorium | **niezaufana** | wstrzyknięcie promptu, kod z build/test, hooki git |
| Agent CLI dostawcy | **niezaufany proces** | dowolny kod, dowolny ruch, próba odczytu poświadczeń |
| Węzeł mesh | zaufany co do tożsamości, **nie** co do żądania | podszycie pod aktora, obejście zgód |
| Sieć | niezaufana | MITM na git/SSH, SSRF do metadata/control-plane |

### 2.2 Granice

1. **Granica sesji** — istnieje **tylko w trybie `container`**: wszystko, co uruchamia kod, biegnie
   w sandboxie, a poza nim zostają Core, baza, sekrety, broker git, adapter i bramka. W
   `trusted_native` tej granicy **nie ma** — proces ma prawa użytkownika usługi (§7.1).
2. **Granica workspace** — filesystem i sieć. W `container` egzekwowana montowaniami; w
   `trusted_native` jest umową, nie mechanizmem.
3. **Granica węzła** — mesh. Żądanie z innego węzła to dane, nie decyzja. Obowiązuje w obu trybach.
4. **Granica egress** — wg `egress_enforcement` (§7.6): `namespace` i `firewall` egzekwują,
   `unrestricted` nie obiecuje niczego.
5. **Granica integralności zmian i sekretów** — **obowiązuje w obu trybach**: broker git poza
   procesem agenta z własną mapą `git_dir`, commit z zaakceptowanych blobów, obowiązkowe pytanie
   przy `push`/`merge`, poświadczenie dostawcy wyłącznie w adapterze. To jest ta część kontraktu,
   której `trusted_native` nie traci.
5. **Granica integralności zmian** — commitowane jest wyłącznie to, co przeszło review; commit
   buduje koordynator z zapisanych blobów, nie z worktree (§11.5).

### 2.3 Czego świadomie nie chronimy

- Właściciel **może** wykonać dowolny kod w swoim sandboxie — to cel produktu. Chronimy host,
  innych użytkowników i dane organizacji.
- Model o wystarczających capability może zepsuć własny worktree. Bezpiecznikiem jest git
  (worktree + gałąź + review + commit poza sandboxem), nie blokada operacji.
- Nie chronimy przed złośliwym administratorem organizacji; chronimy przed *cichym* działaniem
  administratora (§25.4).
- Kod uruchamiany przez agenta CLI dzieli z nim sandbox. Poświadczenie organizacji **nie** jest tam
  obecne (§7.5); wykradziony ticket jest krótkotrwały i ograniczony do jednego runu i budżetu.

---

## 3. Owner-node session coordinator

```
        Węzeł A (dashboard)                              Węzeł B (owner node)
┌──────────────────────────────┐         ┌────────────────────────────────────────────┐
│ UI, protokół binarny         │         │ SessionCoordinator                          │
│ routing po workspace         │──mesh──▶│  • root run + subagenci (AgentRunManager)   │
│ RemoteProxy: serializacja    │         │  • InteractionRegistry (zgody, pytania)     │
│ ŻADNEJ logiki decyzyjnej     │◀─mesh───│  • PolicyEnforcementPoint                   │
└──────────────────────────────┘         │  • sandboxy sesji (mount × network)         │
                                          │  • broker git + agent ssh (POZA sandboxem)  │
                                          │  • adapter dostawcy + bramka egress         │
                                          │  • commit z zaakceptowanych blobów          │
                                          │  • workspace.db, artefakty, worktree        │
                                          └────────────────────────────────────────────┘
```

1. Root run i wszyscy subagenci sesji startują na owner node.
2. Zgody i pytania rejestruje `InteractionRegistry` owner node; węzeł dashboardu przekazuje kartę
   i odsyła decyzję, nie trzyma stanu.
3. `session_events` zapisuje wyłącznie koordynator — jeden pisarz, prosta alokacja `seq`.
4. Węzeł dashboardu nie ma `WorkspaceExecutor`, brokera ani adaptera; ma `RemoteProxy`.
5. Niedostępność owner node jest **projekcją łączności w UI**, nie statusem w bazie.
6. Model LLM harnessu rozwiązywany z perspektywy owner node.

---

## 4. Model pojęciowy i maszyny stanów

### 4.1 Byty

| Pojęcie | Definicja |
|---|---|
| **Workspace** | Trwałe środowisko: właściciel, węzeł, repozytorium, polityki, kwoty, indeks |
| **Session** | Jedno zadanie. Własny worktree + gałąź, własne sandboxy, tryb autonomii, oś czasu. **Wiele runów** |
| **Run** | Wykonanie flow przez agenta (`agent_runs`). Iteracja po review to kolejny run |
| **Operation** | Pojedynczy efekt uboczny z typowanym `op_kind`, precondition i postcondition (§13.1) |
| **CLI instance** | Instancja agenta dostawcy powiązana z runem, z własnym ticketem do adaptera |
| **Patch set** | Snapshot zmian runu: pliki i hunki, do review; źródło blobów dla commitu |
| **Capability** | Atomowe uprawnienie — jednostka zgody i jednostka profilu wykonania |

### 4.2 Maszyny stanów

**Workspace** `provisioning → active ⇄ error → archived → deleted` (saga z trwałymi krokami, §6).

**Session** `creating → idle ⇄ running ⇄ waiting_user → (completed | failed | cancelled) → closing → closed`
plus `interrupted`. Brak `unreachable` — to projekcja UI. `interrupted → idle` przez wznowienie
z uzgodnieniem operacji (§13.1). Brak zdrowego sandboxa → `failed` z powodem, nigdy cichy fallback.

**Run** — istniejące stany `agent_runs`.

**Worktree** `creating → ready → dirty ⇄ clean → detaching → removed` (trwały wiersz, §5.3).
Worktree integracyjny merge'a ma osobny wiersz z `purpose='integration'`.

**CLI instance** `starting → ready ⇄ busy → idle → (ended | failed) → reaped`.
`reaped` = potwierdzone ubicie i `wait` (bez tego wraca D2).

**Operation** `pending → (completed | failed | unknown)`; `unknown` rozstrzyga postcondition albo
człowiek — nigdy ciche ponowienie.

Każde przejście zapisuje zdarzenie; kolumny stanu to projekcja (§13.3).

---

## 5. Model danych

### 5.1 Rejestr — baza główna, synchronizowana, bez sekretów

```sql
CREATE TABLE code_workspaces (
    id TEXT PRIMARY KEY, org_id TEXT NOT NULL, owner_user_id TEXT NOT NULL,
    name TEXT NOT NULL, slug TEXT NOT NULL,
    node_id TEXT NOT NULL,
    -- Domyślny jest 'trusted_native' (decyzja produktowa). Ponieważ to tryb BEZ
    -- izolacji, rozstrzygnięty tryb trafia do zdarzenia audytowego przy każdym
    -- założeniu workspace — pominięcie pola nie może być niewidocznym wyborem.
    exec_mode TEXT NOT NULL DEFAULT 'trusted_native'
        CHECK(exec_mode IN ('container','trusted_native')),
    container_image TEXT,                             -- wymagany dla exec_mode='container'
    -- Jak REALNIE egzekwowana jest polityka sieciowa. Wyliczane przy zakładaniu ze
    -- zdolności węzła; 'unrestricted' NIE obiecuje filtrowania ani audytu hostów.
    egress_enforcement TEXT NOT NULL
        CHECK(egress_enforcement IN ('namespace','firewall','unrestricted')),
    repo_kind TEXT NOT NULL CHECK(repo_kind IN ('empty','git')),
    repo_url TEXT,
    repo_auth_kind TEXT CHECK(repo_auth_kind IN ('none','token','ssh_key')),
    secret_ref TEXT,                              -- UCHWYT, nigdy materiał
    ssh_host_fingerprint TEXT,
    default_branch TEXT, target_branch TEXT,      -- gałąź docelowa merge'a (§11.6)
    autonomy_ceiling TEXT NOT NULL DEFAULT 'normal'
        CHECK(autonomy_ceiling IN ('plan','normal','auto_edit','autonomous')),
    egress_policy TEXT NOT NULL DEFAULT 'org_approved'
        CHECK(egress_policy IN ('local_only','org_approved','any')),
    index_enabled INTEGER NOT NULL DEFAULT 0,
    quota_disk_bytes INTEGER, quota_sessions INTEGER,
    status TEXT NOT NULL CHECK(status IN ('provisioning','active','error','archived','deleted')),
    status_detail TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    UNIQUE(org_id, owner_user_id, slug)
);

CREATE TABLE code_workspace_members (
    workspace_id TEXT NOT NULL REFERENCES code_workspaces(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL, role TEXT NOT NULL CHECK(role IN ('owner','editor','viewer')),
    added_by TEXT NOT NULL, added_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, user_id)
);

CREATE TABLE code_workspace_project_links (
    workspace_id TEXT NOT NULL REFERENCES code_workspaces(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL, linked_by TEXT NOT NULL, created_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, project_id)
);

CREATE TABLE code_workspace_creator_grants (
    org_id TEXT NOT NULL, user_id TEXT NOT NULL,
    granted_by TEXT NOT NULL, created_at TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id)
);

CREATE TABLE code_workspace_allowlist (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT NOT NULL REFERENCES code_workspaces(id) ON DELETE CASCADE,
    capability TEXT NOT NULL, pattern TEXT NOT NULL,
    created_by TEXT NOT NULL, created_at TEXT NOT NULL,
    UNIQUE(workspace_id, capability, pattern)
);

CREATE TABLE code_workspace_saga_steps (
    workspace_id TEXT NOT NULL, step TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','done','failed','compensated')),
    detail TEXT, updated_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, step)
);
```

Sync Ledger obejmuje powyższe (zasięg organizacji). Precedens wyłączenia sekretów jest w kodzie:
`addon_config` synchronizuje wyłącznie `is_secret=0` — „secrets stay node-local by design"
(`sync/core_registry.rs:293-296`).

### 5.2 Vault node-local

```sql
CREATE TABLE code_workspace_secrets (          -- NIE w sync/core_registry.rs
    secret_ref TEXT PRIMARY KEY, workspace_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('git_token','ssh_key')),
    material_enc BLOB NOT NULL, fingerprint TEXT,
    created_by TEXT NOT NULL, created_at TEXT NOT NULL, rotated_at TEXT, last_used_at TEXT
);

-- Poświadczenia dostawców CLI: JAWNE mapowanie (org, węzeł, engine) → materiał.
-- Konto jest organizacyjne, ale materiał jest node-local, bo klucz SettingsCipher
-- jest per węzeł. Bez tej tabeli adapter (§7.5) nie wiedziałby, czyj sekret wstrzyknąć.
CREATE TABLE code_agent_credentials (          -- NIE w sync/core_registry.rs
    org_id TEXT NOT NULL, node_id TEXT NOT NULL, engine_id TEXT NOT NULL,
    material_enc BLOB NOT NULL,
    provider_base_url TEXT NOT NULL,           -- upstream, do którego adapter forwarduje
    fingerprint TEXT,
    created_by TEXT NOT NULL, created_at TEXT NOT NULL, rotated_at TEXT, last_used_at TEXT,
    PRIMARY KEY (org_id, node_id, engine_id)
);

-- Anty-replay asercji mesh: MUSI przeżyć restart (§12.1).
CREATE TABLE session_assertion_jti (           -- NIE synchronizowana
    jti TEXT PRIMARY KEY, expires_at TEXT NOT NULL
);
CREATE INDEX idx_assertion_jti_exp ON session_assertion_jti(expires_at);
```

Autorytatywne miejsce: owner node. Brak sekretu na węźle → `secret_missing`, nie cicha porażka.
Rotacja: nowy wiersz → atomowa podmiana uchwytu → skasowanie starego po pierwszej udanej operacji.
Materiał opuszcza vault wyłącznie do **brokera git** (§11) i **adaptera dostawcy** (§7.5) — oba poza
sandboxem.

### 5.3 Runtime — `workspace.db` na owner node

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, user_id TEXT NOT NULL,
    title TEXT NOT NULL, branch TEXT NOT NULL,
    autonomy_mode TEXT NOT NULL,
    flow_id TEXT NOT NULL, flow_version_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN
      ('creating','idle','running','waiting_user','completed','failed','cancelled',
       'interrupted','closing','closed')),
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, closed_at TEXT
);

CREATE TABLE worktrees (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK(purpose IN ('work','integration')),
    op_id TEXT,                       -- dla 'integration': operacja merge, której dotyczy
    path TEXT NOT NULL,
    branch TEXT,                      -- NULL dla worktree integracyjnego (detached)
    head_commit TEXT NOT NULL,        -- 'work': punkt startowy; 'integration': expected_old
    base_commit TEXT NOT NULL,
    -- 'held' = wynik merge'a czeka na rozwiązanie konfliktu albo poprawkę w kolejnym runie;
    -- NIE wolno go usunąć, bo run rewizji nie miałby na czym pracować (§11.6).
    state TEXT NOT NULL CHECK(state IN
      ('creating','ready','dirty','clean','held','detaching','removed')),
    created_at TEXT NOT NULL, removed_at TEXT,
    UNIQUE(session_id, purpose, op_id)
);

-- Profil sandboxa: montowanie i sieć są NIEZALEŻNE. `lease_id` pozwala na wiele
-- równoległych procesów o tym samym profilu (dwa przebiegi testów naraz).
CREATE TABLE sandboxes (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    mount_access   TEXT NOT NULL CHECK(mount_access   IN ('ro','cow','rw')),
    network_access TEXT NOT NULL CHECK(network_access IN ('none','gateway')),
    lease_id TEXT,                              -- NULL = współdzielony; ustawiony = wyłączny
    owner_run_id TEXT,                          -- kto trzyma lease
    runtime_ref TEXT,
    state TEXT NOT NULL CHECK(state IN ('starting','ready','stopping','stopped','failed')),
    ephemeral INTEGER NOT NULL DEFAULT 0,       -- warstwa COW kasowana po zwolnieniu lease
    created_at TEXT NOT NULL, stopped_at TEXT
);
-- Współdzielone (nie-ephemeral) sandboxy: jeden na profil. Ephemeral: dowolnie wiele.
CREATE UNIQUE INDEX idx_sandbox_shared ON sandboxes(session_id, mount_access, network_access)
    WHERE ephemeral = 0 AND state != 'stopped';
CREATE INDEX idx_sandbox_lease ON sandboxes(session_id, lease_id);

CREATE TABLE session_runs (
    run_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('root','subagent','cli','revision')),
    trigger TEXT NOT NULL CHECK(trigger IN
      ('user','agent_spawn','cli_delegate','review_rejected','test_failed',
       'merge_conflict','merge_verify_failed','resume')),
    parent_run_id TEXT, agent_id TEXT,
    status TEXT NOT NULL, started_at TEXT, finished_at TEXT,
    UNIQUE(session_id, ordinal)
);

CREATE TABLE cli_instances (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
    engine_id TEXT NOT NULL, service_id INTEGER NOT NULL,
    vendor_session_id TEXT, model TEXT,          -- NULL w stanie 'starting': CLI jeszcze go nie zgłosiło
    ticket_id TEXT,                              -- ticket do adaptera dostawcy (§7.5)
    status TEXT NOT NULL CHECK(status IN ('starting','ready','busy','idle','ended','failed','reaped')),
    last_seq INTEGER NOT NULL DEFAULT 0, os_pid INTEGER,
    started_at TEXT NOT NULL, ended_at TEXT
);

CREATE TABLE session_events (
    event_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL, idempotency_key TEXT NOT NULL, schema_version INTEGER NOT NULL,
    kind TEXT NOT NULL, run_id TEXT, agent_id TEXT,
    payload_cbor BLOB NOT NULL,
    artifact_ref TEXT REFERENCES artifacts(sha256),
    security_relevant INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    UNIQUE(session_id, seq), UNIQUE(session_id, idempotency_key)
);
CREATE INDEX idx_session_events_run ON session_events(run_id, seq);

-- Dziennik EFEKTÓW: typowany, z pre/postcondition i OID-ami git (§13.1).
CREATE TABLE session_operations (
    op_id TEXT PRIMARY KEY,                      -- z krotki pochodzenia (§13.1)
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT,
    origin_kind TEXT NOT NULL CHECK(origin_kind IN
      ('tool_call','terminal','ui','shim','flow_block','coordinator')),
    origin_id TEXT NOT NULL, logical_step TEXT NOT NULL,
    op_kind TEXT NOT NULL,                       -- fs_write|fs_delete|fs_rename|exec|git_commit|…
    capability TEXT NOT NULL,
    idempotent INTEGER NOT NULL,
    input_ref TEXT REFERENCES artifacts(sha256), -- TRWAŁE wejście (treść, argv, parametry)
    precondition_cbor BLOB NOT NULL,             -- co musiało być prawdą przed
    postcondition_cbor BLOB NOT NULL,            -- co ma być prawdą po (weryfikowalne)
    result_oids TEXT,                            -- OID-y git: blob/tree/commit/ref
    status TEXT NOT NULL CHECK(status IN ('pending','completed','failed','unknown')),
    result_ref TEXT, error TEXT,
    started_at TEXT NOT NULL, finished_at TEXT,
    UNIQUE(session_id, origin_kind, origin_id, logical_step)
);
CREATE INDEX idx_session_ops_open ON session_operations(session_id, status);

CREATE TABLE patch_sets (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT REFERENCES session_runs(run_id),
    base_commit TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN
      ('open','in_review','accepted','partially_accepted','rejected','superseded','conflicted')),
    created_at TEXT NOT NULL, decided_by TEXT, decided_at TEXT
);
CREATE TABLE patch_files (
    id TEXT PRIMARY KEY,
    patch_set_id TEXT NOT NULL REFERENCES patch_sets(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK(change_kind IN ('add','modify','delete','rename')),
    old_path TEXT,
    patch_base_blob_sha TEXT,        -- NIEZMIENNY stan przy otwarciu patch setu
    current_blob_sha TEXT,           -- RUCHOMY: po każdej kolejnej edycji
    accepted_blob_sha TEXT,          -- wynik akceptacji (pełnej lub częściowej)
    git_blob_oid TEXT,               -- OID po `hash-object -w` — źródło commitu (§11.5)
    mode TEXT NOT NULL DEFAULT '100644',
    status TEXT NOT NULL CHECK(status IN ('pending','accepted','partially_accepted','rejected','conflicted')),
    UNIQUE(patch_set_id, path)
);
CREATE TABLE patch_hunks (
    id TEXT PRIMARY KEY,
    patch_file_id TEXT NOT NULL REFERENCES patch_files(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL, header TEXT NOT NULL, content_ref TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','accepted','rejected')),
    UNIQUE(patch_file_id, idx)
);

CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT, interaction_id TEXT NOT NULL,
    capability TEXT NOT NULL, target_digest TEXT NOT NULL,
    summary TEXT NOT NULL, detail_ref TEXT,
    status TEXT NOT NULL CHECK(status IN ('pending','decided','expired','abandoned')),
    decision TEXT CHECK(decision IN ('allow_once','allow_for_run','allow_for_session','always','deny')),
    requested_at TEXT NOT NULL, decided_at TEXT, decided_by TEXT
);

CREATE TABLE session_grants (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    capability TEXT NOT NULL, pattern TEXT NOT NULL,
    granted_by TEXT NOT NULL, created_at TEXT NOT NULL,
    PRIMARY KEY (session_id, capability, pattern)
);

-- Trwały outbox mirroru audytu (§13.4).
CREATE TABLE audit_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL REFERENCES session_events(event_id),
    payload_cbor BLOB NOT NULL,                  -- JUŻ ZREDAGOWANY
    attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
    created_at TEXT NOT NULL, delivered_at TEXT
);
CREATE INDEX idx_audit_outbox_pending ON audit_outbox(delivered_at, id);

CREATE TABLE artifacts (
    sha256 TEXT PRIMARY KEY, size_bytes INTEGER NOT NULL, kind TEXT NOT NULL,
    refcount INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, last_used_at TEXT
);

CREATE TABLE index_state (
    branch TEXT PRIMARY KEY, indexed_commit TEXT,
    files INTEGER NOT NULL DEFAULT 0, chunks INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT, last_error TEXT
);
```

Alokacja `seq`: jeden pisarz (koordynator), licznik zasiewany `MAX(seq)`, inkrementowany w tej samej
transakcji co INSERT. Pula połączeń: wzorzec `project_studio/project_db.rs` (LRU 16, sweeper,
migracje przy otwarciu, `checkpoint_all`).

### 5.4 Układ katalogów

```
<data>/code-studio/<workspace_id>/
    repo/                        drzewo odniesienia + metadane git (NIGDY nie montowane)
    worktrees/<session_id>/      worktree roboczy sesji
    worktrees/<session_id>-int/  worktree integracyjny merge'a (§11.6)
    workspace.db
    artifacts/<aa>/<sha256>
    vectors/
    toolchain-cache/base/        zaufany cache read-only
    toolchain-cache/ov/<sid>/    overlay per sesja
    tmp/<session_id>/
```

`<data>` = `paths::category_dir(StorageCategory::Data)` (`paths.rs:40-64`).

---

## 6. Provisioning saga

| Krok | Operacja | Kompensacja |
|---|---|---|
| S1 | Wiersz rejestru `provisioning`, rezerwacja kwot | usunięcie wiersza, zwolnienie kwot |
| S2 | Katalog workspace + `workspace.db` + migracje | `remove_dir_all` |
| S3 | Sekret w vaultcie node-local | skasowanie materiału |
| S4 | `git init` **+ początkowy pusty commit** albo `clone` przez broker, pinowanie fingerprintu | usunięcie `repo/` |
| S5 | Zaufany cache + obraz sandboxa | usunięcie, jeśli nasz |
| S6 | `status='active'` | — |

Kroki idempotentne, identyfikowane `(workspace_id, step)` w `code_workspace_saga_steps`; wznowienie
od pierwszego niepotwierdzonego. Awaria przed S6 → `error` z akcją „ponów", nigdy półżywy `active`.
Ta sama mechanika obsługuje otwarcie sesji (worktree, sandboxy, pin wersji flow).

**Pusty commit dla `repo_kind='empty'`** (S4): świeże `git init` nie ma `HEAD`, więc
`git worktree add` zawodzi, a `base_commit` byłby niezdefiniowany — CAS na referencji i budowa
commitu z drzewa bazowego (§11.5) nie miałyby punktu odniesienia. Dlatego S4 tworzy commit
`chore: initialize workspace` z pustego drzewa i ustawia `default_branch`. Dzięki temu niezmiennik
„każdy workspace ma `base_commit`" obowiązuje bez wyjątków.

---

## 7. Wykonanie i izolacja

### 7.1 Dwa tryby wykonania

PEP autoryzuje wywołania narzędzi Core, a nie syscalle (§9.4). Po uruchomieniu `cargo test`, powłoki
albo CLI dostawcy żadna reguła Core już nie działa — ogranicza wyłącznie system operacyjny. Stąd dwa
tryby o **różnej naturze**, nie o różnym poziomie tego samego:

| Tryb | Charakter |
|---|---|
| `container` | Pełna izolacja: wymuszone `ro`/`cow`/`rw`, kontrolowany egress, bezpieczne uruchamianie **niezaufanego** kodu |
| `trusted_native` | Zaufane wykonanie lokalne jako użytkownik usługi TentaFlow, **bez obietnicy izolacji od hosta** |

**Czego `trusted_native` nie obiecuje** — to jest kontrakt, nie lista zastrzeżeń:

- proces uruchomiony przez agenta czyta i zapisuje wszystko, do czego ma prawo użytkownik usługi
  TentaFlow, łącznie z plikami innych workspace'ów i katalogiem danych Core;
- `ro` i brak `fs_write` dotyczą **narzędzi Core**; nie ograniczają syscalli uruchomionej komendy;
- ruch sieciowy jest nieograniczony, chyba że administrator sam skonfiguruje zaporę (§7.6);
- `egress_policy = local_only` jest niedostępna, a `autonomous` wyłączony (§9.5) — serwer odrzuca
  oba, niezależnie od tego, co pokaże UI;
- izolacja **między sesjami** tego samego węzła nie jest gwarantowana: procesy działają jako ten sam
  użytkownik, więc mogą sięgnąć nawzajem swoich katalogów i pamięci.

**Co działa w obu trybach tak samo** — i to jest istotne, bo tryb bez izolacji traci mniej, niż się
wydaje:

- broker git biegnie poza procesem agenta i **nie ufa plikowi `.git` z worktree** (§11.1);
- commit powstaje z zaakceptowanych blobów, nie z worktree (§11.5);
- `git_push`, `git_merge` i `secret_manage` zawsze pytają (§9.3);
- materiał poświadczenia dostawcy nigdy nie trafia do procesu CLI — chroni go adapter i ticket
  (§7.5), mechanizm sieciowy, nie systemowy.

Zastrzeżenie do tej listy: proces działający jako ten sam użytkownik co Core **może próbować** obejść
ochronę plikową — odczytać vault z dysku, podłączyć się do gniazda brokera, przeczytać pamięć innego
procesu. Dlatego `trusted_native` jest trybem **zaufanym**, a nie sandboxem, i tak ma być opisany.

**Opcjonalne utwardzenie bez zarządzania kontami przez użytkownika:** instalator może utworzyć jedno
stałe konto `tentaflow-worker`, pod którym uruchamiane są procesy agentów, a vault i katalog danych
Core zostają zamknięte dla tego konta. Chroni to Core i sekrety, **nie** izoluje workspace'ów od
siebie. Konto powstaje raz, przy instalacji; nikt nim ręcznie nie zarządza. Świadomie **nie**
budujemy kont per workspace ani per sesja — wymagałyby uprzywilejowanego launchera, principali per
sesja i sprzątania osieroconych kont, czyli dużego podsystemu w zamian za izolację i tak słabszą niż
kontener.

**Domyślny jest `trusted_native`**; `container` jest świadomym wyborem mocniejszej izolacji. Tryb
jest **niezmienny** po założeniu — inaczej sesja rozpoczęta pod jednymi gwarancjami kończyłaby się
pod innymi. Ponieważ domyślną wartością jest tryb bez izolacji, obowiązują dwa zabezpieczenia przed
„cichym" wyborem: założenie workspace zawsze emituje zdarzenie audytowe z rozstrzygniętym trybem,
a UI trwale oznacza workspace natywny (§19) — nie tylko przy zakładaniu.

### 7.2 Profile: montowanie × sieć

`mount_access` i `network_access` są **niezależne**; sandbox jest identyfikowany parą (plus flagą
`ephemeral`).

| `mount_access` | Worktree | Zastosowanie |
|---|---|---|
| `ro` | read-only bind | odczyt, przegląd, narzędzia niepiszące |
| `cow` | widoczny do zapisu, zmiany w **jednorazowej** warstwie górnej, odrzucane po operacji | **testy i buildy** — `target/`, `bin/obj`, `.gradle`, `node_modules` powstają normalnie, ale nie wracają do worktree |
| `rw` | read-write bind | implementer, terminal właściciela/editora |

| `network_access` | Znaczenie |
|---|---|
| `none` | brak trasy; jedyne wejście/wyjście to gniazdo shimu git |
| `gateway` | wyłącznie przez bramkę egress (§7.6) |

Przydział: PEP wylicza parę z capability wywołującego (§9.3). `code-tester` dostaje `cow`+`none`
(albo `gateway`, jeśli build wymaga pobrania zależności i sesja ma `net_egress`), `code-reviewer`
`ro`+`none`, `code-implementer` `rw`+`none`, agent CLI `rw`/`cow` + `gateway`, terminal — parę
odpowiadającą roli użytkownika.

Sandbox `cow` z flagą `ephemeral=1` jest niszczony po zwolnieniu lease razem z warstwą górną, więc
dwa przebiegi testów nigdy nie dziedziczą po sobie stanu. Realizacja: Linux overlayfs;
macOS/Windows — kopia przy pierwszym zapisie w katalogu jednorazowym.

**Lease.** Sandbox współdzielony (`ephemeral=0`, `lease_id IS NULL`) jest jeden na profil w sesji.
Sandbox `ephemeral` dostaje `lease_id` i `owner_run_id`, więc **dwa równoległe procesy o tym samym
profilu są możliwe** — np. tester i reviewer pracujący jednocześnie albo dwa przebiegi testów.
Unikalność w schemacie obejmuje wyłącznie sandboxy współdzielone (§5.3). Zwolnienie lease niszczy
warstwę górną i zatrzymuje kontener.

Cache toolchaina: `toolchain-cache/base` read-only + overlay `toolchain-cache/ov/<session_id>`
(przeżywa sesję, w odróżnieniu od `ephemeral`). Współdzielony cache RW jest wektorem zatrucia i nie
występuje.

Limity (`SandboxLimits::code_session`): rootfs read-only, `cap_drop: ALL`, `no-new-privileges`,
użytkownik nie-root, brak gniazda dockera, 8 GiB RAM, 4 CPU, 1024 PID.

**Realizacja w `trusted_native`.** Ten sam model pojęciowy, ale bez egzekucji przez OS:

- `rw` → proces startuje z katalogiem roboczym w worktree;
- `ro` → proces startuje w worktree, a Core **nie daje mu narzędzi zapisu**; ograniczenie dotyczy
  narzędzi, nie syscalli — to jest granica trybu, nie jego wada do ukrycia;
- `cow` → **kopia worktree** do katalogu jednorazowego, tworzona przez reflink (`copy_file_range`
  / `FICLONE` na Btrfs i XFS, klon APFS na macOS, block clone na ReFS), a gdy system plików tego nie
  wspiera — pełna kopia. Proces dostaje kopię jako katalog roboczy i tam trafiają `target/`,
  `node_modules`, `bin/obj`;
- `network` → zależnie od `egress_enforcement` (§7.6).

**Fail-closed przy `cow`.** Gdy kopia jest niemożliwa albo przekracza budżet (rozmiar, czas), operacja
jest **odmawiana**, a nie po cichu wykonywana na prawdziwym worktree. Automatyczna degradacja
`cow → rw` z rewizji 1.5 jest usunięta: dawała testerowi bez `fs_write` zapis do drzewa roboczego,
czyli naruszała uprawnienie, którego ostrzeżenie nie naprawia. Praca na prawdziwym worktree wymaga
osobnej, interaktywnej zgody `profile_degrade_rw` (§9.3) — jednorazowej, z jawnym opisem skutku,
nigdy z allowlisty i nigdy automatycznie.

### 7.3 `.git` nie istnieje w sandboxie; shim ma token związany z aktorem

Metadane git nie są montowane — worktree w sandboxie to zwykłe drzewo plików. `rm -rf .git`, podmiana
hooka i edycja `config` nie mają celu.

W sandboxie jest `git`-**shim**, który zamienia wywołanie na RPC do brokera przez gniazdo uniksowe.
**Token shimu jest związany z `(session_id, run_id, actor_user_id, zbiór capability, ważność)`** —
nie z samą sesją. Konsekwencje:
- proces uruchomiony przez `code-tester` nie ma tokenu z `git_stage`; nie chodzi tylko o odmowę
  w PEP, ale o to, że jego token w ogóle o to nie prosi;
- token wygasa razem z runem; terminal ma własny token związany z aktorem-człowiekiem;
- shim obsługuje wyłącznie podzbiór z §10 i każde wywołanie przechodzi przez PEP.

Narzędzia czytające `.git` bezpośrednio (nie przez `git`) zobaczą brak repozytorium. To znany koszt,
opisany w UI i w instrukcji.

**W `trusted_native`** metadanych git nie da się ukryć — proces ma prawa użytkownika usługi. Ochroną
nie jest tu system plików, tylko to, że **broker nie ufa niczemu, co leży w worktree** (§11.1):
używa własnej mapy `session_id → git_dir + work_tree` i jawnych `--git-dir`/`--work-tree`, więc
podmieniony wskaźnik `.git` nie skieruje go na cudze repozytorium.

### 7.4 Terminal

Startuje w sandboxie z parą profili odpowiadającą roli użytkownika. Niedostępny w trybie `plan`.
Każde uruchomienie powłoki i każdy proces główny to zdarzenie (kind `exec`) z argv **zredagowanym**
przed zapisem (§13.4). Operacje git idą przez shim, więc podlegają PEP — w tym obowiązkowe pytanie
przy `git_push`/`git_merge`.

### 7.5 Agent CLI dostawcy — adapter i ticket

Projekt z rewizji 1.2 (bramka podmienia nagłówek) był **niewykonalny**: proxy `CONNECT` widzi host
i SNI, ale nie wnętrze TLS. Poprawny mechanizm:

```
CLI (sandbox, base URL → adapter)  ──TLS do adaptera──▶  adapter dostawcy (owner node)
   ticket zamiast poświadczenia                              │ waliduje ticket
                                                             │ wstrzykuje materiał z vaultu
                                                             │ mierzy zużycie i budżet
                                                             └──TLS──▶ dostawca
```

- **Adapter** to reverse proxy terminujący TLS lokalnie, jeden na engine, uruchamiany przez
  koordynatora. CLI jest na niego przekierowane **nadpisaniem base URL** — czy dany CLI to
  umożliwia, jest przedmiotem twardej weryfikacji w Fazie 0B.
- **Skąd adapter bierze materiał**: z `code_agent_credentials` po kluczu
  `(org_id, node_id, engine_id)` (§5.2). Konto jest organizacyjne, ale materiał jest node-local, bo
  klucz `SettingsCipher` jest per węzeł; wiersz niesie też `provider_base_url`, czyli upstream, do
  którego adapter forwarduje. Brak wiersza = `credential_missing`, a `delegate_cli` odmawia startu.
- **Wydanie ticketu wymaga capability `cli_delegate`** (§9.2) i przechodzi przez PEP jak każda inna
  operacja — sam fakt posiadania `net_egress` nie wystarcza.
- **Ticket** to krótkotrwały sekret związany z `session_id`, `run_id`, `cli_instance_id`, dozwolonym
  modelem, dozwolonymi metodami i ścieżkami oraz budżetem tokenów/kosztu. Adapter odrzuca wszystko,
  co nie pasuje. Wykradziony ticket daje najwyżej to, co i tak wolno temu runowi, i wygasa z nim.
- **Poświadczenie organizacji nigdy nie wchodzi do sandboxa** — ani jako plik, ani jako zmienna, ani
  jako nagłówek widziany przez proces CLI.
- Certyfikat adaptera jest zaufany wyłącznie w tym sandboxie (własny sklep CA sesji), więc nie
  osłabia zaufania hosta.

**Go/no-go Fazy 0B, per CLI.** Jeśli dany CLI nie da się przekierować na base URL albo prowadzi
własne odświeżanie poświadczenia poza tym kanałem — jego integracja jest **wyłączona**. Nie ma
wariantu „słabszy fallback": obietnica izolacji albo jest dotrzymana, albo funkcji nie ma.

### 7.6 Bramka egress

`network_mode=bridge` nie filtruje hostów i nie jest mechanizmem. Zamiast tego: sandbox bez trasy
domyślnej, jedyne wyjście do bramki; bramka jest jawnym proxy HTTP/HTTPS, rozstrzyga DNS sama i
przypina adres, sprawdza IP względem allowlisty i listy zakazanej (metadata chmurowe, control-plane,
loopback), sprawdza port, weryfikuje SNI przy `CONNECT`, nie podąża za przekierowaniem poza
allowlistę. Protokoły inne niż HTTP/HTTPS nie są routowalne (`ssh`, `git://` nie mają jak wyjść — cały
git jest w brokerze). Ruch do dostawcy modeli idzie przez adapter (§7.5), nie przez `CONNECT`.
Każde żądanie i każda odmowa to zdarzenie.

**`egress_enforcement`** mówi wprost, czym polityka sieciowa jest na danym workspace:

| Wartość | Mechanizm | Co obiecujemy |
|---|---|---|
| `namespace` | kontener bez trasy domyślnej + bramka | filtrowanie hostów, portów, SNI i przekierowań; pełny audyt żądań |
| `firewall` | reguła zapory po właścicielu procesu (Linux `--uid-owner`, macOS PF, Windows WFP), skonfigurowana przy instalacji węzła | filtrowanie i audyt na poziomie zapory |
| `unrestricted` | brak | **nic** — bez filtrowania i **bez audytu hostów** |

Kluczowa poprawka względem 1.5: przy `unrestricted` nie obiecujemy „doradczej allowlisty" ani
odnotowywania naruszeń. Bez haka w jądrze Core nie widzi połączeń wychodzących z cudzego procesu,
więc nie może ich raportować — deklarowanie tego byłoby fikcją audytową. `egress_policy = local_only`
jest dostępna wyłącznie przy `namespace` albo `firewall`, i to **serwer** ją odrzuca (§9.5), nie UI.

Ochrona materiału poświadczenia dostawcy pozostaje pełna także przy `unrestricted`, bo opiera się na
adapterze i tickecie, a nie na trasie sieciowej (§7.5).

**Gniazda lokalne.** Broker, adapter i bridge nasłuchują na gniazdach uniksowych / nazwanych potokach
z kontrolą peera, a nie na `127.0.0.1` — w trybie natywnym pętla zwrotna jest otwarta dla każdego
procesu na hoście. Uczciwe zastrzeżenie: gdy proces agenta działa jako **ten sam** użytkownik co
Core, kontrola peera go nie odróżni. Wtedy jedynym rozróżnieniem jest token shimu, a zabezpieczeniem
przed jego nadużyciem — jego związanie z runem i capability (§7.3) oraz dziennik operacji.

### 7.7 Kontrakt executora

```rust
#[async_trait]
pub trait WorkspaceExecutor: Send + Sync {
    async fn read(&self, p: &RelPath, range: Option<LineRange>) -> Result<FileSlice>;
    async fn write(&self, p: &RelPath, content: &[u8], expect: Precondition) -> Result<WriteOutcome>;
    async fn edit(&self, p: &RelPath, edits: &[TextEdit], expect: Precondition) -> Result<WriteOutcome>;
    async fn list(&self, p: &RelPath, depth: u32) -> Result<Vec<DirEntry>>;
    async fn glob(&self, pattern: &str, limit: usize) -> Result<Vec<RelPath>>;
    async fn grep(&self, q: &GrepQuery) -> Result<GrepResult>;
    async fn stat(&self, p: &RelPath) -> Result<FileStat>;
    async fn mkdir(&self, p: &RelPath) -> Result<()>;
    async fn remove(&self, p: &RelPath, recursive: bool, expect: Precondition) -> Result<()>;
    async fn rename(&self, from: &RelPath, to: &RelPath, expect: Precondition) -> Result<()>;
    async fn exec(&self, sb: SandboxProfile, req: &ExecRequest, sink: Arc<dyn ExecSink>) -> Result<ExitStatus>;
    async fn cancel_exec(&self, exec_id: &str) -> Result<()>;
    async fn pty_open(&self, sb: SandboxProfile, req: &PtyOpen) -> Result<PtyHandle>;
    async fn pty_write(&self, h: &PtyHandle, bytes: &[u8]) -> Result<()>;
    async fn pty_resize(&self, h: &PtyHandle, rows: u16, cols: u16) -> Result<()>;
    async fn pty_close(&self, h: &PtyHandle) -> Result<()>;
}

pub struct SandboxProfile { pub mount: MountAccess, pub network: NetworkAccess, pub ephemeral: bool }
pub enum Precondition { Absent, BlobIs(BlobSha), Any }
```

`Precondition` obejmuje także `remove` i `rename` (P1.2) — usunięcie oczekuje konkretnej zawartości,
a zmiana nazwy dodatkowo nieobecności celu. Operacje git nie są w tym trait — idą przez broker.

### 7.8 Uruchamianie komend

Zawsze wektor argv; powłoka wyłącznie na jawne żądanie, jako jeden zacytowany argument. Zawsze własna
grupa procesów (unix `setsid` + `killpg`, Windows Job Object z `KILL_ON_JOB_CLOSE`). Limity: czas
(domyślnie 300 s), wyjście do kontekstu modelu 1 MiB, liczba równoległych komend. Środowisko: jawna,
minimalna lista; żaden sekret ani ticket nie wchodzi do środowiska komend innych niż proces CLI.

**Co trafia do artefaktu.** Nie surowa linia poleceń i nie surowy strumień wyjścia — artefakt niesie
**kanoniczne dane strukturalne po redakcji** (§13.4): wektor argv element po elemencie ze
zredagowanymi wartościami wyglądającymi na sekret, kod wyjścia, czasy, obcięte i przeskanowane
fragmenty wyjścia. Zapis „pełne wyjście do artefaktów" z wcześniejszej rewizji był sprzeczny
z zasadą redakcji i z własnym testem „brak sekretów w artefaktach": użytkownik może wpisać token
w terminalu, a build może wypisać go w logu. Koszt jest realny — w rzadkich przypadkach redakcja
usunie coś, co było potrzebne do diagnozy; wtedy właściwą drogą jest powtórzenie komendy, a nie
przechowywanie surowej treści.

### 7.9 Terminal — stan VT po stronie serwera

Maszyna VT100 w Rust na owner node (crate `vt100`, już używany w bridge); do przeglądarki płynie
siatka komórek z numerem rewizji. `tf-terminal` renderuje i wysyła klawisze. Zalety: identyczne
zachowanie w kontenerze i zdalnie, scrollback przeżywa reload i restart, brak dużej zależności JS,
po mesh idą zmiany komórek.

---

## 8. Bezpieczny filesystem

`canonicalize` + `starts_with` odrzucone (TOCTOU, brak obsługi tworzenia). Operacje wykonujemy
względem **uchwytu katalogu korzenia sesji**.

| Platforma | Mechanizm |
|---|---|
| Linux | `openat2` z `RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS \| RESOLVE_NO_MAGICLINKS`; fallback `openat` per segment z `O_NOFOLLOW \| O_DIRECTORY` |
| macOS | `openat` per segment z `O_NOFOLLOW`, weryfikacja `st_dev`/`st_ino` |
| Windows | `FILE_FLAG_OPEN_REPARSE_POINT` + odrzucenie reparse points; `GetFinalPathNameByHandle` porównane z korzeniem; odrzucenie ADS, nazw urządzeń, UNC i `\\?\` |

1. Ścieżka względna, normalizowana leksykalnie; `..` odrzucany leksykalnie **i** przez `RESOLVE_BENEATH`.
2. Tworzenie: `O_CREAT | O_EXCL | O_NOFOLLOW` w uchwycie rodzica otwartym tą samą metodą.
3. `rename`/`remove`: `renameat`/`unlinkat` w uchwycie rodzica; usuwanie rekurencyjne po uchwytach.
4. Zapis atomowy: plik tymczasowy w tym samym katalogu + `renameat`.
5. Metadane git chronione **fizycznie** (§7.3) — nie są montowane. Reguła w PEP nie jest tu potrzebna,
   W trybie natywnym `repo/` jest zamknięte uprawnieniami użytkownika agenta (§7.3), a reguła w PEP
   działa jako druga warstwa.
6. Limity: rozmiar pliku, liczba wpisów, głębokość obchodu, budżet `glob`/`grep`.
7. Jedna implementacja dla narzędzia, uploadu i wywołania mesh.

Zależności: `rustix` (albo obecny `libc`, `Cargo.toml:381`) i `windows-sys`.

---

## 9. Zgody, capability i punkt egzekucji

### 9.1 Permission ≠ review

| | **Permission** | **Review** |
|---|---|---|
| Kiedy | przed pojedynczą operacją | przed commitem/merge |
| Przedmiot | capability + cel | snapshot zmian (`patch_set`) |
| Odpowiedź | allow_once / for_run / for_session / always / deny | akceptacja per hunk/plik/całość, odrzucenie, poprawka |
| Trwałość | `approvals`, `session_grants`, `code_workspace_allowlist` | `patch_sets/files/hunks` |
| Blok w grafie | brak (mechanizm poprzeczny) | `patch_review` |

Zakresy: `allow_once`, `allow_for_run` (istniejący `run_grants`, per run —
`agents/interaction.rs:152-159`), `allow_for_session` (`session_grants`), `always`
(`code_workspace_allowlist`).

### 9.2 Macierz RBAC

| Capability | owner | editor | viewer |
|---|---|---|---|
| `fs_read`, `fs_list`, `fs_glob`, `fs_grep`, `code_search` | ✔ | ✔ | ✔ |
| `fs_write`, `fs_delete`, `fs_move`, `fs_mkdir` | ✔ | ✔ | ✖ |
| `exec`, `terminal` | ✔ | ✔ | ✖ (terminal `ro`+`none`) |
| `git_read` | ✔ | ✔ | ✔ |
| `git_branch`, `git_checkout`, `git_stash` | ✔ | ✔ | ✖ |
| `git_network` (fetch/pull) | ✔ | ✔ | ✖ |
| `git_push`, `git_merge` | ✔ | ✖ | ✖ |
| `git_commit`, `git_stage`, `git_worktree` | **system** | **system** | **system** |
| `net_egress` | ✔ | ✔ | ✖ |
| `cli_delegate` (wydanie ticketu adaptera) | ✔ | ✔ | ✖ |
| `review_decide` | ✔ | ✔ | ✖ |
| `session_open` | ✔ | ✔ | ✖ |
| `workspace_settings`, `member_manage`, `secret_manage` | ✔ | ✖ | ✖ |

`git_commit`, `git_stage` i `git_worktree` są **systemowe**: wykonuje je wyłącznie koordynator
(§11.5). Żaden agent modelowy ani terminal ich nie ma — inaczej commit może pominąć review, a sesja
wyjść ze swojej izolacji.

### 9.3 PEP

```rust
pub fn authorize(ctx: &SessionCtx, cap: Capability, target: &Target) -> Decision
// Decision = Allow(SandboxProfile) | AskUser { summary, detail } | Deny { reason }
```

1. capability systemowa, a wołający nie jest koordynatorem → `Deny`;
2. rola nie pozwala → `Deny`;
3. tryb autonomii zakazuje → `Deny`;
4. cel poza granicą (ścieżka spoza worktree, host spoza allowlisty) → `Deny`;
5. **`mandatory_interactive`** (`git_push`, `git_merge`, `secret_manage`) → `AskUser` **zawsze**,
   z pominięciem 6–8; grant `always` dla nich odrzucany przy zapisie, dozwolony wyłącznie `allow_once`;
6. `code_workspace_allowlist` → `Allow`;
7. `session_grants` → `Allow`;
8. grant per-run → `Allow`;
9. tryb autonomii zezwala automatycznie → `Allow`;
10. w pozostałych → `AskUser`.

`Allow` zawsze niesie `SandboxProfile` — nie ma zezwolenia bez wskazania, w jakim profilu operacja
się wykona.

### 9.4 Czego PEP nie robi

PEP autoryzuje **wywołania**, nie syscalle. Proces, który już działa, ogranicza wyłącznie profil
(§7.2) i token shimu (§7.3). Każde nowe capability musi mieć odpowiadający mu profil albo ograniczenie
tokenu — inaczej jest fikcją. Ta reguła obowiązuje przy każdym rozszerzeniu zestawu narzędzi.

### 9.5 Tryby autonomii

| Tryb | Odczyt | `fs_write` | `exec`/`terminal` | `net_egress` | `git_push`/`git_merge` | Commit |
|---|---|---|---|---|---|---|
| `plan` | auto | ✖ | ✖ | ✖ | ✖ | — |
| `normal` | auto | pyta | pyta | pyta | pyta | po review |
| `auto_edit` | auto | auto | pyta | pyta | pyta | po review |
| `autonomous` | auto | auto | auto (allowlista) | allowlista | **pyta zawsze** | **po review** |

Commit nigdy nie jest „automatyczny": w każdym trybie wykonuje go koordynator po zaakceptowanym
patch secie. W `autonomous` automatyczna może być **akceptacja review** (konfigurowalna per
workspace, domyślnie wyłączona), ale sam commit dalej bierze bloby z patch setu, nie z worktree.
Tryb sesji nie przekracza `autonomy_ceiling`.

**Walidacja serwerowa (nie UI).** Handler zapisu workspace i otwarcia sesji odrzuca kombinacje,
których dany tryb nie potrafi dotrzymać — ukrycie opcji w kreatorze nie jest walidacją, bo protokół
binarny jest osiągalny poza nim:

| Kombinacja | Wynik |
|---|---|
| `trusted_native` + `autonomous` | odrzucone; pułapem jest `auto_edit` — automatyczne wykonywanie dowolnych komend bez izolacji nie ma bezpiecznika poza listą nazw |
| `trusted_native` + `egress_policy='local_only'` przy `egress_enforcement='unrestricted'` | odrzucone — polityka bez mechanizmu |
| `container` bez `container_image` | odrzucone |
| zmiana `exec_mode` istniejącego workspace | odrzucone — tryb jest niezmienny (§7.1) |

---

## 10. Narzędzia agenta

| Narzędzie | Capability | Profil | Ograniczenia serwerowe |
|---|---|---|---|
| `core.fs_read` / `fs_list` / `fs_glob` / `fs_grep` | `fs_*` | `ro` | rozmiar, głębokość, limit wyników, timeout regexu |
| `core.fs_write` | `fs_write` | `rw` | `Precondition`, granica worktree |
| `core.fs_edit` | `fs_write` | `rw` | jednoznaczność `old_string`; niejednoznaczność = błąd |
| `core.fs_move` / `fs_delete` / `fs_mkdir` | odpowiednie | `rw` | `Precondition` (także dla delete i rename) |
| `core.exec` | `exec` | wg capability wołającego (`ro`/`cow`/`rw` × `none`/`gateway`) | argv, cwd, timeout, limit wyjścia |
| `core.git_read` | `git_read` | broker | `status,diff,log,show,ls-files` |
| `core.git_branch` | `git_branch` | broker | w obrębie gałęzi sesji |
| `core.git_sync` | `git_network` | broker | `fetch`, `pull` |
| `core.git_push` | `git_push` | broker | `mandatory_interactive`; tylko gałąź sesji |
| `core.code_search` | `code_search` | — | limit, prefiks (od Fazy 7) |
| `core.workspace_info` | `fs_read` | — | bez sekretów i ścieżek hosta |

**Nie ma** `core.git_commit`, `core.git_stage` ani `core.git_merge` w zestawie agenta — commit i merge
wykonuje koordynator (§11.5, §11.6) jako bloki `git_op`, po review.

Reużyte: `core.ask_user`, `core.agent_spawn|wait|list|cancel`, `core.skill_view`,
`core.project_search`, `core.project_list_sources`.

Wspólne: wiązanie sesji z `envelope.meta.code_session` mintowanego przez serwer (wzorzec
`ps_generation`, `project_studio/generation.rs`); wynik obcinany do budżetu `tool_exec`
(`max_result_chars: 16000`, `db/seed.rs:1379`); każde wywołanie tworzy wpis w `session_operations`
i zdarzenia `tool_call`/`tool_result`.

---

## 11. Git — broker na owner node

### 11.1 Architektura

Cały proces git i ssh biegnie w brokerze na owner node, poza sandboxem. Sandbox nie ma metadanych git
(§7.3), nie ma trasy dla ssh (§7.6) i nigdy nie widzi materiału sekretu.

```
sandbox: git-shim ──unix socket + token (session, run, actor, caps, exp)──▶ broker (owner node)
                                                    ├── PEP (capability, mandatory_interactive)
                                                    ├── mapa session_id → git_dir + work_tree
                                                    ├── vault (token / klucz ssh)
                                                    ├── proces `git` z izolowanym configiem
                                                    └── session_operations + zdarzenia
```

**Broker nigdy nie ufa plikowi `.git` z worktree.** Wskaźnik `gitdir:` leży w drzewie, do którego
agent ma prawo zapisu, więc podmieniony mógłby skierować uprzywilejowany proces `git` na inne
repozytorium — także spoza workspace'a. Dlatego broker trzyma **własną mapę**
`session_id → kanoniczny git_dir + kanoniczny work_tree`, ustaloną przy tworzeniu worktree, i każde
wywołanie wykonuje z jawnymi `--git-dir` i `--work-tree`, na uchwytach katalogów (§8), bez
odczytywania czegokolwiek z drzewa roboczego. Dotyczy **obu trybów** — w kontenerze `.git` i tak nie
jest montowany, ale reguła ma być jedna, żeby nie zależała od trybu.

### 11.2 Izolowany config

`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`; `core.hooksPath` na pusty katalog
(**hooki nie są wykonywane**); `credential.helper=` puste; `GIT_TERMINAL_PROMPT=0`;
`core.fsmonitor=false`; `diff.external=`; `*.textconv` wyłączone; `core.pager=cat`;
`protocol.ext.allow=never`; `protocol.file.allow=user`; `GIT_ALLOW_PROTOCOL=https:ssh`;
`http.followRedirects=false`; `submodule.recurse=false`.

### 11.3 Sekrety

Token przez `GIT_ASKPASS` wskazujący pomocnika brokera — nie trafia do linii poleceń, URL ani
`ps`/`cmdline`; wszystko poza sandboxem. Klucz SSH: docelowo agent SSH brokera (klucz nie dotyka
dysku), wariant przejściowy — plik `0600` w katalogu brokera wskazany przez `GIT_SSH_COMMAND`,
kasowany po wywołaniu.

### 11.4 Sieć i host

Pinowanie fingerprintu SSH przy pierwszym kontakcie (pokazany użytkownikowi, zapisany w rejestrze,
dalej `StrictHostKeyChecking=yes`); `accept-new` odrzucone. Adresy prywatne/LAN dozwolone, zawsze
blokowane: metadata chmurowe (`169.254.169.254`, `fd00:ec2::254`), control-plane klastra, loopback
poza zatwierdzonym portem. DNS przypinany na czas operacji; każdy adres wynikowy sprawdzany.
Dodanie repozytorium z adresem prywatnym wymaga `secret_manage` i jest zdarzeniem audytowym.

### 11.5 Commit z zaakceptowanych blobów

Commit **nie** powstaje z worktree. Koordynator, po zaakceptowanym patch secie, buduje go w brokerze
z zapisanych treści — dzięki temu commitowane jest dokładnie to, co człowiek zobaczył w review,
niezależnie od tego, co agent zdążył w międzyczasie zmienić na dysku:

1. dla każdego zaakceptowanego pliku: `git hash-object -w` z treści `accepted_blob_sha` → zapis
   `patch_files.git_blob_oid`;
2. tymczasowy indeks (`GIT_INDEX_FILE` w katalogu brokera) zasiany drzewem `base_commit`, następnie
   `git update-index --cacheinfo <mode>,<oid>,<path>` dla dodań i modyfikacji,
   `--force-remove <path>` dla usunięć, a dla **zmiany nazwy — obie operacje naraz**:
   `--force-remove <old_path>` **i** `--cacheinfo` dla nowej ścieżki (sam `cacheinfo` zostawiłby
   plik pod starą nazwą w drzewie);
3. `git write-tree` → `git commit-tree <tree> -p <base_commit>` z autorem ustawianym przez serwer;
4. `git update-ref refs/heads/<gałąź sesji> <new> <expected_old>` — atomowy CAS na referencji;
5. worktree **nie jest** synchronizowany siłowo. Po commicie `HEAD` gałęzi wskazuje na
   zaakceptowaną treść, a różnica względem plików na dysku (to, co agent zmienił w trakcie review)
   staje się materiałem **kolejnego patch setu**.

**Precondition commitu** (§13.1) to: obecność artefaktu o `accepted_blob_sha`, osiągalność
`base_commit` i wartość referencji równa `expected_old`. **Nie ma** wśród nich wymogu, by worktree
nadal trzymał zaakceptowany hash — to byłoby zaprzeczeniem celu budowania commitu z artefaktów.
Równoległa edycja pliku przez agenta w trakcie review nie blokuje commitu i nie wchodzi do niego.

Wszystkie OID-y (blob, tree, commit, wartość referencji przed i po) trafiają do
`session_operations.result_oids`, co czyni operację weryfikowalną po awarii (§13.1).

### 11.6 Merge przez worktree integracyjny

Merge nie dotyka gałęzi docelowej „w locie". Kluczowe jest **jak** powstaje worktree integracyjny:

1. koordynator odczytuje bieżącą wartość `refs/heads/<target>` jako `expected_old` i zakłada
   worktree integracyjny **odłączony**:
   `git worktree add --detach <path> <expected_old>`.
   **Nie** `git worktree add <path> <target_branch>` — tamta forma melduje gałąź docelową
   w worktree, więc `git merge` przesunąłby referencję docelową natychmiast, przed testami
   i akceptacją. W stanie odłączonym `merge` przesuwa wyłącznie `HEAD` worktree;
2. wynik merge'a jest zapisywany pod **prywatnym refem** `refs/code-studio/integration/<op_id>`, żeby
   przeżył restart i nie padł ofiarą GC, a run rewizji miał do czego wrócić;
3. konflikt **nie jest błędem** — worktree przechodzi w stan `held` z zapisanymi plikami
   konfliktowymi, a run kończy się `revision_requested` (`trigger='merge_conflict'`);
4. na czystym wyniku merge'a uruchamiane są **testy** (profil `cow`, lease własny) i **przegląd
   agenta**, a potem `patch_review` z zakresem merge (§16.2). Czerwone testy →
   `trigger='merge_verify_failed'`, odrzucenie w review → `trigger='review_rejected'`; w obu
   przypadkach worktree zostaje w stanie `held`;
5. **`finalize_merge`** — po rozwiązaniu konfliktu (w runie rewizji) commit scalenia powstaje
   z **zaakceptowanych blobów**, tą samą drogą co zwykły commit (§11.5): `hash-object -w` →
   tymczasowy indeks → `write-tree` → `commit-tree` z **dwoma rodzicami** (`expected_old`
   i szczyt gałęzi sesji). Bez tego kroku commit scalenia pochodziłby z drzewa roboczego, w którym
   agent rozwiązywał konflikt — czyli omijałby review, wbrew regule z §2.2 pkt 5;
6. dopiero po akceptacji: `git update-ref refs/heads/<target> <merge_commit> <expected_old>` —
   atomowo, z oczekiwaną poprzednią wartością. **Zmiana gałęzi docelowej w międzyczasie unieważnia
   całą próbę**: nie wolno tylko odświeżyć `expected_old`, bo przetestowany i zaakceptowany wynik
   dotyczył innej bazy. Operacja wraca do kroku 1 z nowym `expected_old` — nowy merge, nowe testy,
   nowy review. Użytkownik dostaje o tym jawną informację, a nie ciche ponowienie;
7. worktree integracyjny jest usuwany **wyłącznie** po zatwierdzonym `update-ref`, po jawnym
   porzuceniu przez użytkownika albo przy zamknięciu sesji. Usuwanie go po konflikcie lub
   odrzuceniu (jak zakładała rewizja 1.3) zabierałoby kolejnemu runowi stan, na którym ma pracować.
   Prywatny ref jest kasowany razem z worktree.

Domyślnie fast-forward; brak FF wymaga jawnej decyzji użytkownika. `git_merge` jest
`mandatory_interactive` i przysługuje wyłącznie roli owner.

### 11.7 Worktree sesji

`git worktree add worktrees/<session_id> -b cs/<user_slug>/<session_short>`; `repo/` nigdy nie jest
montowane; `git_worktree` jest systemowe; zamknięcie sesji usuwa worktree, gałąź podlega retencji (§25.2).

---

## 12. Transport mesh

### 12.1 `SessionAssertion`

Podpis węzła potwierdza **węzeł**, nie aktora, więc asercję wystawia system tożsamości.

```
nagłówek: alg=Ed25519, kid
claims:   iss, sub (user_id), aud (owner node id), org, workspace, session,
          caps, rbac_rev, iat, nbf, exp (≤ 120 s), jti
```

Wymagania:
- **klucze asymetryczne** Ed25519 z `kid`, rotacja z oknem nakładania (dwa aktywne `kid`),
  dystrybucja jak istniejące klucze HMAC do peerów zaufanych;
- **wystawca związany z kanałem**: `iss` musi odpowiadać uwierzytelnionemu peerowi połączenia mesh —
  zaufany węzeł nie może przedstawić asercji wystawionej rzekomo przez inny węzeł;
- **anty-replay trwały**: `jti` w tabeli `session_assertion_jti` (§5.2) z TTL ≥ `exp`, a nie
  w cache w pamięci — cache znika przy restarcie i otwiera okno powtórki dokładnie wtedy, gdy węzeł
  jest najbardziej podatny. Sprzątanie po `expires_at`;
- **wiązanie z operacją**: asercja użyta do operacji mutującej niesie `op_id` i digest argumentów;
  powtórka może więc co najwyżej odtworzyć operację, którą `session_operations` i tak deduplikuje po
  `op_id` (§13.1). Dziennik operacji jest drugą, niezależną linią obrony;
- `aud` = ten węzeł; `nbf`/`exp` sprawdzane;
- `rbac_rev` porównane z lokalną rewizją; rozjazd → ponowne rozwiązanie uprawnień, przy niepowodzeniu
  odmowa.

Po weryfikacji asercji owner node i tak wykonuje pełną autoryzację lokalną
(`PermissionMatrix::has_permission`, członkostwo, rola, PEP, containment).

**SLA cofnięcia uprawnień — uczciwie:** zmiana wykonana **na owner node** obowiązuje natychmiast
(lokalne sprawdzenie przy każdej operacji). Zmiana wykonana na innym węźle obowiązuje po
**opóźnieniu synchronizacji + do `exp` asercji (≤ 120 s)**. Deklaracja „natychmiast" z rewizji 1.2
była nieprawdziwa dla zmian zdalnych. Dla operacji nieodwracalnych (`git_push`, `git_merge`,
`secret_manage`) owner node dodatkowo odpytuje o świeżość uprawnienia przed wykonaniem — koszt
jednego round-tripu jest tu akceptowalny.

### 12.2 Strumienie

Ramka `(session_id, stream_id, seq, ack)` po UFP/2, wzorowana na `MeshLogChunk`/`MeshDeployProgress`
(`mesh.rs:136-156`) i transferze po `ALPN_ARTIFACT` (`ml_studio/mesh_artifact.rs`).
Backpressure oknem kredytowym (producent blokuje się, nie buforuje bez ograniczeń; wyjście ponad
limit do artefaktu). Reconnect od `after_seq` z bufora N ramek; dla terminala — pełny zrzut siatki VT
plus rewizja. Dedupe po monotonicznym `seq`. Zamknięcie przy końcu sesji, unieważnieniu asercji albo
utracie zaufania, z jawnym powodem.

---

## 13. Trwałość efektów, patchy i artefaktów

### 13.1 Dziennik operacji

Unikalny `idempotency_key` zdarzenia chroni przed podwójnym **zapisem zdarzenia**, nie przed podwójnym
**wykonaniem operacji**. Dlatego każdy efekt przechodzi przez `session_operations`.

**`op_id` z krotki pochodzenia** — nie tylko `(run_id, tool_call_id)`, bo operacje pochodzą także
z terminala, UI, shimu i bloków flow:

```
op_id = H(session_id, origin_kind, origin_id, logical_step)
origin_kind ∈ {tool_call, terminal, ui, shim, flow_block, coordinator}
origin_id:   tool_call_id | pty_handle+seq | request_id | shim_call_id | node_id+iteracja | saga_step
```

**Typowany zapis**: `op_kind`, trwałe `input_ref`, `precondition_cbor`, `postcondition_cbor`,
`result_oids`. `input_ref` niesie **kanoniczne dane strukturalne po redakcji** (§7.8): dla `fs_write`
treść docelową (to nie jest sekret — to zawartość repozytorium, którą i tak commitujemy), dla `exec`
zredagowany wektor argv i parametry. Po awarii wiadomo, co miało się stać, bez przechowywania
surowej linii poleceń.

Przebieg: INSERT `pending` z pre/postcondition → wykonanie → `completed`/`failed` z `result_oids`
w tej samej transakcji co zdarzenie.

Uzgadnianie po restarcie:

| Sytuacja | Postępowanie |
|---|---|
| `pending`, postcondition **spełniony** | `completed` — efekt zaszedł przed awarią |
| `pending`, postcondition niespełniony, precondition spełniony, **`idempotent = 1`** | bezpieczne ponowienie |
| `pending`, postcondition niespełniony, **`idempotent = 0`** | `unknown` — **niezależnie od precondition** |
| `pending`, żaden nie jest spełniony | `unknown` + sonda po `result_oids` (commit: istnienie obiektu i wartość referencji; push: porównanie referencji zdalnej) |
| `unknown` bez rozstrzygnięcia | pozycja do decyzji użytkownika. **Nigdy ciche ponowienie** |

Gating na `idempotent` jest istotny: `exec` mógł się wykonać i wywołać skutki poza zasięgiem
postcondition (wysłać żądanie, zmienić stan zewnętrzny), a spełniony precondition niczego o tym nie
mówi. `exec`, `git_push` i `git_merge` mają `idempotent = 0` z definicji; `fs_write`, `fs_edit`,
`fs_mkdir` i budowa commitu z artefaktów mają `1`, bo ich skutek jest w całości opisany hashem
albo OID-em.

Postcondition są weryfikowalne z definicji: dla `fs_write` to hash pliku, dla `git_commit` istnienie
commita o danym drzewie i rodzicu oraz wartość referencji, dla `exec` — zapisany kod wyjścia.

### 13.2 CAS

Rozdzielamy **niezmienny** stan bazowy patch setu od **ruchomego** stanu bieżącego:

| Pole | Znaczenie |
|---|---|
| `patch_base_blob_sha` | zawartość przy otwarciu patch setu — nigdy się nie zmienia |
| `current_blob_sha` | zawartość po ostatniej zaakceptowanej operacji — rośnie z każdą edycją |
| `accepted_blob_sha` | wynik decyzji review (pełnej lub częściowej) |

| Operacja | `Precondition` | Skutek |
|---|---|---|
| `write`/`edit` (pierwsza) | `BlobIs(patch_base_blob_sha)` | ustawia `current_blob_sha` |
| `write`/`edit` (kolejna) | `BlobIs(current_blob_sha)` | aktualizuje `current_blob_sha` |
| `create` | `Absent` | ustawia `current_blob_sha` |
| `delete` | `BlobIs(current_blob_sha)` | `current_blob_sha = NULL`, `change_kind='delete'` |
| `rename` | źródło `BlobIs(current)`, cel `Absent` | przenosi wpis, zachowuje `patch_base_blob_sha` |
| review | `BlobIs(accepted_blob_sha ?? current_blob_sha)` | domyka decyzję; brak zapisu w worktree |
| commit / `finalize_merge` | **nie dotyczy worktree**: obecność artefaktu `accepted_blob_sha`, osiągalność `base_commit`, referencja = `expected_old` (§11.5) | commit z blobów; stan worktree bez znaczenia |
| revert | `BlobIs(current_blob_sha)` | przywraca `patch_base_blob_sha` |

Reguła z 1.2 („apply oczekuje `base`") była poprawna tylko dla pierwszej edycji; druga edycja tego
samego pliku zawsze zgłaszałaby konflikt.

**Częściowa akceptacja hunków**: zaakceptowane hunki są składane trójstronnie na
`patch_base_blob_sha`, wynik zapisujemy jako `accepted_blob_sha`, a plik jest przepisywany pod CAS
z oczekiwaniem `current_blob_sha`. Nieczyste złożenie (nakładające się konteksty) → `conflicted`
i decyzja na poziomie całego pliku, bez zgadywania.

### 13.3 Zdarzenia są źródłem prawdy

Kolumny `status` to projekcja. Zdarzenie i projekcja zapisywane w jednej transakcji; przy starcie
koordynatora projekcja jest weryfikowana względem ogona zdarzeń, rozjazd rozstrzygają zdarzenia,
korekta jest logowana. UI odtwarza oś czasu ze zdarzeń (kursor `seq`).

### 13.4 Audyt: trwały outbox i redakcja

Zdarzenia `security_relevant = 1` (zgody i odmowy, dostęp do sekretu, wydanie ticketu, egress,
`git_push`/`merge`, dopisanie administratora do członków, zmiana autonomii lub allowlisty) trafiają
do `audit_outbox` **w tej samej transakcji** co zdarzenie, a osobna pętla dostarcza je do `audit_log`
w bazie głównej z ponawianiem i wykładniczym backoffem. Bezpośredni zapis do dwóch baz bez outboxa
gubiłby ślad przy awarii między nimi.

**Redakcja przed zapisem**: argv, wyjście komend i URL przechodzą przez scrubber (wzorce tokenów,
kluczy, nagłówków `Authorization`, ciągów o wysokiej entropii w pozycjach typowych dla sekretów).
Pełne argv może zawierać token wpisany ręcznie przez użytkownika w terminalu — bez redakcji audyt
sam stałby się wyciekiem. Środowisko procesów nie jest logowane nigdy.

### 13.5 Retencja, kwoty, GC, backup

| Dane | Retencja | GC |
|---|---|---|
| `session_events` | 90 dni po zamknięciu sesji | wsadowo po `seq`; audyt ma mirror |
| Artefakty (CAS) | 30 dni od `last_used_at` | refcount = 0 i po terminie |
| Patch sety | do zamknięcia sesji + 30 dni | razem z sesją |
| Worktree robocze | do zamknięcia sesji | `git worktree remove` |
| Worktree integracyjne + `refs/code-studio/integration/<op_id>` | do zatwierdzonego `update-ref`, porzucenia przez użytkownika albo zamknięcia sesji — stan `held` **nie** podlega GC | `git worktree remove` + kasowanie prywatnego refu |
| Gałęzie sesji | 30 dni, chyba że wypchnięte/zmergowane/przypięte | z raportem, nigdy cicho |
| Overlay `ephemeral` | do końca operacji | razem z sandboxem |
| Sekrety | do rotacji/usunięcia | natychmiast przy usunięciu workspace |

Kwoty domyślne: 10 workspace'ów na użytkownika, 3 równoległe sesje, 20 GiB na workspace, 2 GiB
artefaktów na sesję, 1 GiB artefaktu, 4 równoległe komendy, 10 runów rewizji na sesję.

Backup: `workspace.db` w reżimie checkpointu WAL jak bazy projektów; kopia = snapshot bazy
(`VACUUM INTO` po `TRUNCATE`, wzorzec `project_studio/archive.rs`) + repozytorium, bez sekretów.
Usunięcie: dane na owner node → tombstone → potwierdzenie; niedokończone wznawiane przy starcie.

---

## 14. Indeks semantyczny (Faza 7)

Przestrzeń `addon_id = "cs-<workspace_id>"`, namespace `code`, tworzona przy katalogu workspace przez
`NamespaceManager::get_or_create_at` (`services/vector/namespace.rs:654`); kwoty per (org, addon)
obowiązują. Embeddingi przez alias `rag-embeddings`. Zakres: obchód szanujący `.gitignore` (czytany
przez broker) + wykluczenia (`node_modules`, `target`, `dist`, binaria, > 2 MiB). Chunking po
granicach składniowych tam, gdzie tanie, inaczej okna z zakładką; metadane
`{path, lang, start_line, end_line, commit, branch}`. Odświeżanie inkrementalne po zaakceptowanym
patch secie, `checkout`, `pull`, `merge`; debounce, kolejka jednowątkowa per workspace, budżet czasu.
`index_state` per gałąź; rozjazd = miękka degradacja.

**Grep pozostaje autorytatywny.** Do Fazy 7 `core.code_search` nie istnieje — Faza 5 nie zależy od Fazy 7.

---

## 15. Agenci

| Agent | Rola | Capability | Profil sandboxa |
|---|---|---|---|
| `code-orchestrator` | rozmowa, podział zadania, **spawnowanie wykonawców**, scalanie wyników | `agent_spawn/wait/list/cancel`, `ask_user`, odczyt, `workspace_info` | `ro`+`none` |
| `code-planner` | plan, dekompozycja, ryzyka | wyłącznie odczyt | `ro`+`none` |
| `code-implementer` | pisze kod | odczyt + `fs_write` + `exec` | **`rw`+`none`** — bez `git_commit` |
| `code-searcher` | znajduje miejsca zmian | odczyt (+ `code_search` od Fazy 7) | `ro`+`none` |
| `code-reviewer` | przegląd zmian | odczyt + `git_read` | `ro`+`none` |
| `code-tester` | uruchamia testy | odczyt + `exec` z allowlistą | **`cow`+`none`** (`gateway`, gdy build pobiera zależności); warstwa odrzucana po teście |
| `claude-code` / `codex` | delegacja do CLI | brak narzędzi core | `rw`/`cow` + `gateway`, ticket do adaptera |

Zakres egzekucji zależy od trybu (§7.1):
- **`container`** — reviewer i tester **fizycznie** nie zapiszą w worktree (montowanie `ro`), a tester
  i tak zbuduje projekt w warstwie COW;
- **`trusted_native`** — brak `fs_write` odcina im **narzędzia zapisu**, a `cow` daje kopię worktree
  jako katalog roboczy, więc zwykły build nie dotyka drzewa. Proces, który sam sięgnie po oryginalną
  ścieżkę, nie jest powstrzymany — to jest granica trybu, nazwana wprost, nie przeoczenie.

W obu trybach implementer nie wypchnie i **nie zacommituje** gałęzi, bo `git_commit`, `git_stage`
i `git_push` są systemowe albo `mandatory_interactive`, a broker biegnie poza jego procesem.
Prompt nie jest zabezpieczeniem w żadnym trybie.

Agent CLI ma własny `flow_id` (`trigger → workspace_context → delegate_cli → persist_turn → output`),
więc delegacja też jest grafem; wnętrze pętli dostawcy pozostaje nieprzezroczyste i tak opisane w UI.

---

## 16. Flow „Code Harness"

### 16.1 Ograniczenia rdzenia i wynikający z nich kształt

Z `validation.rs:665-760` i `executor.rs:713-800`: region ma dokładnie jedną krawędź `loop_back`;
wejście zewnętrzne tylko do węzła wejściowego, wyjście tylko z wyjściowego; **każda inna krawędź
liczy się do in-degree**, więc powrót do wcześniejszego etapu to cykl; **jedyny warunek stopu to
strukturalny** `last_assistant_has_tool_calls`, a `LoopRegion` niesie tylko `max_iterations`
i `final_pass`.

Wniosek: **pętla deterministyczna nie jest dziś wyrażalna w grafie.** Iteracja ma dwie legalne postacie:

- **wewnątrz runu** — pętla narzędziowa agenta (region `agent_turn` w jego flow): agent sam uruchamia
  testy jako wywołanie narzędzia i iteruje, dopóki woła narzędzia;
- **na poziomie sesji** — **nowy run** (`session_runs.kind='revision'`) z uwagami jako wejściem.

Graf harnessu jest acykliczny, a iteracja widoczna jako łańcuch runów na osi czasu.

**Opcjonalne rozszerzenie rdzenia (wycenione, nie zakładane):** `stop_expr` (CEL) w `LoopRegion` —
parsowanie z configu wejścia, ewaluacja po iteracji w alternatywie ze stopem strukturalnym,
walidacja w R11, test regionu deterministycznego. ~1 dzień w `cache.rs` + `executor.rs` +
`validation.rs`. Decyzja przy Fazie 5; plan działa bez tego.

### 16.2 Graf (acykliczny)

```
trigger
 → conversation_history
 → workspace_context             🆕 wiązanie sesji, stan repo z brokera, AGENTS.md/CLAUDE.md, toolchain
 → agent_context
 → condition run_kind            'user' → pytaj o zakres; 'revision' → wejdź z uwagami
 → ask_user „doprecyzuj zadanie"   ≤ 4 propozycje + własna; timeout → sentinel
 → condition intent               [praca] | [delegacja CLI] | [anuluj]

 ── praca ──────────────────────────────────────────────────────────────────────
 → spawn(agent=code-planner) → await_subagents(all)
 → ask_user „zatwierdź plan"       Zatwierdź | Popraw | Podziel | Anuluj (+ własna)
 → condition plan_decision_1
      ├ Anuluj ──▶ output(anulowano)
      ├ Popraw / Podziel ──▶ spawn(code-planner, feedback) → await
      │                       → ask_user „zatwierdź poprawiony plan"   (Zatwierdź | Anuluj | Przekaż dalej)
      │                       → condition plan_decision_2
      │                             ├ Anuluj ──────▶ output(anulowano)
      │                             ├ Przekaż dalej ▶ persist_turn → output(revision_requested)
      │                             └ Zatwierdź ────┐
      └ Zatwierdź ───────────────────────────────────┤
                                                     ▼
 → spawn(agent=code-implementer) → await_subagents(all)     ⟵ JAWNY wykonawca
 → spawn(code-reviewer) + spawn(code-tester) → await_subagents(mode=all)
 → condition verify
      ├ czerwone ─▶ persist_turn → output(revision_requested, trigger=test_failed)
      └ zielone ──▶
 → patch_review                  🆕 snapshot sprawdzony przez agenta i testy
 → condition review_decision
      ├ Odrzuć / Popraw ─▶ persist_turn → output(revision_requested, trigger=review_rejected)
      └ Akceptuj ────────▶ git_op(commit)   ⟵ KOORDYNATOR, z blobów patch setu (§11.5)
 → ask_user „wypchnąć / scalić?"   Nie | Wypchnij gałąź | Scal do <target_branch>
 → condition delivery
      ├ Nie ──────▶ persist_turn → output
      ├ Wypchnij ─▶ git_op(push) → condition(ok?) → [błąd: ask_user(ponowić?)] → persist_turn → output
      └ Scal ─────▶ git_op(merge_integration)        ⟵ worktree --detach na expected_old (§11.6)
                    → condition merge_conflict?
                        ├ konflikt ─▶ persist_turn → output(revision_requested, merge_conflict)
                        │             [worktree integracyjny ZOSTAJE w stanie held]
                        └ czysto ───▶ spawn(code-tester) + spawn(code-reviewer)   ⟵ NA WYNIKU MERGE'A
                                      → await_subagents(all)
                                      → condition verify_merge
                                          ├ czerwone ─▶ persist_turn
                                          │             → output(revision_requested, merge_verify_failed)
                                          └ zielone ──▶ patch_review(scope=merge)
                                               ├ Odrzuć ─▶ persist_turn
                                               │           → output(revision_requested, review_rejected)
                                               └ Akceptuj ▶ git_op(finalize_merge)  ⟵ commit z blobów
                                                            → git_op(update_target_ref)
                                                            → persist_turn → output(stream)

 ── delegacja CLI ──────────────────────────────────────────────────────────────
 → delegate_cli 🆕 → patch_review → (jak wyżej)
```

**Domknięcie ścieżki planu (P1.6):** w jednym runie dopuszczamy **najwyżej jeden** obrót poprawy planu.
`plan_decision_2` ma trzy wyjścia i żadne nie wraca — kolejna poprawka kończy run jako
`revision_requested`, a koordynator startuje run rewizji. Bez tego ograniczenia ścieżka wymagałaby
krawędzi wstecznej, której graf nie przyjmie.

### 16.3 Pętla sesji

Koordynator, widząc run zakończony z `revision_requested`, tworzy kolejny `session_runs` z `trigger`
i uwagami. Oś czasu pokazuje łańcuch runów z powodem każdego nawrotu. Limit rewizji na sesję
(domyślnie 10); po przekroczeniu obowiązkowe pytanie do użytkownika zamiast automatycznego nawrotu.

**Run rewizji po nieudanym merge'u pracuje na zachowanym worktree.** Dla triggerów
`merge_conflict` i `merge_verify_failed` `workspace_context` wiąże run z worktree integracyjnym
w stanie `held` (a nie z roboczym), bo to tam są pliki konfliktowe i wynik scalenia. Po rozwiązaniu
run przechodzi `finalize_merge` (commit z zaakceptowanych blobów), a potem tę samą ścieżkę
weryfikacji: testy → review → `update_target_ref`.

Jeśli gałąź docelowa zmieniła się w międzyczasie, **próba jest unieważniana w całości** — worktree
integracyjny jest odtwarzany na nowym `expected_old`, a merge, testy i review biegną od nowa.
Samo podmienienie `expected_old` byłoby zatwierdzeniem wyniku, którego nikt nie zweryfikował na
aktualnej bazie.

### 16.4 Nowe bloki

| Blok | Zadanie | Konfiguracja |
|---|---|---|
| `workspace_context` | Wiązanie sesji, stan repo z brokera, gałąź, zmienione pliki, `AGENTS.md`/`CLAUDE.md` jako DANE, toolchain; publikuje `harness_tools` | `include_repo_instructions`, `max_instruction_chars`, `include_git_status` |
| `patch_review` | Domyka patch set, prezentuje diff, blokuje run (mechanika `ask_user`), zapisuje decyzje per hunk/plik, rekonstruuje plik przy częściowej akceptacji, wykrywa konflikt CAS | `scope` (`work`/`merge` — przegląd zmian sesji albo wyniku scalenia), `granularity`, `timeout_secs`, `on_timeout` |
| `git_op` | Operacja git wykonywana przez **koordynatora** w brokerze | `op` (`commit`/`push`/`merge_integration`/`finalize_merge`/`update_target_ref`/`branch`), `message_template`, `require_clean`, `on_error` |
| `exec_command` | Deterministyczna komenda | `argv`, `mount_access`, `network_access`, `ephemeral`, `cwd`, `timeout_secs`, `output_variable`, `fail_on_nonzero` |
| `delegate_cli` | Delegacja do agenta CLI: rejestruje `cli_instances`, wydaje ticket adaptera, streamuje zdarzenia jako run podrzędny, zwraca podsumowanie + patch set | `engine`, `service_id`, `model`, `budget`, `timeout_secs` |
| `code_search` | Dociągnięcie kontekstu (grep od Fazy 2, semantyka od Fazy 7) | `query_template`, `mode`, `limit`, `output_variable` |

Każdy blok: adapter, rejestracja, wiersz w `flow_node_templates` (`db/seed.rs:288`, `:796`), paleta,
tłumaczenia w pięciu lokalizacjach.

### 16.5 Walidacja przed kodowaniem

Przed napisaniem adapterów powstaje **realny `flow_json`** grafu z §16.2 i przechodzi:
`FlowDefinition` → R1–R11 → `CompiledFlow::from_json` → wykonanie na atrapach adapterów. Zielony wynik
zamyka projekt grafu; wynik może zmienić kształt. To pierwsze zadanie Fazy 5.

### 16.6 Wersjonowanie

`flow_versions` i `FlowVersionList/Get/Restore` istnieją (`migrations.rs:6034`, `handlers.rs:1546`,
`:1610`). Do dopisania: factory restore (seed jako nowa wersja), pin wersji przez sesję
(`sessions.flow_version_id`), zakres wersji aktywnej + test startowy kompilacji (wzorzec
`db/seed.rs:2249`); niekompilowalna wersja nie zostaje aktywowana.

---

## 17. Codex i Claude Code

### 17.1 Faza 0B — twardy go/no-go per CLI

Weryfikujemy przy pinowanych wersjach (`claude-code 2.1.221`, `codex 0.146.0`):

1. tryb bezinteraktywny ze strukturalnym strumieniem zdarzeń;
2. wznawianie sesji po identyfikatorze;
3. kanał zatwierdzeń z możliwością odpowiedzi;
4. raportowanie zużycia;
5. lista modeli bez zakładania sesji;
6. **nadpisanie base URL** tak, by cały ruch szedł przez adapter (§7.5);
7. **brak własnego kanału odświeżania poświadczenia** poza adapterem;
8. działanie bez bezpośredniej trasy sieciowej.

Punkty 6–7 są **blokujące**: bez nich integracja danego CLI jest wyłączona w V1. Artefakt: notatka
+ transkrypty + decyzja go/no-go per engine.

### 17.2 Docelowy bridge

Jedna długożyjąca instancja na `cli_instances`, wznawiana po restarcie; strukturalny strumień
mapowany na `session_events` i `ProgressEvent`; ścieżka odpowiedzi na zatwierdzenia przez PEP
i `InteractionRegistry` (D3); lista modeli bez sesji, `sync_coding_agent_models` przestaje tworzyć
sesje (D1); grupa procesów, jawne ubicie i `wait`, stan `reaped`, reap osieroconych (D2); zdarzenia
w `workspace.db`; katalog roboczy = worktree sesji; sandbox i ticket wg §7.5.

### 17.3 Egress danych

| Polityka | Model harnessu | Agent CLI | Sieć sandboxa |
|---|---|---|---|
| `local_only` | tylko usługi lokalne owner node | **niedostępny** | wyłączona (`network_access='none'`). **Wymaga kontenera albo reguły zapory po uid** — inaczej polityka jest w UI ukryta (§7.6) |
| `org_approved` | lokalne + dostawcy z listy organizacji | dozwolony, jeśli engine przeszedł go/no-go i jest na liście | allowlista przez bramkę |
| `any` | dowolny skonfigurowany | jw. | allowlista przez bramkę |

Egzekwowane przy rozwiązywaniu modelu, przy `delegate_cli`, w bramce i w adapterze. `local_only` jest
egzekwowalne, bo sandbox nie ma trasy — to działa wyłącznie w trybie kontenerowym (§7.1). Każda tura
zapisuje zdarzenie z adresatem (dostawca + model). Rozliczenie: harness przez `AiGateway` →
`compliance_ai_events` + `token_usage_daily`; CLI — zużycie mierzone **w adapterze** (a nie tylko
raportowane przez dostawcę), co czyni budżet ticketu egzekwowalnym.

---

## 18. Protokół i punkty zaczepienia

Nowy `MessageBody::CodeStudioBody(CodeStudioPayload)` + `tentaflow-protocol/src/code_studio.rs`;
sub-enum ściśle append-only (ciborium taguje po nazwie), golden test wzorowany na
`project_studio_wire_golden`; pola z `#[serde(default)]`. Osobny sub-enum od startu
(`ProjectStudioPayload` jest na 248 wariantach).

Handler `dispatch/code_studio.rs` z `#[handler(variant = "CodeStudioBody")]`, `#[policy(...)]`,
`#[observed]` (wzorzec `dispatch/storage_admin.rs:25-35`); odczyty `#[policy(UserSession)]` z własnym
sprawdzeniem członkostwa.

Strumienie: `codeStudioSessionStreamRequest`, `codeStudioTerminalStreamRequest`,
`codeStudioIndexStreamRequest` — wzorzec `project_studio_chat_stream_handler`
(`stream_handlers.rs:1824`) + `project_studio_stream_guard` (`:2122`).

Frontend: `tentaflow-protocol-wasm/src/lib.rs` → `www/js/protocol/codec.js` → `www/js/app.js`.
Kafelek: `apps-home.js`, `{ id: 'code-studio', route: 'code-studio', icon: 'terminal' }`, bez
`requiresPowerUser`. REST wyłącznie dla dużych pobrań przez podpisany URL.

---

## 19. UI

Ekrany: lista workspace'ów; kreator (nazwa, węzeł, **tryb wykonania**, źródło, poświadczenia, obraz
sandboxa, tryb autonomii, polityka egress, gałąź docelowa, indeks); workspace jako IDE
(lewa: `tf-tree`, status git, sesje; środek: `tf-tabs` — Edytor / Diff / Terminal / Oś czasu; prawa:
`tf-chat-panel`, karty zgód i review, plan, subagenci); panel git; ustawienia workspace.

Nowe komponenty: `tf-terminal` (siatka + klawisze, VT po stronie serwera), `tf-diff` (akceptacja
per hunk).

**Luka**: `tf-code-editor` deklaruje 7 języków (`tf-code-editor.js:15`). Dla obietnicy „IDE" trzeba
dopisać co najmniej Rust, C#, HTML, CSS, shell i TOML — przez rozszerzenie tokenizera w komponencie.
Alternatywa: zmiana obietnicy na „edytor kodu z podświetlaniem wybranych języków".

UI musi pokazywać wprost: brak metadanych git w sandboxie, profil (`mount`/`network`) bieżącej
operacji, stan łączności z owner node, łańcuch runów rewizji, oraz — dla `git_push`/`git_merge` —
że pytanie jest obowiązkowe i nie da się go wyłączyć.

**Wybór trybu** w kreatorze to dwie karty z opisem charakteru (§7.1), z **preselekcjonowanym
`trusted_native`**. Karta `container` niesie jednozdaniowe „co zyskujesz" (izolacja od hosta,
wymuszone `ro`/`cow`, kontrolowany egress) i jest nieaktywna na węźle bez runtime'u, z informacją co
zainstalować. Karta `trusted_native` niesie „Native — kod ma dostęp do hosta" już na etapie wyboru,
a nie dopiero po założeniu.

**Workspace `trusted_native`** ma dodatkowo:
- trwałe oznaczenie na karcie i w nagłówku sesji: **„Native — kod ma dostęp do hosta"**, nie tylko
  jednorazowy komunikat przy zakładaniu;
- w kreatorze `autonomous` i `local_only` ukryte, z jednozdaniowym wyjaśnieniem (a serwer i tak je
  odrzuca — §9.5);
- widoczne `egress_enforcement`: przy `unrestricted` UI mówi wprost, że ruch sieciowy **nie jest
  filtrowany ani rejestrowany**, zamiast pokazywać allowlistę sugerującą kontrolę;
- prośbę o zgodę `profile_degrade_rw`, gdy kopia worktree jest niemożliwa — z jasnym opisem, że
  testy pobiegną na prawdziwym drzewie roboczym.

Konwencje: jeden rząd `.tf-toolbar` (+ spacer), stopka podsumowania, dwuliniowe komórki, KPI
z `tf-stat-card`, liczba mnoga przez i18n `{count|f1|f2|f3}`, klucze `code_studio.*` w pięciu lokalizacjach.

---

## 20. Powiązanie z Projektami

Relacja N:M przez `code_workspace_project_links`. Mirror uprawnień jednokierunkowy (projekt →
workspace) z zapisem tego, co nadaliśmy — mechanizm `project_studio/ml_link.rs`; mapowanie:
owner/manager → editor, editor/tester → editor, viewer → viewer.

Projekt z linkiem może wskazać workspace jako źródło kodu dla przypadków F3 i czytać strukturę repo.
**Izolacja zostaje**: `test-runner` ma własny sandbox (`SandboxLimits::test_runner`); nie montujemy
worktree. Runner dostaje **wskazany commit** — powtarzalność ważniejsza niż testowanie
niezacommitowanej pracy.

Cykl: agent pisze → review → commit koordynatora → projekt testuje ten commit → wynik wraca jako
zdarzenie sesji i defekt w projekcie.

---

## 21. Platformy

| | Linux | macOS | Windows |
|---|---|---|---|
| Runtime kontenerów | Docker / Podman | Docker Desktop / Podman (VM) | WSL2 lub Hyper-V/VM |
| COW dla profilu `cow` | overlayfs | kopia w katalogu jednorazowym | kopia w katalogu jednorazowym |
| `trusted_native`: kopia dla `cow` | reflink (Btrfs/XFS) → pełna kopia | klon APFS → pełna kopia | block clone (ReFS) → pełna kopia |
| `trusted_native`: `egress_enforcement` | `firewall` (nftables `--uid-owner`) albo `unrestricted` | `firewall` (PF) albo `unrestricted` | `firewall` (WFP) albo `unrestricted` |
| Filesystem brokera | `openat2` (fallback `openat`+`O_NOFOLLOW`) | `openat`+`O_NOFOLLOW` + `st_dev`/`st_ino` | reparse points, final path, ADS, device names, UNC |
| Procesy | `setsid` + `killpg` | jw. | Job Object + `KILL_ON_JOB_CLOSE` |
| Terminal | PTY | PTY | ConPTY |
| Git | broker na owner node |||

`trusted_native` ma ten sam charakter na wszystkich trzech platformach — różni się tylko sposób
robienia taniej kopii dla `cow` i dostępność zapory po właścicielu procesu. Kreator informuje, że na
tym węźle nie ma izolacji od hosta, i podpowiada `container` (na Windows: WSL2) albo zdalny węzeł
kontenerowy, ale nie blokuje wyboru.

Warstwa platformowa wydzielona (`code_studio/fs/{linux,unix,windows}.rs`,
`code_studio/exec/{unix,windows}.rs`, `code_studio/native/{unix,windows}.rs` dla użytkowników i ACL).

---

## 22. Observability i SLO

Metryki: czas startu sesji i sandboxa per profil, czas tworzenia i niszczenia warstwy COW, opóźnienie
narzędzia p50/p95, głębokość kolejki exec, czas oczekiwania na zgodę, opóźnienie strumienia mesh,
zaległość indeksu, liczba operacji `unknown` po restarcie, liczba osieroconych procesów przy starcie
(regresja D2), odmowy bramki i adaptera, wykorzystanie budżetu ticketów, zaległość `audit_outbox`,
liczba runów rewizji na sesję.

SLO wstępne: start sesji (ciepły obraz) < 20 s; narzędzie lokalnie p95 < 150 ms, zdalnie p95 < 400 ms;
echo klawisza p95 < 100 ms lokalnie, < 250 ms zdalnie; zaległość indeksu po patchu < 60 s dla repo
< 50 tys. plików; `audit_outbox` opróżniony < 60 s.

Alerty: run bez postępu, sesja `waiting_user` ponad dobę, sandbox restartujący się w pętli,
`cli_instances` w `starting` > 2 min, operacja `unknown` > 24 h, `audit_outbox` rosnący ponad próg,
workspace nad kwotą.

Logi strukturalne bez sekretów, z redakcją jak w §13.4.

---

## 23. Fazowanie

**Faza 0A — naprawy natychmiastowe.** D1, D2, D3.
*Kryterium:* dobę pracy Core nie przybywa ani jedna sesja u dostawcy i ani jeden osierocony proces.

**Faza 0B — rozpoznanie i decyzje.** Go/no-go per CLI (§17.1, w tym base URL i brak własnego
odświeżania), threat model, ADR „owner-node coordinator", ADR „git w brokerze", ADR „commit
z zaakceptowanych blobów", ADR „`container` vs `trusted_native` — zakres gwarancji" (§7.1),
maszyny stanów, macierz RBAC, decyzja o `stop_expr`, rozstrzygnięcia §25.

**Faza 1 — rejestr, vault, koordynator, broker git, minimalna sesja.** Migracja rejestru + uprawnienia
+ granty; vault node-local; pula `workspace.db`; `SessionCoordinator`; saga z trwałymi krokami;
**broker git: `init` + pusty commit / `clone` / `worktree` / `status`, z PEŁNYM utwardzeniem configu
(§11.2) obowiązującym już przy pierwszym `clone`** — bo klon pobiera treść z niezaufanego zdalnego
repozytorium, więc polityka protokołów, zakaz przekierowań między hostami, wyłączone helpery
i `submodule.recurse=false` muszą działać wtedy, a nie od Fazy 2 — plus pinowanie fingerprintu,
polityka adresów i `GIT_ASKPASS`; minimalny PEP (rola + capability + systemowe); sesja z worktree;
protokół, lista, kreator.
*Kryterium:* zakładam workspace, klonuję repo przez broker, otwieram sesję; awaria w połowie zostawia
`error` wznawialny; token nie pojawia się w `ps` ani w URL.

**Faza 2 — filesystem, zdarzenia, operacje, patche, polityka, pozostałe operacje git.** Warstwa
`openat2`/reparse; pozostałe operacje git (utwardzenie configu weszło w Fazie 1);
`session_events` + `session_operations` z pre/postcondition
+ uzgadnianie; patch sety z CAS (§13.2); `audit_outbox` + redakcja; PEP z `mandatory_interactive`;
narzędzia `fs_*` i `git_read`/`git_branch`/`git_sync`; `core.fs_grep`; **commit z zaakceptowanych
blobów** (§11.5).
*Kryterium:* agent zmienia plik po zgodzie; druga edycja tego samego pliku nie zgłasza fałszywego
konfliktu; przerwanie w `git_commit` kończy się `unknown` z sondą, nie podwójnym commitem;
commit zawiera dokładnie zaakceptowaną treść, mimo równoległej zmiany w worktree.

**Faza 3 — sandbox, profile, exec, PTY, IDE.** `SandboxLimits::code_session`, `mount_access` ×
`network_access`, COW `ephemeral`, brak `.git` w sandboxie + shim z tokenem aktora, bramka egress,
overlay cache; **ścieżka `trusted_native`: kopia worktree przez reflink/clone dla `cow` z zachowaniem fail-closed,
zgoda `profile_degrade_rw`, gniazda lokalne z kontrolą peera, `egress_enforcement`**; `core.exec`;
terminal z VT po stronie serwera; `tf-terminal`, `tf-diff`, edytor, panel git, oś czasu.
*Kryterium (kontener):* proces `code-tester` **nie może** zapisać w worktree, ale **buduje** projekt
w COW; warstwa COW nie przeżywa drugiego przebiegu; `rm -rf .git` nie ma czego usunąć; komenda
anulowana nie zostawia procesów; ruch poza allowlistę odrzucony przez bramkę.
*Kryterium (`trusted_native`):* `cow` tworzy kopię przez reflink tam, gdzie system plików to wspiera,
a przy niemożliwej kopii **odmawia** zamiast pracować na worktree; `profile_degrade_rw` wymaga
jawnej zgody i nie da się jej zapisać w allowliście; **serwer** odrzuca `autonomous` i `local_only`
przy `unrestricted`; broker wykonuje operacje z jawnym `--git-dir` i ignoruje podmieniony wskaźnik
`.git`.

**Faza 4 — mesh.** `CodeStudioOp` z `SessionAssertion` (Ed25519 + `kid` + rotacja, `iss` związany
z kanałem, anty-replay, `rbac_rev`), sprawdzenie świeżości dla operacji nieodwracalnych, ponowna
autoryzacja lokalna, strumienie z reconnect/backpressure/dedupe.
*Kryterium:* workspace na węźle B obsługiwany z węzła A; powtórzona asercja odrzucona; asercja po
zmianie `rbac_rev` odrzucona; `git_push` po zdalnym odebraniu roli odmówiony mimo ważnej asercji.

**Faza 5 — Code Harness i agenci (grep, bez indeksu).** Najpierw walidacja `flow_json` (§16.5), potem
bloki; seed flow i agentów; pętla rewizji na poziomie sesji; merge przez worktree integracyjny;
factory restore + pin wersji; opcjonalnie `stop_expr`.
*Kryterium:* zadanie „dodaj funkcję X i testy" przechodzi plan → zatwierdzenie → implementacja →
przegląd i testy → review → commit → merge; odrzucenie w review tworzy widoczny run rewizji; drugi
obrót planu kończy run zamiast zapętlać; graf przechodzi R1–R11 i `CompiledFlow::from_json`.

**Faza 6 — agenci CLI.** Adapter dostawcy + tickety, bridge wg 0B, `delegate_cli`, agenci
`claude-code`/`codex`, pomiar zużycia w adapterze.
*Kryterium:* delegacja kończy się patch setem w tym samym review; u dostawcy przybywa jedna sesja na
sesję Code Studio; **proces w sandboxie nie ma dostępu do materiału poświadczenia**, a wykradziony
ticket wygasa z runem i nie przekracza budżetu.

**Faza 7 — indeks semantyczny.** Ingest, `core.code_search`, odświeżanie inkrementalne, limity.

**Faza 8 — Projekty.** Linki N:M, mirror uprawnień, workspace jako źródło kodu dla F3.

---

## 24. Testy

- **Containment**: `..`, dowiązanie i junction poza korzeń, ścieżka bezwzględna, `NUL`, ADS, nazwa
  urządzenia, UNC, `\\?\`, **podmiana segmentu na dowiązanie między sprawdzeniem a otwarciem** —
  każdy odrzucony.
- **Profile (kontener)**: proces w `ro` nie zapisze w worktree (test na poziomie OS); proces w `cow`
  **zbuduje** projekt (`target/`, `node_modules`) i jego zmiany **nie pojawią się** w worktree ani
  w następnym przebiegu; `none` nie ma trasy poza gniazdo shimu.
- **Profile (`trusted_native`)**: `cow` buduje projekt w kopii, a worktree pozostaje nietknięty;
  brak możliwości kopii → **odmowa**, nie praca na oryginale; `profile_degrade_rw` wymaga
  interaktywnej zgody i nie jest zapisywalna jako `always`.
- **Walidacja serwerowa**: `trusted_native` + `autonomous` odrzucone przez handler (nie tylko ukryte
  w UI); `local_only` przy `egress_enforcement='unrestricted'` odrzucone; próba zmiany `exec_mode`
  istniejącego workspace odrzucona.
- **Domyślny tryb**: żądanie utworzenia workspace bez `exec_mode` daje `trusted_native` **i** emituje
  zdarzenie audytowe z rozstrzygniętym trybem — pominięcie pola nigdy nie jest wyborem niewidocznym.
- **Broker a `.git`**: podmieniony wskaźnik `gitdir:` w worktree nie zmienia katalogu, na którym
  operuje broker; operacja idzie na `git_dir` z jego własnej mapy.
- **`.git`**: nie istnieje w sandboxie; `rm -rf .git` bez celu; `git status` przez shim działa;
  token shimu testera nie pozwala na `git_stage`.
- **Integralność commitu**: zmiana pliku w worktree **po** akceptacji review nie trafia do commitu;
  commit zawiera dokładnie `accepted_blob_sha`; `update-ref` z nieaktualnym `expected_old` zawodzi.
- **Sekrety**: token i klucz nie występują w zdarzeniach, artefaktach, logach, kontekście modelu ani
  w `ps`/`cmdline`; **materiał poświadczenia dostawcy nie jest osiągalny z sandboxa** (test skanujący
  montowania, środowisko i pamięć procesu CLI); ticket poza swoim runem jest odrzucany; przekroczenie
  budżetu ticketu zatrzymuje ruch.
- **Bramka**: host spoza allowlisty odrzucony; rebinding DNS nie omija kontroli; przekierowanie poza
  allowlistę odrzucone; adres metadata zablokowany; SNI niezgodne z `CONNECT` odrzucone;
  `local_only` faktycznie odcina sieć.
- **PEP**: `always` na `git_push` odrzucone przy zapisie; `git_push` pyta mimo allowlisty i grantów;
  capability systemowe niedostępne dla agenta i terminala; `allow_for_run` znika po runie,
  `allow_for_session` przeżywa run, `always` przeżywa restart.
- **Operacje**: `op_id` stabilny dla terminala, UI, shimu i bloku flow; przerwanie po efekcie a przed
  potwierdzeniem → `completed` z postcondition; przerwanie przed efektem → bezpieczne ponowienie;
  nierozstrzygalne → `unknown` bez ponowienia.
- **CAS**: pierwsza i druga edycja tego samego pliku przechodzą; `delete` i `rename` z `Precondition`;
  równoległa edycja przez agenta i człowieka → `conflicted`; częściowa akceptacja daje deterministyczny
  `accepted_blob_sha`, a nakładające się konteksty → `conflicted`.
- **Merge**: worktree integracyjny jest **odłączony** — `merge` nie przesuwa `refs/heads/<target>`
  (test: wartość referencji docelowej po `merge_integration` jest identyczna jak przed);
  czerwone testy na wyniku merge'a **nie** prowadzą do `update_target_ref`; konflikt i odrzucenie
  zostawiają worktree w stanie `held`, a run rewizji go odnajduje i kontynuuje; fast-forward
  przechodzi; brak FF wymaga decyzji; równoległa zmiana gałęzi docelowej przerywa `update-ref`
  zamiast nadpisać.
- **Commit niezależny od worktree**: zmiana pliku w worktree po akceptacji nie blokuje commitu ani
  do niego nie wchodzi, a staje się kolejnym patch setem; `rename` znika ze starej ścieżki i pojawia
  się pod nową w drzewie commitu.
- **Recovery**: niepotwierdzony `exec`/`push`/`merge` kończy jako `unknown` nawet przy spełnionym
  precondition; niepotwierdzony `fs_write` z zachowanym precondition jest ponawiany.
- **Asercja po restarcie**: `jti` użyte przed restartem jest nadal odrzucane po restarcie.
- **Równoległość sandboxów**: dwa przebiegi testów o tym samym profilu działają jednocześnie na
  osobnych lease i nie widzą swoich warstw COW.
- **Pusty workspace**: `repo_kind='empty'` ma `base_commit` i pozwala otworzyć sesję z worktree.
- **Artefakty**: argv z tokenem i wyjście komendy zawierające token są zredagowane w artefakcie
  (ten sam test, co dla zdarzeń).
- **Asercja**: powtórzona (`jti`) odrzucona; z cudzym `iss` na tym kanale odrzucona; po zmianie
  `rbac_rev` odrzucona; rotacja `kid` nie przerywa działających sesji.
- **Audyt**: zdarzenie bezpieczeństwa trafia do `audit_log` mimo awarii między bazami (outbox);
  argv z tokenem jest zredagowane przed zapisem.
- **Trwałość**: zabicie Core → run domknięty, oś czasu kompletna, projekcja zgodna ze zdarzeniami,
  `cli_instances` w `reaped`, sesja wznawialna.
- **Flow**: `flow_json` przechodzi R1–R11 i `CompiledFlow::from_json`; brak cyklu; drugi obrót planu
  kończy run; run rewizji powstaje po odrzuceniu review; limit rewizji zatrzymuje pętlę pytaniem.
- **Wstrzyknięcie promptu**: `AGENTS.md` żądający podniesienia autonomii nie zmienia trybu.
- **Protokół**: golden test nazw i kolejności wariantów.
- **Mesh**: zerwanie i wznowienie bez luki i duplikatu; wyczerpane okno spowalnia producenta.
- **Platformy**: zestaw powyżej na Linux, macOS i Windows (WSL2).

---

## 25. Decyzje rozstrzygnięte przed Fazą 1

**25.1 Zmiana trybu autonomii w trakcie sesji.** W dół — swobodnie. W górę — tylko do
`autonomy_ceiling`, wymaga `session_open`, jest zdarzeniem audytowym, obowiązuje od następnej
operacji. Commit i tak zawsze idzie przez review (§9.5), więc podniesienie trybu nie omija bramki
integralności.

**25.2 Gałęzie sesji.** `cs/<user_slug>/<session_short_id>` (8 znaków UUID; kolizja → sufiks `-2`).
Po zamknięciu: worktree usuwany natychmiast, gałąź żyje 30 dni i jest kasowana przez GC z wpisem
w raporcie — chyba że wypchnięta, zmergowana albo przypięta. Gałąź z niezmergowanymi zmianami nie
jest kasowana bez wpisu i powiadomienia.

**25.3 Limity.** 10 workspace'ów na użytkownika, 3 równoległe sesje, 20 GiB na workspace, 2 GiB
artefaktów na sesję, 1 GiB artefaktu, 4 równoległe komendy, 10 runów rewizji na sesję, budżet ticketu
CLI per run. Nadpisywalne per organizacja; rezerwowane w sadze.

**25.4 Widoczność administratora.** `code_studio.admin` widzi **metadane** i może archiwizować,
usuwać, zarządzać grantami i kwotami. **Nie ma** dostępu do treści plików, osi czasu, terminala ani
patch setów. Dostęp wymaga dopisania się do `code_workspace_members` — zdarzenie audytowe z mirrorem
w `audit_log` (§13.4), widoczne dla właściciela i niekasowalne. Nie ma „podglądu administratora"
bez śladu.

---

## 26. Pozostałe kwestie otwarte (mockupy)

1. `tf-diff` jako osobny komponent czy tryb `tf-code-editor`.
2. Domyślna granulacja `patch_review` (hunk czy plik).
3. Zakres rozszerzenia tokenizera `tf-code-editor` albo zmiana obietnicy z „IDE" na „edytor".
4. Prezentacja łańcucha runów rewizji — lista liniowa czy zwijane grupy.
5. Prezentacja braku metadanych git w sandboxie, żeby nie zaskakiwała użytkownika terminala.
6. Prezentacja profilu (`mount`/`network`) bieżącej operacji w osi czasu.
7. Układ prawej kolumny przy wielu subagentach.
8. Uniwersalny addon MCP to decyzja platformowa poza tym planem; agenci Code Studio skorzystają z jego
   narzędzi allowlistą `addon_id.*`. Do rozstrzygnięcia tam: serwery MCP po stdio wymagają procesu po
   stronie hosta, czego piaskownica WASM nie daje; ścieżka HTTP/SSE mieści się w `http.request`.
