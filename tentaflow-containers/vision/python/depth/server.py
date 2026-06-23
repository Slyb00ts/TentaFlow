# =============================================================================
# Plik: server.py
# Opis: Serwer FastAPI dla szacowania głębi (monocular depth) — Depth Anything V2
#       (relatywny i metryczny), ZoeDepth i MiDaS (Intel DPT) przez `transformers`
#       `pipeline("depth-estimation")`.
#       Model wybiera env MODEL (repo HF z presetu). Wystawia POST /v1/depth oraz
#       GET /health. Wejście: obraz (data-URL/base64). Wyjście: mapa głębi jako
#       surowy f32 little-endian (base64) + szerokość/wysokość + min/max + flaga
#       is_metric. Konsument (Core/Robots) rzutuje głębię na chmurę punktów przez
#       intrinsics kamery i skaluje metrycznie z ruchu ESKF.
# Przyklad: curl -X POST http://127.0.0.1:8096/v1/depth \
#           -d '{"model":"zoedepth-nyu-kitti","input":[{"url":"data:image/png;base64,..."}]}'
# =============================================================================

import base64
import io
import os
import re
import sys
import threading

import numpy as np
import torch
from fastapi import FastAPI, HTTPException
from PIL import Image
from pydantic import BaseModel
from transformers import pipeline

# Repo HF wybierane przez deploy (preset → env MODEL). Domyślnie ZoeDepth NK:
# metryczny ORAZ sam routuje indoor/outdoor, więc działa bez znajomości sceny
# (mapowanie z kamery wymaga metrów; dobre też dla zwykłego /v1/depth).
MODEL = os.environ.get("MODEL", "Intel/zoedepth-nyu-kitti")
# Modele metryczne (Depth Anything V2 *-Metric-*, ZoeDepth) zwracają metry;
# pozostałe — głębię względną. Mapowanie z kamery wymaga modelu metrycznego.
IS_METRIC = any(k in MODEL.lower() for k in ("metric", "zoedepth"))

_DATA_URL_RE = re.compile(r"^data:[^;,]*(;base64)?,(?P<payload>.*)$", re.DOTALL)

app = FastAPI(title="Depth (Depth Anything / MiDaS)")

# Pipeline ładowany leniwie w wątku tła: wagi ściągają się z HF przy inicjalizacji,
# więc /health odpowiada od razu, a /v1/depth zwraca 503 do czasu gotowości.
_state: dict = {"pipe": None, "error": None}
_LOAD_LOCK = threading.Lock()


class InputImage(BaseModel):
    type: str = "image_url"
    url: str


class DepthRequest(BaseModel):
    # `model` jest opcjonalne — serwis już uruchomiony z konkretnym MODEL; pole
    # istnieje dla zgodności z routingiem /v1/* (Core resolves po nazwie modelu).
    model: str | None = None
    input: list[InputImage]


def _ensure_pipe() -> None:
    if _state["pipe"] is not None:
        return
    with _LOAD_LOCK:
        if _state["pipe"] is not None:
            return
        device = 0 if torch.cuda.is_available() else -1
        try:
            _state["pipe"] = pipeline(
                "depth-estimation", model=MODEL, device=device, trust_remote_code=True
            )
        except Exception as exc:  # noqa: BLE001 — zapamiętaj błąd, zgłoś w /v1/depth
            _state["error"] = str(exc)
            raise


@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_safe_load, name="model-load", daemon=True).start()


def _safe_load() -> None:
    try:
        _ensure_pipe()
    except Exception as exc:  # noqa: BLE001
        # Hard-fail the process so the deploy probe (which waits on process exit,
        # max_wait=None) rolls back, instead of a permanent 503 hang on /health.
        print(f"FATAL: depth model load failed: {exc}", file=sys.stderr, flush=True)
        os._exit(1)


def _decode_image(url: str) -> Image.Image:
    m = _DATA_URL_RE.match(url.strip())
    payload = m.group("payload") if m else url.strip()
    try:
        raw = base64.b64decode(payload)
        return Image.open(io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Nieprawidłowy obraz/base64: {exc}")


@app.get("/health")
def health() -> dict:
    # READINESS, not liveness: TentaFlow's deploy probe treats any 2xx as "ready",
    # so we must return non-2xx until the pipeline is actually loaded (otherwise the
    # service is marked Running while /v1/depth still 503s) — or on a load failure.
    if _state["pipe"] is None:
        detail = _state["error"] or "loading"
        raise HTTPException(status_code=503, detail=f"not ready: {detail}")
    return {"status": "ok", "model": MODEL, "ready": True}


@app.post("/v1/depth")
def depth(req: DepthRequest) -> dict:
    if _state["pipe"] is None:
        if _state["error"]:
            raise HTTPException(status_code=503, detail=f"Model load failed: {_state['error']}")
        raise HTTPException(status_code=503, detail="Model jeszcze się ładuje — spróbuj ponownie.")
    if not req.input:
        raise HTTPException(status_code=400, detail="Brak obrazów w 'input'.")
    pipe = _state["pipe"]
    data = []
    for idx, item in enumerate(req.input):
        img = _decode_image(item.url)
        w, h = img.size
        out = pipe(img)
        # transformers zwraca {"predicted_depth": tensor, "depth": PIL}. Bierzemy
        # surowy predicted_depth, skalujemy do rozmiaru obrazu, jako f32 [H,W].
        pred = out["predicted_depth"]
        if hasattr(pred, "detach"):
            t = pred.detach().to(torch.float32).cpu()
            if t.dim() == 2:
                t = t.unsqueeze(0).unsqueeze(0)
            elif t.dim() == 3:
                t = t.unsqueeze(0)
            t = torch.nn.functional.interpolate(t, size=(h, w), mode="bilinear", align_corners=False)
            arr = t.squeeze().numpy().astype(np.float32)
        else:
            arr = np.asarray(pred, dtype=np.float32)
        arr = np.ascontiguousarray(arr)
        data.append({
            "index": idx,
            "width": w,
            "height": h,
            "format": "f32le",          # row-major width*height float32 little-endian
            "is_metric": IS_METRIC,
            "min": float(arr.min()),
            "max": float(arr.max()),
            "depth_base64": base64.b64encode(arr.tobytes()).decode("ascii"),
        })
    return {"data": data, "model": MODEL, "usage": {"images": len(req.input)}}
