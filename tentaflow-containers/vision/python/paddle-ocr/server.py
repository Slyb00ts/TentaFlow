# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla PaddleOCR-VL. Laduje model VLM ~0.9B
#       (PaddlePaddle/PaddleOCR-VL) przez transformers na GPU (CUDA/ROCm) —
#       framework paddlepaddle nie ma kerneli pod B300 (sm_103), wiec OCR idzie
#       przez transformers + torch cu130. Wystawia POST /ocr i z obrazu zwraca
#       tekst oraz uklad strony. Bez GPU start jest przerywany (brak CPU).
# Przyklad: curl -F image=@strona.png http://127.0.0.1:8095/ocr
# =============================================================================

import base64
import io
import os

import torch
from fastapi import FastAPI, File, HTTPException, UploadFile
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModel, AutoProcessor

MODEL_ID = os.environ.get("MODEL", "PaddlePaddle/PaddleOCR-VL")

# Polecenie OCR przekazywane do VLM — wymusza pelna transkrypcje strony z ukladem.
OCR_PROMPT = os.environ.get("OCR_PROMPT", "OCR:")

app = FastAPI(title="PaddleOCR-VL")

_state: dict = {"model": None, "processor": None, "device": None}


class OcrBase64Request(BaseModel):
    image_base64: str


def _pick_device() -> str:
    # torch ROCm raportuje urzadzenie tez jako "cuda" (HIP). Brak GPU = blad.
    if not torch.cuda.is_available():
        raise RuntimeError(
            "Brak GPU (CUDA/ROCm). Silnik paddle-ocr (PaddleOCR-VL przez "
            "transformers) wymaga GPU; uruchomienie na CPU jest niewspierane. "
            "Na Apple uzyj paddle-ocr-mlx."
        )
    return "cuda"


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    device = _pick_device()
    # bfloat16 dla oszczednosci VRAM na GPU.
    dtype = torch.bfloat16
    processor = AutoProcessor.from_pretrained(MODEL_ID, trust_remote_code=True)
    model = AutoModel.from_pretrained(
        MODEL_ID,
        trust_remote_code=True,
        torch_dtype=dtype,
    ).to(device)
    model.eval()
    _state["processor"] = processor
    _state["model"] = model
    _state["device"] = device


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _run_ocr(image: Image.Image) -> dict:
    _ensure_model()
    processor = _state["processor"]
    model = _state["model"]
    inputs = processor(
        images=image, text=OCR_PROMPT, return_tensors="pt"
    ).to(model.device)
    with torch.inference_mode():
        outputs = model.generate(**inputs, max_new_tokens=4096)
    decoded = processor.batch_decode(outputs, skip_special_tokens=True)
    text = decoded[0] if decoded else ""
    # PaddleOCR-VL zwraca pelna transkrypcje strony z markupem ukladu; oddajemy
    # surowy tekst jako "text" i caly dokument jako pojedynczy blok layoutu.
    return {
        "text": text,
        "layout": [{"text": text, "kind": "document", "bbox": None}],
    }


@app.get("/health")
def health() -> dict:
    return {
        "status": "ok",
        "model": MODEL_ID,
        "device": _state["device"],
        "loaded": _state["model"] is not None,
    }


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
