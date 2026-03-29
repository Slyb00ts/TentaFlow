# TentaFlow Memory Addon — Architecture

## Overview

Memory addon for TentaFlow providing persistent, multi-layered AI memory with knowledge graph, vector search, and automatic consolidation. Designed for real-time conversations where the LLM context window holds only the last 4 messages — everything else comes from memory retrieval.

## Design Principles

1. **Hybrid storage**: Knowledge graph (facts + relations) + vector store (semantic search) + raw messages (full history)
2. **4 memory layers**: Global → Project → User → Conversation
3. **Real-time ingestion**: Every message is processed in-flight — facts extracted, embeddings generated, graph updated
4. **REM consolidation**: Periodic background process that strengthens important memories, removes duplicates, builds abstractions
5. **Model-assisted**: Qwen 0.8B handles extraction, summarization, and retrieval queries — trained specifically for this

## Memory Layers

```
┌─────────────────────────────────────────────┐
│  Layer 1: GLOBAL                            │
│  Shared across all users and projects       │
│  Facts about the world, company, products   │
│  Populated by: admin, ingestion, REM        │
│  Example: "TentaFlow uses WASM for addons"  │
└─────────────────────────────────────────────┘
         ↕ inheritance
┌─────────────────────────────────────────────┐
│  Layer 2: PROJECT                           │
│  Shared within a project/workspace          │
│  Architecture decisions, tech stack, rules  │
│  Populated by: team members, documents      │
│  Example: "We use PostgreSQL 16 + Redis"    │
└─────────────────────────────────────────────┘
         ↕ inheritance
┌─────────────────────────────────────────────┐
│  Layer 3: USER                              │
│  Private per user                           │
│  Preferences, corrections, personal facts   │
│  Populated by: conversation, feedback       │
│  Example: "User prefers Rust over Python"   │
└─────────────────────────────────────────────┘
         ↕ context
┌─────────────────────────────────────────────┐
│  Layer 4: CONVERSATION                      │
│  Per conversation session                   │
│  Working memory — dies after session end    │
│  Facts from current conversation            │
│  Example: "We're debugging the auth bug"    │
└─────────────────────────────────────────────┘
```

### Layer Inheritance

When retrieving memories, the system searches ALL layers and merges results:
- Conversation layer has highest priority (most recent, most relevant)
- User layer overrides Project layer (personal preferences win)
- Project layer overrides Global (project-specific facts win)
- Conflicts resolved by: recency → confidence → layer priority

### Layer Lifecycle

| Layer | Created | Destroyed | Consolidation target |
|-------|---------|-----------|---------------------|
| Global | Manual / REM | Never | — |
| Project | When project created | When project deleted | Global (shared facts) |
| User | When user first chats | When user deleted | Project (if relevant) |
| Conversation | When chat starts | After REM consolidation | User + Project |

## Storage Architecture

Each memory layer stores data in 3 parallel structures:

### 1. Knowledge Graph (Facts + Relations)

Directed graph where nodes are concepts/entities and edges are relationships.

```
Node: {
    id: u64,
    type: Concept | Entity | Attribute | Action | Pattern,
    name: String,
    description: Option<String>,
    embedding: Vec<f32>,        // 384-dim for fast retrieval
    confidence: f32,            // 0.0-1.0
    source: Source,             // UserSaid | Extracted | Inferred | Imported
    created_at: u64,
    last_accessed: u64,
    access_count: u64,
    layer: MemoryLayer,
    tags: Vec<String>,
}

Edge: {
    from: NodeId,
    to: NodeId,
    relation: RelationType,     // IsA, HasProperty, Causes, Requires, ...
    weight: f32,                // 0.0-1.0 (connection strength)
    confidence: f32,
    source: Source,
    is_negation: bool,          // "X is NOT Y"
}
```

Relation types:
- Hierarchy: IsA, InstanceOf, PartOf
- Properties: HasProperty, HasValue
- Causation: Causes, Prevents, Requires, Enables
- Temporal: Before, After, During
- Spatial: LocatedIn, Contains
- Similarity: SimilarTo, OppositeOf, RelatedTo
- User-specific: UserPrefers, UserDislikes, UserCorrected, UserConfirmed
- Meta: LearnedFrom, InferredFrom, ContradictsNode

### 2. Vector Store (Semantic Search)

Every piece of text gets an embedding for semantic retrieval:
- Message embeddings (full messages)
- Summary embeddings (conversation summaries)
- Fact embeddings (extracted facts)
- Document embeddings (ingested documents)

Implementation: HNSW index (ef_construction=200, M=16) per layer.

### 3. Raw Message Store (Full History)

Complete conversation messages stored verbatim:
```
Message: {
    id: u64,
    conversation_id: String,
    role: User | Assistant | System | Tool,
    content: String,
    timestamp: u64,
    summary: Option<String>,    // Generated by Qwen
    facts_extracted: Vec<NodeId>,
    embedding: Vec<f32>,
}
```

## Hierarchical Summary Tree

Conversation summaries are stored as a tree — each level summarizes 5 items from the level below. This scales to any conversation length while preserving detail access.

```
Raw messages:  [1][2][3][4][5]  [6][7][8][9][10]  [11][12][13][14][15]  ...
                      │                │                    │
L1 summaries:    [Sum 1-5]        [Sum 6-10]          [Sum 11-15]      ...
(every 5 msgs)   ~150 tok          ~150 tok             ~150 tok

                      │                │                    │
L2 summaries:    [Sum of L1:1-3 ─────────────────────────────]         ...
(every 5 L1s)    ~200 tok (covers 25 messages)

                                       │
L3 summaries:    [Sum of L2:1-3 ──────────]
(every 5 L2s)    ~250 tok (covers 125 messages)

                                       │
TOP summary:     [Rolling summary — ALWAYS up to date]
                  ~300 tok max, refreshed at every new L1
```

### How it works

1. **Every message** → stored raw (full history)
2. **Every 5 messages** → Qwen generates L1 summary from those 5 messages
3. **Every 5 L1 summaries** → Qwen generates L2 summary from 5 L1s
4. **Every 5 L2 summaries** → Qwen generates L3 summary from 5 L2s
5. **TOP summary** → updated at every new L1, always reflects entire conversation

### Retrieval by depth

| Need | Source | Tokens |
|------|--------|--------|
| Quick context | TOP summary + last 4 messages | ~500 |
| More detail | TOP + relevant L1/L2 by vector search | ~800 |
| Full detail on topic | Find L1 by vector similarity → get raw messages from that window | variable |
| Full history | Raw message store | all |

### Summary node structure

```
SummaryNode: {
    id: u64,
    conversation_id: String,
    level: u8,                      // 1, 2, 3, or 0 for TOP
    sequence: u32,                  // position at this level
    message_range: (u64, u64),      // first and last message ID covered
    parent_id: Option<u64>,         // parent summary node
    children_ids: Vec<u64>,         // child summaries or message IDs
    content: String,                // the summary text
    embedding: Vec<f32>,            // for vector search
    created_at: u64,
    updated_at: u64,                // TOP gets updated frequently
}
```

### Why 5?

- 5 messages ≈ 500-1500 tokens → fits Qwen 0.8B context easily
- 5 L1 summaries ≈ 750 tokens → also fits
- Branching factor 5 means: L1 = 5 msgs, L2 = 25, L3 = 125, L4 = 625
- Most conversations (< 100 messages) need only L1 + TOP
- Very long conversations (500+) go up to L3 — still manageable

### TOP summary update strategy

TOP is NOT a summary of all L1s — it would grow unbounded. Instead:
- At every new L1, Qwen receives: `previous TOP summary + new L1 summary`
- Output: `updated TOP summary` (max 300 tokens)
- This is a ROLLING summary — old details may fade, but key facts persist
- Critical corrections (UserCorrected) are ALWAYS preserved in TOP

## Real-Time Pipeline (per message)

When a new message arrives:

```
Message (msg #N)
   │
   ├─→ [1] Store raw message              (instant, async)
   │
   ├─→ [2] Generate embedding             (Qwen 0.8B, ~10ms)
   │       └─→ Index in vector store
   │
   ├─→ [3] Extract facts (Qwen 0.8B)      (~50ms, async)
   │       ├─→ Entities: "Jan", "PostgreSQL 16"
   │       ├─→ Relations: "Jan prefers PostgreSQL"
   │       ├─→ Corrections: "User said AI was wrong about X"
   │       └─→ Update knowledge graph
   │
   ├─→ [4] Detect user feedback           (Qwen classifier, ~10ms, async)
   │       ├─→ Positive: strengthen related facts
   │       ├─→ Negative: mark correction, weaken wrong facts
   │       └─→ Neutral: no action
   │
   └─→ [5] If N % 5 == 0: Generate L1 summary    (~50ms, async)
           ├─→ Summarize messages N-4 to N
           ├─→ Store as SummaryNode(level=1)
           ├─→ Generate embedding for L1
           ├─→ If count(L1) % 5 == 0: Generate L2 summary
           │       └─→ (recursive: L2 triggers L3 if needed)
           └─→ Update TOP summary: previous TOP + new L1 → new TOP
```

Steps 1-2 are synchronous (needed for retrieval). Steps 3-5 are async (background).

## Retrieval Pipeline (before LLM response)

When preparing context for the main LLM:

```
User query
   │
   ├─→ [1] Vector search across all layers     (<5ms)
   │       └─→ Top-K most similar memories
   │
   ├─→ [2] Graph traversal (spreading activation)  (<5ms)
   │       └─→ Related facts 1-3 hops away
   │
   ├─→ [3] Recent messages (last 4 in context)
   │
   ├─→ [4] Conversation summary
   │
   ├─→ [5] Merge & rank results                (<2ms)
   │       ├─→ Deduplicate
   │       ├─→ Score: relevance × recency × confidence × layer_priority
   │       └─→ Top-N results (fit in context budget)
   │
   └─→ [6] Format as context for LLM
           ├─→ "Relevant memories:"
           ├─→ "Conversation so far (summary):"
           ├─→ "Recent messages:"
           └─→ "User corrections to remember:"
```

## REM Consolidation (Background Process)

Runs periodically (every 1 hour, configurable). Inspired by brain sleep consolidation.

### Phase 1: Conversation → User Memory
- Extract important facts from completed conversations
- Generate final conversation summary
- Move user-specific facts to User layer
- Move project-relevant facts to Project layer

### Phase 2: Strengthen Important Memories
- Nodes with high access_count get confidence boost
- Nodes that were confirmed by user get weight boost
- Nodes referenced across multiple conversations get promoted

### Phase 3: Decay & Cleanup
- Nodes not accessed in N days get confidence decay
- Below threshold → moved to cold storage
- Contradicted nodes → removed or flagged
- Duplicate facts → merged (keep highest confidence)

### Phase 4: Abstraction
- Find patterns in User/Project memories
- Create generalizations: "User always prefers X over Y"
- Promote recurring patterns to higher layers

### Phase 5: Relationship Discovery
- Run inference rules on graph
- If A→B and B→C, create A→C (with lower confidence)
- Cross-reference facts between conversations

## User Feedback Handling

Critical for memory quality. When user corrects AI:

```
User: "No, we use PostgreSQL 16, not MySQL"
   │
   ├─→ Detect correction (Qwen classifier)
   │
   ├─→ Find wrong fact: "Project uses MySQL"
   │       └─→ Mark as UserCorrected, confidence = 0.0
   │
   ├─→ Create correct fact: "Project uses PostgreSQL 16"
   │       └─→ Source: UserCorrected, confidence = 1.0
   │
   └─→ Create edge: wrong_fact --ContradictsNode-→ correct_fact
           └─→ Ensures wrong fact is never retrieved again
```

Types of user feedback:
- **Explicit correction**: "No, that's wrong" → fix fact
- **Implicit confirmation**: User uses AI's answer → strengthen
- **Preference expression**: "I prefer X" → add UserPrefers edge
- **Negative feedback**: "That didn't work" → weaken related approach
- **Context update**: "Actually, we changed to X" → update fact + timestamp

## Addon Tools

```toml
[tools.memory_store]
description = "Store a fact or memory"
keywords = ["remember", "store", "save", "note", "memorize"]

[tools.memory_recall]
description = "Recall memories relevant to a query"
keywords = ["remember", "recall", "what do you know", "remind"]

[tools.memory_search]
description = "Search memories by semantic similarity"
keywords = ["search", "find", "look up", "related"]

[tools.memory_forget]
description = "Remove a specific memory"
keywords = ["forget", "remove", "delete", "erase"]

[tools.memory_correct]
description = "Correct a wrong memory"
keywords = ["correct", "fix", "wrong", "update", "change"]

[tools.memory_summarize]
description = "Generate summary of conversation or topic"
keywords = ["summarize", "summary", "overview", "recap"]

[tools.memory_graph_query]
description = "Query the knowledge graph for related facts"
keywords = ["related", "connected", "graph", "relations"]

[tools.memory_status]
description = "Show memory statistics"
keywords = ["memory", "status", "stats", "how much"]
```

## Qwen 0.8B Tasks for Memory

The fine-tuned model handles 4 memory-specific tasks via special tokens:

### Task 1: Fact Extraction (`<|memory|>`)
```
Input: <|memory|>\nUser message or document text
Output: Structured facts in key-value format
```

### Task 2: User Feedback Detection (`<|feedback|>`)
```
Input: <|feedback|>\nUser: "No, that's wrong, we use PostgreSQL"
Output: CORRECTION|old=MySQL|new=PostgreSQL 16|confidence=1.0
```

### Task 3: Memory Retrieval Query (`<|recall|>`)
```
Input: <|recall|>\nUser asks about database setup
Output: SEARCH|query=database setup|layers=project,user|type=fact,decision
```

### Task 4: Summary Generation (`<|summary|>`)
```
Input: <|summary|>\n[conversation messages]
Output: Structured summary (same format as memory_conversations training data)
```

## Database Schema

```sql
-- Knowledge graph nodes
CREATE TABLE memory_nodes (
    id INTEGER PRIMARY KEY,
    layer TEXT NOT NULL,           -- global, project, user, conversation
    layer_id TEXT NOT NULL,        -- project_id, user_id, conversation_id
    node_type TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    embedding BLOB,               -- f32 vector
    confidence REAL DEFAULT 1.0,
    source TEXT NOT NULL,          -- user_said, extracted, inferred, imported
    created_at INTEGER NOT NULL,
    last_accessed INTEGER NOT NULL,
    access_count INTEGER DEFAULT 0,
    tags TEXT DEFAULT '[]'         -- JSON array
);

-- Knowledge graph edges
CREATE TABLE memory_edges (
    id INTEGER PRIMARY KEY,
    from_node INTEGER NOT NULL REFERENCES memory_nodes(id),
    to_node INTEGER NOT NULL REFERENCES memory_nodes(id),
    relation TEXT NOT NULL,
    weight REAL DEFAULT 1.0,
    confidence REAL DEFAULT 1.0,
    source TEXT NOT NULL,
    is_negation BOOLEAN DEFAULT FALSE,
    created_at INTEGER NOT NULL
);

-- Raw messages
CREATE TABLE memory_messages (
    id INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    summary TEXT,
    embedding BLOB,
    timestamp INTEGER NOT NULL
);

-- Hierarchical summary tree
CREATE TABLE memory_summaries (
    id INTEGER PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    project_id TEXT,
    level INTEGER NOT NULL,           -- 0=TOP, 1=L1(5msg), 2=L2(25msg), 3=L3(125msg)
    sequence INTEGER NOT NULL,        -- position at this level
    message_range_start INTEGER,      -- first message ID covered
    message_range_end INTEGER,        -- last message ID covered
    parent_id INTEGER REFERENCES memory_summaries(id),
    children_ids TEXT DEFAULT '[]',   -- JSON array of child summary IDs
    content TEXT NOT NULL,            -- the summary text
    facts_json TEXT DEFAULT '[]',     -- extracted fact IDs
    embedding BLOB,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- User corrections (critical for learning)
CREATE TABLE memory_corrections (
    id INTEGER PRIMARY KEY,
    user_id TEXT NOT NULL,
    wrong_node_id INTEGER REFERENCES memory_nodes(id),
    correct_node_id INTEGER REFERENCES memory_nodes(id),
    user_quote TEXT NOT NULL,      -- exact user words
    timestamp INTEGER NOT NULL
);

-- REM consolidation log
CREATE TABLE memory_rem_log (
    id INTEGER PRIMARY KEY,
    started_at INTEGER NOT NULL,
    completed_at INTEGER,
    nodes_added INTEGER DEFAULT 0,
    nodes_merged INTEGER DEFAULT 0,
    nodes_decayed INTEGER DEFAULT 0,
    edges_inferred INTEGER DEFAULT 0,
    abstractions_created INTEGER DEFAULT 0
);
```

## Performance Targets

| Operation | Target | Method |
|-----------|--------|--------|
| Store message | <5ms | Async write + background indexing |
| Generate embedding | <10ms | Qwen 0.8B local |
| Extract facts | <50ms | Qwen 0.8B local (async) |
| Vector search | <5ms | HNSW index in RAM |
| Graph traversal | <5ms | In-memory indices |
| Full retrieval | <20ms | Parallel vector + graph |
| REM consolidation | <60s | Background, per-layer |

## Context Assembly for LLM

The main LLM receives a context window built from memory:

```
[System prompt]

[Relevant memories from knowledge graph — max 500 tokens]
- Project uses PostgreSQL 16 with pgvector extension
- User prefers Rust over Python
- Last week we decided to use FAISS for vector search

[Conversation summary — max 300 tokens]
We've been discussing the memory system architecture. Key decisions:
RAG with HNSW, 4 memory layers, Qwen for extraction.

[User corrections to remember — max 200 tokens]
- User corrected: "Not MySQL, we use PostgreSQL 16"
- User said: "Don't suggest mocking the database in tests"

[Last 4 messages — full text]
USER: ...
ASSISTANT: ...
USER: ...
ASSISTANT: ...

[Current user message]
USER: How should we handle the connection pooling?
```

Total context budget: ~2048 tokens for memory + last 4 messages. Rest for LLM response.

## Reuse from tentaflow-memory

| Component | Old | New Addon | Changes |
|-----------|-----|-----------|---------|
| Knowledge graph | Standalone Rust | WASM addon using storage API | Simplified, uses addon storage |
| Node types | 10 types | 5 types (simpler) | Removed Word, Relation, Pattern, Skill, Rule, Example |
| Edge types | 20+ types | 15 types | Added UserPrefers, UserCorrected, ContradictsNode |
| Vector search | Custom HNSW | embeddings-chunker addon | Reuse existing addon |
| Spreading activation | Custom | Simplified (2-hop max) | Faster, less memory |
| REM consolidation | 5 phases | 5 phases (adapted) | Per-layer, uses addon timer |
| Procedural memory | Patterns + Skills | Removed | Too complex for addon, not needed |
| Multilingual | 12 languages | Not needed (embeddings handle) | Model is multilingual |
| Hot/Warm/Cold storage | Custom 3-tier | Addon storage + cache | Simpler, uses addon APIs |
| Serialization | rkyv zero-copy | serde_json (WASM compat) | Slower but portable |
