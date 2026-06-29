# =============================================================================
# Plik: server.py
# Opis: FastAPI server rerankingu dla Qwen3-Reranker na MLX (Apple Silicon).
#       Qwen3-Reranker ocenia pare (query, document) jako klasyfikacje yes/no:
#       budujemy prompt instrukcyjny, liczymy logity ostatniej pozycji i bierzemy
#       softmax po tokenach "yes"/"no". Wystawia OpenAI-compatible /v1/rerank.
# =============================================================================

import os

import mlx.core as mx
from fastapi import FastAPI
from mlx_lm import load
from pydantic import BaseModel

MODEL = os.environ.get("MODEL", "Qwen/Qwen3-Reranker-0.6B")
SERVED_MODEL_NAME = os.environ.get("SERVED_MODEL_NAME", MODEL)

# Format promptu Qwen3-Reranker (system + user + assistant z pustym <think>).
_PREFIX = (
    "<|im_start|>system\nJudge whether the Document meets the requirements "
    'based on the Query and the Instruct provided. Note that the answer can only '
    'be "yes" or "no".<|im_end|>\n<|im_start|>user\n'
)
_SUFFIX = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
_DEFAULT_INSTRUCTION = (
    "Given a web search query, retrieve relevant passages that answer the query"
)

app = FastAPI()
_model, _tokenizer = load(MODEL)
_yes_id = _tokenizer.encode("yes")[-1]
_no_id = _tokenizer.encode("no")[-1]


class RerankDocument(BaseModel):
    text: str


class RerankRequest(BaseModel):
    query: str
    documents: list
    top_n: int | None = None
    model: str | None = None
    instruction: str | None = None


def _doc_text(doc) -> str:
    return doc.get("text", "") if isinstance(doc, dict) else str(doc)


def _score(query: str, document: str, instruction: str) -> float:
    body = f"<Instruct>: {instruction}\n<Query>: {query}\n<Document>: {document}"
    ids = _tokenizer.encode(_PREFIX + body + _SUFFIX)
    logits = _model(mx.array([ids]))[0, -1, :]
    pair = mx.softmax(mx.array([logits[_no_id], logits[_yes_id]]))
    return float(pair[1])


@app.get("/health")
def health():
    return {"status": "ok", "model": SERVED_MODEL_NAME}


@app.post("/v1/rerank")
@app.post("/rerank")
def rerank(req: RerankRequest):
    instruction = req.instruction or _DEFAULT_INSTRUCTION
    scored = [
        {"index": i, "relevance_score": _score(req.query, _doc_text(d), instruction)}
        for i, d in enumerate(req.documents)
    ]
    scored.sort(key=lambda r: r["relevance_score"], reverse=True)
    if req.top_n is not None:
        scored = scored[: req.top_n]
    return {"model": SERVED_MODEL_NAME, "results": scored}
