# =============================================================================
# Plik: export_onnx.py
# Opis: OPCJONALNE narzedzie offline. Eksportuje wagi PyTorch .pth detektora
#       YOLOX (NVIDIA NeMo Retriever) do pliku ONNX dla innych platform (np. CPU
#       lub akceleratory bez wsparcia torch cu130). To NIE jest sciezka
#       serwowania GPU — produkcyjna inferencja na GPU (CUDA cu130 / ROCm) idzie
#       BEZPOSREDNIO w PyTorch przez server.py, bez onnxruntime (B300/sm_103 nie
#       ma kerneli onnxruntime-gpu). Pobiera checkpoint z HF wskazany przez
#       MODEL_REPO, buduje siec YOLOX o liczbie klas odczytanej z checkpointu i
#       zapisuje statyczny graf ONNX.
# Przykład: MODEL_REPO=nvidia/nemotron-page-elements-v3 python export_onnx.py
# =============================================================================

import os
import sys

import torch
from huggingface_hub import hf_hub_download
from yolox.exp import get_exp


# Rozmiar wejscia YOLOX dla detektorow dokumentowych NeMo Retriever.
INPUT_SIZE = (1024, 1024)


def _znajdz_checkpoint(repo: str) -> str:
    """Pobiera plik .pth z repozytorium HF (pierwszy pasujacy wzorzec wag)."""
    for nazwa in ("model.pth", "yolox.pth", "weights.pth", "pytorch_model.pth"):
        try:
            return hf_hub_download(repo_id=repo, filename=nazwa)
        except Exception:
            continue
    raise FileNotFoundError(f"Nie znaleziono pliku wag .pth w repozytorium {repo}")


def _liczba_klas(state_dict: dict) -> int:
    """Odczytuje liczbe klas z ksztaltu glowy klasyfikacyjnej YOLOX."""
    for klucz, tensor in state_dict.items():
        if klucz.endswith("cls_preds.0.weight"):
            return int(tensor.shape[0])
    raise ValueError("Nie udalo sie wyznaczyc liczby klas z checkpointu")


def eksportuj(repo: str, sciezka_wyjscia: str) -> None:
    """Buduje siec YOLOX z wag .pth i zapisuje ja jako graf ONNX."""
    sciezka_pth = _znajdz_checkpoint(repo)
    checkpoint = torch.load(sciezka_pth, map_location="cpu")
    state_dict = checkpoint.get("model", checkpoint)

    liczba_klas = _liczba_klas(state_dict)

    exp = get_exp(exp_name="yolox-l")
    exp.num_classes = liczba_klas
    exp.test_size = INPUT_SIZE
    model = exp.get_model()
    model.load_state_dict(state_dict, strict=False)
    model.eval()
    # YOLOX w trybie eksportu zwraca surowe predykcje (bez dekodowania NMS),
    # ktore server.py dekoduje juz po stronie runtime.
    model.head.decode_in_inference = False

    przyklad = torch.zeros(1, 3, INPUT_SIZE[0], INPUT_SIZE[1])
    os.makedirs(os.path.dirname(sciezka_wyjscia), exist_ok=True)
    torch.onnx.export(
        model,
        przyklad,
        sciezka_wyjscia,
        input_names=["images"],
        output_names=["output"],
        opset_version=17,
        dynamic_axes={"images": {0: "batch"}, "output": {0: "batch"}},
    )
    print(f"[export_onnx] zapisano {sciezka_wyjscia} (klas: {liczba_klas})")


def main() -> int:
    repo = os.environ.get("MODEL_REPO") or (sys.argv[1] if len(sys.argv) > 1 else None)
    if not repo:
        print("[export_onnx] brak MODEL_REPO ani argumentu z repozytorium", file=sys.stderr)
        return 2
    sciezka_wyjscia = os.environ.get(
        "ONNX_PATH",
        os.path.join(os.environ.get("MODEL_DIR", "/data/models"), f"{repo.replace('/', '_')}.onnx"),
    )
    eksportuj(repo, sciezka_wyjscia)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
