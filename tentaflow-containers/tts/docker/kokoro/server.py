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

MODEL_PATH = os.environ.get("KOKORO_MODEL", "/app/models/onnx/model.onnx")
VOICES_DIR = os.environ.get("KOKORO_VOICES_DIR", "/app/models/voices")
DEFAULT_VOICE = os.environ.get("KOKORO_DEFAULT_VOICE", "af_heart")

# kokoro_onnx >=0.4 przyjmuje katalog z plikami `*.bin` ALBO konkretny
# `voices.json`. Skladamy katalog do dict aby moc swobodnie dodawac voices.
def _load_voices(voices_dir: str) -> dict:
    voices = {}
    if not os.path.isdir(voices_dir):
        return voices
    for fname in sorted(os.listdir(voices_dir)):
        if not fname.endswith(".bin"):
            continue
        name = fname[:-4]
        path = os.path.join(voices_dir, fname)
        # voice .bin to surowy float32 array stylu — kokoro-onnx 0.4 reads
        # bytes through `np.fromfile`. Konwertujemy do dict zgodnego z API.
        try:
            arr = np.fromfile(path, dtype=np.float32)
            voices[name] = arr
        except Exception as e:
            log.warning("voice %s: %s", name, e)
    return voices

log.info("loading kokoro from %s", MODEL_PATH)
voices = _load_voices(VOICES_DIR)
log.info("loaded %d voices: %s", len(voices), list(voices.keys()))
# kokoro-onnx 0.4 API: Kokoro(model_path, voices_path_or_dict)
kk = Kokoro(MODEL_PATH, voices)


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
    return {"status": "ok", "voices": list(voices.keys())}


@app.get("/v1/audio/voices")
def list_voices():
    """Lista nazw dostepnych voices dla GUI/panel."""
    return {"voices": list(voices.keys())}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    voice = req.voice or DEFAULT_VOICE
    if voice not in voices:
        raise HTTPException(404, f"voice '{voice}' not in {list(voices.keys())}")
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
