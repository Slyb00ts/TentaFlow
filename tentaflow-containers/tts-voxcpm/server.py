# =============================================================================
# Plik: server.py
# Opis: FastAPI wrapper na VoxCPM2 — POST /tts {text} → audio/wav
#       Stub: VoxCPM API moze sie zmieniac, ten plik trzeba zaktualizowac
#       po wybraniu konkretnego entrypointa biblioteki.
# =============================================================================

import io
import os
import tempfile

import torch
import soundfile as sf
from fastapi import FastAPI, Form
from fastapi.responses import Response


MODEL = os.environ.get("VOXCPM_MODEL", "openbmb/VoxCPM-0.5B")
DEVICE = "cuda" if torch.cuda.is_available() else "cpu"

app = FastAPI()
_tts = None


def get_tts():
    global _tts
    if _tts is None:
        # Importuj lazy zeby brak biblioteki nie blokowal startu serwera
        from voxcpm import VoxCPM  # type: ignore
        print(f"[voxcpm] laduje {MODEL} na {DEVICE}", flush=True)
        _tts = VoxCPM.from_pretrained(MODEL).to(DEVICE)
        _tts.eval()
        print("[voxcpm] gotowy", flush=True)
    return _tts


@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL, "object": "model"}]}


@app.post("/tts")
async def tts(text: str = Form(...)):
    tts_obj = get_tts()
    with torch.inference_mode():
        wav = tts_obj.generate(text)
    if hasattr(wav, "cpu"):
        wav = wav.cpu().numpy()
    buf = io.BytesIO()
    sf.write(buf, wav, 24000, format="WAV", subtype="PCM_16")
    return Response(content=buf.getvalue(), media_type="audio/wav")
