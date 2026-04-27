"""
Kyutai Pocket TTS — server FastAPI z OpenAI-compatible /v1/audio/speech.
Maly model 100M, CPU only, EN/FR/DE/PT/IT/ES, voice cloning przez audio prompt.
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

from pocket_tts import TTSModel

log = logging.getLogger("pocket-tts-server")
logging.basicConfig(level=logging.INFO)

LANGUAGE = os.environ.get("POCKET_TTS_LANGUAGE", "english")
DEFAULT_VOICE = os.environ.get("POCKET_TTS_DEFAULT_VOICE", "alba")
# Wbudowane nazwy z README — caller moze tez podac sciezke do pliku audio
# (lokalnie lub `hf://kyutai/tts-voices/...`).
PREMADE_VOICES = ["alba", "marius", "javert", "jean", "fantine", "cosette",
                  "eponine", "azelma"]
SUPPORTED_LANGS = ["english", "french", "german", "portuguese", "italian", "spanish"]

log.info("loading pocket-tts language=%s", LANGUAGE)
MODEL = TTSModel.load_model(language=LANGUAGE) if "language" in TTSModel.load_model.__code__.co_varnames else TTSModel.load_model()
SAMPLE_RATE = int(MODEL.sample_rate)
log.info("pocket-tts ready, sample_rate=%d", SAMPLE_RATE)

# Cache voice states zeby nie placic 200 ms za kazdym razem na voice load.
_VOICE_CACHE: dict = {}


def _voice_state(name: str):
    if name not in _VOICE_CACHE:
        try:
            _VOICE_CACHE[name] = MODEL.get_state_for_audio_prompt(name)
        except Exception as e:
            log.warning("voice '%s' load failed: %s", name, e)
            raise HTTPException(404, f"voice '{name}' not found: {e}") from e
    return _VOICE_CACHE[name]


class SpeechRequest(BaseModel):
    model: Optional[str] = "pocket-tts"
    input: str
    voice: Optional[str] = None
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0


app = FastAPI(title="Kyutai Pocket TTS", version="2.0.0")


@app.get("/healthz")
def healthz():
    return {"status": "ok", "sample_rate": SAMPLE_RATE, "language": LANGUAGE}


@app.get("/v1/audio/voices")
def list_voices():
    return {"voices": PREMADE_VOICES, "languages": SUPPORTED_LANGS}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    voice_name = req.voice or DEFAULT_VOICE
    try:
        voice_state = _voice_state(voice_name)
        audio = MODEL.generate_audio(voice_state, req.input)
        # generate_audio zwraca 1D torch tensor PCM Float32.
        if hasattr(audio, "numpy"):
            samples = audio.detach().cpu().numpy().astype(np.float32)
        else:
            samples = np.asarray(audio, dtype=np.float32)
    except HTTPException:
        raise
    except Exception as e:
        log.exception("pocket-tts.generate_audio failed")
        raise HTTPException(500, str(e)) from e

    fmt = (req.response_format or "wav").lower()
    if fmt not in ("wav", "flac", "ogg"):
        fmt = "wav"
    sf_format = {"wav": "WAV", "flac": "FLAC", "ogg": "OGG"}.get(fmt, "WAV")
    buf = io.BytesIO()
    sf.write(buf, samples, SAMPLE_RATE, format=sf_format)
    return Response(content=buf.getvalue(), media_type=f"audio/{fmt}")
