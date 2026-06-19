# =============================================================================
# Plik: server.py
# Opis: Serwer TTS sherpa-onnx (modele VITS Piper) wystawiajacy OpenAI-zgodny
#       endpoint /audio/speech (Core gada HTTP wprost do host-mapped portu,
#       bez sidecara). Model VITS pobierany z repo HF wskazanego env MODEL.
# Przykład: MODEL=csukuangfj/vits-piper-en_US-amy-medium PORT=8084 python server.py
# =============================================================================

import glob
import io
import logging
import os

import numpy as np
import sherpa_onnx
import soundfile as sf
import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from huggingface_hub import snapshot_download
from pydantic import BaseModel
from typing import Optional

log = logging.getLogger("sherpa-tts")
logging.basicConfig(level=logging.INFO)

MODEL_REPO = os.environ.get("MODEL") or os.environ.get("MODEL_REPO")
if not MODEL_REPO:
    raise RuntimeError("Brak env MODEL/MODEL_REPO — nie wiadomo ktore repo VITS pobrac.")


def _build_tts(repo: str) -> sherpa_onnx.OfflineTts:
    """Pobiera repo VITS Piper z HF i buduje silnik sherpa-onnx OfflineTts.

    Piper trzyma `*.onnx` + `tokens.txt` + katalog `espeak-ng-data` (fonemizacja).
    Niektore modele (np. chinskie) zamiast espeak uzywaja `lexicon.txt` + `dict_dir`.
    """
    d = snapshot_download(repo)
    onnx = sorted(glob.glob(os.path.join(d, "*.onnx")))
    if not onnx:
        raise RuntimeError(f"Brak pliku *.onnx w repo {repo}")
    tokens = os.path.join(d, "tokens.txt")
    espeak = os.path.join(d, "espeak-ng-data")
    lexicon = os.path.join(d, "lexicon.txt")
    dict_dir_candidates = glob.glob(os.path.join(d, "*dict*"))
    vits = sherpa_onnx.OfflineTtsVitsModelConfig(
        model=onnx[0],
        tokens=tokens,
        data_dir=espeak if os.path.isdir(espeak) else "",
        lexicon=lexicon if os.path.exists(lexicon) else "",
        dict_dir=dict_dir_candidates[0] if dict_dir_candidates else "",
    )
    cfg = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(vits=vits, num_threads=2, provider="cpu"),
        max_num_sentences=1,
    )
    return sherpa_onnx.OfflineTts(cfg)


log.info("loading sherpa-onnx VITS from %s", MODEL_REPO)
TTS = _build_tts(MODEL_REPO)
NUM_SPEAKERS = TTS.num_speakers
SAMPLE_RATE = TTS.sample_rate
log.info("loaded: speakers=%d sample_rate=%d", NUM_SPEAKERS, SAMPLE_RATE)


class SpeechRequest(BaseModel):
    model: Optional[str] = "tts-1"
    input: str
    voice: Optional[str] = None
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0
    language: Optional[str] = "en-us"


app = FastAPI(title="sherpa-onnx TTS", version="1.0.0")


@app.get("/healthz")
@app.get("/health")
def healthz():
    return {"status": "ok", "speakers": NUM_SPEAKERS, "sample_rate": SAMPLE_RATE}


def _speaker_id(voice: Optional[str]) -> int:
    """VITS Piper jest jedno-mowcowy (sid=0); modele multi-speaker przyjmuja
    numeryczny `voice` jako sid. Nienumeryczna nazwa -> 0."""
    if voice is None:
        return 0
    try:
        sid = int(voice)
    except (TypeError, ValueError):
        return 0
    return sid if 0 <= sid < NUM_SPEAKERS else 0


def _synthesize(req: SpeechRequest) -> Response:
    sid = _speaker_id(req.voice)
    audio = TTS.generate(req.input, sid=sid, speed=req.speed or 1.0)
    samples = np.asarray(audio.samples, dtype=np.float32)
    fmt = (req.response_format or "wav").lower()
    if fmt not in ("wav", "flac", "ogg"):
        fmt = "wav"
    sf_format = {"wav": "WAV", "flac": "FLAC", "ogg": "OGG"}[fmt]
    buf = io.BytesIO()
    sf.write(buf, samples, audio.sample_rate, format=sf_format)
    return Response(content=buf.getvalue(), media_type=f"audio/{fmt}")


@app.post("/audio/speech")
@app.post("/v1/audio/speech")
def speech(req: SpeechRequest) -> Response:
    if not req.input.strip():
        raise HTTPException(400, "pole 'input' jest puste")
    try:
        return _synthesize(req)
    except Exception as e:
        log.exception("sherpa generate failed")
        raise HTTPException(500, str(e)) from e


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "8084"))
    uvicorn.run(app, host="0.0.0.0", port=port)
