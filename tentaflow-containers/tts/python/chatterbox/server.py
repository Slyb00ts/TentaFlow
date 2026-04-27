"""
Chatterbox Multilingual TTS server. POST /v1/audio/speech kompatybilne z OpenAI.
23 jezyki w tym polski (`pl`). MPS na Apple, CUDA na nvidia, CPU fallback.
"""
import io
import os
import logging
from typing import Optional

import numpy as np
import soundfile as sf
import torch
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel

log = logging.getLogger("chatterbox-server")
logging.basicConfig(level=logging.INFO)


def _detect_device() -> str:
    if torch.cuda.is_available():
        return "cuda"
    if hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


DEVICE = os.environ.get("CHATTERBOX_DEVICE", _detect_device())
log.info("loading chatterbox multilingual on %s", DEVICE)

# Lazy import — brak `chatterbox.mtl_tts` przed `pip install` da czytelny error.
from chatterbox.mtl_tts import ChatterboxMultilingualTTS, SUPPORTED_LANGUAGES

# Patch torch.load dla MPS (Apple) zeby checkpointy ladowaly sie na M-series.
if DEVICE == "mps":
    _orig_load = torch.load
    def _patched_load(*args, **kwargs):
        kwargs.setdefault("map_location", torch.device(DEVICE))
        return _orig_load(*args, **kwargs)
    torch.load = _patched_load

MODEL = ChatterboxMultilingualTTS.from_pretrained(device=DEVICE)
SAMPLE_RATE = int(getattr(MODEL, "sr", 24000))
log.info("chatterbox loaded, sr=%d, langs=%s", SAMPLE_RATE, SUPPORTED_LANGUAGES)


class SpeechRequest(BaseModel):
    model: Optional[str] = "chatterbox"
    input: str
    voice: Optional[str] = None
    language: Optional[str] = "en"
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0
    # Chatterbox-specific:
    audio_prompt_path: Optional[str] = None
    exaggeration: Optional[float] = 0.5
    cfg_weight: Optional[float] = 0.5


app = FastAPI(title="Chatterbox Multilingual TTS", version="1.0.0")


@app.get("/healthz")
def healthz():
    return {"status": "ok", "sample_rate": SAMPLE_RATE, "languages": list(SUPPORTED_LANGUAGES)}


# OpenAI-compatible stub — Tentaflow deploy runner czeka na 200 z /v1/models
# zanim oznaczy serwis jako gotowy. Bez tego progress utyka na 86% / 404.
@app.get("/v1/models")
def list_models():
    return {
        "object": "list",
        "data": [{
            "id": "chatterbox-multilingual",
            "object": "model",
            "owned_by": "chatterbox",
        }],
    }


@app.get("/v1/audio/voices")
def list_voices():
    """Chatterbox sample-by-sample voice cloning — nie ma dyskretnych voices.
    Zwracamy liste jezykow + flag dla GUI."""
    return {"languages": list(SUPPORTED_LANGUAGES), "voice_cloning_via_audio_prompt": True}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    if req.language not in SUPPORTED_LANGUAGES:
        raise HTTPException(400, f"language '{req.language}' not in {list(SUPPORTED_LANGUAGES)}")
    try:
        kwargs = {"language_id": req.language}
        if req.audio_prompt_path and os.path.exists(req.audio_prompt_path):
            kwargs["audio_prompt_path"] = req.audio_prompt_path
        if req.exaggeration is not None:
            kwargs["exaggeration"] = req.exaggeration
        if req.cfg_weight is not None:
            kwargs["cfg_weight"] = req.cfg_weight
        with torch.no_grad():
            wav = MODEL.generate(req.input, **kwargs)
        # wav jest tensorem (channels, samples) lub (samples,). Konwertujemy.
        if isinstance(wav, torch.Tensor):
            wav = wav.detach().cpu().numpy()
        if wav.ndim == 2:
            samples = wav[0]
        else:
            samples = wav
        samples = samples.astype(np.float32)
    except HTTPException:
        raise
    except Exception as e:
        log.exception("chatterbox.generate failed")
        raise HTTPException(500, str(e)) from e

    fmt = (req.response_format or "wav").lower()
    if fmt not in ("wav", "flac", "ogg"):
        fmt = "wav"
    sf_format = {"wav": "WAV", "flac": "FLAC", "ogg": "OGG"}.get(fmt, "WAV")
    buf = io.BytesIO()
    sf.write(buf, samples, SAMPLE_RATE, format=sf_format)
    return Response(content=buf.getvalue(), media_type=f"audio/{fmt}")
