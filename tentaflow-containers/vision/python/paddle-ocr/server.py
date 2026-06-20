# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla PaddleOCR-VL (VLM ~0.9B) przez transformers na GPU
#       (CUDA/ROCm) — framework paddlepaddle nie ma kerneli pod B300 (sm_103),
#       wiec OCR idzie przez transformers + torch cu130. Wystawia POST /ocr i
#       z obrazu zwraca tekst oraz uklad strony.
# Przyklad: curl -F image=@strona.png http://127.0.0.1:8095/ocr
# =============================================================================

import base64
import io
import os
import threading

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
from fastapi import FastAPI, File, HTTPException, UploadFile
from fastapi.concurrency import run_in_threadpool
from PIL import Image
from pydantic import BaseModel
from transformers import AutoModelForCausalLM, AutoProcessor

MODEL_ID = os.environ.get("MODEL", "PaddlePaddle/PaddleOCR-VL")
# Zadanie OCR: "OCR:" = transkrypcja; mozliwe tez "Table Recognition:",
# "Formula Recognition:", "Chart Recognition:".
OCR_PROMPT = os.environ.get("OCR_PROMPT", "OCR:")

app = FastAPI(title="PaddleOCR-VL")

_state: dict = {"model": None, "processor": None, "device": None}
_load_lock = threading.Lock()


class OcrBase64Request(BaseModel):
    image_base64: str


def _load() -> None:
    """Laduje model w watku tla (blokujacy load ~20s zawieszalby event loop ->
    supervisor ubijalby proces). /ocr zwraca 503 dopoki model sie laduje."""
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


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _run_ocr(image: Image.Image) -> dict:
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
    text = raw.split("Assistant:", 1)[-1].strip() if "Assistant:" in raw else raw.strip()
    return {"text": text, "layout": [{"text": text, "kind": "document", "bbox": None}]}


@app.get("/health")
@app.get("/healthz")
def health() -> dict:
    return {"status": "ok", "model": MODEL_ID, "device": _state["device"], "loaded": _state["model"] is not None}


@app.post("/ocr")
async def ocr(image: UploadFile = File(...)) -> dict:
    if _state["model"] is None:
        raise HTTPException(503, "Model jeszcze sie laduje, sprobuj za chwile.")
    raw = await image.read()
    img = _decode_image(raw)
    return await run_in_threadpool(_run_ocr, img)


@app.post("/ocr/base64")
async def ocr_base64(req: OcrBase64Request) -> dict:
    if _state["model"] is None:
        raise HTTPException(503, "Model jeszcze sie laduje, sprobuj za chwile.")
    try:
        raw = base64.b64decode(req.image_base64)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy base64: {exc}")
    img = _decode_image(raw)
    return await run_in_threadpool(_run_ocr, img)
