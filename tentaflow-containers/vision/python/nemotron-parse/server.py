# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Nemotron-Parse (NVIDIA-Nemotron-Parse-v1.2). Model to
#       transformers VLM (trust_remote_code) z wlasnym task-promptem i logits
#       processorami; uzywamy oficjalnej sciezki z example_with_processor.py.
#       Wystawia OpenAI-kompatybilny POST /v1/chat/completions: obraz pobierany z
#       ostatniej wiadomosci uzytkownika, a wynikowy markdown trafia do
#       message.content. GET /health do prob.
# Przyklad: curl -X POST http://127.0.0.1:8094/v1/chat/completions -d \
#   '{"model":"nemotron-parse","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,..."}}]}]}'
# =============================================================================

import base64
import io
import os
import re
import threading
import time
import uuid
from typing import Any

import torch
from fastapi import FastAPI, HTTPException
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModel, AutoProcessor, AutoTokenizer, GenerationConfig

# Skrypty pomocnicze z repo HF (skopiowane do /app przez Dockerfile).
from postprocessing import extract_classes_bboxes, postprocess_text
from hf_logits_processor import TableInsertionLogitsProcessor, RepetitionStopProcessor

MODEL_ID = os.environ.get("MODEL", "nvidia/NVIDIA-Nemotron-Parse-v1.2")
# Prompt zadania: predykcja bbox + klas + markdown (jak w example_with_processor.py).
TASK_PROMPT = "</s><s><predict_bbox><predict_classes><output_markdown><predict_no_text_in_pic>"

# Prefiks data-URL: "data:image/png;base64,<...>".
_DATA_URL_RE = re.compile(r"^data:[^;,]*(;base64)?,(?P<payload>.*)$", re.DOTALL)

app = FastAPI(title="Nemotron-Parse")

# Stan ladowany leniwie przy pierwszym zadaniu, zeby /health odpowiadal wczesniej.
_state: dict = {"model": None, "tokenizer": None, "processor": None, "gen": None}


class ChatMessage(BaseModel):
    role: str
    # Zgodnie z OpenAI: content moze byc stringiem albo lista czesci (tekst/obraz).
    content: Any


class ChatCompletionRequest(BaseModel):
    model: str | None = None
    messages: list[ChatMessage]


def _require_cuda() -> None:
    if not torch.cuda.is_available():
        raise HTTPException(
            status_code=503,
            detail="Nemotron-Parse wymaga CUDA — brak dostepnego GPU.",
        )


_LOAD_LOCK = threading.Lock()


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    with _LOAD_LOCK:
        if _state["model"] is not None:
            return
        _require_cuda()
        # UWAGA: model NemotronParse (custom trust_remote_code) NIE wspiera
        # `attn_implementation`, a `torch.compile(reduce-overhead)` (CUDA graphs)
        # NIE jest thread-safe — współbieżne generate() w jednym workerze (wątki
        # FastAPI) dzielą stan grafu → race → PUSTE/uszkodzone wyjście. Oba
        # próbowane i ODRZUCONE: bez zysku (direct call 15.7s tak czy tak), a
        # compile psuł concurrency. Skalowanie idzie przez WIELE WORKERÓW
        # (entrypoint.sh `--workers`) = osobne procesy = brak współdzielenia stanu.
        model = (
            AutoModel.from_pretrained(
                MODEL_ID, trust_remote_code=True, torch_dtype=torch.bfloat16
            )
            .to("cuda")
            .eval()
        )
        _state["tokenizer"] = AutoTokenizer.from_pretrained(MODEL_ID)
        _state["processor"] = AutoProcessor.from_pretrained(MODEL_ID, trust_remote_code=True)
        _state["gen"] = GenerationConfig.from_pretrained(MODEL_ID, trust_remote_code=True)
        _state["model"] = model


# Model ladowany w WATKU TLA przy starcie — synchroniczne ladowanie w handlerze
# blokowaloby event-loop uvicorna, przez co /health przestawal odpowiadac i
# supervisor Core ubijal+restartowal proces w petli (zwlaszcza dla wiekszych
# modeli). W tle /health odpowiada od razu, a /v1/chat/completions zwraca 503 do
# czasu gotowosci.
@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_ensure_model, name="model-load", daemon=True).start()


def _decode_data_url(url: str) -> bytes:
    """Wyciaga surowe bajty obrazu z data-URL (base64) lub czystego base64."""
    dopasowanie = _DATA_URL_RE.match(url.strip())
    payload = dopasowanie.group("payload") if dopasowanie else url.strip()
    try:
        return base64.b64decode(payload)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Bledny base64 w url: {exc}")


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001 — zwracamy czytelny blad klientowi
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _wyodrebnij_url_obrazu(content: Any) -> str:
    """Znajduje URL obrazu w content wiadomosci OpenAI. Obsluguje:
    - liste czesci z {"type":"image_url","image_url":{"url":...}}
    - liste czesci z {"type":"image_url","image_url":"<str>"} lub {"url":...}
    - czysty string traktowany jako sam URL/data-URL obrazu."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        for czesc in content:
            if not isinstance(czesc, dict):
                continue
            if czesc.get("type") not in (None, "image_url"):
                continue
            obraz = czesc.get("image_url")
            if isinstance(obraz, dict) and obraz.get("url"):
                return obraz["url"]
            if isinstance(obraz, str) and obraz:
                return obraz
            if isinstance(czesc.get("url"), str) and czesc["url"]:
                return czesc["url"]
    raise HTTPException(status_code=400, detail="Brak obrazu (image_url) w ostatniej wiadomosci uzytkownika.")


def _obraz_z_zadania(req: ChatCompletionRequest) -> Image.Image:
    uzytkownik = [m for m in req.messages if m.role == "user"]
    if not uzytkownik:
        raise HTTPException(status_code=400, detail="Brak wiadomosci uzytkownika z obrazem.")
    url = _wyodrebnij_url_obrazu(uzytkownik[-1].content)
    return _decode_image(_decode_data_url(url))


def _run_parse(image: Image.Image) -> str:
    if _state["model"] is None:
        raise HTTPException(status_code=503, detail="Model jeszcze sie laduje, sprobuj za chwile.")
    model = _state["model"]
    tokenizer = _state["tokenizer"]
    processor = _state["processor"]
    inputs = processor(
        images=[image], text=TASK_PROMPT, return_tensors="pt", add_special_tokens=False
    ).to(model.device)

    table_processor = TableInsertionLogitsProcessor(
        tokenizer=tokenizer, table_prefix="\\begin{tabular}"
    )
    repetition_processor = RepetitionStopProcessor(
        tokenizer=tokenizer, max_repetitions=10, ngram_sizes=[3, 4, 5, 6], window_size=500
    )
    with torch.inference_mode():
        outputs = model.generate(
            **inputs,
            generation_config=_state["gen"],
            logits_processor=[table_processor, repetition_processor],
        )
    table_processor.reset()
    repetition_processor.reset()

    generated_text = processor.batch_decode(outputs, skip_special_tokens=True)[0]
    classes, bboxes, texts = extract_classes_bboxes(generated_text)
    texts = [
        postprocess_text(t, cls=c, table_format="HTML", text_format="markdown", blank_text_in_figures=False)
        for t, c in zip(texts, classes)
    ]
    return "\n\n".join(t for t in texts if t)


@app.get("/health")
def health() -> dict:
    return {"status": "ok", "model": MODEL_ID, "loaded": _state["model"] is not None}


@app.post("/v1/chat/completions")
def chat_completions(req: ChatCompletionRequest) -> dict:
    image = _obraz_z_zadania(req)
    markdown = _run_parse(image)
    return {
        "id": f"chatcmpl-{uuid.uuid4().hex}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": req.model or MODEL_ID,
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": markdown},
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
    }
