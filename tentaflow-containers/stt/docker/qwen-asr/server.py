# =============================================================================
# Plik: server.py
# Opis: Qwen3-ASR STT — OpenAI-zgodny /v1/audio/transcriptions (multipart).
#       Uzywa pakietu `qwen-asr` (Qwen3ASRModel), bo Qwen3-ASR NIE jest w
#       mainline transformers (config ma model_type=qwen3_asr, auto_map=None).
#       Ten sam plik dziala w native venv i w kontenerze Docker.
# =============================================================================

import io
import logging
import os
import threading

import numpy as np
import soundfile as sf
import torch
import uvicorn
from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from starlette.concurrency import run_in_threadpool
from qwen_asr import Qwen3ASRModel

log = logging.getLogger("qwen-asr")
logging.basicConfig(level=logging.INFO)

MODEL_NAME = os.environ.get("QWEN_ASR_MODEL", "Qwen/Qwen3-ASR-1.7B")
DEVICE = "cuda:0" if torch.cuda.is_available() else "cpu"
DTYPE = torch.bfloat16 if torch.cuda.is_available() else torch.float32
# flash_attention_2 gdy flash-attn dostepny (znaczace przyspieszenie); inaczej sdpa.
try:
    import flash_attn  # noqa: F401
    ATTN = "flash_attention_2"
except Exception:
    ATTN = "sdpa"

_asr = None
_load_lock = threading.Lock()


def _load() -> None:
    """Laduje model. Wolane w watku tla przy starcie, zeby NIE blokowac event
    loopu uvicorna (blokujacy load ~60s zawieszal /healthz -> supervisor ubijal
    proces). transcribe zwraca 503 dopoki model sie laduje."""
    global _asr
    with _load_lock:
        if _asr is not None:
            return
        log.info("laduje %s na %s (attn=%s)", MODEL_NAME, DEVICE, ATTN)
        _asr = Qwen3ASRModel.from_pretrained(
            MODEL_NAME,
            dtype=DTYPE,
            device_map=DEVICE,
            attn_implementation=ATTN,
            max_new_tokens=256,
        )
        log.info("qwen-asr gotowy")


app = FastAPI(title="qwen3-asr STT")


@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_load, name="qwen-asr-load", daemon=True).start()


@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL_NAME, "object": "model"}]}


@app.get("/healthz")
@app.get("/health")
def healthz():
    return {"status": "ok", "model": MODEL_NAME, "device": DEVICE, "attn": ATTN}


@app.post("/v1/audio/transcriptions")
@app.post("/audio/transcriptions")
async def transcribe(file: UploadFile = File(...), model: str = Form(None), language: str = Form(None)):
    if _asr is None:
        raise HTTPException(503, "Model jeszcze sie laduje, sprobuj za chwile.")
    raw = await file.read()
    # Blokujaca inferencja w threadpool, zeby nie zawiesic event loopu.
    return await run_in_threadpool(_run, raw, language)


def _run(raw: bytes, language) -> dict:
    try:
        wav, sr = sf.read(io.BytesIO(raw), dtype="float32", always_2d=False)
        wav = np.asarray(wav, dtype=np.float32)
        if wav.ndim > 1:
            wav = wav.mean(axis=1)
        results = _asr.transcribe(
            audio=[(wav, int(sr))],
            language=[language] if language else None,
            return_time_stamps=False,
        )
    except Exception as e:
        log.exception("transcribe failed")
        raise HTTPException(500, str(e)) from e
    text = getattr(results[0], "text", None) if results else None
    return {"text": text or ""}


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=int(os.environ.get("PORT", "8083")))
