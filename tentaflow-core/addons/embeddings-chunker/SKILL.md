# Embeddings Chunker

Intelligent text chunking and embedding generation addon using Jina v5 models.

## Tools

### embeddings_chunker.embed_text
Generate an embedding vector for a single text. Automatically adds the Document: or Query: prefix based on mode.

When to use:
- User wants to generate an embedding for a text
- Converting text to vector for similarity search
- Embedding a search query for vector retrieval

TOON examples:
- `@embeddings_chunker.embed_text|text=Company vacation policy`
- `@embeddings_chunker.embed_text|text=How to request time off?|mode=query`
- `@embeddings_chunker.embed_text|text=Work regulations|mode=document|task=retrieval`

Parameters:
- **text*** — text to generate embedding for
- **mode** — "query" (prefix Query:) or "document" (prefix Document:, default)
- **task** — LoRA adapter: retrieval, text-matching, clustering, classification (default from settings)

### embeddings_chunker.embed_chunks
Split text into chunks and generate embeddings for each. Returns an array of {chunk_text, vector, chunk_index}.

When to use:
- User wants to index a long document for vector search
- Preparing text for RAG pipeline
- Splitting and embedding a document in one step

TOON examples:
- `@embeddings_chunker.embed_chunks|text=<long text>`
- `@embeddings_chunker.embed_chunks|text=<text>|chunk_size=256|chunk_overlap=25`
- `@embeddings_chunker.embed_chunks|text=<text>|mode=document`

Parameters:
- **text*** — text to split and generate embeddings for
- **mode** — "query" or "document" (default)
- **chunk_size** — override chunk size (optional)
- **chunk_overlap** — override overlap (optional)

### embeddings_chunker.embed_batch
Generate embeddings for an array of texts (no chunking). Each text = one vector.

When to use:
- User wants to embed multiple texts at once
- Batch processing for efficiency
- Converting a list of texts to vectors

TOON examples:
- `@embeddings_chunker.embed_batch|texts=["text1","text2","text3"]`
- `@embeddings_chunker.embed_batch|texts=["query1","query2"]|mode=query`

Parameters:
- **texts*** — array of texts to generate embeddings for
- **mode** — "query" or "document" (default)

## Scenarios

### Index a document
```toon
@embeddings_chunker.embed_chunks|text=<document content>|mode=document
```

### Semantic search query
```toon
@embeddings_chunker.embed_text|text=How to request time off?|mode=query|task=retrieval
```

### Batch embed multiple fragments
```toon
@embeddings_chunker.embed_batch|texts=["fragment1","fragment2","fragment3"]|mode=document
```

## Notes
- Mode "query" adds prefix "Query:" — use for search queries
- Mode "document" adds prefix "Document:" — use for indexed content
- Default model: jina-embeddings-v5-text-small (1024 dimensions)
- LoRA adapter affects embedding quality for specific tasks
- Chunk size and overlap are configurable globally in addon settings
