"""FastAPI wrapper XTTS v2 — kopiowany do venv przy bootstrapie bundla.
   Identyczny jak w tentaflow-containers/tts-xtts/server.py (ten sam kod
   dziala zarowno w kontenerze jak i w natywnym venv)."""

# Reuse implementacji z kontenera Docker — zeby nie mnozyc kodu.
# `deploy::python_venv` kopiuje plik z tego katalogu do venv app-dir.
import io, os, tempfile, torch
from fastapi import FastAPI, Form, UploadFile, File
from fastapi.responses import Response
from TTS.api import TTS

MODEL = os.environ.get("XTTS_MODEL", "tts_models/multilingual/multi-dataset/xtts_v2")
DEVICE = "cuda" if torch.cuda.is_available() else ("mps" if torch.backends.mps.is_available() else "cpu")

app = FastAPI()
_tts = None

def get_tts():
    global _tts
    if _tts is None:
        _tts = TTS(MODEL).to(DEVICE)
    return _tts

@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL, "object": "model"}]}

@app.post("/tts")
async def tts(text: str = Form(...), language: str = Form("pl"),
              speaker_wav: UploadFile = File(None)):
    tts_obj = get_tts()
    speaker_path = None
    if speaker_wav is not None:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tf:
            tf.write(await speaker_wav.read())
            speaker_path = tf.name
    try:
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as out:
            tts_obj.tts_to_file(text=text, file_path=out.name,
                                speaker_wav=speaker_path, language=language)
            with open(out.name, "rb") as f:
                wav_bytes = f.read()
        return Response(content=wav_bytes, media_type="audio/wav")
    finally:
        if speaker_path:
            try: os.unlink(speaker_path)
            except OSError: pass
