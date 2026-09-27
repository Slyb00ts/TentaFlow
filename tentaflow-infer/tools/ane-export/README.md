# ane-export — eksport wycinków wag do modeli CoreML dla ANE

Narzędzie z zadania 1.1 planu EKS-A10. Z checkpointu MLX 4-bit (affine, grupa 64)
wycina **ogon wierszy** każdej projekcji (część liczona przez ANE) i pakuje go do
skompilowanego modelu CoreML (`.mlmodelc`) z wejściem `x` i wyjściem `y`.
Bez torch — własny parser nagłówka safetensors + `np.memmap`.

**Kodowanie główne: `per_channel_int8`** — wagi MLX zdekwantyzowane do f32 i
re-kwantyzowane symetrycznie na wiersz wyjściowy (int8, skala f16, bez zero-pointu),
w MIL `constexpr_affine_dequantize` (iOS16, `axis=0`, `zero_point=0`). To jedyna
postać (obok int4 per-channel, za stratnego: 28% błędu), którą kompilator ANE
wykonuje w 100% na ANE — patrz `docs/pomiary/eks-a10-faza0-ane-m1.md` §0.1.

**Kodowanie `blockwise`** (uint4 + skala/offset f16 na grupę 64,
`constexpr_blockwise_shift_scale`) jest numerycznie bit-exact z checkpointem
(poza zaokrągleniem `-bias/scale` do f16), **ale CoreML wykonuje `linear` z takimi
wagami na CPU (BNNS), nie na ANE** — 13–26% prędkości int4 per-channel (raport fazy
0, §0.1a/0.1b/0.1d: na CPU spada każda grupa, każdy offset, LUT per grupa, int8 z
grupą). **Nie używać do ANE**; zostaje jako kontrola numeryczna układu wag.

## Wymagania

- macOS 15+, Xcode CLT (`xcrun coremlcompiler`)
- Python 3.12, `coremltools>=9.0`, `numpy` (venv jak dla `tools/eks-apple`)

## Użycie

```bash
nice -n 10 python ane_export.py <checkpoint_dir_lub_plik.safetensors> <out_dir> \
  --share 0.6 --shapes 256,512,1024 --groups gate_up,down \
  --encoding per_channel_int8 --multifunction --verify
```

| Opcja | Znaczenie |
|-------|-----------|
| `--share S` | udział wierszy liczonych przez ANE; `ane_rows = floor(rows*S/64)*64`, wycinek to wiersze `[rows-ane_rows, rows)` |
| `--shapes` | liczby wierszy T wejścia (stałe kształty `[T, K]` f16) |
| `--groups` | `gate_up` (K=4096), `down` (K=11264), `qkv` (K=4096), `o` (K=4096); części grupy dzielą wejście, wyjście to `concat` po kolumnach |
| `--encoding` | `per_channel_int8` (domyślne, ANE 100%), `blockwise` (bit-exact, ale CPU — nie do ANE), `per_channel` (int4 per-channel przez `linear_quantize_weights`; ANE 100%, ale stratne, rel_L2 ~0,2) |
| `--layers` | `0-31`, `0,5,7`; domyślnie wszystkie z checkpointu |
| `--workers N` | równoległe zadania (warstwa, grupa) w procesach |
| `--multifunction` (domyślne) | jeden `L{nn}_{grupa}.mlmodelc` z funkcjami `T256`, `T512`, `T1024` (wagi raz) |
| `--per-shape` | osobne `L{nn}_{grupa}_T{T}.mlmodelc` (funkcja `main`) |
| `--verify` | kontrola numeryczna (patrz niżej); `--verify-layers 0,20,39` wybiera warstwy |
| `--no-plan` | pomiń sprawdzenie przydziału operacji (`MLComputePlan`) po kompilacji |
| `--keep-mlpackage` | zostaw źródłowe `.mlpackage` w `out_dir/mlpackage/` |

## Kodowania — semantyka

- `per_channel_int8`: `w32 = q_mlx*scale + bias` (f32) → `s = max|w32_row|/127`,
  `q = round(w32/s)` ∈ [−127, 127] int8 → `constexpr_affine_dequantize(quantized_data=q,
  zero_point=0 (int8 [N]), scale=s (f16 [N]), axis=0)`. Blob int8: 2× rozmiar int4.
- `blockwise`: `constexpr_blockwise_shift_scale` liczy `w = scale*(data − offset)`, MLX
  przechowuje `w = q*scale + bias`, stąd `offset = −bias/scale` (f16; `scale==0` → 0).
  `data` to nibble jako `uint8` z dtype `types.np_uint4_dtype` (pakowane do 4 bitów),
  najmłodsze bity słowa uint32 = najwcześniejsza waga (jak `crates/forge-formats/src/mlx.rs`).
  Błąd dekompresji vs numpy: `max|Δw|` 3,05e-5 (gate/up), 6,1e-5 (down).

## Sprawdzenie planu (`MLComputePlan`)

Po kompilacji narzędzie ładuje plan (`ct.models.compute_plan.MLComputePlan`,
`CPU_AND_NE`) i zlicza, gdzie trafia każda operacja. Python API nie wybiera funkcji
multifunction (przydział widać tylko dla domyślnej), więc plan liczony jest na
jednofunkcyjnych kopiach per T (fazy 0 §0.2: funkcje multifunction liczą identycznie).
Wynik w manifeście: `groups[].compute_plan.T{T} = {linear: {ANE: n}, other: {...},
ane_cost_pct}`. Detektor zweryfikowany na modelu blockwise: `linear: {CPU: 1}`, 0%.

## Format `manifest.json`

```jsonc
{
  "version": 1,
  "source": "models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit",
  "source_file": "model.safetensors",
  "source_size_bytes": 4206804396,
  "source_head64mb_sha256": "…",      // sha256 PIERWSZYCH 64 MB pliku (nie całego)
  "encoding": "per_channel_int8", "share": 0.6, "shapes": [256, 512, 1024],
  "d_model": 4096, "inter": 11264, "group_size": 64,
  "n_layers_in_checkpoint": 40, "layers": [0, …, 39], "layout": "multifunction",
  "input_dtype": "fp16", "output_dtype": "fp16",
  "groups": [{
    "layer": 0, "group": "gate_up",
    "model": "L00_gate_up.mlmodelc",          // per_shape: "models": {"256": "L00_gate_up_T256.mlmodelc", …}
    "functions": {"256": "T256", "512": "T512", "1024": "T1024"},
    "input":  {"name": "x", "cols": 4096},
    "output": {"name": "y", "width": 13440},
    "parts": [
      {"proj": "gate", "rows": 11264, "cols": 4096, "ane0": 4544, "ane_rows": 6720, "out_col0": 0},
      {"proj": "up",   "rows": 11264, "cols": 4096, "ane0": 4544, "ane_rows": 6720, "out_col0": 6720}
    ],
    "compute_plan": {"T256": {"linear": {"ANE": 2}, "other": {"ANE": 2}, "ane_cost_pct": 100.0}, …}
  }],
  "sizes_bytes": {"L00_gate_up.mlmodelc": …},
  "timings_s": {"L00_gate_up": {"convert_T256": …, "multifunction": …, "compile": …, "plan": …, "total": …}},
  "export_wall_s": 266.3,
  "verify": [ … ], "verify_wall_s": 8.2     // tylko z --verify
}
```

`proj ∈ {q, k, v, o, gate, up, down}`. Wyjście `y[:, out_col0 : out_col0+ane_rows]`
części odpowiada wierszom `[ane0, ane0+ane_rows)` projekcji `proj`.

## Weryfikacja (`--verify`)

Dla warstw `--verify-layers` (domyślnie 0, 20, 39 — rozkład wag różni się między
warstwami) i każdej grupy, T = najmniejszy kształt, narzędzie buduje jednofunkcyjny
model i liczy wobec referencji = dekwantyzacja MLX grupa-64 w f32:

- **wagi**: `rel_L2(w_model, w_mlx)`, `max|Δw|`, `mean|Δw|` (w_model = to, co model
  zdekwantyzuje, policzone w numpy);
- **wyjście** na `x ~ N(0,1)` i `x ~ Student-t(3)` (ogony jak outliery aktywacji), oba f16:
  `numpy_f32` (czysty błąd kwantyzacji, `x @ w_model^T` w f32), `cpu` (predict
  `CPU_ONLY`), `gpu` (predict `CPU_AND_GPU`). ANE celowo nieużywane (nie zakłóca
  pomiarów na maszynie); przydział na ANE sprawdza `MLComputePlan`.

Uwaga: kernel `linear` na CPU_ONLY akumuluje w fp16 — dla K=4096 dokłada rel_L2 ≈ 6e-3
niezależnie od `compute_precision` (sprawdzone na gęstej stałej f16: CPU 6,2e-3,
GPU 2,1e-4). Stąd kolumna `cpu` jest zawsze gorsza od `numpy_f32`/`gpu`.

### Wynik: per_channel_int8, Bielik-Minitron-7B, share 0,6, T=256

| warstwa | grupa | wagi rel_L2 | max\|Δw\| | y gauss numpy / cpu / gpu | y Student-t3 numpy / cpu / gpu |
|---|---|--:|--:|---|---|
| 0 | gate_up | 8,45e-3 | 4,0e-4 | 8,44e-3 / 1,05e-2 / 8,45e-3 | 8,44e-3 / 1,05e-2 / 8,44e-3 |
| 0 | down | 1,18e-2 | 9,6e-4 | 1,18e-2 / 1,45e-2 / 1,18e-2 | 1,18e-2 / 1,45e-2 / 1,18e-2 |
| 20 | gate_up | 8,46e-3 | 3,5e-4 | 8,46e-3 / 1,05e-2 / 8,47e-3 | 8,46e-3 / 1,05e-2 / 8,46e-3 |
| 20 | down | 1,01e-2 | 1,8e-3 | 1,01e-2 / 1,32e-2 / 1,01e-2 | 1,00e-2 / 1,31e-2 / 1,01e-2 |
| 39 | gate_up | 8,44e-3 | 4,1e-4 | 8,44e-3 / 1,05e-2 / 8,44e-3 | 8,45e-3 / 1,05e-2 / 8,45e-3 |
| 39 | down | 1,18e-2 | 2,9e-3 | 1,17e-2 / 1,44e-2 / 1,17e-2 | 1,16e-2 / 1,43e-2 / 1,16e-2 |

Błąd wyjścia int8 per-channel wobec MLX g64: **0,85% (gate/up), 1,0–1,2% (down)**,
stały między warstwami i niezależny od rozkładu x (Student-t nie pogarsza). Dla
porównania: sama kwantyzacja MLX 4-bit wobec f32 to 1,6e-1 (raport fazy 0 §0.1c),
a int4 per-channel 0,17–0,25.

Blockwise (kontrola, warstwa 0): y gauss cpu 6,3e-3 / gpu 4,8e-4 (gate_up), 8,5e-3 /
4,9e-4 (down); `decompress_weights` max|Δw| 3,05e-5 / 6,1e-5.

## Koszty

Checkpoint ma **40 warstw** (config.json: `num_hidden_layers: 40`; plan zakładał 32).
Share 0,6 → gate/up: 6720 z 11264 wierszy, down: 2432 z 4096.

| element | int8 `.mlmodelc` | int4 (blockwise) | czas eksportu int8 (M1, `nice 10`, 1 worker) |
|---|--:|--:|---|
| `L{nn}_gate_up` (3 funkcje) | 52,6 MiB | 29,6 MiB | ~3,7 s (3× konwersja 0,35 s, multifunction 0,7 s, kompilacja 0,15 s, plan 1,7 s) |
| `L{nn}_down` (3 funkcje) | 26,1 MiB | 14,7 MiB | ~2,2 s |
| warstwa FFN | 78,7 MiB | 44,3 MiB | ~6 s |
| 40 warstw FFN | **3148 MiB (3,07 GiB)** | ≈1,73 GiB | **266 s** (275 s z verify), RSS max 2,4 GB |

Pełny FFN int8 per-channel (share 1,0) ≈ 132 MiB/warstwę, ≈5,1 GiB dla 40 warstw;
wycinek ANE ≈ tyle × share. Multifunction nie duplikuje wag (trzy funkcje dzielą
jeden blob). Pamięć `aned` przy załadowanych 40 warstwach int8: ok. 3,1–3,3 GB —
poza zakresem tego narzędzia (mierzy harness `tools/eks-apple`).

## Pliki wyjściowe

```
out_dir/
├── L00_gate_up.mlmodelc/     # multifunction: T256, T512, T1024
├── L00_down.mlmodelc/
├── …
├── L39_down.mlmodelc/
├── manifest.json
└── mlpackage/                # tylko z --keep-mlpackage
```

Aktualny eksport: `/Users/critix/repos/rust/TentaFlow/.runtime/ane/bielik-minitron-7b-s060-int8/`.
