# =============================================================================
# Plik: server.py
# Opis: FastAPI wrapper na Qwen3-ASR-1.7B — eksponuje
#       OpenAI-compatible /v1/audio/transcriptions.
# =============================================================================

import io
import os
import tempfile
from typing import Optional

import torch
import soundfile as sf
import librosa
from fastapi import FastAPI, UploadFile, Form, File
from fastapi.responses import JSONResponse
from transformers import AutoModelForCausalLM, AutoProcessor


MODEL_NAME = os.environ.get("QWEN_ASR_MODEL", "Qwen/Qwen3-ASR-1.7B")
DEVICE = "cuda" if torch.cuda.is_available() else "cpu"
DTYPE = torch.bfloat16 if DEVICE == "cuda" else torch.float32

app = FastAPI()
_model = None
_processor = None


def get_model():
    global _model, _processor
    if _model is None:
        print(f"[qwen-asr] laduje {MODEL_NAME} na {DEVICE}", flush=True)
        _processor = AutoProcessor.from_pretrained(MODEL_NAME, trust_remote_code=True)
        _model = AutoModelForCausalLM.from_pretrained(
            MODEL_NAME,
            torch_dtype=DTYPE,
            device_map="auto",
            trust_remote_code=True,
        )
        _model.eval()
        print("[qwen-asr] model gotowy", flush=True)
    return _model, _processor


@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL_NAME, "object": "model"}]}


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str = Form("qwen-asr"),
    language: Optional[str] = Form(None),
    response_format: Optional[str] = Form("json"),
    data: Optional[str] = Form(None),
):
    audio_bytes = await file.read()
    audio, sr = sf.read(io.BytesIO(audio_bytes))
    if sr != 16000:
        audio = librosa.resample(audio.astype("float32"), orig_sr=sr, target_sr=16000)
        sr = 16000

    # Typed request-time overrides z BackendClient (multipart `data=<json>`).
    # Klucze: max_new_tokens (default 512). Wartosci spoza zakresu / typu sa
    # ignorowane defensywnie — wrapper nie powinien blokowac requestow.
    overrides = {}
    if data:
        try:
            import json as _json
            overrides = _json.loads(data)
            if not isinstance(overrides, dict):
                overrides = {}
        except Exception:
            overrides = {}
    max_new_tokens = int(overrides.get("max_new_tokens", 512))

    model_obj, processor = get_model()
    inputs = processor(audios=audio, sampling_rate=sr, return_tensors="pt").to(DEVICE)
    if DEVICE == "cuda":
        inputs = {k: v.to(DTYPE) if v.is_floating_point() else v for k, v in inputs.items()}
    with torch.inference_mode():
        out = model_obj.generate(**inputs, max_new_tokens=max_new_tokens)
    text = processor.batch_decode(out, skip_special_tokens=True)[0]

    if response_format == "text":
        return text
    return JSONResponse({"text": text})
