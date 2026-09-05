# =============================================================================
# File: server.py — FastAPI facade of the TF→ONNX converter service (ROADMAP
# Z11). Wraps tf2onnx behind the same async start+poll HTTP contract Core
# already uses for the PyTorch→ONNX LLM export
# (POST /convert -> {conversion_id}, GET /convert_status/{id} -> status).
#
# Conversion runs in a background thread (TensorFlow graph loading + tf2onnx
# conversion are blocking, CPU/IO-bound calls) so the HTTP handler returns
# immediately, matching `dispatch/model_conversion.rs`'s expectation that
# `POST /convert` never blocks on the actual work.
#
# Numeric-compatibility check: when the caller supplies `test_input_path` (a
# `.npy` file holding a REAL sample input — never fabricated here, per the
# "placeholder data gives false confidence" pitfall in ZADANIA.md Z11), the
# job runs the ORIGINAL TensorFlow model and the CONVERTED ONNX model on that
# same input and reports the measured `max_abs_diff`. Core decides pass/fail
# against the caller's tolerance (`dispatch/model_conversion.rs::
# evaluate_tolerance`) — this service only measures, never judges.
# =============================================================================

from __future__ import annotations

import os
import threading
import time
import uuid
from pathlib import Path
from typing import Any, Dict, Literal, Optional

import numpy as np
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

OUTPUT_ROOT = Path(
    os.environ.get(
        "TF_ONNX_CONVERTER_OUTPUT_DIR",
        Path.home() / ".cache" / "tentaflow" / "tf-onnx-converter" / "jobs",
    )
)

SourceFormat = Literal["tensorflow_savedmodel", "tensorflow_h5"]
Precision = Literal["fp32", "fp16"]


class ConvertRequest(BaseModel):
    source_path: str
    source_format: SourceFormat
    precision: Precision = "fp32"
    # Optional: path to a `.npy` file with ONE real sample input. Omitted =
    # the numeric-compatibility check is skipped (max_abs_diff stays null) —
    # NOT treated as a pass, the wizard must show "not validated" as its own
    # state rather than a silent green check.
    test_input_path: Optional[str] = None
    # Server file this conversion_id will use if the caller wants a specific
    # name; default derives one from source_path.
    output_name: Optional[str] = None


class ConvertResponse(BaseModel):
    conversion_id: str


class ConvertStatusResponse(BaseModel):
    status: Literal["running", "succeeded", "failed"]
    onnx_path: Optional[str] = None
    max_abs_diff: Optional[float] = None
    error: Optional[str] = None


class ValidateRequest(BaseModel):
    onnx_path: str
    source_path: str
    source_format: SourceFormat
    test_input_path: str


class ValidateResponse(BaseModel):
    max_abs_diff: float


class Job:
    __slots__ = ("status", "onnx_path", "max_abs_diff", "error", "created_at")

    def __init__(self) -> None:
        self.status: str = "running"
        self.onnx_path: Optional[str] = None
        self.max_abs_diff: Optional[float] = None
        self.error: Optional[str] = None
        self.created_at: float = time.time()


class JobStore:
    """In-memory job table. One process per deploy, jobs are not expected to
    survive a restart — the target `services.config_json` row Core writes on
    each poll is the durable record, this is only the in-flight cursor."""

    def __init__(self) -> None:
        self._jobs: Dict[str, Job] = {}
        self._lock = threading.Lock()

    def create(self) -> str:
        job_id = uuid.uuid4().hex
        with self._lock:
            self._jobs[job_id] = Job()
        return job_id

    def get(self, job_id: str) -> Optional[Job]:
        with self._lock:
            return self._jobs.get(job_id)

    def finish_ok(self, job_id: str, onnx_path: str, max_abs_diff: Optional[float]) -> None:
        with self._lock:
            job = self._jobs.get(job_id)
            if job is None:
                return
            job.status = "succeeded"
            job.onnx_path = onnx_path
            job.max_abs_diff = max_abs_diff

    def finish_error(self, job_id: str, error: str) -> None:
        with self._lock:
            job = self._jobs.get(job_id)
            if job is None:
                return
            job.status = "failed"
            job.error = error


JOBS = JobStore()

app = FastAPI(title="TentaFlow TF→ONNX Converter")


def _convert_to_onnx(source_path: str, source_format: SourceFormat, output_path: Path) -> Any:
    """Runs the actual TF→ONNX conversion. Returns the loaded TF callable used
    for the numeric check (a `tf.keras.Model` for H5, a SavedModel signature
    for SavedModel) so the caller does not have to reload the source twice."""
    import tensorflow as tf
    import tf2onnx

    if source_format == "tensorflow_savedmodel":
        if not Path(source_path).is_dir():
            raise ValueError(f"SavedModel path is not a directory: {source_path}")
        tf2onnx.convert.from_saved_model(source_path, output_path=str(output_path))
        loaded = tf.saved_model.load(source_path)
        infer = loaded.signatures.get("serving_default")
        if infer is None:
            raise ValueError("SavedModel has no 'serving_default' signature")
        return infer
    else:  # tensorflow_h5
        if not Path(source_path).is_file():
            raise ValueError(f"H5 file not found: {source_path}")
        keras_model = tf.keras.models.load_model(source_path)
        tf2onnx.convert.from_keras(keras_model, output_path=str(output_path))
        return keras_model


def _cast_to_fp16(onnx_path: Path) -> None:
    import onnx
    from onnxconverter_common import float16

    model = onnx.load(str(onnx_path))
    model_fp16 = float16.convert_float_to_float16(model, keep_io_types=True)
    onnx.save(model_fp16, str(onnx_path))


def _run_tf(tf_callable: Any, source_format: SourceFormat, x: np.ndarray) -> np.ndarray:
    import tensorflow as tf

    tensor = tf.constant(x)
    if source_format == "tensorflow_savedmodel":
        out = tf_callable(tensor)
        # `serving_default` returns a dict of output tensors — the check
        # compares the first one, matching the single-input/single-output
        # classifiers this track targets today (mirrors the OCR/ADR export
        # comparison in `train_ocr.rs`).
        first = next(iter(out.values()))
        return first.numpy()
    out = tf_callable(tensor, training=False)
    return np.asarray(out)


def _run_onnx(onnx_path: Path, x: np.ndarray) -> np.ndarray:
    import onnxruntime as ort

    session = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    input_name = session.get_inputs()[0].name
    outputs = session.run(None, {input_name: x.astype(np.float32)})
    return outputs[0]


def _numeric_compat_check(
    tf_callable: Any, source_format: SourceFormat, onnx_path: Path, test_input_path: str
) -> float:
    x = np.load(test_input_path)
    tf_out = _run_tf(tf_callable, source_format, x)
    onnx_out = _run_onnx(onnx_path, x)
    if tf_out.shape != onnx_out.shape:
        raise ValueError(
            f"TF/ONNX output shape mismatch: {tf_out.shape} vs {onnx_out.shape} "
            "— the conversion changed the model's output contract"
        )
    return float(np.max(np.abs(tf_out.astype(np.float64) - onnx_out.astype(np.float64))))


def _run_job(job_id: str, req: ConvertRequest) -> None:
    try:
        job_dir = OUTPUT_ROOT / job_id
        job_dir.mkdir(parents=True, exist_ok=True)
        name = req.output_name or Path(req.source_path.rstrip("/")).name or "model"
        onnx_path = job_dir / f"{name}.onnx"

        tf_callable = _convert_to_onnx(req.source_path, req.source_format, onnx_path)
        if req.precision == "fp16":
            _cast_to_fp16(onnx_path)

        max_abs_diff: Optional[float] = None
        if req.test_input_path:
            max_abs_diff = _numeric_compat_check(
                tf_callable, req.source_format, onnx_path, req.test_input_path
            )

        JOBS.finish_ok(job_id, str(onnx_path), max_abs_diff)
    except Exception as exc:  # noqa: BLE001 — reported to Core as `error`, never raised in-thread
        JOBS.finish_error(job_id, str(exc))


@app.post("/convert", response_model=ConvertResponse)
def convert(req: ConvertRequest) -> ConvertResponse:
    if not req.source_path.strip():
        raise HTTPException(status_code=400, detail="source_path is required")
    job_id = JOBS.create()
    thread = threading.Thread(target=_run_job, args=(job_id, req), daemon=True)
    thread.start()
    return ConvertResponse(conversion_id=job_id)


@app.get("/convert_status/{conversion_id}", response_model=ConvertStatusResponse)
def convert_status(conversion_id: str) -> ConvertStatusResponse:
    job = JOBS.get(conversion_id)
    if job is None:
        raise HTTPException(status_code=404, detail="unknown conversion_id")
    return ConvertStatusResponse(
        status=job.status,  # type: ignore[arg-type]
        onnx_path=job.onnx_path,
        max_abs_diff=job.max_abs_diff,
        error=job.error,
    )


@app.post("/validate", response_model=ValidateResponse)
def validate(req: ValidateRequest) -> ValidateResponse:
    """Ad-hoc re-validation of an ALREADY converted ONNX file against a
    (possibly different) real test input — used when the wizard lets a user
    re-check compatibility without re-running the conversion itself."""
    if not Path(req.onnx_path).is_file():
        raise HTTPException(status_code=400, detail=f"onnx_path not found: {req.onnx_path}")
    try:
        tf_callable = _load_for_validation(req.source_path, req.source_format)
        max_abs_diff = _numeric_compat_check(
            tf_callable, req.source_format, Path(req.onnx_path), req.test_input_path
        )
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    return ValidateResponse(max_abs_diff=max_abs_diff)


def _load_for_validation(source_path: str, source_format: SourceFormat) -> Any:
    import tensorflow as tf

    if source_format == "tensorflow_savedmodel":
        loaded = tf.saved_model.load(source_path)
        infer = loaded.signatures.get("serving_default")
        if infer is None:
            raise ValueError("SavedModel has no 'serving_default' signature")
        return infer
    return tf.keras.models.load_model(source_path)


@app.get("/health")
def health() -> Dict[str, str]:
    return {"status": "ok"}
