# DeepSeek Harness — analiza wewnętrzna

Analiza z **kodu źródłowego** (`github.com/deepseek-ai/deepseek-harness`, tag `dsh-0.1.0-rc.7`),
nie z opisów prasowych. Cel: wyciągnąć to, co realnie warto przenieść do naszego Code Harness i
Flow Buildera.

Fakty wyjściowe: wydany **13 sierpnia 2026** jako developer preview, MIT, Node.js/TypeScript,
~81 MB źródeł, ponad 50 pakietów w monorepo pnpm. Uruchamiany przez `npx @deepseek-ai/dsh web`
(Web UI na :3080). Deklarowany status: „iterating rapidly, compatibility-breaking changes
expected".

## 1. Idea nośna: wszystko jest pluginem

Fundamentem jest **Cordis** — mały runtime, w którym każda zdolność jest pluginem montowanym w
współdzielonym kontekście: adapter modelu, rejestr narzędzi, log sesji, sandbox, storage,
scheduler, UI **i sama pętla agenta**.

Rzecz, która robi tu robotę, to nie sama wtyczkowość, tylko **jawnie wyznaczona jedna wyspa
konkretu**. `packages/core/agent-loop/README.md` mówi wprost:

> „This is the only package in the harness that contains concrete loop logic. Everything else is
> an abstract service or a plugin against extension points — new behavior goes into plugins, not
> here."

To jest dokładnie ta dyscyplina, której u nas brakuje: mamy `flow_engine/executor.rs` z logiką
pętli i równolegle rosnące adaptery, które też podejmują decyzje sterujące.

## 2. Cykl życia: session → turn → step

Trzy poziomy, każdy z własnymi zdarzeniami:
- `session/created`, `session/event`, `session/flush`, `session/disposed`
- `turn/end` (z powodem zakończenia)
- `step/start`, `step/end` — pętla dopisuje **dokładnie jeden `step/end` na wejście w krok, w
  `finally`**, więc kroki zakończone, nieudane, anulowane i ucięte limitem tokenów liczą się tak
  samo.

Ten szczegół z `finally` jest ważniejszy, niż wygląda: dzięki niemu statystyki nie kłamią przy
anulowaniu. Nasz `execute_streaming` nie ma odpowiednika — anulowany przebieg po prostu nie
dopisuje nic.

## 3. Log sesji — jeden append-only strumień zdarzeń

To jest serce systemu i źródło wszystkiego, co widać w UI.

- Jeden **logiczny log JSONL na sesję**, domyślnie `.jsonl.zstd` (checksummed header frame +
  append frames).
- Pierwsza linia to niezmienny `SessionHeader`: `id`, `cwd`, `createdAt`, `parentSession`,
  `seedLength`, `origin`, `delegationDepth`, `agentPreset`.
- Każda kolejna linia to jedno `SessionEvent` **verbatim**, a `seq` jest **ciągłe**
  (`events[i].seq === i`).
- `agentPreset` jest trwały **celowo**: decyduje o narzędziach i promptcie wznowionej sesji —
  odtworzenie innej kompozycji odtwarzałoby historię, na którą model nie może już zareagować.

Do tego kompresja bez straty: ciąg ≥3 kolejnych `assistant/chunk` tego samego bloku pakuje się w
jedną linię (`text-chunks` / `reasoning-chunks` / `tool-call-chunks`) z `seq0`/`time0` i gapami
`dt`, odtwarzającymi każdy element **co do sekwencji i czasu**. Odczyt jest ślepy na układ —
`load` zawsze dekoduje.

Konsekwencja, której nie da się przecenić: **wznowienie, fork, wyszukiwanie i replay są za darmo**,
bo są operacjami na jednym strumieniu, a nie osobnymi mechanizmami.

## 4. Pomiar czasu — najważniejsze odkrycie

To, co Cię zachwyciło („każda rzecz jest logowana, ile toole trwają"), **nie jest instrumentacją**.
Nigdzie nie ma pola „czas trwania narzędzia". Czasy są **foldem po parach zdarzeń** z logu
(`packages/session/session-stats`):

| Metryka | Definicja |
|---|---|
| `toolMs` | suma par `tool/call` → `tool/result` **dopasowanych po `callId`** |
| `llmMs` | `step/start` → `assistant/message`, per krok, który złożył wiadomość |
| `ttftMs` | `step/start` → **pierwszy niepusty delta-chunk** |
| `decodeMs` | pierwszy token → złożona wiadomość (tylko kroki mające oba) |
| `steps` / `turns` | liczone z `step/end` / tur z ≥1 zamkniętym krokiem |

Niuanse, które świadczą o dojrzałości pomiaru:
- czekanie na retry **wewnątrz** kroku liczy się jako czas modelu (świadomie),
- `ttft` przeżywa `llm/retry` w kroku — granica pierwszej próby zostaje,
- **niezamknięte wywołania narzędzi są odrzucane na `turn/end`**, a nie zliczane jako trwające
  w nieskończoność.

Jedyne miejsce z jawnym `durationMs` to **hooki** (`packages/hooks/hook-protocol`), gdzie mierzy
się czas ściany procesu zewnętrznego — bo tam nie ma pary zdarzeń do odjęcia.

**Wniosek dla nas:** nie potrzebujemy dokładać liczników do adapterów. Potrzebujemy *ciągłego,
sekwencjonowanego logu zdarzeń z czasami* — reszta to zapytania. Dziś mamy `flow_executions` +
`TraceStep`, czyli model „wiersz na przebieg", z którego nie da się policzyć TTFT ani czasu
pojedynczego toola.

## 5. Narzędzia — 21 pakietów i pipeline z punktami zaczepienia

`packages/core/tools` definiuje **pipeline wykonania**:

```
tools/pre-execute  → bramka allow/deny (rozszerzalna)
guards             → monotoniczne, zarejestrowane
tools/execute      → around-dispatch (timeout / retry / metryki)
tools/post-execute → podmiana wyniku, doklejenie kontekstu
finalizeContent    → granica należąca do definicji narzędzia
tools/result       → notyfikacja tylko do obserwacji
```

Rejestr narzędzi decyduje też, **jak** są prezentowane modelowi: natywny function calling,
**Code Mode**, albo oba — a pojedynczy agent może przesłonić domyślne (`presentAs`).

Timeouty są **zero-config**: `tool-call-timeout-policy` to jeden listener `tools/execute`, który
uzbraja kooperacyjny deadline na `exec.signal`, czytając budżet z **deklaracji samego narzędzia**
(`ToolDefinition.timeoutMs`), i zwraca ustrukturyzowany `TOOL_TIMEOUT`. Model **nie ma**
timeoutu jako argumentu — to polityka wdrożenia, nie decyzja modelu.

Zestaw narzędzi (pakiety `tool-*`): `fs`, `fs-search`, `str-replace-editor`, `bash`,
`bash-persistent`, `pwsh`, `terminal`, `lsp`, `web`, `subagent` (+ `-control`, `-report`),
`skill`, `todo`, `goal`, `plan`, `jobs`, `workflow`, `ralph`, `ask-user`, `session-query`,
`cordis`.

Dwie rzeczy, których u nas nie ma, a są warte uwagi: **`tool-session-query`** (model odpytuje
własny log sesji) i **`tool-ralph`** obok `tool-workflow`.

## 6. Web search — jest, i to jako wymienny seam

`packages/web` daje modelowi dwa narzędzia przez seam `ctx.web`:

| Narzędzie | Argumenty | Zachowanie |
|---|---|---|
| `web_search` | `query` | zwraca opcjonalną odpowiedź + źródła; **`max_results` NIE jest model-facing** (bound ustawia wdrożenie, domyślnie 8) |
| `web_fetch` | `url` | HTML → markdown (turndown, GFM); non-2xx jest **raportowane, nie jest błędem** |

Providery są osobnymi pakietami: **`web-search-deepseek`, `web-search-exa`,
`web-search-perplexity`**, plus `web-fetch-http`. Pakiet narzędziowy **nigdy nie importuje
konkretnego providera**.

Dwa detale warte skopiowania:
- gdy `web_fetch` jest wyłączony w kompozycji, **prompt sam się zmienia** — instrukcja mówi wtedy
  modelowi, żeby korzystał ze snippetów i cytował URL-e, zamiast obiecywać narzędzie, którego nie
  ma;
- oba narzędzia deklarują się jako **bezpieczne do współbieżnego planowania**, bo czytają bez
  mutowania stanu agenta-rodzica.

## 7. Prompt — rejestr sekcji, nie plik

Nie ma „pliku z system promptem". Jest **rejestr składania** (`packages/core/system-prompt`):
pluginy wnoszą **uporządkowane sekcje**, schematy narzędzi i **nazwane zmienne** (`{{model}}`,
`{{cwd}}`), a pętla składa całość **raz na krok**.

- Stały opener (order −100): `You are an AI agent powered by DeepSeek Harness.`
- `persona` to jedyny fragment autorski z configu (order 0), z szablonowaniem `{{…}}`.
- **Kolejność narzędzi w promptcie jest jawnie konfigurowalna** (`toolOrder` z jednym wpisem
  `'<unlisted-tools>'`), bo kolejność rejestracji to artefakt ładowania pluginów. Błędna
  konfiguracja **wywala się głośno przy ładowaniu**, a nie cicho przy generacji.
- Sekcja `complete: true` staje się całym promptem; więcej niż jedna → odrzucenie składania.
- Zakres: `agent.ctx` przesłania sekcję globalną o tej samej nazwie.

To jest dokładnie to, czego brakuje naszemu Code Harness — u nas prompt systemowy jest wpisany w
config węzła `llm` w seedzie, więc nie da się go złożyć z wkładów wielu klocków.

## 8. Telemetria i prywatność

`session-telemetry` to **kontrakt**, nie implementacja: `emit()` musi kolejkować bez blokowania,
`flush()` jest opcjonalną podpowiedzią, `shutdown()` drenuje. Batching, retry i polityka strat
należą do backendu SDK i są **świadomie nieopakowane**. Backend OTel jest osobnym pakietem.

Trzy rzeczy, które warto zauważyć:
- **Waterfall redakcji** `sessionTelemetry/record` — pakiet **nie wozi żadnych reguł**; bez
  zamontowanego listenera rekordy wychodzą dokładnie takie, jakie były. Dokumentacja mówi to
  wprost jako znane ograniczenie, łącznie z ryzykiem poświadczeń w treści plików.
- Redakcja dotyczy **tylko kopii wychodzącej** — kanoniczny log sesji nigdy nie jest przepisywany.
- **Jawna deklaracja udostępniania** (`full` | `feedback-only` | `disabled`) pokazywana
  użytkownikowi przy `/feedback`, i nigdy nie twierdząca, że dostarczono — tylko że przekazano.

Dostarczanie jest **at-most-once** i tak nazwane. Kursor oznacza „przekazane", nie „dostarczone".

## 9. Praktyki inżynierskie warte skopiowania niezależnie od kodu

**Agent Notes — 1404 notatek decyzyjnych.** Ścieżka koduje dwie osie:
`{lifecycle}/{class}/yyyy-mm-dd-temat.md`, gdzie lifecycle ∈ {proposed, implemented, rejected,
archived}, a class ∈ {feature, bug-fix, simplification, architecture, process, testing}.
Notatka **przenosi się między folderami wraz ze statusem**, a notatka `implemented` jest
**utrzymywana w zgodzie z tym, co faktycznie zaszło** — gdy kod przenosi plik albo zmienia
domyślną wartość, notatka jest aktualizowana w tej samej zmianie (fakty, nie decyzja).
Świadomie **nie ma centralnego indeksu** (jest osobna notatka uzasadniająca to).

**Każdy README ma sekcję `Model Experience` i `KV Cache effect`.** Każdy pakiet musi odpowiedzieć,
co wnosi do kontekstu modelu i czy psuje cache KV. Pakiet czysto obserwacyjny pisze wprost:
„None, as this package only observes the session stream". To jest tania, mechaniczna dyscyplina,
która wymusza myślenie o koszcie kontekstu.

**Każdy README ma `Known Limitations and Deferred Work`** z nazwanym warunkiem powrotu do tematu
(„deferred until a deployment states a crash-loss requirement").

## 10. Co konkretnie wziąć do TentaFlow

Uporządkowane wg stosunku wartości do kosztu:

1. **Log zdarzeń z ciągłym `seq` i czasem, jako jedyne źródło prawdy o przebiegu.** To odblokowuje
   TTFT, czas per narzędzie, replay i fork — dziś nie mamy żadnego z nich. Nasz `flow_executions`
   zostaje jako indeks, nie jako zapis przebiegu.
2. **Metryki jako fold po logu, nie jako liczniki w adapterach.** Zero instrumentacji w
   `node_adapters/*`.
3. **Rejestr składania promptu** zamiast promptu wklejonego w config węzła — z jawną, konfigurowalną
   kolejnością narzędzi i głośnym błędem przy złej konfiguracji.
4. **`timeoutMs` w deklaracji narzędzia + jeden wrapper egzekwujący**, zamiast rozsypanych
   deadline'ów. My właśnie zrobiliśmy idle-timeout w `code_studio/exec` — to ten sam problem,
   rozwiązany punktowo.
5. **Odrzucanie niezamkniętych wywołań na koniec tury** przy liczeniu czasu — inaczej pierwsza
   awaria zatruwa statystyki.
6. **Agent Notes** — mamy `docs/*.md`, ale bez cyklu życia i klasyfikacji; wystarczy nałożyć
   konwencję ścieżki.
7. **`Model Experience` / `KV Cache effect` w opisie każdego bloku flow** — u nas odpowiednikiem
   jest opis węzła w palecie.

Czego **nie** kopiować bez zastanowienia: telemetrii bez reguł redakcji (u nas Compliance Core ma
twardsze wymagania niż at-most-once i „deployment owns its rule set"), oraz Code Mode jako
alternatywy dla function callingu — to osobna decyzja produktowa.

## Źródła

- [github.com/deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) — kod źródłowy (podstawa tej analizy)
- [DeepSeek Harness developer preview](https://deepseek.com/harness/en/)
- [Cordis Primer](https://deepseek-harness.github.io/deepseek-harness/en/reference/cordis-primer)
- [The New Stack — DeepSeek open sources an agent harness where everything is a plugin](https://thenewstack.io/deepseek-harness-open-source-plugins/)
- [HyperAI — DeepSeek Releases Open Source Harness for AI Agent Coding](https://hyper.ai/en/stories/4642f3eb645603d9cdaf7e7b10dd60f6)
