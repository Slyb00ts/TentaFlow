# =============================================================================
# Plik: server.py
# Opis: Serwer detekcji YOLOX (NVIDIA NeMo Retriever) liczacy inferencje
#       BEZPOSREDNIO w PyTorch na GPU. Udostepnia kontrakt HTTP zgodny z NVIDIA
#       NIM Object Detection. Przy starcie pobiera wagi .pth dla wskazanego
#       MODEL_REPO, buduje siec YOLOX (liczba klas odczytana z checkpointu),
#       laduje state_dict na GPU i wystawia POST /v1/infer zwracajacy ramki
#       znormalizowane do [0,1] pogrupowane po NAZWIE klasy. Silnik WYMAGA GPU
#       (CUDA/ROCm) — bez dostepnego GPU start jest przerywany, BEZ fallbacku CPU.
#       onnxruntime nie jest uzywany (nowe GPU nie maja kerneli onnxruntime-gpu).
# Przyklad: curl -X POST http://127.0.0.1:8086/v1/infer \
#           -d '{"input":[{"type":"image_url","url":"data:image/png;base64,..."}]}'
# =============================================================================

import base64
import logging
import os
import re
import threading

import cv2
import numpy as np
import torch
import uvicorn
from fastapi import FastAPI, HTTPException
from huggingface_hub import hf_hub_download, list_repo_files
from pydantic import BaseModel
from yolox.exp import get_exp

logger = logging.getLogger("nemotron-yolox")

# Rozmiar wejscia YOLOX dla detektorow dokumentowych NeMo Retriever.
INPUT_SIZE = (1024, 1024)
# Progi filtrowania detekcji.
DEFAULT_SCORE_THRESH = float(os.environ.get("SCORE_THRESHOLD", "0.3"))
DEFAULT_NMS_THRESH = float(os.environ.get("NMS_THRESHOLD", "0.45"))

BUNDLE_DIR = os.path.dirname(os.path.abspath(__file__))

# Prefiks data-URL: "data:image/png;base64,<...>".
_DATA_URL_RE = re.compile(r"^data:[^;,]*(;base64)?,(?P<payload>.*)$", re.DOTALL)

# Autorytatywne listy nazw klas (w kolejnosci indeksow) odczytane z konfiguracji
# w repozytoriach modeli NVIDIA (klasa Exp.labels). Wybor listy nastepuje per
# MODEL_REPO; jesli liczba klas checkpointu nie pasuje do listy, padamy na
# "class_<id>" i logujemy ostrzezenie (bez wywracania serwera).
CLASS_LABELS_BY_REPO: dict[str, list[str]] = {
    "nvidia/nemotron-page-elements-v3": [
        "table", "chart", "title", "infographic", "text", "header_footer",
    ],
    "nvidia/nemotron-table-structure-v1": [
        "border", "cell", "row", "column", "header",
    ],
    "nvidia/nemotron-graphic-elements-v1": [
        "chart_title", "x_title", "y_title", "xlabel", "ylabel",
        "other", "legend_label", "legend_title", "mark_label", "value_label",
    ],
}


def _wymagaj_gpu() -> torch.device:
    """Zwraca urzadzenie GPU lub przerywa start. torch ROCm tez raportuje cuda."""
    if not torch.cuda.is_available():
        raise RuntimeError(
            "Brak dostepnego GPU (torch.cuda.is_available() == False). Silnik "
            "nemotron-yolox serwuje inferencje wylacznie na GPU (CUDA cu130 / "
            "ROCm), bez fallbacku CPU. Sprawdz sterowniki i wariant instalacji torch."
        )
    return torch.device("cuda")


def _znajdz_checkpoint(repo: str) -> str:
    """Pobiera plik .pth z repozytorium HF. Wagi modeli NeMo Retriever leza w
    podkatalogu (np. `nemotron_page_elements_v3/weights.pth`), wiec listujemy
    pliki repo i bierzemy pierwszy *.pth zamiast zgadywac nazwy top-level."""
    pliki = list_repo_files(repo)
    kandydaci = [f for f in pliki if f.endswith(".pth")]
    if not kandydaci:
        raise FileNotFoundError(f"Nie znaleziono pliku wag .pth w repozytorium {repo}")
    return hf_hub_download(repo_id=repo, filename=kandydaci[0])


def _liczba_klas(state_dict: dict) -> int:
    """Odczytuje liczbe klas z ksztaltu glowy klasyfikacyjnej YOLOX."""
    for klucz, tensor in state_dict.items():
        if klucz.endswith("cls_preds.0.weight"):
            return int(tensor.shape[0])
    raise ValueError("Nie udalo sie wyznaczyc liczby klas z checkpointu")


def _zbuduj_model(repo: str, device: torch.device) -> tuple[torch.nn.Module, int]:
    """Buduje siec YOLOX i laduje do niej state_dict z checkpointu .pth na GPU.
    Zwraca model oraz liczbe klas (potrzebna do mapowania class_id -> nazwa).

    page-elements, graphic-elements i table-structure dziela ten sam backbone
    YOLOX-L; roznia sie tylko liczba klas, ktora odczytujemy z checkpointu.
    TODO: gdyby ktoras wersja modelu uzywala innego rozmiaru backbone niz
    yolox-l, nalezy wybrac exp_name na podstawie ksztaltow wag (depth/width).
    """
    sciezka_pth = _znajdz_checkpoint(repo)
    # weights_only=False bo checkpointy NVIDIA zawieraja obiekty numpy; zrodlo
    # zaufane (oficjalne repo HF). torch 2.6+ domyslnie ma weights_only=True.
    checkpoint = torch.load(sciezka_pth, map_location="cpu", weights_only=False)
    state_dict = checkpoint.get("model", checkpoint)

    liczba_klas = _liczba_klas(state_dict)

    exp = get_exp(exp_name="yolox-l")
    exp.num_classes = liczba_klas
    exp.test_size = INPUT_SIZE
    model = exp.get_model()
    model.load_state_dict(state_dict, strict=False)
    # Glowa zwraca surowe predykcje (dekodowanie xywh + NMS robimy ponizej w
    # _postprocess), zgodnie z dotychczasowym formatem wyjscia.
    model.head.decode_in_inference = False
    model.eval()
    model.to(device)
    return model, liczba_klas


def _letterbox(obraz: np.ndarray) -> tuple[np.ndarray, float]:
    """Skaluje obraz z zachowaniem proporcji i dopelnia do INPUT_SIZE (114 grey)."""
    wys, szer = obraz.shape[:2]
    skala = min(INPUT_SIZE[0] / wys, INPUT_SIZE[1] / szer)
    nowy = (int(round(szer * skala)), int(round(wys * skala)))
    zmieniony = cv2.resize(obraz, nowy, interpolation=cv2.INTER_LINEAR)
    plotno = np.full((INPUT_SIZE[0], INPUT_SIZE[1], 3), 114, dtype=np.uint8)
    plotno[: nowy[1], : nowy[0]] = zmieniony
    return plotno, skala


def _preprocess(obraz: np.ndarray, device: torch.device) -> tuple[torch.Tensor, float]:
    """Letterbox + uklad CHW float32 (bez normalizacji 0-1, jak trening YOLOX), na GPU."""
    plotno, skala = _letterbox(obraz)
    tensor = plotno.transpose(2, 0, 1).astype(np.float32)
    tensor = torch.from_numpy(tensor).unsqueeze(0).to(device)
    return tensor, skala


def _siatka_yolox(wymiary: tuple[int, int]) -> tuple[np.ndarray, np.ndarray]:
    """Buduje siatke punktow i strides dla dekodowania surowych predykcji YOLOX."""
    siatki, stride_lista = [], []
    for stride in (8, 16, 32):
        hsiz, wsiz = wymiary[0] // stride, wymiary[1] // stride
        xv, yv = np.meshgrid(np.arange(wsiz), np.arange(hsiz))
        siatka = np.stack((xv, yv), 2).reshape(-1, 2)
        siatki.append(siatka)
        stride_lista.append(np.full((siatka.shape[0], 1), stride))
    return np.concatenate(siatki, 0), np.concatenate(stride_lista, 0)


def _dekoduj(wyjscie: np.ndarray) -> np.ndarray:
    """Zamienia surowe predykcje YOLOX na [x1, y1, x2, y2, obj, cls_scores...]."""
    siatka, strides = _siatka_yolox(INPUT_SIZE)
    wyjscie[..., :2] = (wyjscie[..., :2] + siatka) * strides
    wyjscie[..., 2:4] = np.exp(wyjscie[..., 2:4]) * strides
    return wyjscie


def _nms(boxes: np.ndarray, scores: np.ndarray, prog: float) -> list[int]:
    """Klasyczny non-maximum suppression na ramkach xyxy."""
    x1, y1, x2, y2 = boxes[:, 0], boxes[:, 1], boxes[:, 2], boxes[:, 3]
    pola = (x2 - x1) * (y2 - y1)
    kolejnosc = scores.argsort()[::-1]
    zatrzymane = []
    while kolejnosc.size > 0:
        i = kolejnosc[0]
        zatrzymane.append(int(i))
        xx1 = np.maximum(x1[i], x1[kolejnosc[1:]])
        yy1 = np.maximum(y1[i], y1[kolejnosc[1:]])
        xx2 = np.minimum(x2[i], x2[kolejnosc[1:]])
        yy2 = np.minimum(y2[i], y2[kolejnosc[1:]])
        w = np.maximum(0.0, xx2 - xx1)
        h = np.maximum(0.0, yy2 - yy1)
        iou = (w * h) / (pola[i] + pola[kolejnosc[1:]] - w * h)
        kolejnosc = kolejnosc[1:][iou <= prog]
    return zatrzymane


def _postprocess(wyjscie: np.ndarray, skala: float) -> list[dict]:
    """Dekoduje, filtruje progiem pewnosci i NMS, przelicza ramki do oryginalu."""
    predykcje = _dekoduj(wyjscie)[0]
    srodki = predykcje[:, :4]
    obj = predykcje[:, 4:5]
    klasy = predykcje[:, 5:]

    ramki = np.empty_like(srodki)
    ramki[:, 0] = srodki[:, 0] - srodki[:, 2] / 2.0
    ramki[:, 1] = srodki[:, 1] - srodki[:, 3] / 2.0
    ramki[:, 2] = srodki[:, 0] + srodki[:, 2] / 2.0
    ramki[:, 3] = srodki[:, 1] + srodki[:, 3] / 2.0
    ramki /= skala

    score_per_klasa = obj * klasy
    najlepsza_klasa = score_per_klasa.argmax(axis=1)
    najlepszy_score = score_per_klasa.max(axis=1)

    maska = najlepszy_score > DEFAULT_SCORE_THRESH
    ramki, najlepszy_score, najlepsza_klasa = (
        ramki[maska],
        najlepszy_score[maska],
        najlepsza_klasa[maska],
    )

    detekcje = []
    for cls_id in np.unique(najlepsza_klasa):
        idx = np.where(najlepsza_klasa == cls_id)[0]
        zatrzymane = _nms(ramki[idx], najlepszy_score[idx], DEFAULT_NMS_THRESH)
        for k in zatrzymane:
            j = idx[k]
            x1, y1, x2, y2 = ramki[j].tolist()
            detekcje.append(
                {
                    "bbox": [x1, y1, x2, y2],
                    "score": float(najlepszy_score[j]),
                    "class_id": int(cls_id),
                }
            )
    return detekcje


def _wybierz_etykiety(repo: str, liczba_klas: int) -> list[str]:
    """Dobiera liste nazw klas dla repo. Gdy liczba klas checkpointu nie zgadza
    sie z lista referencyjna (lub repo jest nieznane), padamy na 'class_<id>' i
    logujemy ostrzezenie — nigdy nie wywracamy serwera."""
    etykiety = CLASS_LABELS_BY_REPO.get(repo)
    if etykiety is not None and len(etykiety) == liczba_klas:
        return etykiety
    if etykiety is not None:
        logger.warning(
            "Liczba klas checkpointu (%d) dla %s nie pasuje do listy referencyjnej "
            "(%d) — uzywam nazw class_<id>.",
            liczba_klas, repo, len(etykiety),
        )
    else:
        logger.warning(
            "Nieznane MODEL_REPO '%s' — brak listy nazw klas, uzywam class_<id>.", repo
        )
    return [f"class_{i}" for i in range(liczba_klas)]


def _bounding_boxes_nim(
    detekcje: list[dict], etykiety: list[str], szer: int, wys: int
) -> dict[str, list[dict]]:
    """Grupuje detekcje po NAZWIE klasy i normalizuje ramki do [0,1] wzgledem
    oryginalnego rozmiaru obrazu (format NVIDIA NIM Object Detection)."""
    grupy: dict[str, list[dict]] = {}
    for det in detekcje:
        cls_id = det["class_id"]
        nazwa = etykiety[cls_id] if 0 <= cls_id < len(etykiety) else f"class_{cls_id}"
        x1, y1, x2, y2 = det["bbox"]
        grupy.setdefault(nazwa, []).append(
            {
                "x_min": float(np.clip(x1 / szer, 0.0, 1.0)),
                "y_min": float(np.clip(y1 / wys, 0.0, 1.0)),
                "x_max": float(np.clip(x2 / szer, 0.0, 1.0)),
                "y_max": float(np.clip(y2 / wys, 0.0, 1.0)),
                "confidence": det["score"],
            }
        )
    return grupy


# Docker mapuje MODEL->MODEL_REPO w entrypoincie; native (python-bundle) nie ma
# entrypointu i Core wstrzykuje tylko MODEL — czytamy MODEL_REPO z fallbackiem.
MODEL_REPO = os.environ.get("MODEL_REPO") or os.environ.get("MODEL")
if not MODEL_REPO:
    raise RuntimeError("Brak env MODEL_REPO/MODEL — nie wiadomo ktore repo modelu zaladowac.")

# Model ladowany leniwie w watku tla: budowa sieci + pobranie wag .pth z HF +
# pierwsza inicjalizacja CUDA (na nowych GPU jak B300/sm_103 pierwszy `.to(cuda)`
# kompiluje kernele JIT i potrafi trwac dziesiatki sekund). Synchroniczne
# ladowanie na top-levelu blokowalo start uvicorna — serwer NIGDY nie zaczynal
# nasluchu, /health nie odpowiadal, deploy w Core wisil i przy restarcie zostawal
# osierocony proces. W tle /health odpowiada od razu, /v1/infer zwraca 503 do
# czasu gotowosci. Wzorzec spojny z nemotron-ocr/server.py.
_state: dict = {"model": None, "device": None, "labels": None, "error": None}
_LOAD_LOCK = threading.Lock()
# Serializes GPU inference across FastAPI's threadpool for the sync `/v1/infer`
# handler — one shared model, so concurrent forward passes would race the CUDA stream.
_INFER_LOCK = threading.Lock()


def _ensure_model() -> None:
    if _state["model"] is not None:
        return
    with _LOAD_LOCK:
        if _state["model"] is not None:
            return
        device = _wymagaj_gpu()
        model, liczba_klas = _zbuduj_model(MODEL_REPO, device)
        _state["device"] = device
        _state["labels"] = _wybierz_etykiety(MODEL_REPO, liczba_klas)
        _state["model"] = model


def _background_load() -> None:
    try:
        _ensure_model()
    except Exception as exc:  # noqa: BLE001 — zapamietujemy blad dla /health i /v1/infer
        _state["error"] = str(exc)
        logger.exception("Ladowanie modelu %s nie powiodlo sie", MODEL_REPO)


app = FastAPI(title="nemotron-yolox")


@app.on_event("startup")
def _start_background_load() -> None:
    threading.Thread(target=_background_load, name="model-load", daemon=True).start()


class InputImage(BaseModel):
    type: str = "image_url"
    url: str


class InferRequest(BaseModel):
    input: list[InputImage]


def _decode_data_url(url: str) -> np.ndarray:
    """Dekoduje data-URL (base64) lub czysty base64 do obrazu BGR (OpenCV)."""
    dopasowanie = _DATA_URL_RE.match(url.strip())
    payload = dopasowanie.group("payload") if dopasowanie else url.strip()
    try:
        surowe = base64.b64decode(payload)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"Bledny base64 w url: {exc}")
    bufor = np.frombuffer(surowe, dtype=np.uint8)
    obraz = cv2.imdecode(bufor, cv2.IMREAD_COLOR)
    if obraz is None:
        raise HTTPException(status_code=400, detail="Nie udalo sie zdekodowac obrazu")
    return obraz


@app.get("/health")
def health() -> dict:
    """Zawsze 200 — health probe Core ma odpowiedz od razu, niezaleznie od
    postepu ladowania modelu. `ready` rozroznia model gotowy / w trakcie / blad."""
    ready = _state["model"] is not None
    return {
        "status": "ok",
        "ready": ready,
        "model": MODEL_REPO,
        "labels": _state["labels"],
        "device": str(_state["device"]) if _state["device"] is not None else None,
        "cuda": torch.version.cuda,
        "hip": getattr(torch.version, "hip", None),
        "error": _state["error"],
    }


# SYNC handler on purpose: FastAPI runs `def` path operations in a worker thread,
# so the blocking GPU forward does NOT occupy the event loop. As `async def` the
# multi-second inference ran ON the loop, `/health` stalled during it, and Core's
# supervisor health-probe timed out and respawned the engine mid-request
# ("error sending request to /v1/infer"). Keep sync; serialize GPU with the lock.
@app.post("/v1/infer")
def infer(req: InferRequest) -> dict:
    if not req.input:
        raise HTTPException(status_code=400, detail="Pole 'input' nie moze byc puste.")

    model = _state["model"]
    if model is None:
        if _state["error"] is not None:
            raise HTTPException(status_code=503, detail=f"Model nie zaladowal sie: {_state['error']}")
        raise HTTPException(status_code=503, detail="Model jeszcze sie laduje — sprobuj ponownie.")
    device = _state["device"]
    etykiety = _state["labels"]

    data = []
    for indeks, wejscie in enumerate(req.input):
        obraz = _decode_data_url(wejscie.url)
        wys, szer = obraz.shape[0], obraz.shape[1]
        tensor, skala = _preprocess(obraz, device)
        with _INFER_LOCK:
            with torch.no_grad():
                wyjscie = model(tensor)
            wyjscie = wyjscie.detach().to("cpu", dtype=torch.float32).numpy()
        detekcje = _postprocess(wyjscie, skala)
        data.append(
            {"index": indeks, "bounding_boxes": _bounding_boxes_nim(detekcje, etykiety, szer, wys)}
        )

    return {"data": data}


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "8086"))
    uvicorn.run(app, host="0.0.0.0", port=port)
