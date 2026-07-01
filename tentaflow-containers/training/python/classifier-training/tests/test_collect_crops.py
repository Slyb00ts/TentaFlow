# =============================================================================
# Plik: tests/test_collect_crops.py
# Opis: Testy jednostkowe przygotowania danych klasyfikatora atrybutu w server.py:
#       wycinanie bbox z pełnej rozdzielczości, stratyfikowany deterministyczny split
#       train/valid, pomijanie adnotacji bez atrybutu / z pustą / złą wartością,
#       remap kolejności klas ImageFolder → `values` oraz wagi klas = odwrotność
#       częstości. Dane wejściowe to SYNTETYCZNY mini-COCO budowany w tmpdir.
# =============================================================================

from __future__ import annotations

import json
import os
import sys
import tempfile

import pytest
from PIL import Image

# server.py przy imporcie tworzy katalogi artefaktów/cache z env — kierujemy je do
# tmp, żeby test nie dotykał HOME.
_TMP_ROOT = tempfile.mkdtemp(prefix="clf-test-")
os.environ.setdefault("ARTIFACTS_ROOT", os.path.join(_TMP_ROOT, "artifacts"))
os.environ.setdefault("CACHE_ROOT", os.path.join(_TMP_ROOT, "cache"))
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import server  # noqa: E402

VALUES = ["czysta", "brudna", "uszkodzona", "nieczytelna"]


def _make_image(path: str, size: tuple[int, int], color: tuple[int, int, int]) -> None:
    Image.new("RGB", size, color).save(path)


def _write_coco(split_dir: str, images: list[dict], annotations: list[dict], categories: list[dict]) -> None:
    os.makedirs(split_dir, exist_ok=True)
    with open(os.path.join(split_dir, "_annotations.coco.json"), "w", encoding="utf-8") as f:
        json.dump({"images": images, "annotations": annotations, "categories": categories}, f)


@pytest.fixture()
def dataset(tmp_path, monkeypatch):
    """Buduje mini-COCO z jednym splitem `train/`. Zwraca (dataset_dir, req_factory)
    oraz kieruje CACHE_ROOT serwera na izolowany katalog per test."""
    cache = tmp_path / "cache"
    cache.mkdir()
    monkeypatch.setattr(server, "CACHE_ROOT", str(cache))

    ds = tmp_path / "coco"
    split = ds / "train"
    split.mkdir(parents=True)

    # Dwa obrazy 100x80. Na img1 wstawiamy czerwony prostokąt w bbox, by zweryfikować
    # że wycinamy właściwy fragment.
    img1 = split / "img1.jpg"
    base = Image.new("RGB", (100, 80), (10, 10, 10))
    for x in range(20, 50):
        for y in range(15, 55):
            base.putpixel((x, y), (255, 0, 0))
    base.save(img1)
    _make_image(str(split / "img2.jpg"), (100, 80), (0, 128, 0))

    categories = [{"id": 1, "name": "tablica"}, {"id": 2, "name": "pojazd"}]
    images = [
        {"id": 1, "file_name": "img1.jpg", "width": 100, "height": 80},
        {"id": 2, "file_name": "img2.jpg", "width": 100, "height": 80},
    ]

    return {"dir": str(ds), "categories": categories, "images": images, "split": str(split)}


def _req(dataset_dir: str, **over):
    kw = dict(
        dataset_dir=dataset_dir,
        attribute="stan",
        source_class="",
        values=VALUES,
        output_dir="proj/run",
    )
    kw.update(over)
    return server.TrainRequest(**kw)


def _ann(aid: int, image_id: int, category_id: int, bbox, attrs=None):
    a = {"id": aid, "image_id": image_id, "category_id": category_id, "bbox": bbox}
    if attrs is not None:
        a["attributes"] = attrs
    return a


def test_bbox_crop_ma_rozmiar_ramki_i_zawiera_wlasciwy_fragment(dataset):
    # Ramka [20,15,30,40] na img1 pokrywa dokładnie czerwony prostokąt. Dwie
    # identyczne adnotacje → split kieruje jedną do train, jedną do valid.
    anns = [
        _ann(1, 1, 1, [20, 15, 30, 40], {"stan": "czysta"}),
        _ann(2, 1, 1, [20, 15, 30, 40], {"stan": "czysta"}),
    ]
    _write_coco(dataset["split"], dataset["images"], anns, dataset["categories"])
    req = _req(dataset["dir"], values=["czysta", "brudna"])

    stats = server._collect_crops(req, "job1")

    # Foldery nazwane są INDEKSEM wartości (czysta=0) — nazwa wartości nie trafia
    # do ścieżki (ochrona przed path traversal).
    crop_path = os.path.join(stats["data_root"], "train", "0", "0.jpg")
    assert os.path.isfile(crop_path)
    with Image.open(crop_path) as im:
        assert im.size == (30, 40)
        # Środek wycinka musi być czerwony (dowód, że wycięto właściwy obszar).
        r, g, b = im.convert("RGB").getpixel((15, 20))
        assert r > 200 and g < 60 and b < 60


def test_split_stratyfikowany_zachowuje_proporcje_i_jest_deterministyczny(dataset):
    # 10 adnotacji klasy 'czysta' + 10 'brudna' → po ~15% do valid (2 na klasę).
    anns = []
    aid = 1
    for i in range(10):
        anns.append(_ann(aid, 1, 1, [i, 0, 20, 20], {"stan": "czysta"}))
        aid += 1
    for i in range(10):
        anns.append(_ann(aid, 2, 2, [i, 0, 20, 20], {"stan": "brudna"}))
        aid += 1
    _write_coco(dataset["split"], dataset["images"], anns, dataset["categories"])
    req = _req(dataset["dir"])

    s1 = server._collect_crops(req, "jobA")
    s2 = server._collect_crops(req, "jobB")

    # Proporcje ~15% na klasę, min. 1 do valid.
    assert s1["counts_split"]["czysta"] == {"train": 8, "valid": 2}
    assert s1["counts_split"]["brudna"] == {"train": 8, "valid": 2}
    # Deterministyczny — inny job_id, ten sam podział.
    assert s1["counts_split"] == s2["counts_split"]


def test_pomija_adnotacje_bez_atrybutu_i_z_pusta_wartoscia(dataset):
    anns = [
        _ann(1, 1, 1, [0, 0, 20, 20], {"stan": "czysta"}),   # ok
        _ann(2, 1, 1, [0, 0, 20, 20], {"inny": "x"}),         # brak atrybutu 'stan'
        _ann(3, 1, 1, [0, 0, 20, 20], None),                  # brak attributes
        _ann(4, 1, 1, [0, 0, 20, 20], {"stan": ""}),          # pusta wartość
        _ann(5, 1, 1, [0, 0, 20, 20], {"stan": "kosmita"}),   # wartość spoza values
    ]
    _write_coco(dataset["split"], dataset["images"], anns, dataset["categories"])
    req = _req(dataset["dir"])

    stats = server._collect_crops(req, "job2")

    assert stats["counts_raw"]["czysta"] == 1
    assert stats["skipped_no_attr"] == 2       # {'inny'} + brak attributes
    assert stats["skipped_bad_value"] == 2     # "" i "kosmita" nie ma w values
    # Zsumowanie: tylko 1 realny wycinek trafił do datasetu.
    total_saved = sum(c["train"] + c["valid"] for c in stats["counts_split"].values())
    assert total_saved == 1


def test_source_class_filtruje_po_kategorii(dataset):
    anns = [
        _ann(1, 1, 1, [0, 0, 20, 20], {"stan": "czysta"}),   # tablica → bierzemy
        _ann(2, 2, 2, [0, 0, 20, 20], {"stan": "brudna"}),   # pojazd → odrzucamy
    ]
    _write_coco(dataset["split"], dataset["images"], anns, dataset["categories"])
    req = _req(dataset["dir"], source_class="tablica")

    stats = server._collect_crops(req, "job3")

    assert stats["counts_raw"]["czysta"] == 1
    assert stats["counts_raw"]["brudna"] == 0
    assert stats["skipped_wrong_class"] == 1


def test_remap_kolejnosci_klas_imagefolder_do_values(dataset):
    # Foldery nazwane są INDEKSEM wartości (czysta=0, brudna=1) — nazwa wartości
    # NIGDY nie trafia do ścieżki. Remap odczytuje indeks wprost z nazwy foldera,
    # a mapowanie indeks→nazwa (= kolejność `values`) trzyma classes.json.
    import torch
    from torchvision.datasets import ImageFolder

    anns = [
        _ann(1, 1, 1, [0, 0, 20, 20], {"stan": "czysta"}),
        _ann(2, 1, 1, [20, 0, 20, 20], {"stan": "czysta"}),
        _ann(3, 2, 2, [0, 0, 20, 20], {"stan": "brudna"}),
        _ann(4, 2, 2, [20, 0, 20, 20], {"stan": "brudna"}),
    ]
    _write_coco(dataset["split"], dataset["images"], anns, dataset["categories"])
    req = _req(dataset["dir"], values=["czysta", "brudna"])
    stats = server._collect_crops(req, "job4")

    train_ds = ImageFolder(os.path.join(stats["data_root"], "train"))
    folder_classes = train_ds.classes
    # Same indeksy w nazwach folderów — żadnej nazwy wartości.
    assert folder_classes == ["0", "1"]

    remap = torch.tensor([int(c) for c in folder_classes], dtype=torch.long)
    # folder "0" → values idx 0 (czysta); folder "1" → values idx 1 (brudna).
    assert remap.tolist() == [0, 1]

    # classes.json zachowuje kolejność klas = kolejność `values`.
    with open(os.path.join(stats["data_root"], "classes.json"), encoding="utf-8") as f:
        classes = json.load(f)["classes"]
    assert classes == ["czysta", "brudna"]

    # Etykieta próbki po remapie odpowiada indeksowi zakodowanemu w nazwie foldera.
    idx_to_folder = {v: k for k, v in train_ds.class_to_idx.items()}
    for _, folder_idx in train_ds.samples:
        folder_name = idx_to_folder[folder_idx]
        assert int(remap[folder_idx]) == int(folder_name)


def test_wagi_klas_to_odwrotnosc_czestosci(dataset):
    # 8 'czysta' vs 2 'brudna' w train → waga brudnej > czystej, proporcja 1/count.
    import torch
    from torchvision.datasets import ImageFolder

    anns = []
    aid = 1
    for i in range(10):
        anns.append(_ann(aid, 1, 1, [i * 2, 0, 20, 20], {"stan": "czysta"}))
        aid += 1
    for i in range(2):
        anns.append(_ann(aid, 2, 2, [i * 2, 0, 20, 20], {"stan": "brudna"}))
        aid += 1
    _write_coco(dataset["split"], dataset["images"], anns, dataset["categories"])
    req = _req(dataset["dir"], values=["czysta", "brudna"])
    stats = server._collect_crops(req, "job5")

    train_ds = ImageFolder(os.path.join(stats["data_root"], "train"))
    folder_classes = train_ds.classes
    remap = torch.tensor([int(c) for c in folder_classes], dtype=torch.long)
    num_classes = len(req.values)
    counts = [0] * num_classes
    for _, folder_idx in train_ds.samples:
        tgt = int(remap[folder_idx])
        if tgt >= 0:
            counts[tgt] += 1
    total = sum(counts) or 1
    weights = [total / (num_classes * c) if c > 0 else 0.0 for c in counts]

    i_czysta = req.values.index("czysta")
    i_brudna = req.values.index("brudna")
    # Klasa rzadsza (brudna) dostaje większą wagę.
    assert weights[i_brudna] > weights[i_czysta]
    # Odwrotność częstości: waga * liczność ma być stała w obrębie klas obecnych.
    assert weights[i_czysta] * counts[i_czysta] == pytest.approx(weights[i_brudna] * counts[i_brudna])


def test_tiny_forward_na_cpu_daje_logity_num_classes(dataset):
    # Konstrukcja modelu (od zera, bez pobierania wag) + 1 forward na CPU: sanity
    # że pipeline model→logits ma wymiar = len(values). Lekkie (mobilenet small).
    import torch

    model = server._create_model("mobilenetv4", num_classes=len(VALUES), pretrained=False).eval()
    x = torch.randn(2, 3, 224, 224)
    with torch.no_grad():
        out = model(x)
    assert tuple(out.shape) == (2, len(VALUES))
