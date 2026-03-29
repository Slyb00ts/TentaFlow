#!/usr/bin/env python3
# =============================================================================
# Plik: download_models.py
# Opis: Pobiera bazowe modele z HuggingFace do katalogu lokalnego.
# =============================================================================
from huggingface_hub import snapshot_download
import os

MODELS = [
    {
        "id": "Qwen/Qwen3.5-0.8B",
        "dir": os.path.join(os.path.dirname(__file__), "..", "models", "qwen3.5-0.8b-base"),
    },
    {
        "id": "meta-llama/Llama-Prompt-Guard-2-86M",
        "dir": os.path.join(os.path.dirname(__file__), "..", "models", "llama-prompt-guard-86m"),
    },
]

for model in MODELS:
    print(f"Pobieranie modelu {model['id']}...")
    print(f"Katalog docelowy: {model['dir']}")

    snapshot_download(
        repo_id=model["id"],
        local_dir=model["dir"],
        ignore_patterns=["*.gguf", "*.ggml"],
    )

    print(f"Model pobrany do {model['dir']}\n")
