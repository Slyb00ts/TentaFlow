# Audyt harnessu Flow Buildera — tarcie vs model codex

> Audyt kodu (2026-06-11) pod kątem: (a) „1 flow zamiast 3", (b) „pętla natywna, nie jako subflow".
> Wszystkie cytowania to realne pliki/linie w repo.

## 1. Potwierdzone „3 flowy" — `src/db/seed.rs:830-857`

Agentowy harness jest rozbity na **trzy osobne definicje flow w DB**, połączone subflowami:

| Flow | UUID (seed.rs) | Graf |
|------|------|------|
| **Harness** | `…011` | trigger → conversation_history → agent_router → **subflow(Agent Run)** → output |
| **Agent Run** | `…012` | trigger → agent_context(from_vars) → **loop(body: Agent Iteration)** → output |
| **Agent Iteration** | `…013` | trigger → compact_context → llm(tools) → tool_exec → output |

Czyli jedna „tura agenta" (model → narzędzia → znowu model) jest **trzema grafami**, które trzeba
osobno otworzyć i ogarnąć w Flow Builderze. Pętla (`…012`) i jej ciało (`…013`) to **różne flow**.

## 2. Pętla = ponowne wykonanie subflow, nie inline `continue`

`loop_block.rs:400` — każda iteracja woła `runner.run(body_flow_id, current, ctx, 1, true)`, czyli
**pełny `execute_blocking` osobno skompilowanego flow** per iteracja. Stan przechodzi przez sklonowany
envelope (`context.messages` + `meta`). To dokładnie odwrotność codeksa:

| | codex (`run_turn`, turn.rs:202) | TentaFlow (`loop_block.rs:359`) |
|---|---|---|
| iteracja | `continue` w jednym `loop` | `execute_blocking` całego child-flow |
| historia | jeden append-only akumulator w `Session` | klonowany `envelope.context.messages` N→N+1 |
| kompilacja | zero | **rekompilacja ciała per iteracja** (`subflow_runner.rs:167-170`) |
| koniec tury | `needs_follow_up == false` (strukturalnie) | CEL `until` nad `meta.harness_done` |

## 3. Stan przez magiczne stringi w `meta`

Sygnały sterujące harnessu są stringly-typed i wymieniane **przez granice flow**:
`harness_done`, `harness_exit_reason` (`tool_exec.rs:482-486`), `loop_max_iterations`,
`loop_final_pass`, `loop_iterations`, `loop_exit_reason` (`loop_block.rs`). Stąd obowiązkowy guard
CEL `has(meta.harness_done) && …` (`loop_block.rs:84`) — bo klucz nie istnieje aż `tool_exec` go
ustawi. To kontrakt bez typu, łatwy do zepsucia.

## 4. Heurystyki „nudge" — objaw, nie cecha (`loop_block.rs:200-302`)

Pętla nie widzi *intencji* modelu (czy chciał wołać narzędzia), bo jest **o poziom wyżej** niż
wywołanie modelu — dostaje tylko gotowy envelope. Więc zgaduje z tekstu:
- `looks_like_intermediate_ack` — dopasowanie stringów `"i'll "`, `"let me "`, limit `ACK_MAX_CHARS=240`
- `empty-after-tools` — pusty final po użyciu narzędzi
- liczniki `MAX_ACK_NUDGES=2`, sentinel `[System:` wstrzykiwany i potem strippowany

Codex tego **nie potrzebuje**: `run_turn` jest miejscem wywołania modelu, więc strukturalnie wie, czy
padły tool-calle (`needs_follow_up`). Te heurystyki to bezpośredni koszt „loop-as-subflow".

## 5. Podwójna oś duplikacji: blocking × streaming

Drugi wymiar rozjazdu (poza 3-flow):
- Każdy blok ma `execute` (blocking) **i** `produce_stream` (`loop_block.rs:453` i `:594`).
- `SubflowRunner` ma `run` **i** `run_streaming` (`subflow_runner.rs:65` i `:110`).
- Pętla streamingowa robi iteracje pośrednie **blokująco**, a streamuje tylko finalną
  (`loop_block.rs:568-643`) — z osobnym wrapem `wrap_outcome_as_stream` / `terminal_stream_from`.

`execute_blocking` vs `execute_streaming` w `executor.rs` to trzecia oś. Razem dają wrażenie „wielu
harnessów", choć to jeden model DAG.

## 6. Dwuwarstwowe wykrywanie końca

- `tool_exec.rs:481` — brak tool_calls → `harness_done=true`.
- `loop_block.rs:385` — CEL `until` czyta `meta.harness_done`.

Dwa miejsca, stringowy klucz pomiędzy. Codex: jedna decyzja `needs_follow_up` w tym samym `loop`.

## 7. Co z tego wynika dla przebudowy

Żeby zejść do modelu codeksa (1 flow + inline loop):

1. **Scal 011/012/013 w jeden flow** z natywną iteracją: `…llm(tools) → tool_exec` z **back-edge**
   `tool_exec → llm`, którą executor wykonuje inline, akumulując historię w **tym samym** envelope.
2. **Executor musi dopuścić kontrolowaną krawędź wsteczną** (dziś DAG jest twardo acykliczny —
   walidacja R1–R8 + toposort). Trzeba albo: (a) oznaczony „loop edge" z budżetem iteracji, albo
   (b) konstrukt `loop`/`while` jako region grafu wykonywany inline, bez `SubflowRunner`.
3. **Koniec tury strukturalnie**: warunek stopu = „ostatni assistant bez tool_calls", liczony tam
   gdzie wykonujemy turę — eliminuje `harness_done`/`until`/nudge-heurystyki.
4. **Zunifikuj blocking/streaming**: jedna ścieżka wykonania, streaming jako podgląd deltami; finalna
   iteracja nie wymaga osobnego wrapa.
5. Zachowaj guard rekurencji/budżetu i light-mode audyt, ale jako własność regionu pętli, nie subflow.

`subflow`/`map` zostają jako osobne konstrukty (kompozycja flow / fan-out są zasadne); to **pętla
agentowa** ma przestać być subflowem.
