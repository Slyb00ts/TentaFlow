#!/usr/bin/env python3
# =============================================================================
# Plik: upload_hf.py
# Opis: Publikuje 3 warianty modelu TentaGuard na Hugging Face (org TentaFlow):
#       GGUF Q5_K_M (llama.cpp), NVFP4 (vLLM), MLX 4-bit (Apple). Kazdy wariant
#       trafia do osobnego repo z karta modelu. Pomija warianty, ktorych brak.
# Wymaga: huggingface_hub + token (HF_TOKEN w env albo `huggingface-cli login`).
# Uzycie: HF_TOKEN=hf_xxx .venv-nvfp4/bin/python scripts/upload_hf.py
#         (--dry-run zeby tylko pokazac co zostanie wyslane)
# =============================================================================
import argparse
import os
import sys
import tempfile

from huggingface_hub import HfApi

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
ORG = "TentaFlow"
APP_URL = "https://github.com/Slyb00ts/TentaFlow"
BASE_MODEL = "Qwen/Qwen3.5-0.8B"

# Wspolny opis (gora kazdej karty). {title} podstawiamy per wariant.
COMMON = f"""**TentaGuard** is a lightweight security classifier (guard) — a fine-tune of
[`{BASE_MODEL}`](https://huggingface.co/{BASE_MODEL}). It is used **mainly inside the
[TentaFlow]({APP_URL}) application** to scan external content — messages, documents,
web-search results, etc. — for **hidden attacks** (prompt injection / jailbreak) before it
reaches the main LLM.

The model does NOT generate user-facing replies — it returns a single digit:

| Label | Meaning |
|-------|---------|
| `0` | benign (safe content) |
| `1` | prompt injection / tool abuse (technical attack) |
| `2` | jailbreak (behavioural manipulation) |

If the text contains BOTH injection and jailbreak → `1`.

## Input format

A classifier system prompt + a user message `<|guard|>\\n{{text}}`. **Build the prompt with the
model tokenizer (`apply_chat_template`)** — do not rely on a generic chat template.

## Accuracy (guard test set, 58 examples)

- Exact (0/1/2): **~96.6%** (full precision) / **~94.8%** (Q5_K_M)
- Safe / Unsafe: **~98.3%**

## Authors

Trained by: **Katarzyna Nowak**, **Piotr Jarocki**, **Damian Pala**, **Jakub Rurański**.

## License & attribution

Apache-2.0, inherited from the base model [`{BASE_MODEL}`](https://huggingface.co/{BASE_MODEL}).
This checkpoint is a fine-tune for attack detection, built for the [TentaFlow]({APP_URL}) application.
"""

GGUF_USAGE = """## Usage (llama.cpp)

Requires a **recent llama.cpp** (Qwen3.5 / `qwen35` with SSM layers). Run the server and send a
**raw, tokenizer-built prompt** to `/completion` (NOT `/v1/chat/completions` — the embedded
Qwen3.5 multimodal chat template renders a broken prompt):

```bash
llama-server -m TentaGuard-Q5_K_M.gguf -ngl 99 -c 2048
```

```python
from transformers import AutoTokenizer
import requests
tok = AutoTokenizer.from_pretrained("Qwen/Qwen3.5-0.8B")
SYS = "You are a security classifier. Output ONLY a single digit: 0 benign, 1 injection, 2 jailbreak."
msgs = [{"role":"system","content":SYS},{"role":"user","content":"<|guard|>\\n" + text}]
prompt = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=True)
r = requests.post("http://localhost:8080/completion", json={"prompt": prompt, "n_predict": 5, "temperature": 0})
label = next((c for c in r.json()["content"] if c in "012"), None)
```
"""

NVFP4_USAGE = f"""## Usage (vLLM)

`compressed-tensors` format (`nvfp4-pack-quantized`): 4-bit weights (FP4 E2M1, groups of 16,
FP8 E4M3 block scales + a global FP32 scale), 4-bit activations (W4A4), `lm_head` kept in full
precision. PTQ calibration via [`llm-compressor`](https://github.com/vllm-project/llm-compressor)
on real guard prompts.

NVFP4 is hardware-accelerated on **Blackwell (sm_100+)**; on older GPUs vLLM loads it as
**weight-only** (smaller VRAM, no FP4 acceleration).

```bash
vllm serve {ORG}/TentaGuard-NVFP4
```
"""

MLX_USAGE = f"""## Usage (MLX — Apple Silicon)

4-bit quantization (affine, group_size=64) for `mlx-lm` / mlx-swift.

```python
from mlx_lm import load, generate
model, tok = load("{ORG}/TentaGuard-MLX-4bit")
prompt = tok.apply_chat_template(
    [{{"role":"system","content":"You are a security classifier. Output ONLY 0/1/2."}},
     {{"role":"user","content":"<|guard|>\\n" + text}}],
    add_generation_prompt=True)
print(generate(model, tok, prompt=prompt, max_tokens=5))
```
"""

MODELS = [
    {
        "repo": "TentaGuard-GGUF-Q5_K_M",
        "title": "GGUF (Q5_K_M, llama.cpp)",
        "path": os.path.join(ROOT, "output", "qwen-guard-Q5_K_M.gguf"),
        "is_dir": False,
        "upload_name": "TentaGuard-Q5_K_M.gguf",
        "tags": ["gguf", "llama-cpp", "quantized", "q5_k_m"],
        "library": "gguf",
        "usage": GGUF_USAGE,
    },
    {
        "repo": "TentaGuard-NVFP4",
        "title": "NVFP4 (W4A4, vLLM)",
        "path": os.path.join(ROOT, "output", "qwen-guard-nvfp4"),
        "is_dir": True,
        "tags": ["nvfp4", "fp4", "compressed-tensors", "vllm", "quantized"],
        "library": "transformers",
        "usage": NVFP4_USAGE,
    },
    {
        "repo": "TentaGuard-MLX-4bit",
        "title": "MLX 4-bit (Apple Silicon)",
        "path": os.path.join(ROOT, "output", "qwen-guard-mlx-4bit"),
        "is_dir": True,
        "tags": ["mlx", "apple", "quantized", "4-bit"],
        "library": "mlx",
        "usage": MLX_USAGE,
    },
]


def model_card(m):
    tags = "\n".join(f"- {t}" for t in (m["tags"] + ["tentaguard", "guard", "security", "prompt-injection", "tentaflow"]))
    front = (
        "---\n"
        "license: apache-2.0\n"
        f"base_model:\n- {BASE_MODEL}\n"
        "pipeline_tag: text-classification\n"
        f"library_name: {m['library']}\n"
        "language:\n- en\n- pl\n"
        f"tags:\n{tags}\n"
        "---\n"
    )
    body = f"# TentaGuard — {m['title']}\n\n{COMMON}\n{m['usage']}"
    return front + "\n" + body


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true", help="Tylko pokaz, nie wysylaj")
    ap.add_argument("--private", action="store_true", help="Tworz repo jako prywatne")
    args = ap.parse_args()

    token = os.environ.get("HF_TOKEN")
    api = HfApi(token=token)
    if not args.dry_run:
        who = api.whoami()
        print(f"Zalogowany jako: {who.get('name')}")

    for m in MODELS:
        if not os.path.exists(m["path"]):
            print(f"[POMIJAM] brak artefaktu: {m['path']}")
            continue
        repo_id = f"{ORG}/{m['repo']}"
        size = os.path.getsize(m["path"]) if not m["is_dir"] else sum(
            os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(m["path"]) for f in fs
        )
        print(f"\n=== {repo_id}  ({size/1e6:.0f} MB) ===")
        if args.dry_run:
            print(m["path"], "->", repo_id)
            continue

        api.create_repo(repo_id, repo_type="model", private=args.private, exist_ok=True)

        with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as f:
            f.write(model_card(m))
            readme = f.name
        api.upload_file(path_or_fileobj=readme, path_in_repo="README.md",
                        repo_id=repo_id, repo_type="model")
        os.unlink(readme)

        if m["is_dir"]:
            # Pomijamy README.md z katalogu modelu (np. mlx_lm.convert generuje
            # wlasny stub) — zostawiamy nasza karte wgrana powyzej.
            api.upload_folder(folder_path=m["path"], repo_id=repo_id, repo_type="model",
                              ignore_patterns=["README.md"])
        else:
            api.upload_file(path_or_fileobj=m["path"], path_in_repo=m["upload_name"],
                            repo_id=repo_id, repo_type="model")
        print(f"  OK: https://huggingface.co/{repo_id}")


if __name__ == "__main__":
    main()
