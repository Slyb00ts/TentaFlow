# Code Harness — graf harnessu

Konwencja jak w `docs/target-agent-flow.mmd`. Źródło: `docs/code-harness-flow.mmd`.
Ekrany: `mockups/code-studio-20260814/f01-flow-builder.html` (oba warianty).

```mermaid
flowchart TD
    classDef exist fill:#bfdbfe,stroke:#1e40af,color:#000;
    classDef change fill:#fde68a,stroke:#b45309,color:#000;
    classDef new fill:#bbf7d0,stroke:#15803d,color:#000;
    classDef agent fill:#e9d5ff,stroke:#7e22ce,color:#000;
    classDef io fill:#e5e7eb,stroke:#374151,color:#000;
    classDef store fill:#ddd6fe,stroke:#5b21b6,stroke-width:2px,color:#000;
    classDef opt fill:#fff,stroke:#94a3b8,stroke-dasharray:4 3,color:#000;

    TRIG["trigger"]:::io
    CH["conversation_history"]:::exist
    WC["workspace_context (NOWY)"]:::new
    AC["agent_context — agent sesji + JEGO model"]:::exist
    TRIG --> CH --> WC --> AC --> CC

    subgraph REGION["LOOP REGION 'code_turn' — agent wola NARZEDZIE albo AGENTA"]
        direction TB
        CC["compact_context"]:::exist
        LLM["llm(tools) — model z definicji agenta"]:::exist
        TE["tool_exec"]:::change
        CC --> LLM --> TE
        TE -. "loop_back" .-> CC
    end

    subgraph TOOLS["Narzedzia (core.*)"]
        direction TB
        T1["fs_read / fs_edit / fs_grep"]:::exist
        T2["exec — cargo test, build"]:::exist
        T3["git_commit / git_push / git_merge"]:::exist
        T4["ask_user — pytanie do operatora"]:::exist
    end

    subgraph AGENTS["Agenci (core.agent_spawn)"]
        direction TB
        A1["code-reviewer — wlasny model, tylko odczyt"]:::agent
        A2["code-tester — profil cow"]:::agent
        A3["code-committer — specjalista od gita"]:::agent
        A4["claude-code / codex — agent typu CLI"]:::agent
    end

    TE --> TOOLS
    TE --> AGENTS

    PT["persist_turn"]:::exist
    OUT["output (stream)"]:::io
    TE --> PT --> OUT

    subgraph FORCE["OPCJONALNIE: wymuszony lancuch (wariant zespolowy)"]
        direction LR
        F1["spawn: code-reviewer"]:::opt
        F2["spawn: code-tester"]:::opt
        F3["spawn: code-committer"]:::opt
        F1 --> F2 --> F3
    end
    TE -. "gdy chcesz GWARANCJI zamiast decyzji modelu" .-> FORCE
    FORCE -. .-> PT

    PEP{{"PEP — bramki sa polityka, nie topologia<br/>git_commit: wymaga ZAAKCEPTOWANEGO patch setu<br/>git_push / git_merge: pytaja ZAWSZE<br/>fs_write / exec: wg trybu autonomii"}}:::store
    T3 -. "kazde wywolanie" .-> PEP
    T2 -. .-> PEP

    OPS[("session_operations")]:::store
    TE -. "pending -> completed" .-> OPS
```

## Model: agent woła narzędzie albo agenta

To jedyne dwa rodzaje ruchu. **Narzędzia** to `fs_*`, `exec` (testy i buildy), `git_commit`,
`git_push`, `ask_user`. **Agenci** to wpisy w tabeli `agents`, każdy z własnym modelem, allowlistą
i skillami, wołani przez `core.agent_spawn`: reviewer tylko do odczytu, tester w profilu `cow`,
committer wyspecjalizowany w gicie, agent typu CLI dla Claude Code i Codeksa.

**Commit jest narzędziem, nie blokiem grafu.** W Claude Code to zwykłe wywołanie gita i tutaj jest
tak samo. Wcześniejsze wersje tego dokumentu robiły z commitu, przeglądu i testów osobne węzły —
to był ten sam błąd co wyciąganie testów poza pętlę.

## Bramki są polityką, nie topologią

Bezpieczeństwo nie bierze się z odebrania agentowi narzędzia, tylko z PEP:

`git_commit` przechodzi dopiero, gdy istnieje **zaakceptowany patch set**. Wywołanie bez niego
pokazuje diff do przeglądu i czeka. Commit i tak powstaje z zaakceptowanych blobów, więc nie da się
zacommitować niczego innego niż to, co człowiek zatwierdził — niezależnie od tego, kto wywołał
narzędzie.

`git_push` i `git_merge` pytają **zawsze**, niezależnie od trybu autonomii i bez możliwości zapisania
zgody „na zawsze".

## Dwa warianty, wybierane per flow

**Agent decyduje** (domyślny, 9 bloków). O tym, czy uruchomić testy, poprosić o przegląd i wypchnąć
zmiany, decyduje agent główny — z kontekstu rozmowy. „Popraw i wypchnij" kończy się pushem;
„zobacz tylko, co jest nie tak" nie dotyka gita. Tak działa Claude Code.

**Wymuszony łańcuch** (wariant zespołowy). Za regionem stoją bloki `spawn` z konkretnymi agentami,
więc przegląd, testy i commit wykonają się **zawsze**, niezależnie od decyzji modelu. Cena: agent
główny nie poprawi się przed pokazaniem wyniku, bo łańcuch rusza zaraz po jego turze.

Mieszanie jest normalne i chyba najczęstsze: wymuszone testy i przegląd, ale git zostawiony
agentowi — bo tylko on wie z rozmowy, czy w ogóle chciałeś cokolwiek wypychać.
