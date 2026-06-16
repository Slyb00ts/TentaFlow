# =============================================================================
# Plik: server.py
# Opis: Realny tabularny serwer treningowy AutoGluon. FastAPI wystawia
#       /train_tabular, /status/{job_id} i /health. Trening (TabularPredictor)
#       biegnie w tle na osobnym wątku; wyniki (best model, leaderboard z
#       metrykami na holdoucie) trafiają do współdzielonego słownika statusów
#       w pamięci. CUDA (NVIDIA) gdy dostępna, inaczej CPU. Klasyfikacja + regresja.
# Przykład: POST /train_tabular {"dataset_b64":"<base64 csv>","filename":"d.csv",
#           "target_column":"label","task":"classification","time_limit_secs":120}
# =============================================================================

from __future__ import annotations

import base64
import binascii
import shutil
import tempfile
import threading
import traceback
import uuid
from dataclasses import dataclass, field
from io import BytesIO
from typing import Any, Optional

import pandas as pd
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field
from sklearn.metrics import (
    accuracy_score,
    f1_score,
    mean_squared_error,
    r2_score,
)
from sklearn.model_selection import train_test_split

import autogluon
from importlib.metadata import version as _pkg_version, PackageNotFoundError


def _autogluon_version() -> str:
    # AutoGluon to namespace package — nie ma `autogluon.__version__`;
    # wersję czytamy z metadanych zainstalowanego `autogluon.tabular`.
    try:
        return _pkg_version("autogluon.tabular")
    except PackageNotFoundError:
        return "unknown"
from autogluon.tabular import TabularPredictor

app = FastAPI(title="TentaFlow AutoGluon Training", version="1.0.0")

# Stały seed dla powtarzalności splitu holdout — ten sam zbiór testowy między
# uruchomieniami, żeby metryki leaderboardu dało się porównywać.
RANDOM_STATE = 42

# Domyślny budżet czasowy treningu (sekundy), gdy klient nie poda time_limit.
DEFAULT_TIME_LIMIT_SECS = 120


def _detect_num_gpus() -> int:
    """Wykrywa liczbę widocznych GPU CUDA (NVIDIA) przez torch.

    torch jest tu zależnością tranzytywną AutoGluon; w wariancie CPU (bez kół
    CUDA) `torch.cuda.device_count()` zwraca 0. Każdy błąd importu/inicjalizacji
    CUDA traktujemy jako 0 GPU — fallback na CPU musi być bezwzględny.
    """
    try:
        import torch

        return int(torch.cuda.device_count())
    except Exception:  # noqa: BLE001
        return 0


# Wykrywamy GPU raz na starcie procesu — liczba urządzeń nie zmienia się w trakcie
# życia serwera. AutoGluon GPU to w praktyce wyłącznie NVIDIA/CUDA.
NUM_GPUS = _detect_num_gpus()

# Stan jobów żyje w pamięci procesu — job to byt runtime, nie trwały artefakt.
_JOBS: dict[str, "JobState"] = {}
_JOBS_LOCK = threading.Lock()

# Jeden trening naraz na proces. AutoGluon trenuje wiele modeli równolegle i
# wysyca CPU/RAM — równoległe joby konkurowałyby o zasoby i wydłużały oba.
_TRAIN_SLOT = threading.Semaphore(1)


@dataclass
class JobState:
    job_id: str
    status: str = "running"  # running | succeeded | failed
    error: Optional[str] = None
    result: Optional[dict[str, Any]] = None
    # Katalog tymczasowy modelu AutoGluon — kasowany po zakończeniu joba.
    model_dir: Optional[str] = field(default=None)

    def snapshot(self) -> dict[str, Any]:
        return {
            "job_id": self.job_id,
            "status": self.status,
            "error": self.error,
            "result": self.result,
        }


class TrainTabularRequest(BaseModel):
    dataset_b64: str = Field(min_length=1)
    filename: str = Field(min_length=1)
    target_column: str = Field(min_length=1)
    task: str  # classification | regression
    time_limit_secs: int = Field(default=DEFAULT_TIME_LIMIT_SECS, ge=1, le=86400)


def _update(job_id: str, **changes: Any) -> None:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            return
        for key, value in changes.items():
            setattr(st, key, value)


def _decode_dataset(dataset_b64: str, filename: str) -> pd.DataFrame:
    """Dekoduje base64 do bajtów i wczytuje DataFrame.

    XLSX (rozszerzenie .xlsx) idzie przez read_excel, reszta jako CSV. Czytamy
    z BytesIO, żeby nie dotykać dysku przy parsowaniu danych wejściowych.
    """
    try:
        raw = base64.b64decode(dataset_b64, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise ValueError(f"dataset_b64 is not valid base64: {exc}") from exc
    if not raw:
        raise ValueError("dataset_b64 decoded to empty bytes")
    buf = BytesIO(raw)
    if filename.lower().endswith(".xlsx"):
        return pd.read_excel(buf)
    return pd.read_csv(buf)


def _build_result(
    predictor: TabularPredictor,
    holdout_df: pd.DataFrame,
    target_column: str,
    task: str,
    train_rows: int,
    is_classification: bool,
) -> dict[str, Any]:
    """Liczy metryki NA HOLDOUCIE dla każdego modelu z leaderboardu.

    Metryki bierzemy z własnych predykcji per-model (predictor.predict z
    model=<name>), nie z wewnętrznego score AutoGluon — dzięki temu kontrakt
    metryk (accuracy/f1_macro vs rmse/r2) jest jednoznaczny i porównywalny.
    """
    y_true = holdout_df[target_column]
    features = holdout_df.drop(columns=[target_column])

    leaderboard_df = predictor.leaderboard(holdout_df, silent=True)
    # fit_time per model — czas trenowania danego modelu (gdy AutoGluon go poda).
    fit_times: dict[str, float] = {}
    if "fit_time" in leaderboard_df.columns:
        for _, row in leaderboard_df.iterrows():
            ft = row.get("fit_time")
            fit_times[str(row["model"])] = (
                float(ft) if ft is not None and pd.notna(ft) else 0.0
            )

    rows: list[dict[str, Any]] = []
    for model_name in leaderboard_df["model"].tolist():
        name = str(model_name)
        entry: dict[str, Any] = {
            "model_name": name,
            "framework": "AutoGluon",
            "accuracy": None,
            "f1_macro": None,
            "rmse": None,
            "r2": None,
            "train_secs": fit_times.get(name, 0.0),
        }
        try:
            preds = predictor.predict(features, model=name)
            if is_classification:
                entry["accuracy"] = float(accuracy_score(y_true, preds))
                entry["f1_macro"] = float(
                    f1_score(y_true, preds, average="macro")
                )
            else:
                entry["rmse"] = float(mean_squared_error(y_true, preds) ** 0.5)
                entry["r2"] = float(r2_score(y_true, preds))
        except Exception:  # noqa: BLE001
            # Pojedynczy model może nie dać się ewaluować (np. wymaga GPU); nie
            # wywalamy całego leaderboardu — zostawiamy metryki jako null.
            pass
        rows.append(entry)

    class_labels: list[str] = []
    if is_classification and predictor.class_labels is not None:
        class_labels = [str(c) for c in predictor.class_labels]

    return {
        "task": task,
        "target_column": target_column,
        "train_rows": int(train_rows),
        "holdout_rows": int(len(holdout_df)),
        "class_labels": class_labels,
        "best_model_name": str(predictor.model_best),
        "leaderboard": rows,
    }


def _train_worker(req: TrainTabularRequest, job_id: str) -> None:
    model_dir: Optional[str] = None
    try:
        df = _decode_dataset(req.dataset_b64, req.filename)
        if req.target_column not in df.columns:
            raise ValueError(
                f"target_column '{req.target_column}' not found in dataset columns"
            )
        df = df.dropna(subset=[req.target_column])
        if len(df) < 4:
            raise ValueError("dataset has too few labeled rows to train (need >= 4)")

        is_classification = req.task == "classification"
        # Stratyfikacja po targecie tylko gdy każda klasa ma >= 2 próbki, inaczej
        # train_test_split rzuca wyjątek; przy zbyt rzadkich klasach dzielimy bez.
        stratify = None
        if is_classification:
            counts = df[req.target_column].value_counts()
            if counts.min() >= 2:
                stratify = df[req.target_column]

        train_df, holdout_df = train_test_split(
            df,
            test_size=0.25,
            random_state=RANDOM_STATE,
            stratify=stratify,
        )

        if is_classification:
            n_classes = df[req.target_column].nunique()
            problem_type = "binary" if n_classes == 2 else "multiclass"
        else:
            problem_type = "regression"

        model_dir = tempfile.mkdtemp(prefix=f"ag-{job_id}-")
        _update(job_id, model_dir=model_dir)

        predictor = TabularPredictor(
            label=req.target_column,
            problem_type=problem_type,
            path=model_dir,
        )
        # medium_quality = szybki preset (mniej modeli/baggingu) — trening mieści
        # się w time_limit i nadaje do interaktywnego „silnika best".
        fit_kwargs: dict[str, Any] = {
            "time_limit": req.time_limit_secs,
            "presets": "medium_quality",
        }
        # num_gpus przekazujemy TYLKO gdy są GPU. Przy 0 nie podajemy klucza —
        # AutoGluon liczy na CPU (fallback gdy brak CUDA). Przy >0 AutoGluon sam
        # rozdystrybuuje GPU na modele wspierające akcelerację (GBM-y).
        if NUM_GPUS > 0:
            fit_kwargs["num_gpus"] = NUM_GPUS
        predictor.fit(train_df, **fit_kwargs)

        result = _build_result(
            predictor=predictor,
            holdout_df=holdout_df,
            target_column=req.target_column,
            task=req.task,
            train_rows=len(train_df),
            is_classification=is_classification,
        )
        _update(job_id, status="succeeded", result=result)
    except Exception as exc:  # noqa: BLE001
        _update(
            job_id,
            status="failed",
            error=f"{type(exc).__name__}: {exc}\n{traceback.format_exc()}",
        )
    finally:
        # Artefakty modelu są niepotrzebne po policzeniu metryk — sprzątamy katalog
        # tymczasowy, żeby nie zaśmiecać dysku kolejnymi treningami.
        if model_dir:
            shutil.rmtree(model_dir, ignore_errors=True)
        _TRAIN_SLOT.release()


@app.get("/health")
def health() -> dict[str, Any]:
    return {
        "status": "ok",
        "engine": "autogluon",
        "version": _autogluon_version(),
        "num_gpus": NUM_GPUS,
        "device": "cuda" if NUM_GPUS > 0 else "cpu",
    }


@app.post("/train_tabular")
def train_tabular(req: TrainTabularRequest) -> dict[str, Any]:
    if req.task not in ("classification", "regression"):
        raise HTTPException(400, f"invalid task: {req.task}")

    # Jeden trening naraz: gdy slot zajęty, odmawiamy 429 bez tworzenia joba.
    if not _TRAIN_SLOT.acquire(blocking=False):
        raise HTTPException(429, "another training job is already running")

    job_id = uuid.uuid4().hex
    try:
        with _JOBS_LOCK:
            _JOBS[job_id] = JobState(job_id=job_id)
        thread = threading.Thread(
            target=_train_worker,
            args=(req, job_id),
            name=f"ag-train-{job_id}",
            daemon=True,
        )
        thread.start()
    except BaseException:
        # Worker, który zwolniłby slot, nigdy nie ruszył — slot musi wrócić sam.
        _TRAIN_SLOT.release()
        raise

    return {"job_id": job_id}


@app.get("/status/{job_id}")
def status(job_id: str) -> dict[str, Any]:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            raise HTTPException(404, f"unknown job: {job_id}")
        return st.snapshot()
