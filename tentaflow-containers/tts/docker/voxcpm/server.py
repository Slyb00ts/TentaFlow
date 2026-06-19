# =============================================================================
# Plik: server.py
# Opis: FastAPI wrapper na VoxCPM — OpenAI-zgodny endpoint /v1/audio/speech
#       (Core POST {base}/v1/audio/speech {input,...}). Uzywany w native venv
#       i w kontenerze Docker (ten sam plik).
# =============================================================================

import io
import os
import threading

import soundfile as sf
import torch
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel
from typing import Optional

MODEL = os.environ.get("MODEL") or os.environ.get("VOXCPM_MODEL", "openbmb/VoxCPM-0.5B")
DEVICE = "cuda" if torch.cuda.is_available() else "cpu"

app = FastAPI(title="voxcpm TTS")
_tts = None
_sr = 16000
_load_lock = threading.Lock()


def get_tts():
    """Leniwie laduje VoxCPM. device obslugiwany przez from_pretrained (to NIE
    jest nn.Module — bez .to()/.eval()). optimize=False: domyslny torch.compile
    zawiesza pierwszy generate na minuty. load_denoiser=False: TTS bez czyszczenia
    promptu nie potrzebuje zipenhancera (mniej do pobrania)."""
    global _tts, _sr
    if _tts is None:
        with _load_lock:
            if _tts is None:
                from voxcpm import VoxCPM
                print(f"[voxcpm] laduje {MODEL} na {DEVICE}", flush=True)
                m = VoxCPM.from_pretrained(
                    MODEL, device=DEVICE, load_denoiser=False, optimize=False
                )
                _sr = int(getattr(m.tts_model, "sample_rate", 16000))
                _tts = m
                print(f"[voxcpm] gotowy (sample_rate={_sr})", flush=True)
    return _tts


class SpeechRequest(BaseModel):
    model: Optional[str] = "voxcpm-base"
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
    return {"status": "ok", "model": MODEL, "device": DEVICE, "loaded": _tts is not None}


@app.post("/v1/audio/speech")
@app.post("/audio/speech")
def speech(req: SpeechRequest) -> Response:
    if not req.input.strip():
        raise HTTPException(400, "pole 'input' jest puste")
    tts_obj = get_tts()
    try:
        with torch.inference_mode():
            wav = tts_obj.generate(text=req.input)
        if hasattr(wav, "cpu"):
            wav = wav.cpu().numpy()
        buf = io.BytesIO()
        sf.write(buf, wav, _sr, format="WAV", subtype="PCM_16")
    except Exception as e:
        raise HTTPException(500, str(e)) from e
    return Response(content=buf.getvalue(), media_type="audio/wav")
