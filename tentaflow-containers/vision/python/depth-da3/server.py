# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla Depth Anything 3 (DA3). DA3 NIE działa przez
#       transformers — używa własnego pakietu `depth_anything_3` i API
#       `DepthAnything3.from_pretrained(MODEL).inference([sciezka_obrazu])`. Model
#       wybiera env MODEL (repo HF z presetu). `inference` przyjmuje listę ŚCIEŻEK
#       do plików, więc dekodujemy data-URL do pliku tymczasowego. Wystawia
#       POST /v1/depth (mapa głębi f32 base64 + width/height + is_metric) i
#       GET /health. Modele DA3NESTED-* zwracają metry (is_metric=true), DA3-*
#       głębię względną.
# Przyklad: curl -X POST http://127.0.0.1:8097/v1/depth \
#           -d '{"model":"da3-large","input":[{"url":"data:image/png;base64,..."}]}'
# =============================================================================

import base64
import os
import re
import sys
import tempfile
import threading

import numpy as np
import torch
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

from depth_anything_3.api import DepthAnything3

# Repo HF wybierane przez deploy (preset → env MODEL). Domyślnie DA3 Large (relatywny).
MODEL = os.environ.get("MODEL", "depth-anything/DA3-LARGE")
# DA3NESTED-* zwraca głębię wprost w metrach; pozostałe DA3-* są względne.
# (DA3METRIC-* wymagałby konwersji z ogniskową — nieobsługiwany tutaj.)
IS_METRIC = "nested" in MODEL.lower()

_DATA_URL_RE = re.compile(r"^data:[^;,]*(;base64)?,(?P<payload>.*)$", re.DOTALL)

app = FastAPI(title="Depth Anything 3 (DA3)")

# Model ładowany leniwie w wątku tła (wagi z HF + budowa modelu): /health odpowiada
# od razu, /v1/depth zwraca 503 do czasu gotowości.
_state: dict = {"model": None, "device": None, "error": None}
_LOAD_LOCK = threading.Lock()


class InputImage(BaseModel):
    type: str = "image_url"
    url: str


class DepthRequest(BaseModel):
    # `model` opcjonalne — serwis już uruchomiony z konkretnym MODEL (jak w /v1/*).
    model: str | None = None
    input: list[InputImage]


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    with _LOAD_LOCK:
        if _state["model"] is not None:
            return
        # DA3 is GPU-only (the bundle ships only a CUDA variant; CPU load would be
        # unusably slow). Reject a CPU/no-GPU host so the deploy fails fast instead
        # of hanging on an enormous CPU load.
        if not torch.cuda.is_available():
            raise RuntimeError(
                "DA3 requires a CUDA GPU but none is visible (this engine is GPU-only)"
            )
        device = torch.device("cuda")
        try:
            model = DepthAnything3.from_pretrained(MODEL).to(device=device)
            model.eval()
            _state["model"] = model
            _state["device"] = device
        except Exception as exc:  # noqa: BLE001 — zapamiętaj błąd, zgłoś w /v1/depth
            _state["error"] = str(exc)
            raise


@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_safe_load, name="model-load", daemon=True).start()


def _safe_load() -> None:
    try:
        _ensure_model()
    except Exception as exc:  # noqa: BLE001
        # Hard-fail the process so the deploy probe (which waits on process exit,
        # max_wait=None) rolls back, instead of a permanent 503 hang on /health.
        print(f"FATAL: DA3 model load failed: {exc}", file=sys.stderr, flush=True)
        os._exit(1)


def _decode_to_tempfile(url: str) -> str:
    m = _DATA_URL_RE.match(url.strip())
    payload = m.group("payload") if m else url.strip()
    try:
        raw = base64.b64decode(payload)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidłowy base64: {exc}")
    fd, path = tempfile.mkstemp(suffix=".png")
    with os.fdopen(fd, "wb") as f:
        f.write(raw)
    return path


@app.get("/health")
def health() -> dict:
    # READINESS, not liveness: TentaFlow's deploy probe treats any 2xx as "ready",
    # so we must return non-2xx until the model is actually loaded (otherwise the
    # service is marked Running while /v1/depth still 503s) — or on a load failure.
    if _state["model"] is None:
        detail = _state["error"] or "loading"
        raise HTTPException(status_code=503, detail=f"not ready: {detail}")
    return {"status": "ok", "model": MODEL, "ready": True}


@app.post("/v1/depth")
def depth(req: DepthRequest) -> dict:
    if _state["model"] is None:
        if _state["error"]:
            raise HTTPException(status_code=503, detail=f"Model load failed: {_state['error']}")
        raise HTTPException(status_code=503, detail="Model jeszcze się ładuje — spróbuj ponownie.")
    if not req.input:
        raise HTTPException(status_code=400, detail="Brak obrazów w 'input'.")
    model = _state["model"]
    data = []
    for idx, item in enumerate(req.input):
        path = _decode_to_tempfile(item.url)
        try:
            # Każdy obraz osobno (lista 1-elementowa), żeby zachować semantykę
            # monocular per-obraz — DA3 łączyłby wiele zdjęć jako multi-view.
            with torch.no_grad():
                prediction = model.inference([path])
        except Exception as exc:  # noqa: BLE001
            raise HTTPException(status_code=500, detail=f"DA3 inference failed: {exc}")
        finally:
            try:
                os.remove(path)
            except OSError:
                pass
        depth_maps = prediction.depth  # [N, H, W] float32
        arr = np.asarray(depth_maps[0], dtype=np.float32)
        arr = np.ascontiguousarray(arr)
        h, w = arr.shape[0], arr.shape[1]
        data.append({
            "index": idx,
            "width": int(w),
            "height": int(h),
            "format": "f32le",          # row-major width*height float32 little-endian
            "is_metric": IS_METRIC,
            "min": float(arr.min()),
            "max": float(arr.max()),
            "depth_base64": base64.b64encode(arr.tobytes()).decode("ascii"),
        })
    return {"data": data, "model": MODEL, "usage": {"images": len(req.input)}}
