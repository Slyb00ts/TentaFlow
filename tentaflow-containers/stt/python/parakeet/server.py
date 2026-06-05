# =============================================================================
# Plik: server.py
# Opis: Maly FastAPI wrapper na NVIDIA NeMo Parakeet — eksponuje
#       OpenAI-compatible /v1/audio/transcriptions (multipart file upload).
#       Sidecar widzi to jako zwykly endpoint OpenAI i tlumaczy QUIC.
# =============================================================================

import io
import os
import tempfile
from contextlib import asynccontextmanager
from typing import Optional

import soundfile as sf
import torch
from fastapi import FastAPI, UploadFile, Form, File
from fastapi.responses import JSONResponse
import nemo.collections.asr as nemo_asr


# `MODEL` to zmienna, ktora ustawia deploy (resolve_model_repo z wybranego
# presetu). `NEMO_MODEL` zostaje jako jawny override, a literal to ostatni
# fallback. Wczesniej server czytal tylko `NEMO_MODEL`, przez co ignorowal
# preset z panelu i zawsze ladowal zaszyty domyslny model.
MODEL_NAME = (
    os.environ.get("MODEL")
    or os.environ.get("NEMO_MODEL")
    or "nvidia/parakeet-tdt-0.6b-v3"
)

_asr_model: Optional["nemo_asr.models.ASRModel"] = None


@asynccontextmanager
async def lifespan(_app: FastAPI):
    # Eager-load: readiness (`/v1/models`) odpowiada dopiero gdy model jest
    # realnie zaladowany na urzadzenie. Dzieki temu brak pamieci GPU ubija
    # proces PODCZAS startu (deploy widzi ProcessExited i ustawia czerwony
    # 'failed'), a nie dopiero przy pierwszej transkrypcji w niewidzialnej
    # petli respawnow z zielonym statusem "dziala".
    get_model()
    yield


app = FastAPI(lifespan=lifespan)


def get_model():
    global _asr_model
    if _asr_model is None:
        print(f"[parakeet] laduje model {MODEL_NAME}", flush=True)
        model = nemo_asr.models.ASRModel.from_pretrained(MODEL_NAME)
        model.eval()
        # Jawny wybor urzadzenia, bez polykania wyjatkow: CUDA OOM ma poleciec
        # wyzej i ubic start (-> widoczny blad deployu), zamiast zostawic model
        # w polowicznym stanie i crashowac twardo dopiero w transcribe.
        device = "cuda" if torch.cuda.is_available() else "cpu"
        model.to(device)
        print(f"[parakeet] model gotowy na {device}", flush=True)
        _asr_model = model
    return _asr_model


@app.get("/v1/models")
def list_models():
    return {"object": "list", "data": [{"id": MODEL_NAME, "object": "model"}]}


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str = Form("parakeet"),
    language: Optional[str] = Form(None),
    response_format: Optional[str] = Form("json"),
    data: Optional[str] = Form(None),
):
    # Typed request-time overrides z BackendClient (multipart `data=<json>`).
    # Parakeet/NeMo nie wystawia per-request knobow (beam_size/itd. sa baked
    # w model checkpoint), wiec `overrides` na razie tylko logujemy dla
    # observability — jak NeMo upstream doda parametry, podlinkujemy.
    if data:
        try:
            import json as _json
            _ = _json.loads(data)  # sanity check
        except Exception:
            pass

    audio_bytes = await file.read()
    # zapisz do tmp WAV zeby NeMo zjadl dowolny format
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tf:
        try:
            audio, sr = sf.read(io.BytesIO(audio_bytes))
            sf.write(tf.name, audio, sr, subtype="PCM_16")
        except Exception:
            tf.write(audio_bytes)
        path = tf.name

    try:
        hyps = get_model().transcribe([path])
        text = hyps[0] if isinstance(hyps, list) and hyps else ""
        if hasattr(text, "text"):
            text = text.text
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass

    if response_format == "text":
        return text
    return JSONResponse({"text": text})
