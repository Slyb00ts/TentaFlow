# Memory

Persistent AI memory addon providing knowledge graph, vector search, and conversation history across 4 layers: Global, Project, User, and Conversation.

## Tools

### memory.memory_store
Store a fact, preference, or piece of information.

When to use:
- User explicitly asks to remember something
- User states a preference or fact about themselves
- Important decision is made in conversation
- User corrects AI — store the correction

TOON examples:
- `@memory.memory_store|layer=user|fact=User prefers Rust over Python`
- `@memory.memory_store|layer=project|fact=We use PostgreSQL 16 with pgvector`
- `@memory.memory_store|layer=global|fact=TentaFlow uses WASM sandboxing for addons`

Parameters:
- **fact*** — the information to store
- **layer** — global, project, user, conversation (default: auto-detect)
- **tags** — comma-separated tags for categorization

### memory.memory_recall
Recall relevant memories for a given context or query.

When to use:
- Before generating a response (automatic)
- User asks "what do you know about X"
- User asks "do you remember when we..."
- Need context from previous conversations

TOON examples:
- `@memory.memory_recall|query=database setup for this project`
- `@memory.memory_recall|query=user preferences|layers=user`
- `@memory.memory_recall|query=authentication decisions|layers=project`

Parameters:
- **query*** — what to search for
- **layers** — which layers to search (default: all)
- **limit** — max results (default: 10)

### memory.memory_search
Search memories by semantic similarity.

When to use:
- Looking for specific information
- Finding related facts or decisions

TOON examples:
- `@memory.memory_search|query=API rate limiting|type=decision`
- `@memory.memory_search|query=deployment pipeline|layers=project`

Parameters:
- **query*** — search query
- **layers** — which layers (default: all)
- **type** — fact, decision, correction, preference (default: all)
- **limit** — max results (default: 10)

### memory.memory_forget
Remove a specific memory.

When to use:
- User explicitly asks to forget something
- Information is outdated and should be removed

TOON examples:
- `@memory.memory_forget|query=old database password`
- `@memory.memory_forget|node_id=12345`

Parameters:
- **query** — what to forget (semantic search)
- **node_id** — specific node ID to remove

### memory.memory_correct
Correct a wrong memory with the right information.

When to use:
- User says AI remembered something wrong
- Facts have changed and need updating

TOON examples:
- `@memory.memory_correct|wrong=We use MySQL|correct=We use PostgreSQL 16|source=user_correction`

Parameters:
- **wrong*** — the incorrect fact
- **correct*** — the correct fact
- **source** — user_correction, fact_update (default: user_correction)

### memory.memory_summarize
Generate or retrieve a summary of conversation or topic.

When to use:
- User asks for a recap
- Conversation is getting long and needs summary
- Starting new session and need context

TOON examples:
- `@memory.memory_summarize|conversation_id=current`
- `@memory.memory_summarize|topic=authentication implementation`

Parameters:
- **conversation_id** — conversation to summarize (default: current)
- **topic** — summarize by topic across conversations

### memory.memory_graph_query
Query the knowledge graph for related facts and connections.

When to use:
- Need to understand relationships between concepts
- Finding all facts about an entity
- Tracing decision chains

TOON examples:
- `@memory.memory_graph_query|node=PostgreSQL|depth=2`
- `@memory.memory_graph_query|node=auth system|relations=Requires,Causes`

Parameters:
- **node*** — starting concept
- **depth** — how many hops (default: 2, max: 3)
- **relations** — filter by relation types

### memory.memory_status
Show memory statistics and health.

When to use:
- User asks about memory status
- Debugging memory issues

TOON examples:
- `@memory.memory_status`
- `@memory.memory_status|layer=project`

Parameters:
- **layer** — specific layer (default: all)

## Scenarios

### Automatic context building (every message)
```toon
@memory.memory_recall|query={user_message}|limit=5
@memory.memory_store|fact={extracted_facts}|layer=conversation
```

### User correction flow
```toon
@memory.memory_correct|wrong=We use MySQL|correct=We use PostgreSQL 16
@memory.memory_store|layer=user|fact=User corrected: PostgreSQL 16 not MySQL|tags=correction
```

### Starting new conversation with context
```toon
@memory.memory_recall|query={first_message}|layers=user,project
@memory.memory_summarize|topic=recent_work
```

## Notes
- Memory recall is automatic before every LLM response — no need to call explicitly
- User corrections have highest priority and are always included in context
- Conversation layer is temporary — important facts promoted to User/Project during REM
- REM consolidation runs every hour in background
- Graph queries are fast (<5ms) — use them for relationship exploration
- Vector search is semantic — works across languages
