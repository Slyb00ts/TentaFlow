#!/usr/bin/env python3
"""Benchmark DeepSeek V4 Flash 0731 on the 2x DGX Spark cluster.

Measures what actually matters for a speculative-decode setup: time to first
token (prefill) and steady-state decode rate, at several prompt lengths, with
the generated text kept so quality can be eyeballed rather than assumed.

Streaming is used deliberately -- TTFT is unobservable from a blocking call,
and on this deployment prefill dominates at long context while decode stays
roughly flat, so a single aggregate tok/s would hide the interesting half.
"""
import argparse
import json
import time
import urllib.request

FILLER = (
    "Rozdzial {i}. System rozproszony sklada sie z wezlow wymieniajacych "
    "komunikaty przez zawodna siec. Kazdy wezel utrzymuje wlasny stan, a "
    "spojnosc osiaga sie przez uzgadnianie kolejnosci operacji. "
)


def build_prompt(target_tokens: int) -> str:
    """~1.4 tokena na slowo dla polskiego tekstu — przyblizenie wystarczajace,
    realna dlugosc i tak raportujemy z usage."""
    if target_tokens <= 64:
        return "Wyjasnij w dwoch zdaniach, czym jest pamiec unified."
    words_needed = int(target_tokens / 1.4)
    text, n = [], 0
    i = 0
    while n < words_needed:
        chunk = FILLER.format(i=i)
        text.append(chunk)
        n += len(chunk.split())
        i += 1
    return (
        "Ponizej fragment dokumentacji.\n\n"
        + "".join(text)
        + "\n\nStreszcz powyzszy tekst w trzech punktach."
    )


def run(url: str, model: str, prompt: str, max_tokens: int, timeout: int):
    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "temperature": 1.0,
            "top_p": 0.95,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
    ).encode()
    req = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}
    )
    t0 = time.perf_counter()
    ttft = None
    n_chunks = 0
    text = []
    usage = None
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        for raw in resp:
            line = raw.decode().strip()
            if not line.startswith("data: "):
                continue
            payload = line[6:]
            if payload == "[DONE]":
                break
            obj = json.loads(payload)
            if obj.get("usage"):
                usage = obj["usage"]
            for ch in obj.get("choices", []):
                piece = (ch.get("delta") or {}).get("content") or ""
                if piece:
                    if ttft is None:
                        ttft = time.perf_counter() - t0
                    n_chunks += 1
                    text.append(piece)
    total = time.perf_counter() - t0
    return {
        "ttft_s": ttft,
        "total_s": total,
        "decode_s": (total - ttft) if ttft else None,
        "chunks": n_chunks,
        "usage": usage,
        "text": "".join(text),
    }


def describe_env(container: str) -> None:
    """Stamp what was actually measured. A baseline without the runtime version
    and the serve flags is unusable six weeks later, when the only question that
    matters is 'faster than what, exactly?'."""
    import subprocess

    def sh(cmd: list[str]) -> str:
        try:
            return subprocess.run(
                cmd, capture_output=True, text=True, timeout=60, check=False
            ).stdout.strip()
        except Exception:  # noqa: BLE001
            return "?"

    ver = sh(["docker", "exec", container, "python3", "-c",
              "import vllm,torch;print(vllm.__version__,'|torch',torch.__version__)"])
    args = sh(["docker", "exec", container, "sh", "-c",
               "grep -o 'non-default args:.*' /tmp/vllm-serve.log | head -1"])
    keep = ("speculative_config", "kv_cache_dtype", "block_size", "max_num_seqs",
            "gpu_memory_utilization", "max_model_len")
    flags = [p.strip() for p in args.split(",") if any(k in p for k in keep)]
    print(f"vLLM       : {ver}")
    print(f"kontener   : {container}")
    for f in flags:
        print(f"  {f}")
    print()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://10.10.10.24:5001/v1/chat/completions")
    ap.add_argument("--model", default="deepseek-ai/DeepSeek-V4-Flash-0731")
    ap.add_argument("--max-tokens", type=int, default=256)
    ap.add_argument("--timeout", type=int, default=900)
    ap.add_argument("--container", default="tentaflow-vllm-dspark-5001")
    ap.add_argument("--repeat", type=int, default=1, help="powtorzenia per kontekst")
    ap.add_argument(
        "--contexts",
        default="64,2048,8192,32768",
        help="docelowe dlugosci promptu w tokenach",
    )
    ap.add_argument("--show-text", type=int, default=0, help="ile znakow odpowiedzi")
    args = ap.parse_args()

    describe_env(args.container)
    print(f"{'prompt tok':>10} {'TTFT s':>8} {'decode tok/s':>13} "
          f"{'out tok':>8} {'total s':>8}")
    print("-" * 52)
    for target in [int(x) for x in args.contexts.split(",")]:
        prompt = build_prompt(target)
        for _ in range(max(1, args.repeat)):
            try:
                r = run(args.url, args.model, prompt, args.max_tokens, args.timeout)
            except Exception as exc:  # noqa: BLE001
                print(f"{target:>10}  BLAD: {exc}")
                continue
            u = r["usage"] or {}
            pin = u.get("prompt_tokens", "?")
            out = u.get("completion_tokens", r["chunks"])
            rate = (out / r["decode_s"]) if r["decode_s"] and out else 0.0
            print(
                f"{pin:>10} {r['ttft_s']:>8.2f} {rate:>13.1f} "
                f"{out:>8} {r['total_s']:>8.2f}"
            )
            if args.show_text:
                print(f"    → {r['text'][:args.show_text]}...\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
