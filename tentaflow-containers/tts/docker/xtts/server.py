"""FastAPI wrapper XTTS v2 — kopiowany do venv przy bootstrapie bundla i uzywany
   tez w kontenerze Docker (ten sam kod). Wystawia OpenAI-zgodny endpoint
   /v1/audio/speech (Core gada przez backend client POST {base}/v1/audio/speech
   z JSON {input, voice, language, speed}). `voice` mapuje sie na wbudowanego
   mowce XTTS v2 (model jest wielomowcowy); pusty/nieznany -> domyslny mowca."""

import os
import tempfile

# XTTS v2 jest na licencji CPML — coqui-tts pyta interaktywnie o zgode przez
# input() przy ladowaniu modelu, co w procesie bez TTY rzuca EOFError. Env
# COQUI_TOS_AGREED=1 akceptuje licencje bez promptu. MUSI byc przed importem TTS.
os.environ.setdefault("COQUI_TOS_AGREED", "1")

import torch
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel
from typing import Optional
from TTS.api import TTS

MODEL = os.environ.get("XTTS_MODEL", "tts_models/multilingual/multi-dataset/xtts_v2")
DEVICE = "cuda" if torch.cuda.is_available() else ("mps" if torch.backends.mps.is_available() else "cpu")

app = FastAPI(title="xtts-v2 TTS")
_tts = None


def get_tts():
    global _tts
    if _tts is None:
        _tts = TTS(MODEL).to(DEVICE)
    return _tts


def _speakers(tts_obj) -> list:
    """Lista wbudowanych mowcow XTTS v2 (multi-dataset). Pusta dla modeli
    jedno-mowcowych."""
    try:
        mgr = tts_obj.synthesizer.tts_model.speaker_manager
        return list(mgr.speakers) if mgr and mgr.speakers else []
    except (AttributeError, TypeError):
        return []


class SpeechRequest(BaseModel):
    model: Optional[str] = "xtts-v2"
    input: str
    voice: Optional[str] = None
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0
    language: Optional[str] = "en"


@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL, "object": "model"}]}


@app.get("/healthz")
@app.get("/health")
def healthz():
    return {"status": "ok", "model": MODEL, "device": DEVICE}


@app.post("/v1/audio/speech")
@app.post("/audio/speech")
def speech(req: SpeechRequest) -> Response:
    if not req.input.strip():
        raise HTTPException(400, "pole 'input' jest puste")
    tts_obj = get_tts()
    speakers = _speakers(tts_obj)
    speaker = req.voice if (req.voice and req.voice in speakers) else (speakers[0] if speakers else None)
    lang = req.language or "en"
    try:
        kwargs = {"text": req.input, "language": lang, "speed": req.speed or 1.0}
        if speaker is not None:
            kwargs["speaker"] = speaker
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as out:
            tts_obj.tts_to_file(file_path=out.name, **kwargs)
            with open(out.name, "rb") as f:
                wav_bytes = f.read()
        os.unlink(out.name)
    except Exception as e:
        raise HTTPException(500, str(e)) from e
    return Response(content=wav_bytes, media_type="audio/wav")
