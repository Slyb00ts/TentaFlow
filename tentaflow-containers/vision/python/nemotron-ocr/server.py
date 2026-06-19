# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Nemotron-OCR. Uzywa oficjalnego pipeline'u
#       `nemotron_ocr.inference.pipeline.NemotronOCR` (detector + recognizer +
#       relational). Wystawia POST /ocr (multipart) oraz /ocr/base64, zwraca
#       rozpoznane regiony tekstu. GET /health do probow. Model nie jest
#       modelem transformers — ma wlasny pipeline i wagi .pth pobierane z HF.
# Przyklad: curl -F image=@strona.png http://127.0.0.1:8093/ocr
# =============================================================================

import base64
import io
import os
import threading

import numpy as np
import torch
from fastapi import FastAPI, File, HTTPException, UploadFile
from PIL import Image
from pydantic import BaseModel

from nemotron_ocr.inference.pipeline import NemotronOCR

# merge_level steruje granulacja laczenia tekstu (word/sentence/paragraph).
MERGE_LEVEL = os.environ.get("OCR_MERGE_LEVEL", "paragraph")

app = FastAPI(title="Nemotron-OCR")

# Pipeline ladowany leniwie przy pierwszym zadaniu — wagi .pth (detector,
# recognizer, relational) sciagaja sie z HF przy inicjalizacji, wiec /health
# odpowiada zanim model bedzie gotowy.
_state: dict = {"pipeline": None}


class OcrBase64Request(BaseModel):
    image_base64: str


def _require_cuda() -> None:
    if not torch.cuda.is_available():
        raise HTTPException(
            status_code=503,
            detail="Nemotron-OCR wymaga CUDA — brak dostepnego GPU.",
        )


_LOAD_LOCK = threading.Lock()


def _ensure_pipeline() -> None:
    if _state["pipeline"] is not None:
        return
    with _LOAD_LOCK:
        if _state["pipeline"] is not None:
            return
        _require_cuda()
        # model_dir=None => pipeline sam pobiera checkpointy z HF Hub i cachuje.
        _state["pipeline"] = NemotronOCR(model_dir=None)


# Ladowanie w WATKU TLA przy starcie — synchroniczne ladowanie w handlerze
# blokowaloby event-loop, /health przestawal odpowiadac i supervisor Core
# restartowal proces w petli. W tle /health odpowiada od razu, /ocr zwraca 503
# do czasu gotowosci.
@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_ensure_pipeline, name="model-load", daemon=True).start()


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001 — zwracamy czytelny blad klientowi
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _run_ocr(image: Image.Image) -> dict:
    if _state["pipeline"] is None:
        raise HTTPException(status_code=503, detail="Model jeszcze sie laduje, sprobuj za chwile.")
    pipeline = _state["pipeline"]
    # Pipeline nie przyjmuje PIL.Image — wspiera NumPy (H, W, C), wiec konwertujemy.
    predictions = pipeline(np.asarray(image), merge_level=MERGE_LEVEL, visualize=False)
    # Pipeline zwraca liste dictow regionow. Skladamy plaski tekst z pola "text"
    # (gdy obecne), a pelne regiony oddajemy w "regions".
    parts = [str(p.get("text", "")) for p in predictions if isinstance(p, dict)]
    text = "\n".join(t for t in parts if t)
    return {"text": text, "regions": predictions}


@app.get("/health")
def health() -> dict:
    return {"status": "ok", "model": "nvidia/nemotron-ocr-v1", "loaded": _state["pipeline"] is not None}


@app.post("/ocr")
async def ocr(image: UploadFile = File(...)) -> dict:
    raw = await image.read()
    return _run_ocr(_decode_image(raw))


@app.post("/ocr/base64")
def ocr_base64(req: OcrBase64Request) -> dict:
    try:
        raw = base64.b64decode(req.image_base64)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Bledny base64: {exc}")
    return _run_ocr(_decode_image(raw))
