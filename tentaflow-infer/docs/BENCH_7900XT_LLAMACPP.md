# FORGE wobec llama.cpp na Radeonie RX 7900 XT (2026-07-27)

Stanowisko: RX 7900 XT (gfx1100, RDNA3, 84 CU, 20 GiB), ROCm 7.2.4.
llama.cpp zbudowany z `llama.cpp-master` (`ff067f76`) pod `-DGGML_HIP=ON
-DAMDGPU_TARGETS=gfx1100`, katalog builda `/mnt/d/lcpp-master-gfx1100`.
Wszystkie pomiary p1024/tg128, `HIP_VISIBLE_DEVICES=1`, karta pod pełnymi
zegarami (2585 MHz rdzeń, 1249 MHz pamięć).

Starszy build `112c7815` NIE wczytuje checkpointu 27B: `missing tensor
'blk.64.ssm_conv1d.weight'` — nie zna bloku NextN. Do MTP potrzebny jest master.

## Qwen3.6-27B (ThinkingCap NVFP4 MTP GGUF, 18,2 GB)

Model w całości rezydentny: 20 067 z 20 464 MiB VRAM, GPU 100% — żadnego
stronicowania przez PCIe. Na 6900 XT ten model NIE WCHODZI (16 GiB).

| | FORGE | llama.cpp | |
|---|--:|--:|---|
| prefill p1024 | 27,3 tok/s | **716,3** tok/s | llama.cpp **26,2x** |
| decode tg128, bez spekulacji | 9,5 tok/s | **33,0** tok/s | llama.cpp **3,5x** |
| decode tg128, MTP po obu stronach | 47,1 tok/s | **73,4** tok/s | llama.cpp **1,56x** |

llama.cpp z `--spec-type draft-mtp`: 73,3 / 73,4 / 73,5 tok/s w trzech
przebiegach; bez MTP 33,2 tok/s, co zgadza się z `llama-bench tg128` (32,93).

## qwen-guard 0,8B Q8_0 (ta sama architektura `qwen35`)

| | FORGE | llama.cpp | |
|---|--:|--:|---|
| prefill p1024 | 2 647,6 tok/s | **18 480,9** tok/s | llama.cpp **7,0x** |
| decode tg128 | **317,3** tok/s | 246,4 tok/s | FORGE **1,29x** |

## Diagnoza: prefill hybrydowy na AMD liczy się jak decode

Prefill FORGE dla 27B jest PŁASKI względem długości promptu — 27,3 tok/s przy
p128, 27,5 przy p512, 27,3 przy p1024. Prefill, który nie przyspiesza z długością
wsadu, nie amortyzuje odczytu wag: każdy chunk czyta cały komplet 18,2 GB.

Powód jest strukturalny. Hybrydowy prefill layer-major (T32/T128) stoi na
kernelach `mma`/`ldmatrix` i jest zabramkowany predykatem
`hybrid_prefill_t128_backend_capable(vendor, warp) == vendor == Nvidia`. Na AMD
`hybrid_prefill_nvfp4_chunk_limit` zwraca **16**, więc 1024 tokeny to 64 przebiegi
po wszystkich wagach. Ta sama bramka tłumaczy obie luki: 7x na 0,8B i 26x na 27B —
to jeden defekt, nie dwa.

Potwierdzenie od drugiej strony: MTP daje w FORGE **4,96x** (9,5 → 47,1 tok/s).
Spekulacja mnoży przepustowość niemal liniowo tylko wtedy, gdy krok jest
zdominowany przez STAŁY narzut na krok, a nie przez liczenie — czyli dokładnie
to samo, co widać w płaskim prefillu.

Model GĘSTY nie ma tego problemu: Bielik-7B NVFP4 robi na tej karcie 1 521 tok/s
prefillu. Wąskim gardłem jest ścieżka hybrydowa (`qwen35`/DeltaNet) na AMD, nie
rozmiar modelu i nie kwantyzacja.

## Wniosek

Największa dźwignia na AMD to NIE kolejne kernele WMMA, tylko zdjęcie bramki
`Vendor::Nvidia` z hybrydowego prefillu — przeniesienie ścieżki layer-major na
WMMA (`src/arch_wmma.mojo` daje już prymityw 16x16x16, zmierzone 98 TOPS int8 i
102 TFLOPS f16). Dopóki tego nie ma, każdy model `qwen35` na Radeonie liczy
prefill tak jak decode.

Kolejność przenoszenia rodzin GEMM na WMMA według tego, co realnie blokuje:
1. ścieżka layer-major hybrydowego prefillu (odblokowuje T32/T128 na AMD),
2. `gemm_nvfp4_dot4_*` (Bielik, Qwen 27B),
3. `gemm_q4_k_dot4_*` (OLMoE i pozostałe GGUF K-quant).

## Zastrzeżenia metodyczne

- `llama-bench tg128` dekoduje z PUSTYM kontekstem, a `forge bench` po prompcie,
  więc porównanie decode jest przechylone NA KORZYŚĆ llama.cpp. Przy 26x i 3,5x
  różnicy nie zmienia to wniosku, ale przy 1,29x na 0,8B — może.
- Porównanie MTP-do-MTP zestawia `llama-cli --spec-type draft-mtp` (prompt ~10
  tokenów) z `forge bench --speculative mtp` (prompt 128). Oba są zdominowane
  przez decode, ale nie jest to ten sam kształt.
- OLMoE-1B-7B Q4_K_M NIE wczytał się w llama.cpp z `-ngl 99` (na CPU wchodzi),
  więc dla MoE nie ma tu punktu odniesienia.
