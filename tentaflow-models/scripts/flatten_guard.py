#!/usr/bin/env python3
# =============================================================================
# Plik: flatten_guard.py
# Opis: Splaszcza checkpoint Qwen3.5 zapisany jako multimodalny
#       Qwen3_5ForConditionalGeneration do czystego tekstowego CausalLM.
#       Pelny zapis zagniezdza dekoder tekstowy pod powtorzonymi prefiksami
#       `language_model.` (np. model.language_model.language_model.language_model.*)
#       i dorzuca wieze wizyjna — tekstowe loadery (AutoModelForCausalLM uzywany
#       przez export_nvfp4.py, MLX, llama.cpp) oczekuja plaskiego `model.layers.*`,
#       wiec inaczej laduja losowe wagi.
#
#       Zwija wszystkie `language_model.`, usuwa wizje i glowice `mtp`, NIE dodaje
#       jawnego lm_head (tie_word_embeddings — transformers zwiaze go sam, a MLX
#       odrzuca jawny lm_head). Obsluguje wejscie sharded (po index.json) oraz
#       tryb in-place (dst == src lub dst=None).
#
# Uzycie (CLI):  python3 scripts/flatten_guard.py [src] [dst]
#   domyslnie: output/qwen-guard-full -> output/qwen-guard-full (in-place)
# Uzycie (import): from flatten_guard import flatten_checkpoint
# =============================================================================
import glob
import json
import os
import re
import shutil
import sys

COLLAPSE = re.compile(r"^model\.(?:language_model\.)+")

_TOKENIZER_FILES = [
    "tokenizer.json", "tokenizer_config.json", "special_tokens_map.json",
    "vocab.json", "merges.txt", "chat_template.jinja", "generation_config.json",
    "preprocessor_config.json", "config.json",
]


def _shard_paths(src):
    """Lista plikow .safetensors w `src` (pojedynczy plik lub sharded)."""
    single = os.path.join(src, "model.safetensors")
    if os.path.exists(single):
        return [single]
    shards = sorted(glob.glob(os.path.join(src, "model*.safetensors")))
    if not shards:
        raise SystemExit(f"BLAD: brak plikow model*.safetensors w {src}")
    return shards


def flatten_checkpoint(src, dst=None):
    """Splaszcza checkpoint z `src` do `dst` (domyslnie in-place)."""
    from safetensors import safe_open
    from safetensors.torch import save_file

    in_place = dst is None or os.path.abspath(dst) == os.path.abspath(src)
    out_dir = src if in_place else dst
    os.makedirs(out_dir, exist_ok=True)

    new = {}
    dropped = 0
    for shard in _shard_paths(src):
        with safe_open(shard, framework="pt") as f:
            for k in f.keys():
                if "visual" in k or k.startswith("mtp."):
                    dropped += 1
                    continue
                new[COLLAPSE.sub("model.", k)] = f.get_tensor(k)

    # Zapis do pliku tymczasowego, potem atomowa podmiana (bezpieczne in-place:
    # stare shardy musza istniec az save_file je odczyta).
    tmp = os.path.join(out_dir, "model.flat.tmp.safetensors")
    save_file(new, tmp, metadata={"format": "pt"})

    if in_place:
        for shard in _shard_paths(src):
            os.remove(shard)
        idx = os.path.join(src, "model.safetensors.index.json")
        if os.path.exists(idx):
            os.remove(idx)
    else:
        for fn in _TOKENIZER_FILES:
            p = os.path.join(src, fn)
            if os.path.exists(p):
                shutil.copy(p, out_dir)

    os.replace(tmp, os.path.join(out_dir, "model.safetensors"))

    # Wylacz glowice MTP (multi-token-prediction): full-FT gubi jej wagi, a my je
    # odrzucamy — zostawienie mtp_num_hidden_layers>0 sprawia, ze konwersja GGUF
    # oczekuje dodatkowego bloku (blk.<N>) i runtime llama.cpp odmawia zaladowania
    # ("missing tensor blk.24.attn_norm.weight"). Guard go nie uzywa.
    cfg_path = os.path.join(out_dir, "config.json")
    if os.path.exists(cfg_path):
        cfg = json.load(open(cfg_path))
        for scope in (cfg, cfg.get("text_config", {})):
            if isinstance(scope, dict) and "mtp_num_hidden_layers" in scope:
                scope["mtp_num_hidden_layers"] = 0
        json.dump(cfg, open(cfg_path, "w"), indent=2)

    print(f"flatten: {len(new)} tensorow (pominieto wizji/mtp: {dropped}, mtp_num_hidden_layers=0) -> {out_dir}")
    return out_dir


def main():
    root = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    src = sys.argv[1] if len(sys.argv) > 1 else os.path.join(root, "output", "qwen-guard-full")
    dst = sys.argv[2] if len(sys.argv) > 2 else None
    flatten_checkpoint(src, dst)


if __name__ == "__main__":
    main()
