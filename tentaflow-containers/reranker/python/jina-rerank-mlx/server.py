# =============================================================================
# Plik: server.py
# Opis: FastAPI server rerankingu dla jina-reranker-v3 na MLX (Apple Silicon).
#       Repo `jina-reranker-v3-mlx` dostarcza wlasny modul `rerank` z klasa
#       MLXReranker (natywny port MLX, 100% zgodnosc score z oryginalem). Sciaga
#       repo przez snapshot_download, dopina je do sys.path i woła jego rerank().
#       Wystawia OpenAI-compatible /v1/rerank.
# =============================================================================

import os
import sys

from fastapi import FastAPI
from huggingface_hub import snapshot_download
from pydantic import BaseModel

MODEL = os.environ.get("MODEL", "jinaai/jina-reranker-v3-mlx")
SERVED_MODEL_NAME = os.environ.get("SERVED_MODEL_NAME", MODEL)

# Repo niesie wlasny kod (rerank.py z MLXReranker) — pobierz i dopnij do sys.path.
_local = snapshot_download(MODEL)
if _local not in sys.path:
    sys.path.insert(0, _local)

from rerank import MLXReranker  # noqa: E402  (dostepne po snapshot_download)

app = FastAPI()
_reranker = MLXReranker(_local)


class RerankRequest(BaseModel):
    query: str
    documents: list
    top_n: int | None = None
    model: str | None = None


def _doc_text(doc) -> str:
    return doc.get("text", "") if isinstance(doc, dict) else str(doc)


@app.get("/health")
def health():
    return {"status": "ok", "model": SERVED_MODEL_NAME}


@app.post("/v1/rerank")
@app.post("/rerank")
def rerank(req: RerankRequest):
    docs = [_doc_text(d) for d in req.documents]
    ranked = _reranker.rerank(req.query, docs)
    # MLXReranker.rerank zwraca liste (index, score) malejaco wg score; normalizuj
    # do formatu OpenAI rerank niezaleznie od ksztaltu krotki/slownika.
    results = []
    for item in ranked:
        if isinstance(item, dict):
            idx = item.get("index", item.get("corpus_id"))
            score = item.get("relevance_score", item.get("score"))
        else:
            idx, score = item[0], item[1]
        results.append({"index": int(idx), "relevance_score": float(score)})
    if req.top_n is not None:
        results = results[: req.top_n]
    return {"model": SERVED_MODEL_NAME, "results": results}
