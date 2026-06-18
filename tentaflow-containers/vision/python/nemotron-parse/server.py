# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Nemotron-Parse. Laduje NVIDIA-Nemotron-Parse-v1.2
#       przez transformers na GPU (CUDA/ROCm; bez GPU start jest przerywany)
#       i wystawia POST /parse — z obrazu strony zwraca tekst oraz strukture
#       (markdown + bloki layoutu). GET /health do probow.
# Przyklad: curl -F image=@faktura.png http://127.0.0.1:8094/parse
# =============================================================================

import base64
import io
import os

import torch
from fastapi import FastAPI, File, HTTPException, UploadFile
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModelForVision2Seq, AutoProcessor

MODEL_ID = os.environ.get("MODEL", "nvidia/NVIDIA-Nemotron-Parse-v1.2")

app = FastAPI(title="Nemotron-Parse")

_state: dict = {"model": None, "processor": None, "device": None}


class ParseBase64Request(BaseModel):
    image_base64: str


def _pick_device() -> str:
    # torch ROCm raportuje urzadzenie tez jako "cuda" (HIP). Brak GPU = blad.
    if not torch.cuda.is_available():
        raise RuntimeError(
            "Brak GPU (CUDA/ROCm). Silnik nemotron-parse wymaga GPU; "
            "uruchomienie na CPU jest niewspierane."
        )
    return "cuda"


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    device = _pick_device()
    # bfloat16 dla oszczednosci VRAM na GPU.
    dtype = torch.bfloat16
    processor = AutoProcessor.from_pretrained(MODEL_ID, trust_remote_code=True)
    model = AutoModelForVision2Seq.from_pretrained(
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


def _run_parse(image: Image.Image) -> dict:
    _ensure_model()
    processor = _state["processor"]
    model = _state["model"]
    inputs = processor(images=image, return_tensors="pt").to(model.device)
    with torch.inference_mode():
        outputs = model.generate(**inputs, max_new_tokens=4096)
    decoded = processor.batch_decode(outputs, skip_special_tokens=True)
    markdown = decoded[0] if decoded else ""
    # mBART dekoder Nemotron-Parse zwraca tekst z markupem ukladu; oddajemy
    # surowy markdown jako "text" i strukture jako pojedynczy blok dokumentu.
    return {
        "text": markdown,
        "layout": [{"text": markdown, "kind": "document", "bbox": None}],
    }


@app.get("/health")
def health() -> dict:
    return {
        "status": "ok",
        "model": MODEL_ID,
        "device": _state["device"],
        "loaded": _state["model"] is not None,
    }


@app.post("/parse")
async def parse(image: UploadFile = File(...)) -> dict:
    raw = await image.read()
    return _run_parse(_decode_image(raw))


@app.post("/parse/base64")
def parse_base64(req: ParseBase64Request) -> dict:
    try:
        raw = base64.b64decode(req.image_base64)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Bledny base64: {exc}")
    return _run_parse(_decode_image(raw))
