# =============================================================================
# Plik: server.py
# Opis: Realny serwer treningowy LLM (Hugging Face). FastAPI wystawia /train,
#       /status/{job_id}, /health i pobranie artefaktu. Trening (SFT/LoRA/QLoRA/
#       DPO) biegnie w tle na osobnym wątku; postęp raportowany przez
#       TrainerCallback do współdzielonego słownika statusów w pamięci.
# Przykład: POST /train {"base_model":"Qwen/Qwen3.5-0.8B","method":"lora",
#           "objective":"sft","train_data":[{"text":"..."}],"output_dir":"/out"}
# =============================================================================

from __future__ import annotations

import os
import threading
import traceback
import uuid
from dataclasses import dataclass, field
from typing import Any, Optional

import torch
from datasets import Dataset
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field, field_validator
from transformers import (
    AutoModelForCausalLM,
    AutoTokenizer,
    BitsAndBytesConfig,
    TrainerCallback,
)

app = FastAPI(title="TentaFlow ML Training", version="1.0.0")

# Wszystkie artefakty muszą lądować pod jednym, zaufanym katalogiem. Dowolne
# `output_dir` z requestu jest sprowadzane do podkatalogu pod ARTIFACTS_ROOT,
# co odcina path traversal (np. "../../etc").
ARTIFACTS_ROOT = os.path.realpath(os.environ.get("ARTIFACTS_ROOT", "/out"))

# Zdalny kod modelu (custom architektury z HF) jest wyłączony domyślnie — to
# wykonywanie cudzego Pythona przy ładowaniu wag. Włącz świadomie przez env.
ALLOW_REMOTE_CODE = os.environ.get("ALLOW_REMOTE_CODE") == "1"

# Stan jobów żyje w pamięci procesu. Artefakty (adapter / merged model) lądują
# na dysku w `output_dir` (montowany wolumen), więc przeżywają restart serwera,
# ale metadane statusu nie — to świadomy wybór (job to byt runtime, nie trwały).
_JOBS: dict[str, "JobState"] = {}
_JOBS_LOCK = threading.Lock()

# Jeden trening naraz na proces. Trening LLM wysyca GPU/RAM — równoległe joby
# kończą się OOM. Slot jest zwalniany przez worker po zakończeniu (sukces/błąd).
_TRAIN_SLOT = threading.Semaphore(1)


def _sanitize_output_dir(output_dir: str) -> str:
    """Sprowadza dowolne `output_dir` do bezpiecznej ścieżki pod ARTIFACTS_ROOT.

    Path traversal (`..`, ścieżki absolutne wskazujące poza root) jest odcinany
    przez porównanie realpath z ARTIFACTS_ROOT. Zwraca znormalizowaną ścieżkę
    docelową albo rzuca ValueError.
    """
    if not output_dir or not output_dir.strip():
        raise ValueError("output_dir must be a non-empty string")
    # Ścieżkę z requestu traktujemy jako względną do roota (wiodący "/" ignorujemy),
    # żeby klient nie mógł wskazać dowolnego miejsca w systemie plików.
    relative = output_dir.lstrip("/")
    # A client may pass a path already rooted at ARTIFACTS_ROOT (e.g. "/out/run1");
    # strip that leading component so it does not nest as "/out/out/run1".
    root_name = os.path.basename(ARTIFACTS_ROOT.rstrip("/"))
    if root_name and (relative == root_name or relative.startswith(root_name + "/")):
        relative = relative[len(root_name):].lstrip("/")
    if not relative:
        raise ValueError("output_dir must name a subdirectory under the artifacts root")
    candidate = os.path.realpath(os.path.join(ARTIFACTS_ROOT, relative))
    root_prefix = ARTIFACTS_ROOT + os.sep
    if candidate != ARTIFACTS_ROOT and not candidate.startswith(root_prefix):
        raise ValueError(
            f"output_dir escapes ARTIFACTS_ROOT ({ARTIFACTS_ROOT}): {output_dir}"
        )
    return candidate


def _validate_base_model(base_model: str) -> None:
    """base_model musi być niepustym repo-id HF, nie lokalną ścieżką z `..`."""
    if not base_model or not base_model.strip():
        raise ValueError("base_model must be a non-empty string")
    if ".." in base_model:
        raise ValueError("base_model must not contain '..'")


def _supports_bf16() -> bool:
    """bf16 tylko gdy GPU realnie wspiera (Ampere+). Inaczej fp16."""
    return torch.cuda.is_available() and torch.cuda.is_bf16_supported()


@dataclass
class JobState:
    job_id: str
    status: str = "running"  # running | succeeded | failed
    step: int = 0
    total_steps: int = 0
    train_loss: Optional[float] = None
    eval_loss: Optional[float] = None
    error: Optional[str] = None
    output_dir: str = ""
    artifact_path: Optional[str] = None

    def snapshot(self) -> dict[str, Any]:
        return {
            "job_id": self.job_id,
            "status": self.status,
            "step": self.step,
            "total_steps": self.total_steps,
            "train_loss": self.train_loss,
            "eval_loss": self.eval_loss,
            "error": self.error,
            "artifact_path": self.artifact_path,
        }


class Hyperparams(BaseModel):
    epochs: float = Field(default=3.0, ge=1.0, le=50.0)
    lr: float = Field(default=2e-4, gt=0.0, le=1.0)
    batch_size: int = Field(default=1, ge=1, le=64)
    grad_accum: int = Field(default=8, ge=1, le=64)
    lora_r: int = Field(default=16, ge=1, le=256)
    lora_alpha: int = Field(default=32, ge=1, le=1024)
    lora_dropout: float = Field(default=0.05, ge=0.0, lt=1.0)
    max_seq_len: int = Field(default=1024, ge=8, le=8192)


class TrainRequest(BaseModel):
    job_id: Optional[str] = None
    base_model: str
    train_data: list[dict[str, Any]] = Field(min_length=1)
    eval_data: Optional[list[dict[str, Any]]] = None
    method: str = "lora"  # lora | qlora | full
    objective: str = "sft"  # sft | dpo
    hyperparams: Hyperparams = Field(default_factory=Hyperparams)
    output_dir: str
    merge_adapter: bool = False


def _update(job_id: str, **changes: Any) -> None:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            return
        for key, value in changes.items():
            setattr(st, key, value)


class ProgressCallback(TrainerCallback):
    """Mostek między Trainerem a globalnym słownikiem statusów."""

    def __init__(self, job_id: str) -> None:
        self.job_id = job_id

    def on_train_begin(self, args, state, control, **kwargs):  # noqa: ANN001
        _update(self.job_id, total_steps=int(state.max_steps))

    def on_step_end(self, args, state, control, **kwargs):  # noqa: ANN001
        _update(self.job_id, step=int(state.global_step))

    def on_log(self, args, state, control, logs=None, **kwargs):  # noqa: ANN001
        if not logs:
            return
        changes: dict[str, Any] = {}
        if "loss" in logs:
            changes["train_loss"] = float(logs["loss"])
        if "eval_loss" in logs:
            changes["eval_loss"] = float(logs["eval_loss"])
        if changes:
            _update(self.job_id, **changes)


def _format_record(record: dict[str, Any], tokenizer) -> str:  # noqa: ANN001
    """Sprowadza heterogeniczny rekord do pojedynczego stringa treningowego.

    Obsługiwane kształty: {text}, {prompt,response}, {messages:[{role,content}]}.
    Dla `messages` używamy chat template tokenizera, gdy jest dostępny — inaczej
    spłaszczamy do "role: content". To samo formatowanie stosuje się do SFT.
    """
    if "text" in record:
        return str(record["text"])

    if "messages" in record:
        messages = record["messages"]
        if getattr(tokenizer, "chat_template", None):
            return tokenizer.apply_chat_template(
                messages, tokenize=False, add_generation_prompt=False
            )
        return "\n".join(f"{m.get('role', '')}: {m.get('content', '')}" for m in messages)

    if "prompt" in record and "response" in record:
        prompt = record["prompt"]
        response = record["response"]
        if getattr(tokenizer, "chat_template", None):
            return tokenizer.apply_chat_template(
                [
                    {"role": "user", "content": prompt},
                    {"role": "assistant", "content": response},
                ],
                tokenize=False,
                add_generation_prompt=False,
            )
        return f"{prompt}\n{response}"

    raise ValueError(
        "record must contain one of: 'text', 'prompt'+'response', or 'messages'"
    )


def _lora_config(hp: Hyperparams):  # noqa: ANN001
    from peft import LoraConfig

    return LoraConfig(
        r=hp.lora_r,
        lora_alpha=hp.lora_alpha,
        lora_dropout=hp.lora_dropout,
        bias="none",
        task_type="CAUSAL_LM",
        # Brak jawnej listy → peft wybiera wszystkie projekcje liniowe poza
        # output head (target_modules="all-linear"), co działa dla Qwen/Llama
        # bez ręcznego mapowania nazw per architektura.
        target_modules="all-linear",
    )


def _load_tokenizer(base_model: str):  # noqa: ANN001
    tokenizer = AutoTokenizer.from_pretrained(
        base_model, trust_remote_code=ALLOW_REMOTE_CODE
    )
    # Qwen/Llama często nie mają pad_token — bez tego collator/padding pada.
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token
    return tokenizer


def _load_model(base_model: str, method: str):  # noqa: ANN001
    """Ładuje model bazowy. QLoRA → 4-bit nf4 + double quant; reszta → bf16/fp16."""
    compute_dtype = torch.bfloat16 if _supports_bf16() else torch.float16
    common = dict(trust_remote_code=ALLOW_REMOTE_CODE)
    if method == "qlora":
        bnb = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_quant_type="nf4",
            bnb_4bit_use_double_quant=True,
            bnb_4bit_compute_dtype=compute_dtype,
        )
        # QLoRA pins the quantized weights to a single GPU at load time;
        # `device_map="auto"` would shard onto meta tensors and break training.
        model = AutoModelForCausalLM.from_pretrained(
            base_model, quantization_config=bnb, device_map={"": 0}, **common
        )
        from peft import prepare_model_for_kbit_training

        model = prepare_model_for_kbit_training(model)
        return model
    # LoRA / full: load on CPU (no device_map → no meta tensors); the Trainer
    # moves the model to the GPU itself. `device_map="auto"` here triggers
    # "Cannot copy out of meta tensor" during training.
    return AutoModelForCausalLM.from_pretrained(
        base_model, torch_dtype=compute_dtype, **common
    )


def _run_sft(req: TrainRequest, job_id: str) -> str:
    from trl import SFTConfig, SFTTrainer

    tokenizer = _load_tokenizer(req.base_model)
    model = _load_model(req.base_model, req.method)

    train_texts = [{"text": _format_record(r, tokenizer)} for r in req.train_data]
    train_ds = Dataset.from_list(train_texts)
    eval_ds = None
    if req.eval_data:
        eval_ds = Dataset.from_list(
            [{"text": _format_record(r, tokenizer)} for r in req.eval_data]
        )

    hp = req.hyperparams
    use_bf16 = _supports_bf16()
    sft_config = SFTConfig(
        output_dir=req.output_dir,
        num_train_epochs=hp.epochs,
        per_device_train_batch_size=hp.batch_size,
        gradient_accumulation_steps=hp.grad_accum,
        learning_rate=hp.lr,
        # trl 0.12.1: pole nazywa się max_seq_length (nie max_length).
        max_seq_length=hp.max_seq_len,
        # Dataset ma jawną kolumnę "text" zbudowaną przez _format_record.
        dataset_text_field="text",
        packing=False,
        bf16=use_bf16,
        fp16=not use_bf16,
        logging_steps=1,
        save_strategy="no",
        report_to=[],
        eval_strategy="epoch" if eval_ds is not None else "no",
    )

    peft_config = None if req.method == "full" else _lora_config(hp)

    # trl 0.12.1: SFTTrainer przyjmuje tokenizer= (deprecation, ale działa);
    # processing_class doszedł dopiero w nowszym trl.
    trainer = SFTTrainer(
        model=model,
        args=sft_config,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        tokenizer=tokenizer,
        peft_config=peft_config,
        callbacks=[ProgressCallback(job_id)],
    )
    trainer.train()
    return _save_artifact(trainer, tokenizer, req)


def _run_dpo(req: TrainRequest, job_id: str) -> str:
    from trl import DPOConfig, DPOTrainer

    tokenizer = _load_tokenizer(req.base_model)
    model = _load_model(req.base_model, req.method)

    # DPO wymaga par preferencji: {prompt, chosen, rejected}.
    for r in req.train_data:
        if not {"prompt", "chosen", "rejected"} <= set(r):
            raise ValueError("DPO records must contain 'prompt', 'chosen', 'rejected'")
    train_ds = Dataset.from_list(
        [
            {"prompt": r["prompt"], "chosen": r["chosen"], "rejected": r["rejected"]}
            for r in req.train_data
        ]
    )
    eval_ds = None
    if req.eval_data:
        eval_ds = Dataset.from_list(
            [
                {"prompt": r["prompt"], "chosen": r["chosen"], "rejected": r["rejected"]}
                for r in req.eval_data
            ]
        )

    hp = req.hyperparams
    use_bf16 = _supports_bf16()
    # max_prompt_length to połowa budżetu sekwencji; reszta na chosen/rejected.
    dpo_config = DPOConfig(
        output_dir=req.output_dir,
        num_train_epochs=hp.epochs,
        per_device_train_batch_size=hp.batch_size,
        gradient_accumulation_steps=hp.grad_accum,
        learning_rate=hp.lr,
        beta=0.1,
        max_length=hp.max_seq_len,
        max_prompt_length=hp.max_seq_len // 2,
        bf16=use_bf16,
        fp16=not use_bf16,
        logging_steps=1,
        save_strategy="no",
        report_to=[],
        eval_strategy="epoch" if eval_ds is not None else "no",
    )

    peft_config = None if req.method == "full" else _lora_config(hp)

    # trl 0.12.1: DPOTrainer przyjmuje tokenizer= (deprecation, ale działa).
    trainer = DPOTrainer(
        model=model,
        args=dpo_config,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        tokenizer=tokenizer,
        peft_config=peft_config,
        callbacks=[ProgressCallback(job_id)],
    )
    trainer.train()
    return _save_artifact(trainer, tokenizer, req)


def _save_artifact(trainer, tokenizer, req: TrainRequest) -> str:  # noqa: ANN001
    """Zapisuje adapter (LoRA/QLoRA) lub pełny model do output_dir.

    Gdy `merge_adapter=True` i metoda to lora/qlora, łączy wagi adaptera z bazą
    i zapisuje samodzielny model do podkatalogu `merged/`.
    """
    os.makedirs(req.output_dir, exist_ok=True)
    trainer.save_model(req.output_dir)
    tokenizer.save_pretrained(req.output_dir)

    if req.merge_adapter and req.method in ("lora", "qlora"):
        merged_dir = os.path.join(req.output_dir, "merged")
        os.makedirs(merged_dir, exist_ok=True)
        merged = trainer.model.merge_and_unload()
        merged.save_pretrained(merged_dir)
        tokenizer.save_pretrained(merged_dir)
        return merged_dir

    return req.output_dir


def _train_worker(req: TrainRequest, job_id: str) -> None:
    # Slot współbieżności trzymamy przez cały czas treningu i zwalniamy w finally,
    # niezależnie od wyniku — inaczej kolejne joby byłyby blokowane na zawsze.
    try:
        if req.objective == "dpo":
            artifact = _run_dpo(req, job_id)
        elif req.objective == "sft":
            artifact = _run_sft(req, job_id)
        else:
            raise ValueError(f"unknown objective: {req.objective}")
        _update(job_id, status="succeeded", artifact_path=artifact)
    except torch.cuda.OutOfMemoryError as exc:  # noqa: PERF203
        torch.cuda.empty_cache()
        _update(job_id, status="failed", error=f"CUDA OOM: {exc}")
    except Exception as exc:  # noqa: BLE001
        _update(
            job_id,
            status="failed",
            error=f"{type(exc).__name__}: {exc}\n{traceback.format_exc()}",
        )
    finally:
        _TRAIN_SLOT.release()


@app.get("/health")
def health() -> dict[str, Any]:
    cuda = torch.cuda.is_available()
    return {
        "status": "ok",
        "cuda": cuda,
        "gpus": torch.cuda.device_count() if cuda else 0,
    }


@app.post("/train")
def train(req: TrainRequest) -> dict[str, Any]:
    if req.method not in ("lora", "qlora", "full"):
        raise HTTPException(400, f"invalid method: {req.method}")
    if req.objective not in ("sft", "dpo"):
        raise HTTPException(400, f"invalid objective: {req.objective}")

    # Walidacja wejścia PRZED zajęciem slotu/utworzeniem joba — błędne żądanie
    # nie może uruchomić treningu ani zablokować kolejki.
    try:
        _validate_base_model(req.base_model)
        req.output_dir = _sanitize_output_dir(req.output_dir)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc

    job_id = req.job_id or uuid.uuid4().hex

    # Jeden trening naraz: gdy slot zajęty, odmawiamy 429 bez tworzenia joba.
    if not _TRAIN_SLOT.acquire(blocking=False):
        raise HTTPException(429, "another training job is already running")

    try:
        with _JOBS_LOCK:
            if job_id in _JOBS and _JOBS[job_id].status == "running":
                raise HTTPException(409, f"job {job_id} already running")
            _JOBS[job_id] = JobState(job_id=job_id, output_dir=req.output_dir)

        thread = threading.Thread(
            target=_train_worker,
            args=(req, job_id),
            name=f"train-{job_id}",
            daemon=True,
        )
        thread.start()
    except BaseException:
        # Nie udało się wystartować workera (np. konflikt 409) — slot musi wrócić,
        # bo worker, który by go zwolnił, nigdy nie ruszył.
        _TRAIN_SLOT.release()
        raise

    return {"job_id": job_id, "status": "running"}


@app.get("/status/{job_id}")
def status(job_id: str) -> dict[str, Any]:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            raise HTTPException(404, f"unknown job: {job_id}")
        return st.snapshot()


@app.get("/models/{job_id}/path")
def model_path(job_id: str) -> dict[str, Any]:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            raise HTTPException(404, f"unknown job: {job_id}")
        if st.status != "succeeded" or not st.artifact_path:
            raise HTTPException(409, f"job {job_id} has no artifact (status={st.status})")
        return {"job_id": job_id, "artifact_path": st.artifact_path}
