"""
Kyutai TTS FastAPI server. OpenAI-compatible /v1/audio/speech.
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

from moshi.models.loaders import CheckpointInfo
from moshi.models.tts import DEFAULT_DSM_TTS_REPO, TTSModel

log = logging.getLogger("kyutai-server")
logging.basicConfig(level=logging.INFO)

DEVICE = os.environ.get("KYUTAI_DEVICE", "cpu")
HF_REPO = os.environ.get("KYUTAI_REPO", DEFAULT_DSM_TTS_REPO)
DEFAULT_VOICE = os.environ.get(
    "KYUTAI_DEFAULT_VOICE",
    "expresso/ex03-ex01_happy_001_channel1_334s.wav",
)

log.info("loading kyutai TTS from %s on %s", HF_REPO, DEVICE)
checkpoint_info = CheckpointInfo.from_hf_repo(HF_REPO)
tts_model = TTSModel.from_checkpoint_info(
    checkpoint_info, n_q=32, temp=0.6, device=DEVICE
)
log.info("model ready, sample_rate=%s", tts_model.mimi.sample_rate)


class SpeechRequest(BaseModel):
    model: Optional[str] = "kyutai-tts"
    input: str
    voice: Optional[str] = None
    response_format: Optional[str] = "wav"
    speed: Optional[float] = 1.0


app = FastAPI(title="Kyutai TTS", version="1.0.0")


@app.get("/healthz")
def healthz():
    return {"status": "ok", "sample_rate": tts_model.mimi.sample_rate}


@app.get("/v1/audio/voices")
def list_voices():
    """Wymienia dostepne voices ze sciezek pre-cached w HF cache."""
    # tts_model.get_voice_path nie ma listy, wiec scanujemy snapshot dir:
    from huggingface_hub import snapshot_download
    voice_repo = "kyutai/tts-voices"
    base = snapshot_download(voice_repo, allow_patterns=["*.safetensors"])
    voices = []
    for root, _, files in os.walk(base):
        for f in files:
            if f.endswith(".safetensors"):
                rel = os.path.relpath(os.path.join(root, f), base)
                voices.append(rel)
    return {"voices": sorted(voices)[:60]}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    voice = req.voice or DEFAULT_VOICE
    try:
        # Rozpakuj voice path. Jezeli zaczynamy od `.safetensors` traktujemy
        # bezposrednio, w innym razie przez get_voice_path() (HF lookup).
        if voice.endswith(".safetensors") and os.path.exists(voice):
            voice_path = voice
        else:
            voice_path = tts_model.get_voice_path(voice)

        entries = tts_model.prepare_script([req.input], padding_between=1)
        cond = tts_model.make_condition_attributes([voice_path], cfg_coef=2.0)

        # Generujemy w trybie blocking — zwracamy gotowy WAV. Streaming ws/SSE
        # wymagalby trzymania state per polaczenie; tutaj prosty single-shot.
        with torch.no_grad():
            output = tts_model.generate(
                [entries], [cond],
            )
        # `output` jest typu `TTSResult` zaleznie od wersji moshi; bierzemy
        # frames -> Mimi decode -> waveform.
        audio = tts_model.mimi.decode(output.frames[:, 1:].to(DEVICE))
        samples = audio[0, 0].cpu().numpy().astype(np.float32)
        sr = int(tts_model.mimi.sample_rate)
    except Exception as e:
        log.exception("kyutai TTS failed")
        raise HTTPException(500, str(e)) from e

    fmt = (req.response_format or "wav").lower()
    if fmt not in ("wav", "flac", "ogg"):
        fmt = "wav"
    sf_format = {"wav": "WAV", "flac": "FLAC", "ogg": "OGG"}.get(fmt, "WAV")
    buf = io.BytesIO()
    sf.write(buf, samples, sr, format=sf_format)
    return Response(content=buf.getvalue(), media_type=f"audio/{fmt}")
