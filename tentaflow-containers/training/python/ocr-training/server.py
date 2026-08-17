# =============================================================================
# Plik: server.py
# Opis: Realny serwer treningowy CZYTNIKA OCR tablic (CRNN + CTC). FastAPI wystawia
#       /train (COCO dataset_dir → wycinki klasy źródłowej → podział na wiersze →
#       trening na mieszance realnych wierszy i danych syntetycznych),
#       /status/{job_id}, /cancel/{job_id}, /export (→ ONNX opset 17 + alfabet),
#       /health. Trening biegnie w tle na osobnym wątku; postęp (epoka, train loss,
#       val_exact na realnych i syntetycznych wierszach) czytany ze statusu w
#       pamięci i z metrics.csv (polling).
# Przykład: POST /train {"dataset_dir":"/data/coco","attribute":"kod",
#           "source_class":"tablica_adr","output_dir":"ocr/proj/run",
#           "adr_pairs":[{"kemler":"30","un":"1202"}],
#           "hyperparams":{"epochs":30,"batch_size":64,"learning_rate":0.001,
#                          "synthetic_per_epoch":20000,"real_repeat":8}}
# =============================================================================

from __future__ import annotations

import csv
import gc
import json
import os
import random
import re
import threading
import time
import traceback
import uuid
from dataclasses import dataclass, field
from typing import Any, Optional

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

import gen_synth
from model import ALPHABET, IMG_H, IMG_W

app = FastAPI(title="TentaFlow OCR Training", version="1.0.0")

# Artefakty (checkpointy, ONNX) lądują pod jednym zaufanym katalogiem; dowolny
# `output_dir` z requestu sprowadzamy do podkatalogu (odcięcie path traversal).
_DEFAULT_ARTIFACTS_ROOT = os.path.join(os.path.expanduser("~"), ".tentaflow", "ocr-out")
ARTIFACTS_ROOT = os.path.realpath(os.environ.get("ARTIFACTS_ROOT") or _DEFAULT_ARTIFACTS_ROOT)
os.makedirs(ARTIFACTS_ROOT, exist_ok=True)

# Limity zasobów (ochrona przed DoS): odrzucamy zbyt duże pliki COCO, zbyt liczne
# adnotacje i zbyt wiele wierszy realnych.
_MAX_COCO_BYTES = 200 * 1024 * 1024
_MAX_ANNOTATIONS = 2_000_000
_MAX_REAL_ROWS = 200_000
# Górny limit pikseli obrazu (obrona przed decompression bomb w PIL/cv2).
_MAX_IMAGE_PIXELS = 178_956_970
# Najmniejszy sensowny wycinek tablicy — mniejszy nie ma z czego wyciąć wiersza.
_MIN_CROP_PX = 12
# Nazwa atrybutu/klasy trafia do metadanych i logów; treść walidowana też po
# stronie Rust — tu drugi bezpiecznik przed nietypowymi znakami.
_SAFE_NAME_RE = re.compile(r"^[A-Za-z0-9_.\- ]{1,64}$")
# Etykieta wiersza to wyłącznie cyfry (alfabet CRNN) — 1..8 znaków.
_LABEL_RE = re.compile(r"^[0-9]{1,8}$")
# Podział wycinka na wiersze: ta sama przerwa wokół linii środkowej co w runtime
# (`SPLIT_MARGIN` w `vision/adr_ocr.rs`). Trening MUSI widzieć wiersze tak samo
# pociete jak inferencja, inaczej model uczy się innego kadru niż dostaje.
_SPLIT_MARGIN = 0.06
# Deterministyczny podział realnych wierszy: co N-ty (po stabilnym sortowaniu)
# idzie do walidacji.
_VALID_STRIDE = 7

_JOBS: dict[str, "JobState"] = {}
_JOBS_LOCK = threading.Lock()
# Jeden trening naraz — model wysyca GPU; równoległe joby = OOM.
_TRAIN_SLOT = threading.Semaphore(1)
# Eksport ONNX buduje model i alokuje GPU — dopuszczamy jeden naraz.
_EXPORT_SLOT = threading.Semaphore(1)


class _Cancelled(Exception):
    """Trening przerwany na żądanie Core (`POST /cancel/{job_id}`)."""


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
    status: str = "running"  # running | succeeded | failed | cancelled
    epoch: int = 0
    total_epochs: int = 0
    train_loss: Optional[float] = None
    # Exact-match na WSTRZYMANYCH REALNYCH wierszach — to jest metryka, która
    # mówi cokolwiek o jakości na zdjęciach z drogi.
    val_exact_real: Optional[float] = None
    # Exact-match na świeżo wygenerowanych wierszach syntetycznych (kontrola, że
    # model w ogóle się uczy, gdy realnych wierszy jest bardzo mało).
    val_exact_synth: Optional[float] = None
    error: Optional[str] = None
    artifact_path: Optional[str] = None
    stage: str = "przygotowanie"
    start_time: float = field(default_factory=time.monotonic)
    cancel_requested: bool = False

    def snapshot(self) -> dict[str, Any]:
        elapsed = time.monotonic() - self.start_time
        eta = (elapsed / self.epoch * (self.total_epochs - self.epoch)) if self.epoch > 0 else None
        return {
            "job_id": self.job_id,
            "status": self.status,
            "epoch": self.epoch,
            "total_epochs": self.total_epochs,
            "train_loss": self.train_loss,
            "val_exact_real": self.val_exact_real,
            "val_exact_synth": self.val_exact_synth,
            "error": self.error,
            "artifact_path": self.artifact_path,
            "gpu_mem_mb": _gpu_mem_mb(),
            "elapsed_s": elapsed,
            "eta_s": eta,
            "stage": self.stage,
        }


class Hyperparams(BaseModel):
    epochs: int = Field(default=30, ge=1, le=1000)
    batch_size: int = Field(default=64, ge=1, le=1024)
    learning_rate: float = Field(default=1e-3, gt=0.0, le=1.0)
    # Liczba próbek syntetycznych na epokę (0 = tylko realne wiersze).
    synthetic_per_epoch: int = Field(default=20_000, ge=0, le=2_000_000)
    # Ile razy realne wiersze powtarzamy w epoce — realnych etykiet jest z natury
    # mało, bez powtórzeń syntetyk by je zdominował.
    real_repeat: int = Field(default=8, ge=1, le=1000)


class AdrPair(BaseModel):
    kemler: str
    un: str


class TrainRequest(BaseModel):
    job_id: Optional[str] = None
    # Katalog COCO (train/valid/... z _annotations.coco.json w podkatalogach).
    dataset_dir: str
    # Atrybut adnotacji COCO niosący odczyt (`attributes[attribute]`), np. "kod".
    attribute: str
    # Kategoria COCO, z której bierzemy wycinki ("" = wszystkie klasy).
    source_class: str = ""
    output_dir: str
    # Katalog ADR wdrożenia (kemler/UN) — źródło etykiet syntetycznych.
    adr_pairs: list[AdrPair] = Field(default_factory=list)
    hyperparams: Hyperparams = Field(default_factory=Hyperparams)


class ExportRequest(BaseModel):
    checkpoint_path: str
    output_dir: str


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


def _sanitize_output_dir(output_dir: str) -> str:
    if not output_dir or not output_dir.strip():
        raise ValueError("output_dir must be a non-empty string")
    relative = output_dir.lstrip("/")
    root_name = os.path.basename(ARTIFACTS_ROOT.rstrip("/"))
    if root_name and (relative == root_name or relative.startswith(root_name + "/")):
        relative = relative[len(root_name):].lstrip("/")
    if not relative:
        raise ValueError("output_dir must name a subdirectory")
    candidate = os.path.realpath(os.path.join(ARTIFACTS_ROOT, relative))
    if candidate != ARTIFACTS_ROOT and not candidate.startswith(ARTIFACTS_ROOT + os.sep):
        raise ValueError("output_dir escapes the artifacts root")
    return candidate


def _iter_coco_splits(dataset_dir: str):
    """(images_dir, coco_json) dla każdego podkatalogu z _annotations.coco.json
    (train/, valid/, ...) oraz — gdy plik leży w korzeniu — dla samego dataset_dir."""
    root_json = os.path.join(dataset_dir, "_annotations.coco.json")
    if os.path.isfile(root_json):
        yield dataset_dir, root_json
    for name in sorted(os.listdir(dataset_dir)):
        sub = os.path.join(dataset_dir, name)
        if not os.path.isdir(sub):
            continue
        j = os.path.join(sub, "_annotations.coco.json")
        if os.path.isfile(j):
            yield sub, j


def _review_gate_active(dataset_dir: str) -> bool:
    """Czy dataset przeszedł przez nasz edytor, czyli czy bramka `approved` ma sens.
    Aktywna, gdy JAKIKOLWIEK obraz w JAKIMKOLWIEK splicie niesie pole `approved`."""
    for _images_dir, coco_json in _iter_coco_splits(dataset_dir):
        if os.path.getsize(coco_json) > _MAX_COCO_BYTES:
            continue
        with open(coco_json, encoding="utf-8") as f:
            coco = json.load(f)
        for im in coco.get("images", []):
            if "approved" in im:
                return True
    return False


def parse_label_rows(value: Any) -> list[str]:
    """Rozbija wartość atrybutu OCR na etykiety kolejnych WIERSZY tablicy.

    Tablica ADR ma dwa wiersze, a operator zapisuje odczyt jako `<kemler>/<UN>`
    (np. "99/3257") — separatorem jest `/`. Wartość bez separatora to tablica
    jednowierszowa. Zwraca [] gdy którykolwiek człon nie jest samymi cyframi
    (alfabet CRNN to wyłącznie 0-9), bo wtedy nie wiemy, czego uczyć."""
    if not isinstance(value, str):
        return []
    parts = [p.strip() for p in value.strip().split("/")]
    parts = [p for p in parts if p != ""]
    if not parts or len(parts) > 2:
        return []
    if not all(_LABEL_RE.match(p) for p in parts):
        return []
    return parts


def split_rows(gray, rows_expected: int):
    """Dzieli wycinek na wiersze DOKŁADNIE jak runtime (`split_rows` w
    `vision/adr_ocr.rs`): górna połowa do `mid - gap`, dolna od `mid + gap`, gdzie
    `gap = 6% wysokości`. Dla tablicy jednowierszowej zwraca cały wycinek."""
    h = gray.shape[0]
    if rows_expected < 2:
        return [gray]
    mid = h // 2
    gap = int(h * _SPLIT_MARGIN)
    top_end = min(max(mid - gap, 1), h)
    bot_start = min(mid + gap, max(h - 1, 0))
    top = gray[0:top_end, :]
    bot = gray[bot_start:h, :]
    if top.size == 0 or bot.size == 0:
        return []
    return [top, bot]


def to_model_input(gray):
    """Wiersz → wejście CRNN: rozciągnięcie CAŁEGO wiersza do 32x128 (ten sam
    pełnowierszowy stretch, na którym trenuje ścieżka syntetyczna) i normalizacja
    (p/255 - 0.5)/0.5. Runtime dokłada jeszcze opcjonalny content-trim wiersza —
    to zabezpieczenie inferencji z własnym warunkiem rezygnacji, nie inna
    geometria, więc trening zostaje przy czystym stretchu."""
    import cv2

    resized = cv2.resize(gray, (IMG_W, IMG_H), interpolation=cv2.INTER_AREA)
    return resized


def _collect_real_rows(req: TrainRequest, job_id: str) -> dict[str, Any]:
    """Wczytuje splity COCO, bierze tylko obrazy ZATWIERDZONE przez człowieka
    (`approved: true` — nieobejrzane predykcje auto-labela nie mogą uczyć modelu na
    jego własnych wyjściach; dataset bez tego pola nie przeszedł przez nasz edytor
    i idzie w całości), filtruje adnotacje po kategorii i atrybucie OCR, wycina
    bbox z PEŁNEJ rozdzielczości, dzieli na wiersze i paruje z członami etykiety.
    Zwraca listy (obraz 32x128 uint8, etykieta) dla train i valid oraz statystyki."""
    import cv2
    import numpy as np
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = _MAX_IMAGE_PIXELS

    source_class = (req.source_class or "").strip()
    candidates: list[tuple[str, tuple[int, int, int, int], list[str]]] = []
    skipped_unapproved = 0
    skipped_wrong_class = 0
    skipped_no_attr = 0
    skipped_bad_label = 0
    total_annotations = 0
    images_seen = 0
    images_approved = 0

    review_gate = _review_gate_active(req.dataset_dir)

    for images_dir, coco_json in _iter_coco_splits(req.dataset_dir):
        size = os.path.getsize(coco_json)
        if size > _MAX_COCO_BYTES:
            raise ValueError(f"plik COCO za duży ({size} B > {_MAX_COCO_BYTES} B): {coco_json}")
        with open(coco_json, encoding="utf-8") as f:
            coco = json.load(f)
        cat_name = {c["id"]: c["name"] for c in coco.get("categories", [])}
        split_images = coco.get("images", [])
        images_seen += len(split_images)
        img_meta = {
            im["id"]: im
            for im in split_images
            if not review_gate or im.get("approved") is True
        }
        images_approved += len(img_meta)
        annotations = coco.get("annotations", [])
        total_annotations += len(annotations)
        if total_annotations > _MAX_ANNOTATIONS:
            raise ValueError(f"za dużo adnotacji ({total_annotations} > {_MAX_ANNOTATIONS})")

        for ann in annotations:
            if len(candidates) >= _MAX_REAL_ROWS:
                print(
                    f"[collect_rows] job={job_id} osiągnięto limit wierszy "
                    f"({_MAX_REAL_ROWS}) — pomijam pozostałe adnotacje",
                    flush=True,
                )
                break
            im = img_meta.get(ann.get("image_id"))
            if im is None:
                skipped_unapproved += 1
                continue
            cname = cat_name.get(ann.get("category_id"), "")
            if source_class and cname != source_class:
                skipped_wrong_class += 1
                continue
            attrs = ann.get("attributes") or {}
            if req.attribute not in attrs:
                skipped_no_attr += 1
                continue
            rows = parse_label_rows(attrs.get(req.attribute))
            if not rows:
                skipped_bad_label += 1
                continue
            bbox = ann.get("bbox")
            if not bbox or len(bbox) != 4:
                skipped_bad_label += 1
                continue
            # Tylko nazwa pliku — odcięcie komponentów ścieżki (path traversal).
            file_name = os.path.basename(im["file_name"])
            path = os.path.join(images_dir, file_name)
            if os.path.realpath(path) != os.path.join(os.path.realpath(images_dir), file_name):
                continue
            candidates.append((path, tuple(int(round(v)) for v in bbox), rows))

    # Deterministyczna kolejność: ten sam dataset zawsze daje ten sam podział.
    candidates.sort(key=lambda c: (c[0], c[1]))

    train_rows: list[tuple[Any, str]] = []
    valid_rows: list[tuple[Any, str]] = []
    decoded_crops = 0
    failed_crops = 0
    for idx, (path, bbox, rows) in enumerate(candidates):
        if idx % 50 == 0 and _cancel_requested(job_id):
            raise _Cancelled
        x, y, w, h = bbox
        try:
            with Image.open(path) as img:
                gray_full = np.array(img.convert("L"))
        except Exception:  # noqa: BLE001
            failed_crops += 1
            continue
        fh, fw = gray_full.shape[:2]
        x1 = max(0, x)
        y1 = max(0, y)
        x2 = min(fw, x + max(1, w))
        y2 = min(fh, y + max(1, h))
        if x2 - x1 < _MIN_CROP_PX or y2 - y1 < _MIN_CROP_PX:
            failed_crops += 1
            continue
        crop = gray_full[y1:y2, x1:x2]
        parts = split_rows(crop, len(rows))
        if len(parts) != len(rows):
            failed_crops += 1
            continue
        decoded_crops += 1
        target = valid_rows if idx % _VALID_STRIDE == _VALID_STRIDE - 1 else train_rows
        for part, label in zip(parts, rows):
            target.append((to_model_input(part), label))

    # Bardzo mały zbiór: wszystko do treningu, walidacja idzie na syntetyku.
    if train_rows and len(valid_rows) * 4 > len(train_rows):
        train_rows.extend(valid_rows)
        valid_rows = []

    stats = {
        "review_gate": review_gate,
        "images_seen": images_seen,
        "images_approved": images_approved,
        "annotations_seen": total_annotations,
        "labelled_crops": len(candidates),
        "decoded_crops": decoded_crops,
        "failed_crops": failed_crops,
        "rows_train": len(train_rows),
        "rows_valid": len(valid_rows),
        "skipped_unapproved": skipped_unapproved,
        "skipped_wrong_class": skipped_wrong_class,
        "skipped_no_attr": skipped_no_attr,
        "skipped_bad_label": skipped_bad_label,
    }
    if not candidates and review_gate and images_approved == 0:
        raise ValueError(
            f"brak zatwierdzonych obrazów (0 z {images_seen}) — zatwierdź adnotacje "
            'przyciskiem „Zapisz i zatwierdź" w edytorze'
        )
    return {"train": train_rows, "valid": valid_rows, "stats": stats}


def _greedy_decode(logits) -> list[str]:
    """CTC greedy decode [B,T,C] → napisy (blank=0, kompresja powtórzeń) — ta sama
    reguła co w runtime."""
    idx = logits.argmax(-1).cpu().numpy()
    out = []
    for row in idx:
        prev = 0
        chars = []
        for v in row:
            if v != 0 and v != prev:
                chars.append(ALPHABET[v - 1])
            prev = v
        out.append("".join(chars))
    return out


def _train_worker(req: TrainRequest, job_id: str) -> None:
    import numpy as np
    import torch
    import torch.nn as nn
    from torch.utils.data import DataLoader, Dataset

    from model import CRNN

    char2idx = {c: i + 1 for i, c in enumerate(ALPHABET)}
    output_dir = ""
    try:
        output_dir = _sanitize_output_dir(req.output_dir)
        os.makedirs(output_dir, exist_ok=True)
        metrics_csv = os.path.join(output_dir, "metrics.csv")
        hp = req.hyperparams

        gen_synth.set_catalogue([(p.kemler, p.un) for p in req.adr_pairs])

        _update(job_id, stage="budowa wierszy")
        collected = _collect_real_rows(req, job_id)
        real_train = collected["train"]
        real_valid = collected["valid"]
        stats = collected["stats"]
        with open(os.path.join(output_dir, "row_stats.json"), "w", encoding="utf-8") as f:
            json.dump(stats, f, ensure_ascii=False, indent=2)
        print(f"[ocr] job={job_id} wiersze realne: {stats}", flush=True)

        if not real_train and hp.synthetic_per_epoch == 0:
            raise RuntimeError(
                "brak realnych wierszy do treningu i wyłączone dane syntetyczne — "
                f"sprawdź atrybut '{req.attribute}' i klasę '{req.source_class}' "
                "(wartość musi mieć format <kemler>/<UN>, same cyfry)"
            )

        class SynthDataset(Dataset):
            """Próbki generowane W LOCIE; długość ogranicza tylko jedną epokę."""

            def __init__(self, length: int) -> None:
                self.length = length

            def __len__(self) -> int:
                return self.length

            def __getitem__(self, _idx: int):
                gray, text = gen_synth.make_sample()
                return gray, text

        class RealRowDataset(Dataset):
            """Realne wiersze, powtórzone `repeat` razy, z tą samą augmentacją co
            syntetyk — kilkaset ręcznych etykiet inaczej zostaje zapamiętane."""

            def __init__(self, rows, repeat: int, augment: bool) -> None:
                self.rows = rows
                self.repeat = max(1, repeat)
                self.augment = augment

            def __len__(self) -> int:
                return len(self.rows) * self.repeat

            def __getitem__(self, idx: int):
                gray, text = self.rows[idx % len(self.rows)]
                if self.augment:
                    rgb = np.repeat(gray[:, :, None], 3, axis=2)
                    rgb = gen_synth.augment(rgb)
                    gray = rgb[:, :, 0] if rgb.ndim == 3 else rgb
                    gray = to_model_input(gray)
                return gray, text

        def collate(batch):
            xs = []
            targets = []
            tlens = []
            texts = []
            for gray, text in batch:
                x = torch.from_numpy(np.ascontiguousarray(gray)).float()
                x = x.div_(255.0).sub_(0.5).div_(0.5).unsqueeze(0)
                xs.append(x)
                targets.append(torch.tensor([char2idx[c] for c in text], dtype=torch.long))
                tlens.append(len(text))
                texts.append(text)
            return (
                torch.stack(xs, 0),
                torch.cat(targets),
                torch.tensor(tlens, dtype=torch.long),
                texts,
            )

        def worker_init(wid: int) -> None:
            seed = (torch.initial_seed() % 2**31) + wid
            random.seed(seed)
            np.random.seed(seed % 2**31)

        train_parts = []
        if hp.synthetic_per_epoch > 0:
            if not gen_synth.discover_fonts():
                raise RuntimeError(
                    "brak czcionek TrueType na tym węźle — dane syntetyczne nie mogą "
                    "powstać; zainstaluj np. dejavu-fonts/liberation-fonts albo ustaw "
                    "liczbę próbek syntetycznych na 0"
                )
            train_parts.append(SynthDataset(hp.synthetic_per_epoch))
        if real_train:
            train_parts.append(RealRowDataset(real_train, hp.real_repeat, augment=True))
        train_ds = train_parts[0] if len(train_parts) == 1 else torch.utils.data.ConcatDataset(train_parts)

        device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
        use_amp = device.type == "cuda"
        model = CRNN().to(device)

        workers = min(8, max(0, (os.cpu_count() or 2) - 1))
        train_loader = DataLoader(
            train_ds, batch_size=hp.batch_size, shuffle=True, num_workers=workers,
            collate_fn=collate, worker_init_fn=worker_init, drop_last=True,
            pin_memory=use_amp, persistent_workers=workers > 0,
        )
        if len(train_loader) == 0:
            raise RuntimeError(
                "zbiór treningowy mniejszy niż jeden batch — zmniejsz rozmiar batcha "
                "albo dodaj dane syntetyczne"
            )
        real_valid_loader = (
            DataLoader(
                RealRowDataset(real_valid, 1, augment=False),
                batch_size=hp.batch_size, shuffle=False, num_workers=0, collate_fn=collate,
            )
            if real_valid
            else None
        )
        synth_valid_loader = (
            DataLoader(
                SynthDataset(max(hp.batch_size, 2000)),
                batch_size=hp.batch_size, shuffle=False, num_workers=min(4, workers),
                collate_fn=collate, worker_init_fn=worker_init,
            )
            if hp.synthetic_per_epoch > 0
            else None
        )

        ctc = nn.CTCLoss(blank=0, zero_infinity=True)
        opt = torch.optim.AdamW(model.parameters(), lr=hp.learning_rate, weight_decay=1e-4)
        steps_per_epoch = len(train_loader)
        sched = torch.optim.lr_scheduler.OneCycleLR(
            opt, max_lr=hp.learning_rate, total_steps=hp.epochs * steps_per_epoch, pct_start=0.1,
        )
        scaler = torch.amp.GradScaler("cuda", enabled=use_amp)

        @torch.no_grad()
        def exact_match(loader) -> Optional[float]:
            if loader is None:
                return None
            model.eval()
            correct = total = 0
            for xs, _tcat, _tlens, texts in loader:
                xs = xs.to(device, non_blocking=True)
                with torch.autocast("cuda", enabled=use_amp, dtype=torch.float16):
                    logits = model(xs)
                for pred, truth in zip(_greedy_decode(logits.float()), texts):
                    correct += int(pred == truth)
                    total += 1
            return correct / total if total else None

        with open(metrics_csv, "w", newline="", encoding="utf-8") as f:
            csv.writer(f).writerow(["epoch", "train/loss", "val/exact_real", "val/exact_synth"])

        best_metric = -1.0
        best_ckpt = os.path.join(output_dir, "checkpoint_best.pth")
        for epoch in range(1, hp.epochs + 1):
            if _cancel_requested(job_id):
                raise _Cancelled
            _update(job_id, stage="trening")
            model.train()
            running = 0.0
            seen = 0
            for xs, tcat, tlens, _texts in train_loader:
                if _cancel_requested(job_id):
                    raise _Cancelled
                xs = xs.to(device, non_blocking=True)
                tcat = tcat.to(device)
                with torch.autocast("cuda", enabled=use_amp, dtype=torch.float16):
                    logits = model(xs)
                    logp = logits.log_softmax(-1).permute(1, 0, 2)  # [T,B,C]
                    in_lens = torch.full(
                        (xs.size(0),), logp.size(0), dtype=torch.long, device=device,
                    )
                    loss = ctc(logp, tcat, in_lens, tlens.to(device))
                opt.zero_grad(set_to_none=True)
                scaler.scale(loss).backward()
                scaler.unscale_(opt)
                nn.utils.clip_grad_norm_(model.parameters(), 5.0)
                scaler.step(opt)
                scaler.update()
                sched.step()
                running += loss.item() * xs.size(0)
                seen += xs.size(0)
            train_loss = running / seen if seen else 0.0

            _update(job_id, stage="ewaluacja")
            val_real = exact_match(real_valid_loader)
            val_synth = exact_match(synth_valid_loader)

            with open(metrics_csv, "a", newline="", encoding="utf-8") as f:
                csv.writer(f).writerow([
                    epoch,
                    f"{train_loss:.6f}",
                    "" if val_real is None else f"{val_real:.6f}",
                    "" if val_synth is None else f"{val_synth:.6f}",
                ])
            _update(
                job_id, epoch=epoch, train_loss=train_loss,
                val_exact_real=val_real, val_exact_synth=val_synth,
            )

            # Wybór najlepszego checkpointu: realne wiersze mają pierwszeństwo, bo
            # to one mówią o jakości na zdjęciach; syntetyk jest fallbackiem tylko
            # gdy realnego splitu walidacyjnego nie ma z czego zrobić.
            metric = val_real if val_real is not None else (val_synth or 0.0)
            if metric >= best_metric:
                best_metric = metric
                # Checkpoint zawiera WYŁĄCZNIE state_dict (tensory) — bezpieczny do
                # load przez weights_only=True. Metadane obok, w sidecar-JSON.
                torch.save({"model_state": model.state_dict()}, best_ckpt)
                with open(_checkpoint_meta_path(best_ckpt), "w", encoding="utf-8") as f:
                    json.dump(
                        {
                            "alphabet": ALPHABET,
                            "img_h": IMG_H,
                            "img_w": IMG_W,
                            "epoch": epoch,
                            "val_exact_real": val_real,
                            "val_exact_synth": val_synth,
                            "attribute": req.attribute,
                            "source_class": req.source_class,
                            "rows_train": stats["rows_train"],
                            "rows_valid": stats["rows_valid"],
                        },
                        f,
                        ensure_ascii=False,
                    )

        _update(
            job_id, status="succeeded",
            artifact_path=best_ckpt if os.path.exists(best_ckpt) else output_dir,
        )
    except _Cancelled:
        _update(job_id, status="cancelled", stage="anulowany")
    except Exception as exc:  # noqa: BLE001
        _update(
            job_id, status="failed",
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
        _TRAIN_SLOT.release()


def _checkpoint_meta_path(checkpoint_path: str) -> str:
    return os.path.splitext(checkpoint_path)[0] + ".meta.json"


@app.get("/health")
def health() -> dict[str, Any]:
    try:
        import torch

        cuda, gpus = torch.cuda.is_available(), torch.cuda.device_count()
    except Exception:  # noqa: BLE001
        cuda, gpus = False, 0
    return {
        "status": "ok",
        "cuda": cuda,
        "gpus": gpus,
        "fonts": len(gen_synth.discover_fonts()),
    }


@app.post("/train")
def train(req: TrainRequest) -> dict[str, Any]:
    if not os.path.isdir(req.dataset_dir):
        raise HTTPException(400, f"dataset_dir not found: {req.dataset_dir}")
    if not _SAFE_NAME_RE.match(req.attribute or ""):
        raise HTTPException(400, f"invalid attribute: {req.attribute!r}")
    if req.source_class and not _SAFE_NAME_RE.match(req.source_class):
        raise HTTPException(400, f"invalid source_class: {req.source_class!r}")
    try:
        _sanitize_output_dir(req.output_dir)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc
    # Brak czcionek wyklucza dane syntetyczne — mówimy o tym PRZED zajęciem slotu,
    # zamiast wywalać joba po minucie zbierania wierszy.
    if req.hyperparams.synthetic_per_epoch > 0 and not gen_synth.discover_fonts():
        raise HTTPException(
            400,
            "brak czcionek TrueType na tym węźle — zainstaluj np. dejavu-fonts albo "
            "ustaw liczbę próbek syntetycznych na 0",
        )

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


@app.post("/cancel/{job_id}")
def cancel(job_id: str) -> dict[str, Any]:
    """Podnosi flagę anulowania; pętla treningu (i budowa wierszy) kończy job jako
    `cancelled` przy najbliższym batchu. Job już zakończony wraca z aktualnym
    statusem i `cancelled: false` — anulowanie nie jest błędem wołającego."""
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            raise HTTPException(404, "job not found")
        if st.status != "running":
            return {"job_id": job_id, "status": st.status, "cancelled": False}
        st.cancel_requested = True
        return {"job_id": job_id, "status": st.status, "cancelled": True}


@app.post("/export")
def export(req: ExportRequest) -> dict[str, Any]:
    """Checkpoint torcha → ONNX opset 17 (dynamiczny batch) + `adr_ocr_alphabet.txt`
    obok. Nazwy plików są kontraktem runtime'u (`vision/adr_ocr.rs` szuka
    `adr_ocr.onnx` i `adr_ocr_alphabet.txt` w katalogu modeli wizji)."""
    import numpy as np
    import torch

    from model import CRNN

    if not os.path.exists(req.checkpoint_path):
        raise HTTPException(400, f"checkpoint not found: {req.checkpoint_path}")
    try:
        out_dir = _sanitize_output_dir(req.output_dir)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc
    if not _EXPORT_SLOT.acquire(blocking=False):
        raise HTTPException(409, "another export is running")
    try:
        os.makedirs(out_dir, exist_ok=True)
        ckpt = torch.load(req.checkpoint_path, map_location="cpu", weights_only=True)
        state = ckpt["model_state"] if isinstance(ckpt, dict) and "model_state" in ckpt else ckpt
        model = CRNN()
        model.load_state_dict(state)
        model.eval()

        onnx_path = os.path.join(out_dir, "adr_ocr.onnx")
        dummy = torch.randn(1, 1, IMG_H, IMG_W)
        torch.onnx.export(
            model, dummy, onnx_path,
            input_names=["input"], output_names=["logits"],
            dynamic_axes={"input": {0: "batch"}, "logits": {0: "batch"}},
            opset_version=17, do_constant_folding=True, dynamo=False,
        )
        alphabet_path = os.path.join(out_dir, "adr_ocr_alphabet.txt")
        with open(alphabet_path, "w", encoding="utf-8") as f:
            f.write(ALPHABET + "\n")

        # Kontrola liczbowa torch vs onnxruntime — eksport, który rozjeżdża się z
        # modelem, jest gorszy od braku eksportu (runtime czytałby cicho bzdury).
        import onnxruntime as ort

        sess = ort.InferenceSession(onnx_path, providers=["CPUExecutionProvider"])
        x = torch.randn(3, 1, IMG_H, IMG_W)
        with torch.no_grad():
            y_torch = model(x).numpy()
        y_onnx = sess.run(None, {"input": x.numpy()})[0]
        max_diff = float(np.abs(y_torch - y_onnx).max())
        if max_diff > 1e-3:
            raise RuntimeError(f"eksport ONNX rozjeżdża się z modelem (max diff {max_diff})")

        return {
            "onnx_path": onnx_path,
            "alphabet_path": alphabet_path,
            "size_bytes": os.path.getsize(onnx_path),
            "max_abs_diff": max_diff,
        }
    except HTTPException:
        raise
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(500, f"export failed: {type(exc).__name__}: {exc}") from exc
    finally:
        _EXPORT_SLOT.release()
