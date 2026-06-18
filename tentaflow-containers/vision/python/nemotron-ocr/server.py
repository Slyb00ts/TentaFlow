# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Nemotron-OCR. Laduje model nvidia/nemotron-ocr-v1 na
#       CUDA i wystawia POST /ocr przyjmujacy obraz (multipart lub base64),
#       zwraca rozpoznany tekst oraz uklad (bboxy linii). GET /health do probow.
# Przyklad: curl -F image=@strona.png http://127.0.0.1:8093/ocr
# =============================================================================

import base64
import io
import os

import torch
from fastapi import FastAPI, File, HTTPException, UploadFile
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModel, AutoProcessor

# Repo modelu — nadpisywalne przez MODEL (deploy ustawia repo z preseta).
MODEL_ID = os.environ.get("MODEL", "nvidia/nemotron-ocr-v1")

app = FastAPI(title="Nemotron-OCR")

# Stan globalny modelu — ladowany leniwie przy pierwszym zadaniu, zeby serwer
# odpowiadal na /health zanim wagi sie sciagna.
_state: dict = {"model": None, "processor": None}


class OcrBase64Request(BaseModel):
    image_base64: str


def _require_cuda() -> str:
    if not torch.cuda.is_available():
        raise HTTPException(
            status_code=503,
            detail="Nemotron-OCR wymaga CUDA — brak dostepnego GPU.",
        )
    return "cuda"


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    device = _require_cuda()
    processor = AutoProcessor.from_pretrained(MODEL_ID, trust_remote_code=True)
    model = AutoModel.from_pretrained(
        MODEL_ID,
        trust_remote_code=True,
        torch_dtype=torch.bfloat16,
    ).to(device)
    model.eval()
    _state["processor"] = processor
    _state["model"] = model


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001 — zwracamy czytelny blad klientowi
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _run_ocr(image: Image.Image) -> dict:
    _ensure_model()
    processor = _state["processor"]
    model = _state["model"]
    inputs = processor(images=image, return_tensors="pt").to(model.device)
    with torch.inference_mode():
        outputs = model.generate(**inputs, max_new_tokens=4096)
    decoded = processor.batch_decode(outputs, skip_special_tokens=True)
    text = decoded[0] if decoded else ""
    # Layout zwracany przez procesor, gdy model dostarcza bboxy linii; przy
    # braku struktury oddajemy pojedynczy blok z calym tekstem.
    layout = _state.get("last_layout") or [{"text": text, "bbox": None}]
    return {"text": text, "layout": layout}


@app.get("/health")
def health() -> dict:
    return {"status": "ok", "model": MODEL_ID, "loaded": _state["model"] is not None}


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
