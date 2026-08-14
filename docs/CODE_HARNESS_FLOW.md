# Code Harness — graf harnessu

Diagram poglądowy. Konwencja jak w `docs/target-agent-flow.mmd`. Źródło: `docs/code-harness-flow.mmd`.

```mermaid
flowchart TD
    classDef exist fill:#bfdbfe,stroke:#1e40af,color:#000;
    classDef change fill:#fde68a,stroke:#b45309,color:#000;
    classDef new fill:#bbf7d0,stroke:#15803d,color:#000;
    classDef ask fill:#fed7aa,stroke:#c2410c,stroke-width:2px,color:#000;
    classDef io fill:#e5e7eb,stroke:#374151,color:#000;
    classDef store fill:#ddd6fe,stroke:#5b21b6,stroke-width:2px,color:#000;
    classDef term fill:#fecaca,stroke:#b91c1c,color:#000;

    TRIG["trigger — start sesji / runu"]:::io
    CH["conversation_history (read-mode)"]:::exist
    WC["workspace_context (NOWY)<br/>wiazanie sesji mintowane przez serwer<br/>stan repo + AGENTS.md/CLAUDE.md jako DANE"]:::new
    AC["agent_context<br/>agent sesji + JEGO model -> meta['model']<br/>skille, mailbox, harness_tools, budzet"]:::exist

    TRIG --> CH --> WC --> AC --> CC

    subgraph REGION["LOOP REGION 'code_turn' — jedna petla nad jedna historia, jak w Claude Code / Codex"]
        direction TB
        CC["compact_context (gdy ctx > prog)<br/>wejscie regionu: max_iterations, final_pass"]:::exist
        LLM["llm(tools) — tura modelu<br/>model PUSTY w bloku: bierze sie z agenta"]:::exist
        TE["tool_exec — TU dzieje sie cala robota:<br/>fs_read / fs_edit · exec (cargo test, build)<br/>git_read · code_search<br/>core.ask_user + zgody -> waiting_user<br/>core.agent_spawn -> subagent w tle"]:::change
        CC --> LLM --> TE
        TE -. "loop_back — czerwone testy to KOLEJNA ITERACJA,<br/>nie rozgalezienie grafu" .-> CC
    end

    TE --> REVIEW

    REVIEW["patch_review (NOWY)<br/>przeglad CZLOWIEKA: diff per hunk, CAS"]:::new
    CREV{"zaakceptowany?"}:::exist
    REVIEW --> CREV
    CREV -->|"nie"| OREV
    CREV -->|"tak"| GCOMMIT

    GCOMMIT["git_op: commit (NOWY)<br/>buduje KOORDYNATOR z zaakceptowanych blobow"]:::new
    ASK["ask_user: wypchnac / scalic?<br/>push i merge zawsze pytaja"]:::ask
    PT["persist_turn — zapis delty tury"]:::exist
    OUT["output (stream) — sesja wraca do idle"]:::io
    OREV["output: revision_requested<br/>koordynator startuje NOWY run (limit 10/sesje)"]:::term

    GCOMMIT --> ASK --> PT --> OUT
    OREV ==>|"petla SESJI, pietro wyzej niz petla agenta"| TRIG

    OPS[("session_operations<br/>pre/postcondition, OID-y git")]:::store
    EVT[("session_events + audit_outbox")]:::store
    TE -. "kazdy efekt: pending -> completed" .-> OPS
    GCOMMIT -. "OID blob/tree/commit/ref" .-> OPS
    REGION -. "trace / audit / run_log" .-> EVT
```

## Zasada podziału: co jest w pętli, a co poza nią

**W pętli jest wszystko, co agent może zrobić i powtórzyć.** Czytanie i edycja plików, uruchamianie
testów i buildów, wyszukiwanie, delegacja do subagenta, pytania do użytkownika i prośby o zgodę.
Czerwone testy nie są rozgałęzieniem grafu — agent dostaje wynik jako rezultat narzędzia, poprawia
kod i odpala je znowu. To kolejna iteracja tej samej pętli, dokładnie jak w Claude Code i Codeksie,
gdzie cały harness to jeden `loop` nad jedną narastającą historią (`docs/CODEX_HARNESS_INTERNALS.md`).

Pętla kończy się **strukturalnie**: gdy model przestaje wołać narzędzia. Nie ma warunku „testy
zielone?", bo to agent ma doprowadzić je do zieleni, zanim uzna robotę za skończoną.

**Poza pętlą zostaje tylko to, czego agentowi nie wolno zrobić samemu.** Przegląd zmian przez
człowieka, commit budowany przez koordynatora z zaakceptowanych blobów, wypchnięcie albo scalenie.
Trzy bloki — każdy dlatego, że wymaga kogoś innego niż agent, a nie dlatego, że tak wygląda porządniej.

Odrzucenie w przeglądzie nie wraca krawędzią wsteczną: kończy run jako `revision_requested`,
a koordynator startuje kolejny z uwagami. To **pętla sesji**, piętro wyżej niż pętla agenta —
rdzeń i tak odrzuciłby cykl w zewnętrznym DAG-u (R11 + toposort Kahna).

## Skąd bierze się model

Blok `llm` ma pole `model` **puste**, tak jak w zaseedowanym „Agent Run" (`seed.rs:1376`).
Model wnosi agent: `agent_context` bierze go z `agents.model` i stempluje w `meta['model']`
(`agent_context.rs:303-312`), a `llm` czyta stamtąd (`llm.rs:36-52`). Dzięki temu jeden blok
w grafie obsługuje wszystkich agentów — każdy ze swoim modelem, narzędziami i skillami.

Agent jest wybierany na trzy sposoby: `agent_context` wskazuje agenta sesji, blok `spawn` wskazuje
subagenta wprost w grafie, a `core.agent_spawn` pozwala orkiestratorowi wybrać go w trakcie pracy.
Czwarta droga, `agent_router`, dobiera agenta modelem spośród oznaczonych `routable` — tutaj nieużywana,
bo agent sesji jest znany od startu.
