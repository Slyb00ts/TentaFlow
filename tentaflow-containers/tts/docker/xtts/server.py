# =============================================================================
# Plik: server.py
# Opis: FastAPI wrapper na XTTS v2 (coqui-ai/TTS) — voice cloning.
#       Eksponuje POST /tts {text, speaker_wav, language} → audio/wav
# =============================================================================

import io
import os
import tempfile

import torch
from fastapi import FastAPI, Form, UploadFile, File
from fastapi.responses import Response
from TTS.api import TTS


MODEL = os.environ.get("XTTS_MODEL", "tts_models/multilingual/multi-dataset/xtts_v2")
DEVICE = "cuda" if torch.cuda.is_available() else "cpu"

app = FastAPI()
_tts = None


def get_tts():
    global _tts
    if _tts is None:
        print(f"[xtts] laduje {MODEL} na {DEVICE}", flush=True)
        _tts = TTS(MODEL).to(DEVICE)
        print("[xtts] gotowy", flush=True)
    return _tts


@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL, "object": "model"}]}


@app.post("/tts")
async def tts(
    text: str = Form(...),
    language: str = Form("pl"),
    speaker_wav: UploadFile = File(None),
):
    tts_obj = get_tts()
    speaker_path = None
    if speaker_wav is not None:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tf:
            tf.write(await speaker_wav.read())
            speaker_path = tf.name

    try:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as out:
            tts_obj.tts_to_file(
                text=text,
                file_path=out.name,
                speaker_wav=speaker_path,
                language=language,
            )
            with open(out.name, "rb") as f:
                wav_bytes = f.read()
        return Response(content=wav_bytes, media_type="audio/wav")
    finally:
        for p in (speaker_path,):
            if p:
                try:
                    os.unlink(p)
                except OSError:
                    pass
