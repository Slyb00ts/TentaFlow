# =============================================================================
# Plik: gen_tiny_whisper.py
# Opis: Buduje MAŁY checkpoint Whispera w formacie mlx-whisper (nazewnictwo
#       OpenAI, kwantyzacja affine) i zapisuje wyjście jego enkodera policzone
#       przez SAM mlx-whisper. To wyrocznia dla referencyjnego forwardu CPU:
#       duży model liczyłby się na CPU minutami, a mały daje tę samą matematykę
#       w ułamku sekundy.
# Użycie: ./mlxenv/bin/python gen_tiny_whisper.py <katalog-wyjściowy>
# =============================================================================

import json
import struct
import sys
from pathlib import Path

import mlx.core as mx
import mlx.nn as nn
from mlx.utils import tree_flatten
from mlx_whisper.whisper import ModelDimensions, Whisper

# Wymiary dobrane tak, żeby przejść wszystkie ścieżki (wielogłowicowa uwaga,
# cross-attention, dwie warstwy enkodera i dekodera) i zmieścić się w teście.
DIMS = ModelDimensions(
    n_mels=16,
    n_audio_ctx=8,
    n_audio_state=64,
    n_audio_head=4,
    n_audio_layer=2,
    n_vocab=51,
    n_text_ctx=6,
    n_text_state=64,
    n_text_head=4,
    n_text_layer=2,
)
GROUP_SIZE = 64
BITS = 4
SEED = 20260802


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)

    mx.random.seed(SEED)
    model = Whisper(DIMS, dtype=mx.float16)

    # Wagi z generatora o ustalonym ziarnie: test ma być odtwarzalny co do bitu.
    def randomize(module):
        for name, value in module.parameters().items():
            if isinstance(value, mx.array):
                module.update({name: mx.random.normal(value.shape).astype(mx.float16) * 0.05})
    model.apply_to_modules(lambda _, m: randomize(m))

    # Kwantyzacja dokładnie tą samą funkcją, której używa konwerter mlx-whisper.
    nn.quantize(model, group_size=GROUP_SIZE, bits=BITS)
    mx.eval(model.parameters())

    weights = dict(tree_flatten(model.parameters()))
    mx.save_safetensors(str(out / "model.safetensors"), weights)

    config = {
        **DIMS.__dict__,
        "quantization": {"group_size": GROUP_SIZE, "bits": BITS},
        "model_type": "whisper",
    }
    (out / "config.json").write_text(json.dumps(config, indent=1))
    (out / "generation_config.json").write_text(
        json.dumps(
            {
                "decoder_start_token_id": 42,
                "eos_token_id": 41,
                "is_multilingual": True,
                "suppress_tokens": [1, 2],
                "begin_suppress_tokens": [3],
            },
            indent=1,
        )
    )

    # Wejście enkodera: mel [n_mels, n_audio_ctx * 2], deterministyczne i o tym
    # samym wzorze, który odtwarza test po stronie Rusta.
    n_in = DIMS.n_audio_ctx * 2
    mel = mx.array(
        [
            [((c * 37 + t * 11) % 101) / 101.0 - 0.5 for t in range(n_in)]
            for c in range(DIMS.n_mels)
        ],
        dtype=mx.float16,
    )
    # mlx-whisper oczekuje [batch, czas, mele].
    enc = model.encoder(mel.T[None])
    mx.eval(enc)

    values = enc.astype(mx.float32).reshape(-1)
    with (out / "encoder_out.bin").open("wb") as f:
        f.write(b"WENC")
        f.write(struct.pack("<III", 1, DIMS.n_audio_ctx, DIMS.n_audio_state))
        f.write(bytes(memoryview(values)))

    print(f"zapisano {out}")
    print(f"  tensorów: {len(weights)}")
    print(f"  wyjście enkodera: {enc.shape}, zakres {enc.min().item():.4f}..{enc.max().item():.4f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
