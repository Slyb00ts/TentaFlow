# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Nemotron-OCR udostepniajacy kontrakt HTTP zgodny z
#       NVIDIA NIM dla OCR. Uzywa oficjalnego pipeline'u
#       `nemotron_ocr.inference.pipeline.NemotronOCR` (detector + recognizer +
#       relational). Wystawia POST /v1/infer (NIM OCR) oraz GET /health.
# Przyklad: curl -X POST http://127.0.0.1:8093/v1/infer \
#           -d '{"input":[{"type":"image_url","url":"data:image/png;base64,..."}]}'
# =============================================================================

import base64
import io
import os
import re
import threading
from typing import Any

import numpy as np
import torch
from fastapi import FastAPI, HTTPException
from PIL import Image
from pydantic import BaseModel

from nemotron_ocr.inference.pipeline import NemotronOCR

# Pipeline akceptuje granulacje "word"/"sentence"/"paragraph"; NIM domyslnie laczy
# w akapity. Pole merge_levels w zadaniu moze to nadpisac per obraz.
DEFAULT_MERGE_LEVEL = os.environ.get("OCR_MERGE_LEVEL", "paragraph")
VALID_MERGE_LEVELS = {"word", "sentence", "paragraph"}

# Prefiks data-URL: "data:image/png;base64,<...>".
_DATA_URL_RE = re.compile(r"^data:[^;,]*(;base64)?,(?P<payload>.*)$", re.DOTALL)

app = FastAPI(title="Nemotron-OCR")

# Pipeline ladowany leniwie w watku tla — wagi .pth (detector, recognizer,
# relational) sciagaja sie z HF przy inicjalizacji, wiec /health odpowiada zanim
# model bedzie gotowy, a /v1/infer zwraca 503 do czasu gotowosci.
_state: dict = {"pipeline": None}


class InputImage(BaseModel):
    type: str = "image_url"
    url: str


class InferRequest(BaseModel):
    input: list[InputImage]
    # Jeden wpis na obraz albo pojedyncza wartosc stosowana do wszystkich obrazow.
    merge_levels: list[str] | None = None


def _require_cuda() -> None:
    if not torch.cuda.is_available():
        raise HTTPException(
            status_code=503,
            detail="Nemotron-OCR wymaga CUDA — brak dostepnego GPU.",
        )


_LOAD_LOCK = threading.Lock()


def _ensure_pipeline() -> None:
    if _state["pipeline"] is not None:
        return
    with _LOAD_LOCK:
        if _state["pipeline"] is not None:
            return
        _require_cuda()
        # model_dir=None => pipeline sam pobiera checkpointy z HF Hub i cachuje.
        _state["pipeline"] = NemotronOCR(model_dir=None)


# Ladowanie w WATKU TLA przy starcie — synchroniczne ladowanie w handlerze
# blokowaloby event-loop, /health przestawal odpowiadac i supervisor Core
# restartowal proces w petli. W tle /health odpowiada od razu, /v1/infer zwraca
# 503 do czasu gotowosci.
@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_ensure_pipeline, name="model-load", daemon=True).start()


def _decode_data_url(url: str) -> bytes:
    """Wyciaga surowe bajty obrazu z data-URL (base64) lub czystego base64."""
    dopasowanie = _DATA_URL_RE.match(url.strip())
    payload = dopasowanie.group("payload") if dopasowanie else url.strip()
    try:
        return base64.b64decode(payload)
    except Exception as exc:  # noqa: BLE001 — zwracamy czytelny blad klientowi
        raise HTTPException(status_code=400, detail=f"Bledny base64 w url: {exc}")


def _decode_image(raw: bytes) -> Image.Image:
    try:
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidlowy obraz: {exc}")


def _normalize_merge_level(poziom: str) -> str:
    if poziom not in VALID_MERGE_LEVELS:
        raise HTTPException(
            status_code=400,
            detail=f"Nieobslugiwany merge_level '{poziom}'. Dozwolone: {sorted(VALID_MERGE_LEVELS)}.",
        )
    return poziom


def _resolve_merge_levels(req: InferRequest) -> list[str]:
    """Mapuje merge_levels na liste rownej liczbie obrazow.

    Brak pola => domyslny poziom dla wszystkich. Pojedyncza wartosc => stosowana
    do wszystkich obrazow. W przeciwnym razie liczba poziomow musi rownac sie
    liczbie obrazow."""
    liczba_obrazow = len(req.input)
    if not req.merge_levels:
        return [DEFAULT_MERGE_LEVEL] * liczba_obrazow
    poziomy = [_normalize_merge_level(p) for p in req.merge_levels]
    if len(poziomy) == 1:
        return poziomy * liczba_obrazow
    if len(poziomy) != liczba_obrazow:
        raise HTTPException(
            status_code=400,
            detail="merge_levels musi miec 1 wpis lub tyle wpisow ile obrazow w input.",
        )
    return poziomy


def _polygon_z_regionu(region: dict) -> list[dict[str, float]]:
    """Buduje 4-punktowy wielokat (znormalizowany 0..1) z osiowo-rownoleglej
    ramki regionu. Pipeline zwraca pola left/right oraz upper/lower, gdzie
    'upper' to wieksza wspolrzedna Y, a 'lower' mniejsza (patrz pipeline.py).
    Normalizacja po wymiarach obrazu jest juz zrobiona w pipeline."""
    left = float(region["left"])
    right = float(region["right"])
    upper = float(region["upper"])
    lower = float(region["lower"])
    y_min = min(upper, lower)
    y_max = max(upper, lower)
    x_min = min(left, right)
    x_max = max(left, right)
    # Kolejnosc: lewy-gorny, prawy-gorny, prawy-dolny, lewy-dolny.
    return [
        {"x": x_min, "y": y_min},
        {"x": x_max, "y": y_min},
        {"x": x_max, "y": y_max},
        {"x": x_min, "y": y_max},
    ]


def _text_detections(regiony: list[dict]) -> list[dict[str, Any]]:
    detekcje = []
    for region in regiony:
        if not isinstance(region, dict):
            continue
        if isinstance(region.get("left"), str):  # marker "nan" z pipeline
            continue
        tekst = str(region.get("text", ""))
        try:
            pewnosc = float(region.get("confidence", 0.0))
        except (TypeError, ValueError):
            pewnosc = 0.0
        detekcje.append(
            {
                "text_prediction": {"text": tekst, "confidence": pewnosc},
                "bounding_box": {"points": _polygon_z_regionu(region)},
            }
        )
    return detekcje


def _run_ocr(image: Image.Image, merge_level: str) -> list[dict]:
    if _state["pipeline"] is None:
        raise HTTPException(status_code=503, detail="Model jeszcze sie laduje, sprobuj za chwile.")
    pipeline = _state["pipeline"]
    # Pipeline nie przyjmuje PIL.Image — wspiera NumPy (H, W, C), wiec konwertujemy.
    predictions = pipeline(np.asarray(image), merge_level=merge_level, visualize=False)
    return _text_detections(predictions)


@app.get("/health")
def health() -> dict:
    return {"status": "ok", "model": "nvidia/nemotron-ocr-v1", "loaded": _state["pipeline"] is not None}


@app.post("/v1/infer")
async def infer(req: InferRequest) -> dict:
    if not req.input:
        raise HTTPException(status_code=400, detail="Pole 'input' nie moze byc puste.")
    poziomy = _resolve_merge_levels(req)

    data = []
    suma_bajtow = 0
    for indeks, (wejscie, poziom) in enumerate(zip(req.input, poziomy)):
        raw = _decode_data_url(wejscie.url)
        suma_bajtow += len(raw)
        obraz = _decode_image(raw)
        detekcje = _run_ocr(obraz, poziom)
        data.append({"index": indeks, "text_detections": detekcje})

    return {
        "data": data,
        "usage": {"images_size_mb": round(suma_bajtow / (1024 * 1024), 6)},
    }
