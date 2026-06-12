# Codex CLI — wewnętrzna architektura harnessu (Rust core)

> Źródło: `github.com/openai/codex`, commit `f4278010`, crate `codex-rs/core/`.
> Cel: zrozumieć, jak codex robi **pętlę agenta bez subflowów** — pojedynczy `loop { ... continue }`
> nad jedną, narastającą historią konwersacji. To wzorzec docelowy dla harnessu TentaFlow.

---

## 1. Widok komponentów — SQ/EQ (Submission Queue / Event Queue)

Cała komunikacja UI ↔ silnik idzie przez **dwa kanały**. Pętla submisji nigdy sama nie woła
modelu — tylko *spawnuje* turę jako osobny task Tokio, więc zostaje responsywna na kolejne
`Op` (przerwania, zatwierdzenia, „steering").

```mermaid
flowchart LR
    subgraph UI["UI / Embedder"]
        direction TB
        U1["submit(Op)"]
        U2["next_event() → EventMsg"]
    end

    subgraph SESSION["Session  (codex-rs/core/src/session)"]
        direction TB
        SQ(["SQ — tx_sub/rx_sub<br/>BOUNDED<br/>async_channel"])
        EQ(["EQ — tx_event/rx_event<br/>UNBOUNDED"])
        SL["submission_loop()<br/>handlers.rs:697<br/>while rx_sub.recv() → match Op"]
        HIST[("Conversation History<br/>append-only<br/>JEDEN akumulator")]
        APPR{{"pending approvals<br/>oneshot channels<br/>w turn_state"}}
    end

    subgraph TURN["Turn Task  (1 naraz, osobny tokio::spawn)"]
        direction TB
        RT["RegularTask::run()<br/>regular.rs:36<br/>steering loop"]
        RUNTURN["run_turn()<br/>turn.rs:135<br/>★ PĘTLA AGENTA ★"]
        TOOLS["N× tool task<br/>parallel.rs<br/>FuturesOrdered in_flight"]
    end

    MODEL["Model API<br/>client_session.stream()"]

    U1 --> SQ --> SL
    SL -->|"Op::UserInput → spawn_task"| RT
    SL -.->|"Op::ExecApproval / PatchApproval<br/>notify_approval()"| APPR
    SL -.->|"Op::Interrupt → CancellationToken"| RUNTURN
    RT --> RUNTURN
    RUNTURN -->|"render historii → prompt"| MODEL
    MODEL -->|"ResponseEvent stream"| RUNTURN
    RUNTURN --> TOOLS
    TOOLS -->|"wynik → record_conversation_items"| HIST
    RUNTURN <-->|"clone_history / record"| HIST
    APPR -.->|"odblokowuje tool future"| TOOLS
    RUNTURN -->|"EventMsg"| EQ
    TOOLS -->|"ExecApprovalRequest"| EQ
    EQ --> U2

    classDef key fill:#fde68a,stroke:#b45309,stroke-width:2px,color:#000;
    classDef store fill:#bfdbfe,stroke:#1e40af,color:#000;
    class RUNTURN key;
    class HIST,SQ,EQ store;
```

---

## 2. Pętla zewnętrzna — od `Op` do tury

```mermaid
sequenceDiagram
    autonumber
    participant UI
    participant SQ as SQ (rx_sub)
    participant SL as submission_loop
    participant ST as spawn_task / start_task
    participant RT as RegularTask
    participant EQ as EQ (tx_event)

    UI->>SQ: submit(Op::UserInput)
    SQ->>SL: rx_sub.recv()
    SL->>SL: match Op → user_input_or_turn
    SL->>ST: spawn_task(turn_context, input, RegularTask)
    Note over ST: abort_all_tasks(Replaced)<br/>jedna aktywna tura naraz
    ST->>ST: ActiveTurn + CancellationToken
    ST->>EQ: (po starcie)
    ST-->>RT: tokio::spawn(future)
    RT->>EQ: EventMsg::TurnStarted
    Note over RT: ↓ wchodzi w pętlę agenta (diagram 3)
    RT-->>ST: zwraca last_agent_message
    ST->>EQ: EventMsg::TurnComplete
    Note over SL: przez cały czas SL dalej<br/>obsługuje Interrupt/Approval z SQ
```

---

## 3. ★ PĘTLA AGENTA ★ — iteracja tool-calli bez subflowów (`run_turn`, turn.rs:135)

To jest sedno. **Jeden `loop`**, jeden akumulator (historia). Każda iteracja: renderuj historię →
streamuj model → wykonaj tool-calle jako nakładające się futures → dopisz ich wyniki do historii →
`continue`. Koniec tury, gdy sampling zwraca `needs_follow_up == false` (brak tool-calli, model
zakończył turę przez `end_turn`, brak „steered" inputu).

```mermaid
flowchart TD
    START([run_turn — wejście]) --> LOOP{{"loop  (turn.rs:202)"}}

    LOOP --> DRAIN["1. drain pending input → historia<br/>uruchom input hooks"]
    DRAIN --> BUILD["2. zbuduj prompt z CAŁEJ historii<br/>clone_history().for_prompt()"]
    BUILD --> SAMPLE["3. run_sampling_request → try_run_sampling_request<br/>client_session.stream()  (turn.rs:1800)"]

    SAMPLE --> STREAM{{"pętla strumienia ResponseEvent<br/>(turn.rs:1860)"}}

    STREAM -->|"OutputTextDelta /<br/>ReasoningDelta"| DELTA["emit EventMsg::AgentMessageContentDelta<br/>(strumień do UI)"]
    DELTA --> STREAM

    STREAM -->|"OutputItemDone(item)"| CLASSIFY["handle_output_item_done()<br/>ToolRouter::build_tool_call<br/>(stream_events_utils.rs:405)"]

    CLASSIFY -->|"to TOOL CALL"| TOOLPUSH["• zapisz wywołanie do historii<br/>• tool future → push na in_flight<br/>(FuturesOrdered)<br/>• needs_follow_up = true"]
    TOOLPUSH --> STREAM

    CLASSIFY -->|"to MESSAGE / reasoning"| MSG["• finalizuj TurnItem<br/>• zapisz do historii<br/>• last_agent_message = treść"]
    MSG --> STREAM

    STREAM -->|"Completed{end_turn}"| DONE["flush text + token usage<br/>if end_turn==false → needs_follow_up=true<br/>break z wynikiem"]

    DONE --> DRAINIF["4. drain_in_flight()<br/>await KAŻDY tool future<br/>→ record_conversation_items(wynik)<br/>(turn.rs:2255) — tu wyniki wracają do kontekstu"]

    DRAINIF --> DECIDE{"needs_follow_up ?<br/>= model_needs_follow_up<br/>|| has_pending_input"}

    DECIDE -->|"TAK"| CONTINUE["continue  (turn.rs:364)"]
    CONTINUE -.->|"historia już zawiera wyniki toolów"| LOOP

    DECIDE -->|"NIE"| STOPHOOK["run_turn_stop_hooks()"]
    STOPHOOK -->|"hook wstrzykuje kontynuację"| CONTINUE
    STOPHOOK -->|"brak — koniec"| BREAK([break → zwróć last_agent_message])

    classDef hot fill:#fde68a,stroke:#b45309,stroke-width:2px,color:#000;
    classDef tool fill:#fbcfe8,stroke:#9d174d,color:#000;
    classDef stop fill:#bbf7d0,stroke:#15803d,color:#000;
    class LOOP,CONTINUE hot;
    class TOOLPUSH,DRAINIF tool;
    class BREAK,DECIDE stop;
```

**Klucz dla TentaFlow:** model jest wołany ponownie przez zwykłe `continue` w jednym `loop`.
Brak rekurencji, brak sub-tasków per iteracja, brak „subflow". Jedyne `spawn`y to:
(a) jeden task na całą turę, (b) jeden task na *pojedyncze* wywołanie toola (równoległość),
zbierane z powrotem przez `in_flight` / `drain_in_flight` w **tej samej** iteracji.

---

## 4. Gating zatwierdzeń (approval / sandbox) — bez deadlocka

Zatwierdzenie blokuje **tylko future konkretnego toola**, nie pętlę. Bo tura biegnie na osobnym
tasku, a `submission_loop` dalej drenuje SQ i może dostarczyć decyzję.

```mermaid
sequenceDiagram
    autonumber
    participant TF as tool future (shell.rs)
    participant TS as turn_state (pending approvals)
    participant EQ
    participant UI
    participant SQ
    participant SL as submission_loop

    TF->>TF: approval_policy wymaga zgody
    TF->>TS: insert_pending_approval(id, oneshot tx)
    TF->>EQ: EventMsg::ExecApprovalRequest
    EQ->>UI: pokaż prośbę
    Note over TF: rx_approve.await — BLOKUJE tylko ten future
    UI->>SQ: submit(Op::ExecApproval{decision})
    SQ->>SL: rx_sub.recv()  (pętla NIE jest zablokowana)
    SL->>TS: notify_approval(id, decision)
    TS-->>TF: oneshot rozwiązany → tool rusza dalej
```

---

## 5. Dwie warstwy „końca"

| Poziom | Sygnał | Plik |
|--------|--------|------|
| sampling pass | `SamplingRequestResult.needs_follow_up` (tool-call / RespondToModel / `end_turn==false` / pending input) | `turn.rs:1227` |
| tura | `run_turn` przerywa `loop`, gdy `!needs_follow_up` i stop-hooki nie żądają kontynuacji | `turn.rs:362` |
| sesja | `on_task_finished` → `EventMsg::TurnComplete`, czyści `active_turn` | `tasks/mod.rs:554` |

---

## 6. Model współbieżności (skrót)

- **1× submission loop** / sesja — drenuje SQ, dispatchuje `Op`, **nigdy** nie woła modelu.
- **1× turn task** naraz — `ActiveTurn` + `CancellationToken`; nowa tura abortuje starą (`Replaced`).
- **N× tool task** / sampling pass — `tokio::spawn` per tool, zbierane przez `FuturesOrdered`;
  równoległość bramkowana `RwLock` (`parallel_execution`).
- **EQ unbounded** dla `EventMsg`; **oneshot** per zatwierdzenie/prośba o input.
- Sama iteracja agentowa = **jeden `loop` + `continue`** nad append-only historią.
