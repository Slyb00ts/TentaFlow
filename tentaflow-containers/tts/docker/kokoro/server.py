"""
Kokoro 82M TTS server. Eksponuje OpenAI-compatible `/v1/audio/speech`
endpoint dla Linux/Windows hostow.
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
from kokoro_onnx import Kokoro

log = logging.getLogger("kokoro-server")
logging.basicConfig(level=logging.INFO)

MODEL_PATH = os.environ.get("KOKORO_MODEL", "/app/models/kokoro-v1.0.onnx")
VOICES_PATH = os.environ.get("KOKORO_VOICES", "/app/models/voices-v1.0.bin")
DEFAULT_VOICE = os.environ.get("KOKORO_DEFAULT_VOICE", "af_heart")

# kokoro-onnx 0.4.x: Kokoro(model_path, voices_path) gdzie voices_path to
# pojedynczy plik npz (`voices-v1.0.bin`) ladowany przez np.load i indeksowany
# nazwa glosu. Model i glosy musza pochodzic z tego samego wydania kokoro-onnx
# (inny model.onnx ma niezgodne typy wejsc ONNX -> InvalidArgument).
log.info("loading kokoro model=%s voices=%s", MODEL_PATH, VOICES_PATH)
kk = Kokoro(MODEL_PATH, VOICES_PATH)
voices = kk.get_voices()
log.info("loaded %d voices: %s", len(voices), voices)


class SpeechRequest(BaseModel):
    model: Optional[str] = "tts-1"
    input: str
    voice: Optional[str] = None
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0
    language: Optional[str] = "en-us"


app = FastAPI(title="kokoro-onnx TTS", version="1.0.0")


@app.get("/healthz")
def healthz():
    return {"status": "ok", "voices": voices}


@app.get("/v1/audio/voices")
def list_voices():
    """Lista nazw dostepnych voices dla GUI/panel."""
    return {"voices": voices}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    voice = req.voice or DEFAULT_VOICE
    if voice not in voices:
        raise HTTPException(404, f"voice '{voice}' not in {voices}")
    try:
        samples, sr = kk.create(
            req.input,
            voice=voice,
            speed=req.speed or 1.0,
            lang=req.language or "en-us",
        )
    except Exception as e:
        log.exception("kokoro.create failed")
        raise HTTPException(500, str(e)) from e
    fmt = (req.response_format or "wav").lower()
    if fmt not in ("wav", "flac", "mp3", "ogg"):
        fmt = "wav"
    buf = io.BytesIO()
    sf_format = {"wav": "WAV", "flac": "FLAC", "mp3": "MP3", "ogg": "OGG"}.get(fmt, "WAV")
    sf.write(buf, samples, sr, format=sf_format)
    return Response(content=buf.getvalue(), media_type=f"audio/{fmt}")
