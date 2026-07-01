# =============================================================================
# Plik: server.py
# Opis: Realny serwer treningowy detekcji obiektów (RF-DETR). FastAPI wystawia
#       /train (COCO dataset_dir → fine-tuning RF-DETR), /status/{job_id},
#       /export/{...} (→ ONNX), /health. Trening biegnie w tle na osobnym wątku;
#       postęp (epoka, train loss, mAP@50) czytany z metrics.csv pisanego przez
#       RF-DETR do output_dir.
# Przykład: POST /train {"dataset_dir":"/data/coco","class_names":[...],
#           "output_dir":"recog/proj/run","hyperparams":{"epochs":50,...}}
# =============================================================================

from __future__ import annotations

import csv
import gc
import json
import os
import threading
import time
import traceback
import uuid
from dataclasses import dataclass, field
from typing import Any, Optional

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

app = FastAPI(title="TentaFlow RF-DETR Training", version="1.0.0")

# Artefakty (checkpointy, ONNX) lądują pod jednym zaufanym katalogiem; dowolny
# `output_dir` z requestu sprowadzamy do podkatalogu (odcięcie path traversal).
# Docker montuje ARTIFACTS_ROOT=/out; deploy natywny → katalog w HOME (zapis.).
_DEFAULT_ARTIFACTS_ROOT = os.path.join(os.path.expanduser("~"), ".tentaflow", "rfdetr-out")
ARTIFACTS_ROOT = os.path.realpath(os.environ.get("ARTIFACTS_ROOT") or _DEFAULT_ARTIFACTS_ROOT)
os.makedirs(ARTIFACTS_ROOT, exist_ok=True)

# Warianty RF-DETR dozwolone jako model bazowy treningu (rozmiar/szybkość).
_VARIANTS = {
    "nano": "RFDETRNano",
    "small": "RFDETRSmall",
    "medium": "RFDETRMedium",
    "base": "RFDETRBase",
    "large": "RFDETRLarge",
}

_JOBS: dict[str, "JobState"] = {}
_JOBS_LOCK = threading.Lock()
# Jeden trening naraz — RF-DETR wysyca GPU; równoległe joby = OOM.
_TRAIN_SLOT = threading.Semaphore(1)
_EXPORTS: dict[str, "ExportState"] = {}
_EXPORTS_LOCK = threading.Lock()
_EXPORT_SLOT = threading.Semaphore(1)


def _gpu_mem_mb() -> float:
    """Zarezerwowana pamięć GPU procesu w MB (0.0 gdy brak CUDA)."""
    try:
        import torch

        if torch.cuda.is_available():
            return torch.cuda.memory_reserved() / 1e6
    except Exception:  # noqa: BLE001
        pass
    return 0.0


@dataclass
class JobState:
    job_id: str
    status: str = "running"  # running | succeeded | failed
    epoch: int = 0
    total_epochs: int = 0
    train_loss: Optional[float] = None
    map50: Optional[float] = None
    map5095: Optional[float] = None
    error: Optional[str] = None
    artifact_path: Optional[str] = None
    # Etap joba do podglądu na żywo (przygotowanie → trening → ewaluacja → eksport).
    stage: str = "przygotowanie"
    # Znacznik startu joba (monotoniczny) do liczenia elapsed_s/eta_s.
    start_time: float = field(default_factory=time.monotonic)

    def snapshot(self) -> dict[str, Any]:
        elapsed = time.monotonic() - self.start_time
        # ETA szacujemy liniowo z tempa ukończonych epok; przed epoką 0 brak podstawy.
        eta = (elapsed / self.epoch * (self.total_epochs - self.epoch)) if self.epoch > 0 else None
        return {
            "job_id": self.job_id,
            "status": self.status,
            "epoch": self.epoch,
            "total_epochs": self.total_epochs,
            "train_loss": self.train_loss,
            "map50": self.map50,
            "map50_95": self.map5095,
            "error": self.error,
            "artifact_path": self.artifact_path,
            "gpu_mem_mb": _gpu_mem_mb(),
            "elapsed_s": elapsed,
            "eta_s": eta,
            "stage": self.stage,
        }


@dataclass
class ExportState:
    export_id: str
    status: str = "running"
    onnx_path: Optional[str] = None
    size_bytes: Optional[int] = None
    error: Optional[str] = None

    def snapshot(self) -> dict[str, Any]:
        return {
            "export_id": self.export_id,
            "status": self.status,
            "onnx_path": self.onnx_path,
            "size_bytes": self.size_bytes,
            "error": self.error,
        }


class Hyperparams(BaseModel):
    epochs: int = 50
    batch_size: int = 4
    grad_accum: int = 4
    lr: float = 1e-4
    resolution: int = 560
    early_stopping: bool = True


class TrainRequest(BaseModel):
    job_id: Optional[str] = None
    # Ścieżka do katalogu COCO (train/valid/test + _annotations.coco.json),
    # przygotowana przez Core na tym samym węźle co serwis.
    dataset_dir: str
    class_names: list[str] = Field(min_length=1)
    variant: str = "base"  # nano|small|medium|base|large
    output_dir: str
    hyperparams: Hyperparams = Field(default_factory=Hyperparams)


class ExportRequest(BaseModel):
    checkpoint_path: str
    class_names: list[str] = Field(min_length=1)
    variant: str = "base"
    resolution: int = 560
    output_dir: str


class DetectRequest(BaseModel):
    checkpoint_path: str
    class_names: list[str] = Field(min_length=1)
    variant: str = "base"
    threshold: float = 0.5
    # Obraz: base64 (małe zdjęcia z UI) ALBO ścieżka na serwerze (duże).
    image_b64: Optional[str] = None
    image_path: Optional[str] = None


# Cache załadowanych modeli detekcji per (checkpoint, variant) — RFDETRBase ładuje
# wagi z dysku przy każdej konstrukcji, więc trzymamy je między /detect.
_DETECT_MODELS: dict[str, Any] = {}
_DETECT_LOCK = threading.Lock()


def _detect_model(checkpoint_path: str, variant: str):  # noqa: ANN001
    key = f"{variant}:{checkpoint_path}"
    with _DETECT_LOCK:
        model = _DETECT_MODELS.get(key)
        if model is None:
            cls = _resolve_variant(variant)
            model = cls(pretrain_weights=checkpoint_path)
            _DETECT_MODELS[key] = model
        return model


def _sanitize_output_dir(output_dir: str) -> str:
    if not output_dir or not output_dir.strip():
        raise ValueError("output_dir must be a non-empty string")
    relative = output_dir.lstrip("/")
    root_name = os.path.basename(ARTIFACTS_ROOT.rstrip("/"))
    if root_name and (relative == root_name or relative.startswith(root_name + "/")):
        relative = relative[len(root_name):].lstrip("/")
    if not relative:
        raise ValueError("output_dir must name a subdirectory under the artifacts root")
    candidate = os.path.realpath(os.path.join(ARTIFACTS_ROOT, relative))
    root_prefix = ARTIFACTS_ROOT + os.sep
    if candidate != ARTIFACTS_ROOT and not candidate.startswith(root_prefix):
        raise ValueError(f"output_dir escapes ARTIFACTS_ROOT ({ARTIFACTS_ROOT}): {output_dir}")
    return candidate


def _update(job_id: str, **changes: Any) -> None:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            return
        for key, value in changes.items():
            setattr(st, key, value)


def _read_metrics(metrics_csv: str) -> dict[str, Any]:
    """Czyta ostatnie wartości z metrics.csv RF-DETR: epoka, train/loss,
    val/test mAP_50. Kolumny bywają puste w wierszach pośrednich (krok vs epoka),
    więc bierzemy ostatnią NIEPUSTĄ wartość każdej metryki."""
    out: dict[str, Any] = {}
    if not os.path.exists(metrics_csv):
        return out
    try:
        with open(metrics_csv, newline="", encoding="utf-8") as f:
            rows = list(csv.DictReader(f))
    except Exception:  # noqa: BLE001
        return out
    if not rows:
        return out

    def last_float(col: str) -> Optional[float]:
        for row in reversed(rows):
            v = (row.get(col) or "").strip()
            if v:
                try:
                    return float(v)
                except ValueError:
                    return None
        return None

    def last_int(col: str) -> Optional[int]:
        f = last_float(col)
        return int(f) if f is not None else None

    out["epoch"] = last_int("epoch")
    out["train_loss"] = last_float("train/loss")
    # mAP@50: preferuj test (po treningu), inaczej val (w trakcie).
    out["map50"] = last_float("test/mAP_50")
    if out["map50"] is None:
        out["map50"] = last_float("val/mAP_50")
    out["map50_95"] = last_float("test/mAP_50_95")
    if out["map50_95"] is None:
        out["map50_95"] = last_float("val/mAP_50_95")
    return out


def _resolve_variant(variant: str):  # noqa: ANN001
    import rfdetr

    name = _VARIANTS.get(variant.lower())
    if name is None or not hasattr(rfdetr, name):
        raise ValueError(f"unknown RF-DETR variant: {variant} (allowed: {list(_VARIANTS)})")
    return getattr(rfdetr, name)


def _train_worker(req: TrainRequest, job_id: str) -> None:
    output_dir = _sanitize_output_dir(req.output_dir)
    metrics_csv = os.path.join(output_dir, "metrics.csv")
    stop_poll = threading.Event()

    # Wątek-poller: w trakcie treningu odświeża status z metrics.csv co 5 s,
    # żeby Core widział postęp (RF-DETR/PTL nie daje prostego callbacku metryk).
    def poll_metrics() -> None:
        while not stop_poll.wait(5.0):
            m = _read_metrics(metrics_csv)
            if m:
                _update(
                    job_id,
                    epoch=m.get("epoch") or 0,
                    train_loss=m.get("train_loss"),
                    map50=m.get("map50"),
                    map50_95=m.get("map50_95"),
                    stage="trening",
                )

    poller = threading.Thread(target=poll_metrics, daemon=True)
    poller.start()
    try:
        os.makedirs(output_dir, exist_ok=True)
        cls = _resolve_variant(req.variant)
        model = cls(gradient_checkpointing=True)
        hp = req.hyperparams
        model.train(
            dataset_dir=req.dataset_dir,
            output_dir=output_dir,
            epochs=hp.epochs,
            batch_size=hp.batch_size,
            grad_accum_steps=hp.grad_accum,
            lr=hp.lr,
            resolution=hp.resolution,
            early_stopping=hp.early_stopping,
            class_names=req.class_names,
            num_workers=2,
            tensorboard=False,
            run_test=True,
            log_per_class_metrics=True,
        )
        stop_poll.set()
        _update(job_id, stage="ewaluacja")
        # Po treningu domykamy metryki finalnym odczytem (test mAP).
        m = _read_metrics(metrics_csv)
        best = os.path.join(output_dir, "checkpoint_best_ema.pth")
        artifact = best if os.path.exists(best) else output_dir
        _update(
            job_id,
            status="succeeded",
            epoch=m.get("epoch") or hp.epochs,
            train_loss=m.get("train_loss"),
            map50=m.get("map50"),
            map50_95=m.get("map50_95"),
            artifact_path=artifact,
        )
    except Exception as exc:  # noqa: BLE001
        stop_poll.set()
        _update(
            job_id,
            status="failed",
            error=f"{type(exc).__name__}: {exc}\n{traceback.format_exc()}",
        )
    finally:
        stop_poll.set()
        try:
            import torch

            gc.collect()
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:  # noqa: BLE001
            pass
        _TRAIN_SLOT.release()


def _export_worker(req: ExportRequest, export_id: str) -> None:
    try:
        out_dir = _sanitize_output_dir(req.output_dir)
        os.makedirs(out_dir, exist_ok=True)
        cls = _resolve_variant(req.variant)
        model = cls(pretrain_weights=req.checkpoint_path, resolution=req.resolution)
        model.export(output_dir=out_dir, dynamic_batch=True, opset_version=17)
        # RF-DETR zapisuje inference_model.onnx; znajdź wynikowy .onnx.
        onnx = None
        for name in os.listdir(out_dir):
            if name.endswith(".onnx"):
                onnx = os.path.join(out_dir, name)
                break
        if onnx is None:
            raise RuntimeError("export produced no .onnx file")
        (
            open(os.path.join(out_dir, "classes.json"), "w", encoding="utf-8")
            .write(json.dumps({"classes": req.class_names, "resolution": req.resolution}, ensure_ascii=False))
        )
        _update_export(
            export_id, status="succeeded", onnx_path=onnx, size_bytes=os.path.getsize(onnx)
        )
    except Exception as exc:  # noqa: BLE001
        _update_export(
            export_id,
            status="failed",
            error=f"{type(exc).__name__}: {exc}\n{traceback.format_exc()}",
        )
    finally:
        try:
            import torch

            gc.collect()
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:  # noqa: BLE001
            pass
        _EXPORT_SLOT.release()


def _update_export(export_id: str, **changes: Any) -> None:
    with _EXPORTS_LOCK:
        st = _EXPORTS.get(export_id)
        if st is None:
            return
        for key, value in changes.items():
            setattr(st, key, value)


@app.get("/health")
def health() -> dict[str, Any]:
    try:
        import torch

        cuda = torch.cuda.is_available()
        gpus = torch.cuda.device_count() if cuda else 0
    except Exception:  # noqa: BLE001
        cuda, gpus = False, 0
    return {"status": "ok", "cuda": cuda, "gpus": gpus}


@app.post("/train")
def train(req: TrainRequest) -> dict[str, Any]:
    if req.variant.lower() not in _VARIANTS:
        raise HTTPException(400, f"invalid variant: {req.variant}")
    if not os.path.isdir(req.dataset_dir):
        raise HTTPException(400, f"dataset_dir not found: {req.dataset_dir}")
    try:
        _sanitize_output_dir(req.output_dir)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc

    if not _TRAIN_SLOT.acquire(blocking=False):
        raise HTTPException(409, "another training job is running")

    job_id = req.job_id or uuid.uuid4().hex
    with _JOBS_LOCK:
        _JOBS[job_id] = JobState(job_id=job_id, total_epochs=req.hyperparams.epochs)
    threading.Thread(target=_train_worker, args=(req, job_id), daemon=True).start()
    return {"job_id": job_id, "status": "running"}


@app.get("/status/{job_id}")
def status(job_id: str) -> dict[str, Any]:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
    if st is None:
        raise HTTPException(404, "job not found")
    return st.snapshot()


@app.post("/export")
def export(req: ExportRequest) -> dict[str, Any]:
    if not os.path.exists(req.checkpoint_path):
        raise HTTPException(400, f"checkpoint not found: {req.checkpoint_path}")
    if not _EXPORT_SLOT.acquire(blocking=False):
        raise HTTPException(409, "another export is running")
    export_id = uuid.uuid4().hex
    with _EXPORTS_LOCK:
        _EXPORTS[export_id] = ExportState(export_id=export_id)
    threading.Thread(target=_export_worker, args=(req, export_id), daemon=True).start()
    return {"export_id": export_id}


@app.get("/export_status/{export_id}")
def export_status(export_id: str) -> dict[str, Any]:
    with _EXPORTS_LOCK:
        st = _EXPORTS.get(export_id)
    if st is None:
        raise HTTPException(404, "export not found")
    return st.snapshot()


@app.post("/detect")
def detect(req: DetectRequest) -> dict[str, Any]:
    """Detekcja na pojedynczym obrazie wytrenowanym modelem RF-DETR. Zwraca listę
    [{class_id, class_name, score, bbox_xyxy}]. Obraz z base64 albo ze ścieżki."""
    import base64
    import io as _io

    from PIL import Image

    if req.variant.lower() not in _VARIANTS:
        raise HTTPException(400, f"invalid variant: {req.variant}")
    if not os.path.exists(req.checkpoint_path):
        raise HTTPException(400, f"checkpoint not found: {req.checkpoint_path}")

    if req.image_b64:
        try:
            raw = base64.b64decode(req.image_b64)
            image = Image.open(_io.BytesIO(raw)).convert("RGB")
        except Exception as exc:  # noqa: BLE001
            raise HTTPException(400, f"invalid image_b64: {exc}") from exc
    elif req.image_path:
        if not os.path.isfile(req.image_path):
            raise HTTPException(400, f"image_path not found: {req.image_path}")
        image = Image.open(req.image_path).convert("RGB")
    else:
        raise HTTPException(400, "podaj image_b64 albo image_path")

    try:
        model = _detect_model(req.checkpoint_path, req.variant)
        det = model.predict(image, threshold=req.threshold)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(500, f"detect failed: {type(exc).__name__}: {exc}") from exc

    detections = []
    for box, score, cls in zip(det.xyxy, det.confidence, det.class_id):
        idx = int(cls)
        name = req.class_names[idx] if 0 <= idx < len(req.class_names) else str(idx)
        x1, y1, x2, y2 = [float(v) for v in box]
        detections.append(
            {
                "class_id": idx,
                "class_name": name,
                "score": round(float(score), 4),
                "bbox_xyxy": [round(x1, 1), round(y1, 1), round(x2, 1), round(y2, 1)],
            }
        )
    return {"detections": detections, "width": image.width, "height": image.height}
