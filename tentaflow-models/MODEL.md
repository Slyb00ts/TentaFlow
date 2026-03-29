# TentaFlow Qwen 3.5-0.8B — Orchestrator Model

## Overview

Fine-tuned Qwen/Qwen3.5-0.8B as AI orchestrator. NOT a general-purpose LLM — this model is a **conductor** that routes, plans, extracts, and validates. It never generates end-user responses. Runs locally on GPU (13ms) or CPU (700ms).

## What Qwen does and does NOT do

| Qwen DOES | Qwen does NOT |
|-----------|---------------|
| Route messages to correct handlers | Generate text responses for users |
| Select which tools to call | Execute complex reasoning |
| Select which LLM model to use | Write code |
| Create execution plans (steps) | Create creative content |
| Extract facts and fields from text | Answer knowledge questions |
| Detect user feedback/corrections | Have conversations |
| Validate results (pass/fail) | Self-correct complex errors |
| Decide when to escalate to big LLM | Replace big LLM |

**Qwen = conductor of the orchestra. It knows WHO should play, WHEN, and WHAT to pass them. It does NOT play any instrument itself.**

## Architecture

```
User message
      │
      ├─→ [ALWAYS] <|guard|> security check
      │
      ▼
┌──────────────┐
│  <|intent|>  │  → TOOLS, MEMORY, FEEDBACK, RECALL, EXTRACT, MODEL, PLAN
└──────┬───────┘
       │
       ▼  (parallel where possible)
  ┌────┼────┬────────┬──────────┬──────┬──────┐
  ▼    ▼    ▼        ▼          ▼      ▼      ▼
tools model memory feedback  recall extract  plan
  │    │    │        │          │      │      │
  ▼    ▼    ▼        ▼          ▼      ▼      ▼
  └────┴────┴────────┴──────────┴──────┴──────┘
                     │
                     ▼
              Selected LLM model
              (chosen by <|model|>)
              generates final response
```

## Special Tokens

| Token | Task | Qwen does | Output |
|-------|------|-----------|--------|
| `<\|guard\|>` | Security | Classify safe/unsafe | `0`/`1`/`2` |
| `<\|intent\|>` | Route | Decide WHAT tasks to run | `TOOLS,RECALL,MODEL` |
| `<\|tools\|>` | Tool call | Select tool + fill params | TOON format |
| `<\|model\|>` | Model route | Select which LLM to use | `model_alias\|task=...\|context=...` |
| `<\|plan\|>` | Plan | Create step-by-step execution plan | Numbered steps |
| `<\|memory\|>` | Extract facts | Pull facts from text for storage | Structured facts |
| `<\|summary\|>` | Summarize | Create/update hierarchical summary | Summary text |
| `<\|feedback\|>` | Detect feedback | Classify user feedback | Type + details |
| `<\|recall\|>` | Recall | Generate search params for memory | Search query |
| `<\|extract\|>` | Extract fields | Pull specific fields from document | `field=value` |
| `<\|check\|>` | Validate | Check if step result is OK | `OK`/`RETRY`/`ESCALATE` |

## Intent Router

### Available tasks

| Task | When |
|------|------|
| `TEXT` | Simple answer — route directly to default LLM (standalone) |
| `TOOLS` | Need to call external tool/addon |
| `MODEL` | Need specific LLM (code model, image model, medical model) |
| `MEMORY` | Store fact/decision/preference |
| `FEEDBACK` | User corrects/confirms/complains |
| `RECALL` | Need context from memory |
| `EXTRACT` | Pull specific fields from document |
| `PLAN` | Complex task requiring multiple steps |

### When PLAN vs direct execution

- **Direct** (no PLAN): Simple single-action requests — "Send email to Kate", "Check calendar"
- **PLAN**: Multi-step tasks — "Find the report, extract key numbers, and email summary to the team"

## Model Routing (`<|model|>`)

### How it works

TentaFlow has multiple LLM models available, each with different strengths. Qwen selects the best model for the task.

### Model registry (configured in TentaFlow UI)

Each model has:
- `alias` — unique name (e.g., "code-gen", "vision", "medical")
- `category` — code, chat, image, audio, medical, legal, rag, embedding
- `description` — what it's good at (SKILL.md equivalent, editable in UI)
- `context_size` — max tokens
- `speed` — fast/medium/slow
- `cost` — free(local)/cheap/expensive

### Input/Output

Qwen gets a FILTERED list of models — only those the current user has access to.

```
Input:
<|model|>
MODELS:
  default|category=chat|desc=General purpose assistant
  code-gen|category=code|desc=Code generation and review, supports Rust Python TypeScript
  rag-expert|category=rag|desc=Answers from documents with citations
TASK: User wants to generate a Rust function that parses CSV files

Output:
code-gen|task=generate_code|lang=rust|context=CSV parsing function
```

If user asks for image generation but no vision model on the list:
```
Output:
#UNAVAILABLE|reason=No image generation model available for your account. Contact admin for access.
```

### Routing rules Qwen learns

- "Write code" / "fix this function" → code model
- "Generate image" / "draw" / "create picture" → vision model
- "Based on the document" / "according to the report" → rag model
- General questions / conversation → default model
- Domain-specific (medical, legal) → domain model
- If unsure → default model (safe fallback)
- If needed model not on list → `#UNAVAILABLE` with reason

### Model list filtering

List is pre-filtered by TentaFlow per user permissions:
- Admin sees all models
- Regular user sees only granted models
- Qwen does NOT check permissions — it gets already filtered list
- Same approach as tools (filtered by ToolDispatcher before reaching Qwen)

### Model fallback

If selected model is unavailable at runtime:
1. Qwen selects → `code-gen`
2. `code-gen` is offline → TentaFlow falls back to `default`
3. Transparent to user

## Planning (`<|plan|>`)

### When to plan

Qwen creates a plan when the task requires multiple steps that depend on each other.

### Input

```
<|plan|>
TOOLS: [top 5-10 tools from retrieval]
MODELS: [available models]
USER: Find the Q1 report in SharePoint, extract revenue and costs,
      compare with Q4, and email the analysis to the CFO.
```

### Output

```
1. TOOL sharepoint-rag.search_files|query=Q1 report 2026
2. TOOL sharepoint-rag.get_file_content|file_id={from_1}
3. EXTRACT revenue,costs,period|document={from_2}
4. RECALL Q4 report data
5. MODEL rag-expert|task=compare Q1 vs Q4 revenue and costs|context={from_3,4}
6. TOOL outlook.send_email|to=cfo|subject=Q1 vs Q4 Analysis|body={from_5}
7. DONE
```

### Plan step types

| Type | What | Who executes |
|------|------|-------------|
| `TOOL` | Call addon tool | Qwen (tool calling) |
| `EXTRACT` | Extract fields | Qwen (extract) |
| `RECALL` | Get from memory | Qwen (recall) → vector/graph search |
| `MODEL` | Call LLM model | Selected model (big LLM) |
| `CHECK` | Validate result | Qwen (check) |
| `STORE` | Save to memory | Qwen (memory) |
| `ASK` | Ask user for missing info | Return question to user |
| `DONE` | Plan complete | Return to user |

### When plan needs more info

If Qwen can't create a complete plan because information is missing:

```
Input:
<|plan|>
TOOLS: outlook.send_email(to*,subject*,body*), ...
USER: Wyślij raport do klienta

Output:
#ASK|question=Do którego klienta mam wysłać raport? Czy mam konkretny raport na myśli?
```

Qwen asks BEFORE starting execution — better to ask than guess wrong.

### Step references

`{from_N}` — output of step N is passed as input to this step. Executor handles data flow.

## Self-Correction (`<|check|>`)

After each step, Qwen validates the result:

```
Input:
<|check|>
STEP: TOOL bash|cmd=cargo build
RESULT: error[E0308]: mismatched types, expected `String`, found `&str`
ATTEMPT: 1/3

Output:
RETRY|fix=Change &str to String::from() at line 42
```

### Decision tree

```
Result of step
      │
      ▼
  <|check|>
      │
      ├─ OK         → continue to next step
      ├─ RETRY       → try again with fix (max 3 attempts)
      │   └─ includes suggested fix
      └─ ESCALATE    → send to big LLM with full context
          └─ Qwen knows it can't fix this
```

### When Qwen escalates

- After 3 failed retries
- Error is too complex (logic error, not syntax)
- Task requires reasoning Qwen can't do
- Qwen outputs `ESCALATE` — explicitly admits it needs help

## Special Outputs — when Qwen can't proceed

These outputs are used across ALL tasks when Qwen needs to signal a problem:

| Output | When | Example |
|--------|------|---------|
| `#UNAVAILABLE\|reason=...` | Required tool/model not on user's list | `#UNAVAILABLE\|reason=No email tool available. Contact admin.` |
| `#ASK\|question=...` | Missing info needed to proceed | `#ASK\|question=Which client should I send the report to?` |
| `#MISSING@tool\|param=?` | Tool found but required params unknown | `#MISSING@outlook.send_email\|to=?\|subject=?` |
| `ESCALATE` | Qwen can't handle (from `<\|check\|>`) | Complex error after 3 retries |

### Tools — unavailable

```
Input:
<|tools|>
teams: send_message(to*,message*), list_chats()
<|query|>
Wyślij maila do Kasi

Output:
#UNAVAILABLE|reason=No email tool available. You have access to Teams messaging — should I send a Teams message instead?
```

Qwen suggests alternatives when possible.

### Models — unavailable

```
Input:
<|model|>
MODELS:
  default|category=chat|desc=General assistant
TASK: Generate an image of a sunset

Output:
#UNAVAILABLE|reason=No image generation model available for your account.
```

### Plan — needs more info

```
Input:
<|plan|>
TOOLS: jira.create_issue(project*,summary*,type*), ...
USER: Stwórz ticket

Output:
#ASK|question=W jakim projekcie? Jaki typ (bug/task/story)? Opisz krótko problem.
```

## Full Pipeline Example

```
User: "Nie, to jest źle — deadline to 15 kwietnia nie 30 marca.
       Zaktualizuj to w Jira i wyślij info do zespołu na Slack."

1. GUARD: <|guard|> → 0 (safe)

2. INTENT: <|intent|> → FEEDBACK,MEMORY,TOOLS,RECALL

3. PARALLEL:
   ├─ <|feedback|> → CORRECTION: deadline 30.03→15.04, CONFIDENCE=1.0
   ├─ <|memory|> → FACT: Project deadline is April 15
   ├─ <|recall|> → SEARCH: deadline, jira ticket, team channel
   └─ (tools wait for recall context)

4. TOOLS (sequential, needs recall results):
   ├─ <|tools|> → @jira.update_issue|issue_id={from_recall}|due_date=2026-04-15
   └─ <|tools|> → @slack.send_message|channel=#team|message=Deadline updated to April 15

5. CHECK:
   ├─ Jira update → <|check|> → OK
   └─ Slack message → <|check|> → OK

6. MODEL: <|model|> → default|task=confirm_to_user
   └─ Big LLM: "Zaktualizowałem deadline w Jira na 15 kwietnia
                i poinformowałem zespół na Slacku."
```

## Training Data Summary

| Task | Token | Dataset | Target records |
|------|-------|---------|---------------|
| Guard | `<\|guard\|>` | guard short + extended | 5000+ |
| Intent | `<\|intent\|>` | intent router | 3000+ |
| Tools | `<\|tools\|>` | toolcalling (auto from addons) | 3000+ |
| Model routing | `<\|model\|>` | model routing | 2000+ |
| Planning | `<\|plan\|>` | execution plans | 2000+ |
| Memory | `<\|memory\|>` | documents + conversations + rag + transcripts | 3000+ |
| Summary | `<\|summary\|>` | hierarchical summaries | 2000+ |
| Extract | `<\|extract\|>` | field extraction | 2000+ |
| Feedback | `<\|feedback\|>` | feedback detection | 1000+ |
| Recall | `<\|recall\|>` | recall queries | 1000+ |
| Check | `<\|check\|>` | result validation | 1000+ |

## GGUF Deployment

```bash
./scripts/retrain.sh --fresh    # train all tasks from scratch
# Output: output/qwen-all-lora-Q5_K_M.gguf (~551MB)
```

Single GGUF file serves ALL tasks — differentiated by special tokens.
Load once in llama.cpp → batched inference for parallel tasks.
