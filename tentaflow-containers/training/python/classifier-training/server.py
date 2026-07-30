# =============================================================================
# Plik: server.py
# Opis: Realny serwer treningowy klasyfikatora obrazu (timm). FastAPI wystawia
#       /train (COCO dataset_dir → wycinki bbox → fine-tuning timm), /status/{job_id},
#       /export (→ ONNX opset 17), /predict (pojedynczy wycinek), /health. Trening
#       biegnie w tle na osobnym wątku; postęp (epoka, train loss, val_acc,
#       val_macro_f1) czytany ze statusu w pamięci i z metrics.csv (polling).
# Przykład: POST /train {"dataset_dir":"/data/coco","attribute":"stan",
#           "source_class":"","values":["czysta","brudna","uszkodzona","nieczytelna"],
#           "output_dir":"recog/proj/run","variant":"efficientnet_b0",
#           "hyperparams":{"epochs":30,...}}
# =============================================================================

from __future__ import annotations

import csv
import gc
import json
import os
import re
import shutil
import threading
import time
import traceback
import uuid
from dataclasses import dataclass, field
from typing import Any, Optional

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

app = FastAPI(title="TentaFlow Classifier Training", version="1.0.0")

# Artefakty (checkpointy, ONNX) lądują pod jednym zaufanym katalogiem; dowolny
# `output_dir` z requestu sprowadzamy do podkatalogu (odcięcie path traversal).
# Docker montuje ARTIFACTS_ROOT=/out; deploy natywny → katalog w HOME (zapis.).
_DEFAULT_ARTIFACTS_ROOT = os.path.join(os.path.expanduser("~"), ".tentaflow", "classifier-out")
ARTIFACTS_ROOT = os.path.realpath(os.environ.get("ARTIFACTS_ROOT") or _DEFAULT_ARTIFACTS_ROOT)
os.makedirs(ARTIFACTS_ROOT, exist_ok=True)

# Wycinki bbox (ImageFolder) budowane per job lądują pod osobnym cache — duże,
# regenerowalne, więc trzymane poza katalogiem artefaktów.
_DEFAULT_CACHE_ROOT = os.path.join(os.path.expanduser("~"), ".tentaflow", "classifier-cache")
CACHE_ROOT = os.path.realpath(os.environ.get("CACHE_ROOT") or _DEFAULT_CACHE_ROOT)
os.makedirs(CACHE_ROOT, exist_ok=True)

# Warianty klasyfikatora → nazwa modelu timm. Wagi pretrained pobierane z tagu;
# gdy tag niedostępny offline, degradujemy do wariantu bez tagu (patrz _create_model).
_TIMM_MAP = {
    "mobilenetv4": "mobilenetv4_conv_small.e2400_r224_in1k",
    "efficientnet_b0": "efficientnet_b0.ra_in1k",
    "resnet50": "resnet50.a1_in1k",
}
# Fallback bez tagu wag (gdy pretrained z tagu nie da się pobrać offline).
_TIMM_FALLBACK = {
    "mobilenetv4": "mobilenetv4_conv_small",
    "efficientnet_b0": "efficientnet_b0",
    "resnet50": "resnet50",
}

# Limity zasobów (ochrona przed DoS): odrzucamy zbyt duże pliki COCO i zbyt liczne
# adnotacje, a łączną liczbę cropów per job ograniczamy do rozsądnego pułapu.
_MAX_COCO_BYTES = 200 * 1024 * 1024
_MAX_ANNOTATIONS = 2_000_000
_MAX_CROPS = 200_000
# Górny limit pikseli obrazu (obrona przed decompression bomb w PIL). Wartość ~178 Mpx
# odpowiada domyślnemu progowi ostrzeżenia PIL; przekroczenie → obraz pomijany.
_MAX_IMAGE_PIXELS = 178_956_970
# Nazwa atrybutu i wartości trafiają do metadanych i logów; treść walidowana też po
# stronie Rust — tu drugi bezpiecznik przed nietypowymi znakami.
_SAFE_NAME_RE = re.compile(r"^[A-Za-z0-9_.\- ]{1,64}$")

_JOBS: dict[str, "JobState"] = {}
_JOBS_LOCK = threading.Lock()
# Jeden trening naraz — model wysyca GPU; równoległe joby = OOM.
_TRAIN_SLOT = threading.Semaphore(1)
# Eksport ONNX jest ciężki (buduje model, alokuje GPU) — dopuszczamy jeden naraz.
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
    status: str = "running"  # running | succeeded | failed | cancelled
    epoch: int = 0
    total_epochs: int = 0
    train_loss: Optional[float] = None
    val_acc: Optional[float] = None
    val_macro_f1: Optional[float] = None
    # F1 per klasa z ostatniej walidacji (`{nazwa: f1}`) — macro samo nie mówi,
    # która klasa ciągnie wynik w dół.
    val_f1_per_class: Optional[dict[str, float]] = None
    error: Optional[str] = None
    artifact_path: Optional[str] = None
    # Etap joba do podglądu na żywo (przygotowanie → budowa cropów → trening → ewaluacja → eksport).
    stage: str = "przygotowanie"
    # Znacznik startu joba (monotoniczny) do liczenia elapsed_s/eta_s.
    start_time: float = field(default_factory=time.monotonic)
    # Żądanie anulowania złożone przez Core (`POST /cancel/{job_id}`). Pętla treningu
    # i budowa cropów sprawdzają je kooperacyjnie i kończą job jako `cancelled`.
    cancel_requested: bool = False

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
            "val_acc": self.val_acc,
            "val_macro_f1": self.val_macro_f1,
            "val_f1_per_class": self.val_f1_per_class,
            "error": self.error,
            "artifact_path": self.artifact_path,
            "gpu_mem_mb": _gpu_mem_mb(),
            "elapsed_s": elapsed,
            "eta_s": eta,
            "stage": self.stage,
        }


class Hyperparams(BaseModel):
    epochs: int = 30
    batch_size: int = 32
    learning_rate: float = 1e-3
    image_size: int = 224
    freeze_backbone: bool = False


class TrainRequest(BaseModel):
    job_id: Optional[str] = None
    # Ścieżka do katalogu COCO (train/valid + _annotations.coco.json w podkatalogach),
    # przygotowana przez Core na tym samym węźle co serwis.
    dataset_dir: str
    # Nazwa atrybutu adnotacji COCO (pole `attributes[attribute]`), np. "stan".
    attribute: str
    # Nazwa kategorii, z której bierzemy wycinki. Pusty string = dowolna kategoria.
    source_class: str = ""
    # Klasy wyjściowe klasyfikatora w KOLEJNOŚCI (indeksy = kolejność w tej liście).
    values: list[str] = Field(min_length=2)
    variant: str = "efficientnet_b0"  # mobilenetv4|efficientnet_b0|resnet50
    output_dir: str
    hyperparams: Hyperparams = Field(default_factory=Hyperparams)


class ExportRequest(BaseModel):
    checkpoint_path: str
    output_dir: str
    variant: str = "efficientnet_b0"
    values: list[str] = Field(min_length=2)
    image_size: int = 224


class PredictRequest(BaseModel):
    image_b64: str
    variant: str = "efficientnet_b0"
    checkpoint_path: str


# Cache załadowanych modeli klasyfikacji per (checkpoint, variant) — ładowanie wag
# z dysku przy każdej konstrukcji jest kosztowne, trzymamy je między /predict.
_PREDICT_MODELS: dict[str, Any] = {}
_PREDICT_LOCK = threading.Lock()


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


def _sanitize_checkpoint_path(checkpoint_path: str) -> str:
    """Sprowadza ścieżkę checkpointu z requestu pod zaufany root artefaktów. Odrzuca
    dowolną ścieżkę spoza ARTIFACTS_ROOT (obrona przed wczytaniem obcego pliku)."""
    if not checkpoint_path or not checkpoint_path.strip():
        raise ValueError("checkpoint_path must be a non-empty string")
    candidate = os.path.realpath(checkpoint_path)
    root_prefix = ARTIFACTS_ROOT + os.sep
    if candidate != ARTIFACTS_ROOT and not candidate.startswith(root_prefix):
        raise ValueError(f"checkpoint_path escapes ARTIFACTS_ROOT ({ARTIFACTS_ROOT})")
    return candidate


def _checkpoint_meta_path(checkpoint_path: str) -> str:
    """Ścieżka do sidecar-JSON z metadanymi checkpointu (obok pliku .pth)."""
    return os.path.splitext(checkpoint_path)[0] + ".json"


def _read_checkpoint_meta(checkpoint_path: str) -> dict[str, Any]:
    """Czyta metadane checkpointu (variant, values, image_size) z sidecar-JSON.
    Metadane trzymamy poza pickle, więc load wag może iść przez weights_only=True."""
    meta_path = _checkpoint_meta_path(checkpoint_path)
    if not os.path.isfile(meta_path):
        raise FileNotFoundError(f"brak metadanych checkpointu: {meta_path}")
    with open(meta_path, encoding="utf-8") as f:
        return json.load(f)


def _validate_name(name: str, field: str) -> str:
    """Waliduje treść nazwy atrybutu/wartości: whitelist znaków, bez separatorów
    ścieżek i `.`/`..` (drugi bezpiecznik obok walidacji w Core)."""
    stripped = (name or "").strip()
    if stripped in ("", ".", ".."):
        raise ValueError(f"{field} nie może być puste ani '.'/'..'")
    if not _SAFE_NAME_RE.match(name):
        raise ValueError(f"{field} zawiera niedozwolone znaki: {name!r}")
    return name


def _update(job_id: str, **changes: Any) -> None:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        if st is None:
            return
        for key, value in changes.items():
            setattr(st, key, value)


def _resolve_timm_name(variant: str) -> tuple[str, str]:
    key = variant.lower()
    if key not in _TIMM_MAP:
        raise ValueError(f"unknown variant: {variant} (allowed: {list(_TIMM_MAP)})")
    return _TIMM_MAP[key], _TIMM_FALLBACK[key]


def _create_model(variant: str, num_classes: int, pretrained: bool = True):  # noqa: ANN201
    """Tworzy model timm dla wariantu. Gdy wagi z tagu (np. `.ra_in1k`) są
    niedostępne offline, degraduje do wariantu bez tagu z pretrained=True, a w
    ostateczności do pretrained=False (trening od zera)."""
    import timm

    tagged, fallback = _resolve_timm_name(variant)
    if not pretrained:
        return timm.create_model(fallback, pretrained=False, num_classes=num_classes)
    try:
        return timm.create_model(tagged, pretrained=True, num_classes=num_classes)
    except Exception:  # noqa: BLE001
        try:
            return timm.create_model(fallback, pretrained=True, num_classes=num_classes)
        except Exception:  # noqa: BLE001
            return timm.create_model(fallback, pretrained=False, num_classes=num_classes)


def _iter_coco_splits(dataset_dir: str):
    """Zwraca (images_dir, coco_json) dla każdego podkatalogu z _annotations.coco.json
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


class _Cancelled(Exception):
    """Trening przerwany na żądanie Core (`POST /cancel/{job_id}`)."""


def _cancel_requested(job_id: str) -> bool:
    with _JOBS_LOCK:
        st = _JOBS.get(job_id)
        return bool(st and st.cancel_requested)


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


def _collect_crops(req: TrainRequest, job_id: str) -> dict[str, Any]:
    """Wczytuje wszystkie splity COCO z dataset_dir, bierze tylko obrazy ZATWIERDZONE
    przez człowieka (`approved: true` — nieobejrzane predykcje auto-labela nie mogą
    uczyć modelu na jego własnych wyjściach; dataset, w którym żaden obraz nie ma tego
    pola, nie przeszedł przez nasz edytor i idzie w całości), filtruje adnotacje po
    kategorii
    (source_class lub dowolna) i atrybucie (attributes[attribute] ∈ values), wycina
    bbox z PEŁNEJ rozdzielczości obrazu (PIL) i zapisuje do ImageFolder w cache jako
    train/<indeks-wartości>/<n>.jpg oraz valid/<indeks-wartości>/<n>.jpg (split
    stratyfikowany ~15%). Nazwy folderów to INDEKSY z kolejności `values` — nazwa
    wartości nigdy nie trafia do ścieżki (obrona przed path traversal). Mapowanie
    indeks→nazwa zapisujemy w data_root/classes.json. Zwraca statystyki liczności
    per klasa i ścieżkę data_root."""
    from PIL import Image

    # Obrona przed decompression bomb — obrazy powyżej progu odrzucamy przy otwarciu.
    Image.MAX_IMAGE_PIXELS = _MAX_IMAGE_PIXELS

    value_index = {v: i for i, v in enumerate(req.values)}
    source_class = (req.source_class or "").strip()

    # Zbieramy kandydatów jako (image_path, bbox, value) — split robimy dopiero po
    # zebraniu całości (stratyfikacja per klasa niezależna od źródłowego splitu).
    crops: dict[str, list[tuple[str, tuple[int, int, int, int]]]] = {v: [] for v in req.values}
    counts_raw: dict[str, int] = {v: 0 for v in req.values}
    skipped_no_attr = 0
    skipped_bad_value = 0
    skipped_wrong_class = 0
    skipped_unapproved = 0
    total_annotations = 0
    total_candidates = 0
    images_seen = 0
    images_approved = 0

    review_gate = _review_gate_active(req.dataset_dir)

    for images_dir, coco_json in _iter_coco_splits(req.dataset_dir):
        size = os.path.getsize(coco_json)
        if size > _MAX_COCO_BYTES:
            raise ValueError(
                f"plik COCO za duży ({size} B > {_MAX_COCO_BYTES} B): {coco_json}"
            )
        with open(coco_json, encoding="utf-8") as f:
            coco = json.load(f)
        cat_name = {c["id"]: c["name"] for c in coco.get("categories", [])}
        split_images = coco.get("images", [])
        images_seen += len(split_images)
        # Bramka zatwierdzenia: adnotacje z obrazów nieobejrzanych przez człowieka
        # w ogóle nie wchodzą do puli cropów.
        img_meta = {
            im["id"]: im
            for im in split_images
            if not review_gate or im.get("approved") is True
        }
        images_approved += len(img_meta)
        annotations = coco.get("annotations", [])
        total_annotations += len(annotations)
        if total_annotations > _MAX_ANNOTATIONS:
            raise ValueError(
                f"za dużo adnotacji ({total_annotations} > {_MAX_ANNOTATIONS})"
            )
        for ann in annotations:
            if total_candidates >= _MAX_CROPS:
                print(
                    f"[collect_crops] job={job_id} osiągnięto limit cropów "
                    f"({_MAX_CROPS}) — pomijam pozostałe adnotacje",
                    flush=True,
                )
                break
            im = img_meta.get(ann.get("image_id"))
            if im is None:
                skipped_unapproved += 1
                continue
            cid = ann.get("category_id")
            cname = cat_name.get(cid, "")
            if source_class and cname != source_class:
                skipped_wrong_class += 1
                continue
            attrs = ann.get("attributes") or {}
            if req.attribute not in attrs:
                skipped_no_attr += 1
                continue
            value = attrs.get(req.attribute)
            if value not in value_index:
                skipped_bad_value += 1
                continue
            bbox = ann.get("bbox")
            if not bbox or len(bbox) != 4:
                continue
            # Tylko nazwa pliku — odcięcie komponentów ścieżki (path traversal).
            file_name = os.path.basename(im["file_name"])
            path = os.path.join(images_dir, file_name)
            if os.path.realpath(path) != os.path.join(os.path.realpath(images_dir), file_name):
                continue
            crops[value].append((path, tuple(int(round(v)) for v in bbox)))
            counts_raw[value] += 1
            total_candidates += 1
        else:
            continue
        break

    # Split stratyfikowany, deterministyczny (~15% do valid): sortujemy kandydatów
    # stabilnie i co ~7-my rekord idzie do walidacji. Foldery nazywamy INDEKSEM
    # wartości; puste klasy (0 próbek) pomijamy, by ImageFolder ich nie odrzucił.
    data_root = os.path.join(CACHE_ROOT, job_id)
    counts_split: dict[str, dict[str, int]] = {}
    empty_values: list[str] = []
    for value in req.values:
        idx = value_index[value]
        items = sorted(crops[value])
        n = len(items)
        if n == 0:
            counts_split[value] = {"train": 0, "valid": 0}
            empty_values.append(value)
            continue
        n_val = max(1, round(0.15 * n))
        # deterministyczny wybór indeksów walidacyjnych rozłożonych równomiernie
        stride = n / n_val
        val_idx: set[int] = {int(i * stride) for i in range(n_val)}
        tr_dir = os.path.join(data_root, "train", str(idx))
        va_dir = os.path.join(data_root, "valid", str(idx))
        os.makedirs(tr_dir, exist_ok=True)
        os.makedirs(va_dir, exist_ok=True)
        n_tr = n_va = 0
        for i, (path, bbox) in enumerate(items):
            if i % 200 == 0 and _cancel_requested(job_id):
                raise _Cancelled
            x, y, w, h = bbox
            try:
                with Image.open(path) as img:
                    img = img.convert("RGB")
                    x1 = max(0, x)
                    y1 = max(0, y)
                    x2 = min(img.width, x + max(1, w))
                    y2 = min(img.height, y + max(1, h))
                    if x2 <= x1 or y2 <= y1:
                        continue
                    crop = img.crop((x1, y1, x2, y2))
            except Exception:  # noqa: BLE001
                continue
            if i in val_idx:
                crop.save(os.path.join(va_dir, f"{n_va}.jpg"), quality=92)
                n_va += 1
            else:
                crop.save(os.path.join(tr_dir, f"{n_tr}.jpg"), quality=92)
                n_tr += 1
        counts_split[value] = {"train": n_tr, "valid": n_va}

    if empty_values:
        print(
            f"[collect_crops] job={job_id} klasy bez próbek (pominięte foldery, "
            f"zachowane w num_classes): {empty_values}",
            flush=True,
        )

    # Mapowanie indeks→nazwa (kolejność = kolejność `values`) obok datasetu — używane
    # przy remapie/eksporcie zamiast nazw w ścieżkach.
    os.makedirs(data_root, exist_ok=True)
    with open(os.path.join(data_root, "classes.json"), "w", encoding="utf-8") as f:
        json.dump({"classes": req.values}, f, ensure_ascii=False)

    if total_candidates == 0 and review_gate and images_approved == 0:
        raise ValueError(
            f"brak zatwierdzonych obrazów (0 z {images_seen}) — zatwierdź adnotacje "
            'przyciskiem „Zapisz i zatwierdź" w edytorze'
        )

    return {
        "data_root": data_root,
        "counts_raw": counts_raw,
        "counts_split": counts_split,
        "skipped_no_attr": skipped_no_attr,
        "skipped_bad_value": skipped_bad_value,
        "skipped_wrong_class": skipped_wrong_class,
        "skipped_unapproved": skipped_unapproved,
        "review_gate": review_gate,
        "images_seen": images_seen,
        "images_approved": images_approved,
    }


def _build_transforms(image_size: int):  # noqa: ANN201
    from torchvision import transforms

    mean = [0.485, 0.456, 0.406]
    std = [0.229, 0.224, 0.225]
    train_tf = transforms.Compose([
        transforms.RandomResizedCrop(image_size, scale=(0.7, 1.0)),
        transforms.RandomHorizontalFlip(),
        transforms.ColorJitter(brightness=0.2, contrast=0.2, saturation=0.2),
        transforms.ToTensor(),
        transforms.Normalize(mean, std),
    ])
    val_tf = transforms.Compose([
        transforms.Resize(int(image_size * 1.15)),
        transforms.CenterCrop(image_size),
        transforms.ToTensor(),
        transforms.Normalize(mean, std),
    ])
    return train_tf, val_tf


def _macro_f1(confusion: list[list[int]], num_classes: int) -> tuple[float, float, list[float]]:
    """Liczy accuracy, macro-F1 i F1 PER KLASA z macierzy pomyłek [true][pred].
    Per-klasowe F1 wychodzi na zewnątrz, bo samo macro nie mówi, KTÓRA klasa
    zawodzi — a przy silnie niezbalansowanym zbiorze to jedyna informacja, która
    kieruje następnym krokiem (dosypać zdjęć konkretnego stanu)."""
    total = sum(sum(r) for r in confusion)
    correct = sum(confusion[i][i] for i in range(num_classes))
    acc = correct / total if total else 0.0
    f1s = []
    for c in range(num_classes):
        tp = confusion[c][c]
        fp = sum(confusion[r][c] for r in range(num_classes)) - tp
        fn = sum(confusion[c][r] for r in range(num_classes)) - tp
        prec = tp / (tp + fp) if (tp + fp) else 0.0
        rec = tp / (tp + fn) if (tp + fn) else 0.0
        f1 = 2 * prec * rec / (prec + rec) if (prec + rec) else 0.0
        f1s.append(f1)
    macro_f1 = sum(f1s) / num_classes if num_classes else 0.0
    return acc, macro_f1, f1s


def _train_worker(req: TrainRequest, job_id: str) -> None:
    import torch
    from torch import nn
    from torch.utils.data import DataLoader
    from torchvision.datasets import ImageFolder

    output_dir = _sanitize_output_dir(req.output_dir)
    metrics_csv = os.path.join(output_dir, "metrics.csv")
    try:
        os.makedirs(output_dir, exist_ok=True)
        hp = req.hyperparams

        _update(job_id, stage="budowa cropów")
        stats = _collect_crops(req, job_id)
        data_root = stats["data_root"]
        with open(os.path.join(output_dir, "crop_stats.json"), "w", encoding="utf-8") as f:
            json.dump(stats, f, ensure_ascii=False, indent=2)

        train_tf, val_tf = _build_transforms(hp.image_size)
        train_ds = ImageFolder(os.path.join(data_root, "train"), transform=train_tf)
        valid_ds = ImageFolder(os.path.join(data_root, "valid"), transform=val_tf)
        if len(train_ds) == 0:
            raise RuntimeError("brak wycinków treningowych (sprawdź attribute/source_class/values)")

        # Foldery nazwane są INDEKSEM wartości (str(i)); ImageFolder sortuje je
        # alfabetycznie, więc budujemy remap: indeks ImageFolder → indeks w req.values,
        # czytając indeks wprost z nazwy foldera.
        folder_classes = train_ds.classes
        remap = torch.tensor(
            [int(c) if c.isdigit() and int(c) < len(req.values) else -1 for c in folder_classes],
            dtype=torch.long,
        )

        num_classes = len(req.values)
        device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
        model = _create_model(req.variant, num_classes).to(device)

        if hp.freeze_backbone:
            # Zamrażamy cechy, trenujemy tylko głowę (klasyfikator).
            classifier = model.get_classifier()
            head_params = set(classifier.parameters())
            for p in model.parameters():
                p.requires_grad = p in head_params

        # Wagi klas = odwrotność częstości (łagodzi silne niezbalansowanie, np. brudna).
        counts = [0] * num_classes
        for _, folder_idx in train_ds.samples:
            tgt = int(remap[folder_idx])
            if tgt >= 0:
                counts[tgt] += 1
        total = sum(counts) or 1
        weights = torch.tensor(
            [total / (num_classes * c) if c > 0 else 0.0 for c in counts],
            dtype=torch.float32,
            device=device,
        )
        criterion = nn.CrossEntropyLoss(weight=weights)
        params = [p for p in model.parameters() if p.requires_grad]
        optimizer = torch.optim.AdamW(params, lr=hp.learning_rate, weight_decay=1e-4)
        # Wygaszanie LR cosinusem przez cały przebieg. Bez niego (stałe LR) model
        # dobijał do minimum i przez resztę epok wokół niego skakał: na produkcji
        # macro-F1 oscylowało w paśmie ~0.72-0.90 do samego końca, więc wartość z
        # OSTATNIEJ epoki była loterią, a nie wynikiem. Malejące LR pozwala się w
        # tym minimum ustatkować. `T_max` = liczba epok, krok raz na epokę.
        scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, T_max=hp.epochs)

        pin = device.type == "cuda"
        # Wycinki leżą na dysku jako JPEG, więc każdy batch wymaga dekodowania na
        # CPU. Przy sztywnych 4 workerach większy `batch_size` NIE przyspieszał —
        # wąskim gardłem stawał się dekoder, a karta czekała. Skalujemy do liczby
        # rdzeni (z zapasem jednego na resztę procesu), a `persistent_workers`
        # oszczędza respawn puli na każdej epoce (przy 60+ epokach to realny czas).
        workers = min(8, max(0, (os.cpu_count() or 2) - 1))
        train_loader = DataLoader(
            train_ds, batch_size=hp.batch_size, shuffle=True, num_workers=workers,
            pin_memory=pin, drop_last=False,
            persistent_workers=workers > 0, prefetch_factor=4 if workers > 0 else None,
        )
        val_loader = DataLoader(
            valid_ds, batch_size=hp.batch_size, shuffle=False, num_workers=workers,
            pin_memory=pin, persistent_workers=workers > 0,
        )
        remap = remap.to(device)

        with open(metrics_csv, "w", newline="", encoding="utf-8") as f:
            csv.writer(f).writerow(["epoch", "train/loss", "val/acc", "val/macro_f1"])

        best_f1 = -1.0
        best_ckpt = os.path.join(output_dir, "checkpoint_best.pth")
        for epoch in range(1, hp.epochs + 1):
            if _cancel_requested(job_id):
                raise _Cancelled
            _update(job_id, stage="trening")
            model.train()
            running = 0.0
            seen = 0
            for imgs, folder_targets in train_loader:
                if _cancel_requested(job_id):
                    raise _Cancelled
                imgs = imgs.to(device, non_blocking=True)
                targets = remap[folder_targets.to(device)]
                optimizer.zero_grad()
                out = model(imgs)
                loss = criterion(out, targets)
                loss.backward()
                optimizer.step()
                running += loss.item() * imgs.size(0)
                seen += imgs.size(0)
            train_loss = running / seen if seen else 0.0

            # Walidacja: macierz pomyłek → accuracy + macro-F1.
            _update(job_id, stage="ewaluacja")
            confusion = [[0] * num_classes for _ in range(num_classes)]
            if len(valid_ds) > 0:
                model.eval()
                with torch.no_grad():
                    for imgs, folder_targets in val_loader:
                        imgs = imgs.to(device, non_blocking=True)
                        targets = remap[folder_targets.to(device)]
                        preds = model(imgs).argmax(dim=1)
                        for t, p in zip(targets.tolist(), preds.tolist()):
                            confusion[t][p] += 1
            val_acc, val_macro_f1, per_class_f1 = _macro_f1(confusion, num_classes)

            with open(metrics_csv, "a", newline="", encoding="utf-8") as f:
                csv.writer(f).writerow([
                    epoch, f"{train_loss:.6f}", f"{val_acc:.6f}", f"{val_macro_f1:.6f}",
                ])
            _update(
                job_id, epoch=epoch, train_loss=train_loss,
                val_acc=val_acc, val_macro_f1=val_macro_f1,
                val_f1_per_class=dict(zip(req.values, per_class_f1)),
            )

            scheduler.step()

            if val_macro_f1 >= best_f1:
                best_f1 = val_macro_f1
                # Checkpoint zawiera WYŁĄCZNIE state_dict (tensory) — bezpieczny do
                # load przez weights_only=True. Metadane (variant/values/image_size)
                # trzymamy w sidecar-JSON obok, poza pickle.
                torch.save({"model_state": model.state_dict()}, best_ckpt)
                with open(_checkpoint_meta_path(best_ckpt), "w", encoding="utf-8") as f:
                    json.dump(
                        {
                            "variant": req.variant,
                            "values": req.values,
                            "image_size": hp.image_size,
                            "epoch": epoch,
                            "val_macro_f1": val_macro_f1,
                            "val_acc": val_acc,
                            "val_f1_per_class": dict(zip(req.values, per_class_f1)),
                        },
                        f,
                        ensure_ascii=False,
                    )

        with open(os.path.join(output_dir, "classes.json"), "w", encoding="utf-8") as f:
            json.dump({"classes": req.values, "image_size": hp.image_size}, f, ensure_ascii=False)

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
        # Sprzątanie cache cropów (sukces i porażka) — katalog jest duży i regenerowalny.
        shutil.rmtree(os.path.join(CACHE_ROOT, job_id), ignore_errors=True)
        try:
            import torch

            gc.collect()
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:  # noqa: BLE001
            pass
        _TRAIN_SLOT.release()


def _load_checkpoint_model(checkpoint_path: str, variant: str):  # noqa: ANN201
    import torch

    key = f"{variant}:{checkpoint_path}"
    with _PREDICT_LOCK:
        cached = _PREDICT_MODELS.get(key)
        if cached is not None:
            return cached
        meta = _read_checkpoint_meta(checkpoint_path)
        values = meta["values"]
        image_size = meta.get("image_size", 224)
        ckpt = torch.load(checkpoint_path, map_location="cpu", weights_only=True)
        state = ckpt["model_state"] if isinstance(ckpt, dict) and "model_state" in ckpt else ckpt
        model = _create_model(variant, len(values), pretrained=False)
        model.load_state_dict(state)
        model.eval()
        device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
        model.to(device)
        entry = (model, values, image_size, device)
        _PREDICT_MODELS[key] = entry
        return entry


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
    if req.variant.lower() not in _TIMM_MAP:
        raise HTTPException(400, f"invalid variant: {req.variant}")
    if not os.path.isdir(req.dataset_dir):
        raise HTTPException(400, f"dataset_dir not found: {req.dataset_dir}")
    try:
        _validate_name(req.attribute, "attribute")
        for value in req.values:
            _validate_name(value, "value")
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


@app.post("/cancel/{job_id}")
def cancel(job_id: str) -> dict[str, Any]:
    """Podnosi flagę anulowania; pętla treningu kończy job jako `cancelled` przy
    najbliższym batchu. Job już zakończony wraca z aktualnym statusem i
    `cancelled: false` — anulowanie nie jest błędem wołającego."""
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
    """Eksportuje checkpoint do ONNX (opset 17, dynamic batch). Klasy w kolejności
    `values`. Zwraca {onnx_path}."""
    import torch

    if req.variant.lower() not in _TIMM_MAP:
        raise HTTPException(400, f"invalid variant: {req.variant}")
    try:
        checkpoint_path = _sanitize_checkpoint_path(req.checkpoint_path)
        out_dir = _sanitize_output_dir(req.output_dir)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc
    if not os.path.exists(checkpoint_path):
        raise HTTPException(400, f"checkpoint not found: {req.checkpoint_path}")
    os.makedirs(out_dir, exist_ok=True)

    if not _EXPORT_SLOT.acquire(blocking=False):
        raise HTTPException(409, "another export is running")
    try:
        try:
            meta = _read_checkpoint_meta(checkpoint_path)
        except FileNotFoundError:
            meta = {}
        image_size = meta.get("image_size", req.image_size)
        ckpt = torch.load(checkpoint_path, map_location="cpu", weights_only=True)
        state = ckpt["model_state"] if isinstance(ckpt, dict) and "model_state" in ckpt else ckpt
        model = _create_model(req.variant, len(req.values), pretrained=False)
        model.load_state_dict(state)
        model.eval()
        dummy = torch.randn(1, 3, image_size, image_size)
        onnx_path = os.path.join(out_dir, "model.onnx")
        torch.onnx.export(
            model, dummy, onnx_path,
            input_names=["input"], output_names=["logits"],
            opset_version=17,
            dynamic_axes={"input": {0: "batch"}, "logits": {0: "batch"}},
        )
        with open(os.path.join(out_dir, "classes.json"), "w", encoding="utf-8") as f:
            json.dump({"classes": req.values, "image_size": image_size}, f, ensure_ascii=False)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(500, f"export failed: {type(exc).__name__}: {exc}") from exc
    finally:
        _EXPORT_SLOT.release()
    return {"onnx_path": onnx_path}


@app.post("/predict")
def predict(req: PredictRequest) -> dict[str, Any]:
    """Klasyfikacja pojedynczego wycinka (base64). Zwraca {label, probs}."""
    import base64
    import io as _io

    import torch
    from PIL import Image

    if req.variant.lower() not in _TIMM_MAP:
        raise HTTPException(400, f"invalid variant: {req.variant}")
    try:
        checkpoint_path = _sanitize_checkpoint_path(req.checkpoint_path)
    except ValueError as exc:
        raise HTTPException(400, str(exc)) from exc
    if not os.path.exists(checkpoint_path):
        raise HTTPException(400, f"checkpoint not found: {req.checkpoint_path}")

    Image.MAX_IMAGE_PIXELS = _MAX_IMAGE_PIXELS
    try:
        raw = base64.b64decode(req.image_b64)
        image = Image.open(_io.BytesIO(raw)).convert("RGB")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(400, f"invalid image_b64: {exc}") from exc

    try:
        model, values, image_size, device = _load_checkpoint_model(checkpoint_path, req.variant)
        _, val_tf = _build_transforms(image_size)
        tensor = val_tf(image).unsqueeze(0).to(device)
        with torch.no_grad():
            probs = torch.softmax(model(tensor), dim=1)[0].tolist()
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(500, f"predict failed: {type(exc).__name__}: {exc}") from exc

    best = max(range(len(values)), key=lambda i: probs[i])
    return {
        "label": values[best],
        "probs": {values[i]: round(float(probs[i]), 6) for i in range(len(values))},
    }
