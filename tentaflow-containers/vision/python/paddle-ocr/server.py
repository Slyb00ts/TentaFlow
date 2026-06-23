# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla PaddleOCR-VL (VLM ~0.9B) udostepniajacy kontrakt HTTP
#       zgodny z NVIDIA NIM dla OCR. OCR liczony przez transformers + torch cu130
#       (paddlepaddle nie ma kerneli pod nowe GPU). Wystawia POST /v1/infer
#       (NIM OCR) oraz GET /health. PaddleOCR-VL to model generacyjny i zwraca
#       transkrypcje calej strony bez ramek per-region, wiec kazdy obraz mapujemy
#       na jedna detekcje obejmujaca caly kadr (wielokat 0..1).
# Przyklad: curl -X POST http://127.0.0.1:8095/v1/infer \
#           -d '{"input":[{"type":"image_url","url":"data:image/png;base64,..."}]}'
# =============================================================================

import base64
import io
import os
import re
import threading
from typing import Any

# SHIM: modeling_paddleocr_vl wola create_causal_mask(inputs_embeds=...), a
# mainline transformers (4.52+) nazywa ten parametr `input_embeds` (bez 's').
# PaddleOCR-VL bylo rozwijane pod fork/dev. Mapujemy kwarg PRZED zaladowaniem
# zdalnego kodu modelu (modeling robi `from ... import create_causal_mask`).
import transformers.masking_utils as _mu

_orig_ccm = _mu.create_causal_mask


def _ccm_shim(*args, **kwargs):
    if "inputs_embeds" in kwargs:
        kwargs["input_embeds"] = kwargs.pop("inputs_embeds")
    return _orig_ccm(*args, **kwargs)


_mu.create_causal_mask = _ccm_shim

import torch
from fastapi import FastAPI, HTTPException
from fastapi.concurrency import run_in_threadpool
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModelForCausalLM, AutoProcessor

MODEL_ID = os.environ.get("MODEL", "PaddlePaddle/PaddleOCR-VL")
# Zadanie OCR: "OCR:" = transkrypcja; mozliwe tez "Table Recognition:",
# "Formula Recognition:", "Chart Recognition:".
OCR_PROMPT = os.environ.get("OCR_PROMPT", "OCR:")

# Prefiks data-URL: "data:image/png;base64,<...>".
_DATA_URL_RE = re.compile(r"^data:[^;,]*(;base64)?,(?P<payload>.*)$", re.DOTALL)

app = FastAPI(title="PaddleOCR-VL")

_state: dict = {"model": None, "processor": None, "device": None}
_load_lock = threading.Lock()


class InputImage(BaseModel):
    type: str = "image_url"
    url: str


class InferRequest(BaseModel):
    input: list[InputImage]
    # Akceptowane dla zgodnosci z kontraktem NIM OCR; PaddleOCR-VL nie laczy
    # regionow (zwraca pelna transkrypcje strony), wiec wartosc jest ignorowana.
    merge_levels: list[str] | None = None


def _load() -> None:
    """Laduje model w watku tla (blokujacy load ~20s zawieszalby event loop ->
    supervisor ubijalby proces). /v1/infer zwraca 503 dopoki model sie laduje."""
    with _load_lock:
        if _state["model"] is not None:
            return
        if not torch.cuda.is_available():
            raise RuntimeError("Brak GPU (CUDA/ROCm). paddle-ocr wymaga GPU; na Apple uzyj paddle-ocr-mlx.")
        proc = AutoProcessor.from_pretrained(MODEL_ID, trust_remote_code=True)
        model = AutoModelForCausalLM.from_pretrained(
            MODEL_ID, trust_remote_code=True, torch_dtype=torch.bfloat16
        ).to("cuda").eval()
        _state["processor"] = proc
        _state["model"] = model
        _state["device"] = "cuda"


@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_load, name="paddle-ocr-load", daemon=True).start()


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
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _run_ocr(image: Image.Image) -> str:
    proc = _state["processor"]
    model = _state["model"]
    messages = [{"role": "user", "content": [
        {"type": "image", "image": image},
        {"type": "text", "text": OCR_PROMPT},
    ]}]
    inputs = proc.apply_chat_template(
        messages, tokenize=True, add_generation_prompt=True,
        return_dict=True, return_tensors="pt",
    ).to(model.device)
    with torch.inference_mode():
        outputs = model.generate(**inputs, max_new_tokens=4096)
    decoded = proc.batch_decode(outputs, skip_special_tokens=True)
    raw = decoded[0] if decoded else ""
    # apply_chat_template zawiera prompt ("...Assistant: <tekst>"); bierzemy ogon.
    return raw.split("Assistant:", 1)[-1].strip() if "Assistant:" in raw else raw.strip()


def _full_page_detection(tekst: str) -> dict[str, Any]:
    """Buduje pojedyncza detekcje NIM obejmujaca caly kadr (wielokat 0..1).
    PaddleOCR-VL nie zwraca ramek per-region — transkrybuje cala strone."""
    return {
        "text_prediction": {"text": tekst, "confidence": 1.0},
        "bounding_box": {
            "points": [
                {"x": 0.0, "y": 0.0},
                {"x": 1.0, "y": 0.0},
                {"x": 1.0, "y": 1.0},
                {"x": 0.0, "y": 1.0},
            ]
        },
    }


@app.get("/health")
@app.get("/healthz")
def health() -> dict:
    return {"status": "ok", "model": MODEL_ID, "device": _state["device"], "loaded": _state["model"] is not None}


@app.post("/v1/infer")
async def infer(req: InferRequest) -> dict:
    if _state["model"] is None:
        raise HTTPException(503, "Model jeszcze sie laduje, sprobuj za chwile.")
    if not req.input:
        raise HTTPException(status_code=400, detail="Pole 'input' nie moze byc puste.")

    data = []
    suma_bajtow = 0
    for indeks, wejscie in enumerate(req.input):
        raw = _decode_data_url(wejscie.url)
        suma_bajtow += len(raw)
        obraz = _decode_image(raw)
        tekst = await run_in_threadpool(_run_ocr, obraz)
        data.append({"index": indeks, "text_detections": [_full_page_detection(tekst)]})

    return {
        "data": data,
        "usage": {"images_size_mb": round(suma_bajtow / (1024 * 1024), 6)},
    }
