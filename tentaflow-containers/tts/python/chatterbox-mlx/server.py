"""
Chatterbox MLX server. Apple Silicon natywnie przez `mlx-audio`.
OpenAI-compatible /v1/audio/speech.
"""
import io
import os
import logging
from typing import Optional

import numpy as np
import soundfile as sf
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel

from mlx_audio.tts.utils import load_model

log = logging.getLogger("chatterbox-mlx-server")
logging.basicConfig(level=logging.INFO)

MODEL_REPO = os.environ.get("CHATTERBOX_MLX_MODEL", "mlx-community/chatterbox-turbo-4bit")
DEFAULT_LANG = os.environ.get("CHATTERBOX_MLX_LANGUAGE", "English")
SUPPORTED_LANGS = [
    "English", "Spanish", "French", "German", "Italian", "Portuguese",
    "Polish", "Turkish", "Russian", "Dutch", "Czech", "Arabic",
    "Chinese", "Japanese", "Hungarian", "Korean",
]

log.info("loading chatterbox-mlx %s", MODEL_REPO)
MODEL = load_model(MODEL_REPO)
SAMPLE_RATE = int(getattr(MODEL, "sample_rate", 24000))
log.info("ready, sr=%d", SAMPLE_RATE)


class SpeechRequest(BaseModel):
    model: Optional[str] = "chatterbox-mlx"
    input: str
    voice: Optional[str] = None
    language: Optional[str] = None  # "Polish", "English", ...
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0


app = FastAPI(title="Chatterbox MLX (mlx-audio)", version="0.4.2")


@app.get("/healthz")
def healthz():
    return {"status": "ok", "sample_rate": SAMPLE_RATE, "model": MODEL_REPO}


# OpenAI-compatible stub — Tentaflow deploy runner czeka na 200 z /v1/models
# zanim oznaczy serwis jako gotowy. Bez tego progress utyka na 86% / 404.
@app.get("/v1/models")
def list_models():
    return {
        "object": "list",
        "data": [{
            "id": MODEL_REPO,
            "object": "model",
            "owned_by": "chatterbox-mlx",
        }],
    }


@app.get("/v1/audio/voices")
def list_voices():
    return {"languages": SUPPORTED_LANGS, "voice_cloning_via_audio_prompt": True}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    lang = req.language or DEFAULT_LANG
    try:
        kwargs = {"lang_code": lang}
        if req.voice:
            kwargs["voice"] = req.voice
        # mlx-audio.generate jest generatorem; zbieramy wszystkie segmenty.
        chunks = []
        for result in MODEL.generate(req.input, **kwargs):
            audio = result.audio
            if hasattr(audio, "tolist"):
                arr = np.asarray(audio.tolist(), dtype=np.float32)
            else:
                arr = np.asarray(audio, dtype=np.float32)
            if arr.ndim > 1:
                arr = arr.reshape(-1)
            chunks.append(arr)
        if not chunks:
            raise HTTPException(500, "model.generate() did not yield audio")
        samples = np.concatenate(chunks).astype(np.float32)
    except HTTPException:
        raise
    except Exception as e:
        log.exception("chatterbox-mlx.generate failed")
        raise HTTPException(500, str(e)) from e

    fmt = (req.response_format or "wav").lower()
    if fmt not in ("wav", "flac", "ogg"):
        fmt = "wav"
    sf_format = {"wav": "WAV", "flac": "FLAC", "ogg": "OGG"}.get(fmt, "WAV")
    buf = io.BytesIO()
    sf.write(buf, samples, SAMPLE_RATE, format=sf_format)
    return Response(content=buf.getvalue(), media_type=f"audio/{fmt}")
