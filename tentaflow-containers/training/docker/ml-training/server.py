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
import shutil
import subprocess
import sys
import tempfile
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
# Domyślnie kontener Dockera ustawia ARTIFACTS_ROOT=/out (zamontowany wolumen).
# W deployu NATYWNYM (python-bundle) /out nie istnieje i nie jest zapisywalny,
# więc fallback to katalog w HOME procesu serwisu (zapisywalny, trwały).
_DEFAULT_ARTIFACTS_ROOT = os.path.join(os.path.expanduser("~"), ".tentaflow", "ml-training-out")
ARTIFACTS_ROOT = os.path.realpath(os.environ.get("ARTIFACTS_ROOT") or _DEFAULT_ARTIFACTS_ROOT)
os.makedirs(ARTIFACTS_ROOT, exist_ok=True)

# Paski postępu tqdm (Trainer ORAZ datasets.map) piszą na stderr. Gdy supervisor
# zamknie/zapcha pipe → BrokenPipeError ubija trening na etapie _prepare_dataset.
# Wyłączamy je globalnie — postęp i loss raportujemy przez TrainerCallback do /status.
os.environ.setdefault("TQDM_DISABLE", "1")
os.environ.setdefault("HF_DATASETS_DISABLE_PROGRESS_BARS", "1")
try:
    import datasets as _datasets
    _datasets.disable_progress_bars()
except Exception:
    pass

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

# Eksport (merge LoRA + konwersja GGUF) ma własny rejestr stanów i własny slot.
# Merge ładuje cały model bazowy do RAM, więc jeden eksport naraz na proces.
_EXPORTS: dict[str, "ExportState"] = {}
_EXPORTS_LOCK = threading.Lock()
_EXPORT_SLOT = threading.Semaphore(1)

# Katalog z narzędziami llama.cpp (skrypt convert_hf_to_gguf.py + gguf-py/).
# Nadpisywalny przez env; fallback do cache native-libs TentaFlow.
LLAMA_CPP_DIR = os.environ.get("LLAMA_CPP_DIR") or os.path.expanduser(
    "~/.cache/tentaflow-native-libs/src/llama.cpp"
)

# Typy produkowane wprost przez convert_hf_to_gguf.py (bez kwantyzacji K).
_CONVERT_OUTTYPES = ("f16", "q8_0")
# Typy K-quant wymagające binarki llama-quantize (konwersja f16 → docelowy typ).
# Klucz = wartość API (lowercase), wartość = nazwa typu dla llama-quantize.
_QUANTIZE_OUTTYPES = {
    "q2_k": "Q2_K",
    "q3_k_m": "Q3_K_M",
    "q4_k_s": "Q4_K_S",
    "q4_k_m": "Q4_K_M",
    "q5_k_m": "Q5_K_M",
    "q6_k": "Q6_K",
}
_ALLOWED_OUTTYPES = _CONVERT_OUTTYPES + tuple(_QUANTIZE_OUTTYPES)


def _find_quantize_bin() -> Optional[str]:
    """Lokalizuje binarkę llama-quantize: env LLAMA_QUANTIZE_BIN, potem typowe
    katalogi build pod LLAMA_CPP_DIR. None gdy brak (K-quant niedostępny)."""
    env_bin = os.environ.get("LLAMA_QUANTIZE_BIN")
    if env_bin and os.path.exists(env_bin):
        return env_bin
    for sub in ("build-quantize/bin", "build/bin", "build-quantize", "build"):
        cand = os.path.join(LLAMA_CPP_DIR, sub, "llama-quantize")
        if os.path.exists(cand):
            return cand
    return None


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
    status: str = "running"  # running | succeeded | failed | cancelled
    step: int = 0
    total_steps: int = 0
    train_loss: Optional[float] = None
    eval_loss: Optional[float] = None
    error: Optional[str] = None
    output_dir: str = ""
    # Żądanie anulowania (`POST /cancel/{job_id}`) — ProgressCallback zatrzymuje
    # Trainer na najbliższym kroku, a worker zamyka job jako `cancelled`.
    cancel_requested: bool = False
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


@dataclass
class ExportState:
    export_id: str
    status: str = "running"  # running | succeeded | failed
    gguf_path: Optional[str] = None
    size_bytes: Optional[int] = None
    error: Optional[str] = None

    def snapshot(self) -> dict[str, Any]:
        return {
            "export_id": self.export_id,
            "status": self.status,
            "gguf_path": self.gguf_path,
            "size_bytes": self.size_bytes,
            "error": self.error,
        }


class ExportRequest(BaseModel):
    adapter_path: str
    base_model: str
    outtype: str = "f16"  # f16 | q8_0
    export_id: Optional[str] = None


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
    method: str = "lora"  # lora | qlora | dora | full
    objective: str = "sft"  # sft | dpo | kd
    # KD (knowledge distillation): repo-id modelu-nauczyciela (zwykle większy,
    # mocniejszy). Wymagane gdy objective=="kd"; ignorowane dla sft/dpo.
    teacher_model: Optional[str] = None
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


def _cancel_requested(job_id: str) -> bool:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        return bool(st and st.cancel_requested)


class ProgressCallback(TrainerCallback):
    """Mostek między Trainerem a globalnym słownikiem statusów."""

    def __init__(self, job_id: str) -> None:
        self.job_id = job_id

    def on_train_begin(self, args, state, control, **kwargs):  # noqa: ANN001
        _update(self.job_id, total_steps=int(state.max_steps))

    def on_step_end(self, args, state, control, **kwargs):  # noqa: ANN001
        _update(self.job_id, step=int(state.global_step))
        if _cancel_requested(self.job_id):
            control.should_training_stop = True

    def on_log(self, args, state, control, logs=None, **kwargs):  # noqa: ANN001
        if not logs:
            return
        changes: dict[str, Any] = {}
        # Per-krok Trainer loguje {"loss": ...}; podsumowanie po treningu
        # {"train_loss": ...} (inny klucz). Łapiemy oba, żeby także krótkie
        # przebiegi (mało kroków) raportowały train_loss do /status i krzywej.
        if "loss" in logs:
            changes["train_loss"] = float(logs["loss"])
        elif "train_loss" in logs:
            changes["train_loss"] = float(logs["train_loss"])
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

    # prompt+response ORAZ question+answer (format datasetu destylacji ML Studio) —
    # oba mapuja sie na pare user/assistant (chat template gdy dostepny).
    pr_keys = ("prompt", "response") if ("prompt" in record and "response" in record) else None
    if pr_keys is None and "question" in record and "answer" in record:
        pr_keys = ("question", "answer")
    if pr_keys is not None:
        prompt = record[pr_keys[0]]
        response = record[pr_keys[1]]
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
        "record must contain one of: 'text', 'prompt'+'response', "
        "'question'+'answer', or 'messages'"
    )


def _lora_config(hp: Hyperparams, use_dora: bool = False):  # noqa: ANN001
    from peft import LoraConfig

    return LoraConfig(
        r=hp.lora_r,
        lora_alpha=hp.lora_alpha,
        lora_dropout=hp.lora_dropout,
        bias="none",
        task_type="CAUSAL_LM",
        # DoRA = LoRA z dekompozycją wagi na magnitudę+kierunek (use_dora=True).
        # Wyższa wierność kosztem nieco wolniejszego treningu; ten sam adapter.
        use_dora=use_dora,
        # Brak jawnej listy → peft wybiera wszystkie projekcje liniowe poza
        # output head (target_modules="all-linear"), co działa dla Qwen/Llama
        # bez ręcznego mapowania nazw per architektura.
        target_modules="all-linear",
    )


def _peft_config_for(req: TrainRequest):  # noqa: ANN001
    """Zwraca peft config dla metody: full→None, dora→LoRA+use_dora, lora/qlora→LoRA."""
    if req.method == "full":
        return None
    return _lora_config(req.hyperparams, use_dora=req.method == "dora")


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
        # tqdm pisze pasek postępu na stderr; gdy supervisor zamknie/zapcha pipe
        # → BrokenPipeError ubija trening. Wyłączamy paski (loss leci przez callback).
        disable_tqdm=True,
        eval_strategy="epoch" if eval_ds is not None else "no",
    )

    peft_config = _peft_config_for(req)

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
        # tqdm pisze pasek postępu na stderr; gdy supervisor zamknie/zapcha pipe
        # → BrokenPipeError ubija trening. Wyłączamy paski (loss leci przez callback).
        disable_tqdm=True,
        eval_strategy="epoch" if eval_ds is not None else "no",
    )

    peft_config = _peft_config_for(req)

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


def _to_messages(record: dict[str, Any]) -> dict[str, Any]:
    """Mapuje rekord {prompt,response} na format konwersacyjny `messages`
    wymagany przez DataCollatorForChatML (GKD). Brak response → sama tura usera."""
    prompt = record.get("prompt") or record.get("question") or record.get("text") or ""
    response = record.get("response") or record.get("answer") or record.get("completion") or ""
    messages = [{"role": "user", "content": prompt}]
    if response:
        messages.append({"role": "assistant", "content": response})
    return {"messages": messages}


def _run_kd(req: TrainRequest, job_id: str) -> str:
    """Knowledge distillation (GKD) — student uczy się rozkładu nauczyciela.
    Student = base_model (+LoRA/QLoRA); teacher = req.teacher_model (większy)."""
    from trl import GKDConfig, GKDTrainer

    if not req.teacher_model or not req.teacher_model.strip():
        raise ValueError("KD requires 'teacher_model' (repo-id modelu-nauczyciela)")
    _validate_base_model(req.teacher_model)

    tokenizer = _load_tokenizer(req.base_model)
    model = _load_model(req.base_model, req.method)

    train_ds = Dataset.from_list([_to_messages(r) for r in req.train_data])
    eval_ds = (
        Dataset.from_list([_to_messages(r) for r in req.eval_data])
        if req.eval_data
        else None
    )

    hp = req.hyperparams
    use_bf16 = _supports_bf16()
    gkd_config = GKDConfig(
        output_dir=req.output_dir,
        num_train_epochs=hp.epochs,
        per_device_train_batch_size=hp.batch_size,
        gradient_accumulation_steps=hp.grad_accum,
        learning_rate=hp.lr,
        max_seq_length=hp.max_seq_len,
        # lmbda=on-policy frac, beta=interpolacja JSD, temperatura softmaxu.
        lmbda=0.5,
        beta=0.5,
        temperature=0.9,
        max_new_tokens=hp.max_seq_len // 2,
        bf16=use_bf16,
        fp16=not use_bf16,
        logging_steps=1,
        save_strategy="no",
        report_to=[],
        disable_tqdm=True,
        eval_strategy="epoch" if eval_ds is not None else "no",
    )

    peft_config = _peft_config_for(req)

    # Teacher ładowany przez trainer z teacher_model_name_or_path (w eval-mode,
    # bez gradientów). Student dostaje peft_config gdy lora/qlora.
    gkd_config.teacher_model_name_or_path = req.teacher_model
    trainer = GKDTrainer(
        model=model,
        teacher_model=req.teacher_model,
        args=gkd_config,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        processing_class=tokenizer,
        peft_config=peft_config,
        callbacks=[ProgressCallback(job_id)],
    )
    trainer.train()
    return _save_artifact(trainer, tokenizer, req)


def _save_artifact(trainer, tokenizer, req: TrainRequest) -> str:  # noqa: ANN001
    """Zapisuje adapter (LoRA/QLoRA) lub pełny model do output_dir.

    Gdy `merge_adapter=True` i metoda to lora/qlora/dora, łączy wagi adaptera z bazą
    i zapisuje samodzielny model do podkatalogu `merged/`.
    """
    os.makedirs(req.output_dir, exist_ok=True)
    trainer.save_model(req.output_dir)
    tokenizer.save_pretrained(req.output_dir)

    if req.merge_adapter and req.method in ("lora", "qlora", "dora"):
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
        elif req.objective == "kd":
            artifact = _run_kd(req, job_id)
        elif req.objective == "sft":
            artifact = _run_sft(req, job_id)
        else:
            raise ValueError(f"unknown objective: {req.objective}")
        if _cancel_requested(job_id):
            _update(job_id, status="cancelled")
        else:
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
        # Zwalniamy VRAM po KAŻDYM jobie (sukces/błąd) — inaczej żyjący proces
        # ml-training trzyma wagi modelu (KD ładuje dwa!) i kolejne treningi
        # wchodzą na wysycone GPU → OOM. Lokalne referencje modeli w _run_* są
        # już poza zasięgiem, więc gc.collect()+empty_cache odzyskuje pamięć.
        try:
            import gc

            gc.collect()
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:  # noqa: BLE001
            pass
        _TRAIN_SLOT.release()


def _update_export(export_id: str, **changes: Any) -> None:
    with _EXPORTS_LOCK:
        st = _EXPORTS.get(export_id)
        if st is None:
            return
        for key, value in changes.items():
            setattr(st, key, value)


def _resolve_base_path(base_model: str) -> str:
    """Zwraca lokalną ścieżkę modelu bazowego dla konwertera.

    Najpierw próbuje `from_pretrained(base_model)` (jak trening — model jest w
    cache HF_HOME, więc to działa offline). Gdy się nie uda, rozwija repo-id przez
    `snapshot_download(local_files_only=True)` i zwraca ścieżkę katalogu. Sam
    `base_model` (repo-id lub katalog) jest zwracany, bo `from_pretrained` go
    przyjmuje — pełną ścieżkę zwracamy tylko gdy fallback był konieczny.
    """
    try:
        AutoModelForCausalLM.from_pretrained(
            base_model, torch_dtype=torch.float16, trust_remote_code=ALLOW_REMOTE_CODE
        )
        return base_model
    except Exception:
        from huggingface_hub import snapshot_download

        return snapshot_download(base_model, local_files_only=True)


def _export_worker(req: ExportRequest, export_id: str) -> None:
    # Slot eksportu trzymamy przez całą operację (merge model w RAM) i zwalniamy
    # w finally — niezależnie od wyniku, żeby nie zablokować kolejnych eksportów.
    work: Optional[str] = None
    try:
        if not os.path.isdir(LLAMA_CPP_DIR):
            raise RuntimeError(
                f"narzędzia konwersji GGUF niedostępne (LLAMA_CPP_DIR={LLAMA_CPP_DIR})"
            )
        convert_script = os.path.join(LLAMA_CPP_DIR, "convert_hf_to_gguf.py")
        if not os.path.exists(convert_script):
            raise RuntimeError(
                f"narzędzia konwersji GGUF niedostępne (LLAMA_CPP_DIR={LLAMA_CPP_DIR})"
            )

        base_path = _resolve_base_path(req.base_model)

        work = tempfile.mkdtemp(prefix="tf-export-")
        base = AutoModelForCausalLM.from_pretrained(
            base_path, torch_dtype=torch.float16, trust_remote_code=ALLOW_REMOTE_CODE
        )
        from peft import PeftModel

        model = PeftModel.from_pretrained(base, req.adapter_path).merge_and_unload()
        merged_dir = os.path.join(work, "merged")
        model.save_pretrained(merged_dir, safe_serialization=True)
        AutoTokenizer.from_pretrained(
            base_path, trust_remote_code=ALLOW_REMOTE_CODE
        ).save_pretrained(merged_dir)

        out_dir = os.path.join(ARTIFACTS_ROOT, "exports", export_id)
        os.makedirs(out_dir, exist_ok=True)
        env = dict(os.environ, PYTHONPATH=os.path.join(LLAMA_CPP_DIR, "gguf-py"))

        # K-quant (Q4_K_M itd.) konwerter robi w dwóch krokach: najpierw f16
        # GGUF, potem llama-quantize do typu docelowego. f16/q8_0 idą wprost.
        is_kquant = req.outtype in _QUANTIZE_OUTTYPES
        convert_outtype = "f16" if is_kquant else req.outtype
        gguf_path = os.path.join(out_dir, f"model-{req.outtype}.gguf")
        convert_path = (
            os.path.join(out_dir, "model-f16-intermediate.gguf")
            if is_kquant
            else gguf_path
        )

        r = subprocess.run(
            [
                sys.executable,
                convert_script,
                merged_dir,
                "--outfile",
                convert_path,
                "--outtype",
                convert_outtype,
            ],
            env=env,
            capture_output=True,
            text=True,
        )
        if r.returncode != 0 or not os.path.exists(convert_path):
            raise RuntimeError("GGUF convert failed: " + r.stderr[-1500:])

        if is_kquant:
            quant_bin = _find_quantize_bin()
            if quant_bin is None:
                raise RuntimeError(
                    "llama-quantize niedostępny — zbuduj go (cmake --target "
                    "llama-quantize) lub ustaw LLAMA_QUANTIZE_BIN; f16/q8_0 nie "
                    "wymagają binarki"
                )
            q = subprocess.run(
                [quant_bin, convert_path, gguf_path, _QUANTIZE_OUTTYPES[req.outtype]],
                capture_output=True,
                text=True,
            )
            # Sprzątamy pośredni f16 niezależnie od wyniku — bywa duży (~1 GB).
            try:
                os.remove(convert_path)
            except OSError:
                pass
            if q.returncode != 0 or not os.path.exists(gguf_path):
                raise RuntimeError("llama-quantize failed: " + q.stderr[-1500:])

        size = os.path.getsize(gguf_path)
        _update_export(
            export_id, status="succeeded", gguf_path=gguf_path, size_bytes=size
        )
    except Exception as exc:  # noqa: BLE001
        _update_export(
            export_id,
            status="failed",
            error=f"{type(exc).__name__}: {exc}\n{traceback.format_exc()}",
        )
    finally:
        if work is not None:
            shutil.rmtree(work, ignore_errors=True)
        _EXPORT_SLOT.release()


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
    if req.method not in ("lora", "qlora", "dora", "full"):
        raise HTTPException(400, f"invalid method: {req.method}")
    if req.objective not in ("sft", "dpo", "kd"):
        raise HTTPException(400, f"invalid objective: {req.objective}")
    if req.objective == "kd" and not (req.teacher_model or "").strip():
        raise HTTPException(400, "objective 'kd' requires 'teacher_model'")

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


@app.post("/cancel/{job_id}")
def cancel(job_id: str) -> dict[str, Any]:
    """Podnosi flagę anulowania; Trainer zatrzymuje się na najbliższym kroku
    (`should_training_stop`), a worker zamyka job jako `cancelled`. Job już
    zakończony wraca z `cancelled: false`."""
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            raise HTTPException(404, f"unknown job: {job_id}")
        if st.status != "running":
            return {"job_id": job_id, "status": st.status, "cancelled": False}
        st.cancel_requested = True
        return {"job_id": job_id, "status": st.status, "cancelled": True}


@app.get("/models/{job_id}/path")
def model_path(job_id: str) -> dict[str, Any]:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            raise HTTPException(404, f"unknown job: {job_id}")
        if st.status != "succeeded" or not st.artifact_path:
            raise HTTPException(409, f"job {job_id} has no artifact (status={st.status})")
        return {"job_id": job_id, "artifact_path": st.artifact_path}


@app.post("/export")
def export(req: ExportRequest) -> dict[str, Any]:
    if req.outtype not in _ALLOWED_OUTTYPES:
        raise HTTPException(
            400, f"invalid outtype: {req.outtype} (allowed: {_ALLOWED_OUTTYPES})"
        )
    try:
        _validate_base_model(req.base_model)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc
    if not req.adapter_path or not os.path.isdir(req.adapter_path):
        raise HTTPException(400, f"adapter_path is not a directory: {req.adapter_path}")
    if not os.path.exists(os.path.join(req.adapter_path, "adapter_config.json")):
        raise HTTPException(
            400, f"adapter_path has no adapter_config.json: {req.adapter_path}"
        )

    export_id = req.export_id or uuid.uuid4().hex

    # Jeden eksport naraz: gdy slot zajęty, odmawiamy 429 bez tworzenia stanu.
    if not _EXPORT_SLOT.acquire(blocking=False):
        raise HTTPException(429, "another export is already running")

    try:
        with _EXPORTS_LOCK:
            if export_id in _EXPORTS and _EXPORTS[export_id].status == "running":
                raise HTTPException(409, f"export {export_id} already running")
            _EXPORTS[export_id] = ExportState(export_id=export_id)

        thread = threading.Thread(
            target=_export_worker,
            args=(req, export_id),
            name=f"export-{export_id}",
            daemon=True,
        )
        thread.start()
    except BaseException:
        # Worker, który zwolniłby slot, nigdy nie ruszył — slot musi wrócić.
        _EXPORT_SLOT.release()
        raise

    return {"export_id": export_id}


@app.get("/export_status/{export_id}")
def export_status(export_id: str) -> dict[str, Any]:
    with _EXPORTS_LOCK:
        st = _EXPORTS.get(export_id)
        if st is None:
            raise HTTPException(404, f"unknown export: {export_id}")
        return st.snapshot()
