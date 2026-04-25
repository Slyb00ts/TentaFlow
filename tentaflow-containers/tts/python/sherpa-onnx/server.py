"""FastAPI wrapper sherpa-onnx VITS TTS.

Eksponuje minimalny endpoint /tts (POST text -> audio/wav) plus /v1/models
zeby health probe `bundle_smoke` przeszedl. Model VITS Piper laduje sie
z HuggingFace przy starcie (auto-download, cache w HF_HOME).

Domyslny model: rhasspy/piper-voices/en/en_US/lessac/medium (en, ~30MB).
Override przez env MODEL_REPO + MODEL_FILE.
"""

import io
import os
import sys
import tempfile
from pathlib import Path

import sherpa_onnx
import soundfile as sf
from fastapi import FastAPI
from fastapi.responses import Response
from huggingface_hub import hf_hub_download
from pydantic import BaseModel

MODEL_REPO = os.environ.get("MODEL_REPO", "csukuangfj/vits-piper-en_US-lessac-medium")
MODEL_FILE = os.environ.get("MODEL_FILE", "en_US-lessac-medium.onnx")
TOKENS_FILE = os.environ.get("TOKENS_FILE", "tokens.txt")
DATA_DIR = os.environ.get("DATA_DIR", "espeak-ng-data")

app = FastAPI()
_tts: sherpa_onnx.OfflineTts | None = None
_sample_rate: int = 22050


def _ensure_tts() -> sherpa_onnx.OfflineTts:
    """Lazy-load TTS — przy pierwszym /tts requeście pobierze model z HF
    (jesli nie w cache HF_HOME), zbuduje OfflineTts i zachowa singleton."""
    global _tts, _sample_rate
    if _tts is not None:
        return _tts

    print(f"[sherpa-onnx] Pobieranie modelu {MODEL_REPO}/{MODEL_FILE}", flush=True)
    model_path = hf_hub_download(repo_id=MODEL_REPO, filename=MODEL_FILE)
    tokens_path = hf_hub_download(repo_id=MODEL_REPO, filename=TOKENS_FILE)
    # espeak-ng-data jest wymagane dla VITS; pobieramy z tego samego repo
    # (rhasspy/csukuangfj packuja espeak-ng-data wraz z modelem).
    espeak_dir = ""
    try:
        espeak_dir = str(Path(hf_hub_download(repo_id=MODEL_REPO, filename=f"{DATA_DIR}/phontab")).parent)
    except Exception as e:
        print(f"[sherpa-onnx] Brak espeak-ng-data w repo: {e} — kontynuuje bez", flush=True)

    cfg = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(
            vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                model=model_path,
                tokens=tokens_path,
                data_dir=espeak_dir,
            ),
            num_threads=int(os.environ.get("NUM_THREADS", "2")),
            provider=os.environ.get("PROVIDER", "cpu"),
        ),
    )
    _tts = sherpa_onnx.OfflineTts(cfg)
    _sample_rate = _tts.sample_rate
    print(f"[sherpa-onnx] Model gotowy, sample_rate={_sample_rate}", flush=True)
    return _tts


@app.get("/v1/models")
def list_models() -> dict:
    return {"object": "list", "data": [{"id": MODEL_REPO, "object": "model"}]}


@app.get("/health")
def health() -> dict:
    return {"status": "ok"}


class TtsRequest(BaseModel):
    text: str
    sid: int = 0
    speed: float = 1.0


@app.post("/tts")
def tts(req: TtsRequest) -> Response:
    tts_engine = _ensure_tts()
    audio = tts_engine.generate(req.text, sid=req.sid, speed=req.speed)
    # audio.samples to lista float32 [-1, 1], audio.sample_rate to int.
    buf = io.BytesIO()
    sf.write(buf, audio.samples, audio.sample_rate, format="WAV", subtype="PCM_16")
    buf.seek(0)
    return Response(content=buf.read(), media_type="audio/wav")


if __name__ == "__main__":
    print("Uruchom przez `uvicorn server:app`", file=sys.stderr)
    sys.exit(2)
