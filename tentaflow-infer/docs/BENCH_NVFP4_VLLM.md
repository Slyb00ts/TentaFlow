# FORGE a vLLM 0.25.1: NVFP4 Bielik na RTX 4090

**Data:** 2026-07-20

**GPU:** NVIDIA GeForce RTX 4090, Ada `sm_89`, 24 564 MiB

**Sterownik:** 610.43.02

**vLLM:** 0.25.1, wersja zweryfikowana wewnątrz kontenera

**Mojo:** nightly 26.5, `1.0.0b3.dev2026071614`

## Zakres porównania

Oba silniki używały tych samych plików checkpointu:

```text
.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/
snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7/
```

Jest to eksport `compressed-tensors` w formacie `nvfp4-pack-quantized`: wagi
4-bitowe są pakowane w `U8`, skale blokowe mają format `F8_E4M3`, skale globalne
`F32`, a normy, embeddingi i `lm_head` pozostają w `BF16`.

RTX 4090 nie ma natywnych rdzeni tensorowych NVFP4. vLLM wybiera na tej karcie
`MarlinNvFp4LinearKernel` w trybie weight-only. FORGE zachowuje źródłowe wagi
NVFP4 dla decode, a w trybie `FORGE_GEMM=fp8mod-ffn` przepakowuje na GPU wybrane
projekcje do FP8 i wykonuje prefill na rdzeniach tensorowych FP8. Jest to zatem
porównanie tego samego checkpointu i jakości modelu, ale różnych strategii
wykonania na Ada.

`llama.cpp` nie został ujęty w tabeli, ponieważ użyta wersja nie ładuje bezpośrednio
tego checkpointu `compressed-tensors` NVFP4. Konwersja do GGUF zmieniłaby format
wag i przestałaby być porównaniem tego samego artefaktu.

## Protokół

- Jeden strumień, rozgrzany proces i wyłączony prefix cache.
- Prefill: 4096 tokenów wejściowych.
- Decode: 256 żądanych tokenów; FORGE raportuje 255 przejść dekodera po pierwszym
  tokenie.
- Temperatura 0 i ten sam checkpoint oraz tokenizer.
- FORGE: `FORGE_GEMM=fp8mod-ffn`, domyślna ścieżka GQA 4:1 i dwugłowicowy kernel
  łączenia wyników `combine2`.
- vLLM: CUDA Graphs włączone, `--max-num-seqs 32`,
  `--gpu-memory-utilization 0.85` i `--max-model-len 8192`.
- Wyniki po rozgrzaniu; procesy były uruchamiane osobno na tej samej karcie.

Polecenie FORGE:

```bash
FORGE_GEMM=fp8mod-ffn target/release/forge bench <checkpoint> \
  --prompt-tokens 4096 --tokens 256 --reps 5 --prefix-cache off
```

Uruchomienie serwera vLLM:

```bash
docker run --rm --gpus all --privileged -v /dev:/dev --ipc=host \
  -v .runtime/models:/models:ro -p 8000:8000 <obraz-vllm-0.25.1> \
  <checkpoint-w-kontenerze> --served-model-name bielik-nvfp4 \
  --gpu-memory-utilization 0.85 --max-model-len 8192 --max-num-seqs 32
```

Pomiar vLLM:

```bash
vllm bench serve --backend openai --base-url http://127.0.0.1:8000 \
  --model bielik-nvfp4 --tokenizer <checkpoint> --dataset-name random \
  --random-input-len 4096 --random-output-len 256 \
  --num-prompts 6 --max-concurrency 1 --ignore-eos
```

## Surowy wynik końcowy

| Silnik | Prefill pp4096 | Decode | Różnica FORGE do vLLM |
|---|---:|---:|---:|
| FORGE, hybrydowy FP8 + GQA `combine2` | **10 302,7 tok/s** | **143,100 tok/s** | prefill **+5,85%**, decode **-2,24%** |
| vLLM 0.25.1, Marlin NVFP4 | 9 732,9 tok/s | 146,372 tok/s | punkt odniesienia |

FORGE przekracza vLLM w prefill o 569,8 tok/s. W decode brakuje 3,272 tok/s,
czyli około 2,2%. Cel wydajnościowy został więc osiągnięty dla prefill, ale nie
dla decode.

Baseline FORGE przed zmianami wynosił około 4 371 tok/s dla prefill i 130,8 tok/s
dla decode. Oznacza to około 2,36 raza szybszy prefill oraz około 9,4% szybszy
decode względem początkowej implementacji.

## Zmiany składające się na wynik

- **Hybrydowy prefill FP8:** Mojo przepakowuje na GPU projekcje Q, O,
  `gate`, `up` i `down` z NVFP4 do rezydentnych wag FP8. Projekcje K/V pozostają
  NVFP4. `lm_head` jest pakowany z F16 do FP8 dla pojedynczego strumienia.
- **Decode GQA 4:1:** jeden CTA obsługuje cztery głowice Q i współdzieli odczyt
  K/V; `combine2` łączy po dwie głowice. Ścieżka jest ograniczona do
  `head_dim=128`, GQA 4:1, KV F16 i braku Q/K norm.
- **Małe batche NVFP4:** warianty B4, B8 i B16 współdzielą odczyt wag, a BM32
  obsługuje następny przedział liczby sekwencji. Pozostałe kształty używają
  dotychczasowych kerneli.
- **Bezpieczny fallback:** przed alokacją sprawdzane są możliwości urządzenia,
  obsługiwane kształty, obecność artefaktów i dostępny VRAM. Niespełnienie warunku
  pozostawia model na ścieżce NVFP4; nie powoduje częściowej konwersji wag.

Pakowanie odbywa się jednorazowo podczas ładowania modelu. Pomiar całego
przepakowania NVFP4 do FP8 na RTX 4090 wyniósł około 49 ms. Zweryfikowana jakość
hybrydowej ścieżki: `NLL=1,15248`, `PPL=3,1660`; baseline NVFP4:
`NLL=1,15260`, `PPL=3,1664`. Test kanonicznego promptu dla Warszawy przeszedł.

## Profil i odrzucone warianty

Nsight Systems wskazał po optymalizacji decode główne koszty w projekcjach
NVFP4 połączonych z normą/SwiGLU, residualach, normie i attention. W całym kroku
pozostaje około 243 uruchomień kerneli, więc dalsza poprawa decode wymaga przede
wszystkim fuzji lub zmniejszenia kosztu dominujących projekcji, nie pracy CPU.

Odrzucono po pomiarze między innymi większą liczbę splitów attention, BK64,
RPB24, wariant dwóch grup na lane, materializację wag do gęstego formatu oraz
wczesny prototyp Marlin. Każdy z tych wariantów był wolniejszy lub neutralny;
nie pozostał w ścieżce wykonania.

## Ograniczenia sprzętowe

- HAL projektu obsługuje obecnie wyłącznie CUDA. Nie ma backendu ROCm/HIP dla AMD
  ani Metal dla Apple, dlatego przenośność tych zmian na te platformy nie została
  zaimplementowana ani zmierzona.
- Natywne instrukcje FP4 architektury Blackwell nie są zaimplementowane. Obecna
  ścieżka Ada korzysta z programowego NVFP4 i tensorowych obliczeń FP8.
- Fallback możliwości chroni uruchomienie na nieobsługiwanej konfiguracji CUDA;
  nie jest substytutem backendu wieloproducentowego.

## Werdykt

Na RTX 4090 FORGE z kernelami Mojo przekracza vLLM 0.25.1 w prefill pp4096 o
5,85%. Decode pozostaje około 2,24% wolniejszy. Wynik nie potwierdza jeszcze
realizacji celu „szybciej w prefill i decode”, a obsługa AMD, Metal i natywnego
NVFP4 na Blackwell pozostaje otwartą pracą.
