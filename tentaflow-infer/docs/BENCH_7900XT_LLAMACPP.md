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

| | FORGE (T16) | FORGE (layer-major WMMA) | llama.cpp | |
|---|--:|--:|--:|---|
| prefill p1024 | 27,3 tok/s | **501,8** tok/s | **716,3** tok/s | llama.cpp **1,43x** |
| decode tg128, bez spekulacji | 9,5 tok/s | 9,5 tok/s | **33,0** tok/s | llama.cpp **3,5x** |
| decode tg128, MTP po obu stronach | 47,1 tok/s | 48,4 tok/s | **73,4** tok/s | llama.cpp **1,52x** |

Kolumna „T16" to stan sprzed przeniesienia ścieżki layer-major na WMMA:
**prefill urósł 18,4x**, a stosunek do llama.cpp spadł z 26,2x do 1,43x. Wyjście
jest bitowo identyczne — ta sama suma SHA 32 tokenów na obu ścieżkach.
Decode pozostaje nietknięty, bo dotyczy go inna ścieżka, i to on jest teraz
największą luką.

llama.cpp z `--spec-type draft-mtp`: 73,3 / 73,4 / 73,5 tok/s w trzech
przebiegach; bez MTP 33,2 tok/s, co zgadza się z `llama-bench tg128` (32,93).

## qwen-guard 0,8B Q8_0 (ta sama architektura `qwen35`)

| | FORGE (dot4) | FORGE (WMMA + layer-major) | llama.cpp | |
|---|--:|--:|--:|---|
| prefill p1024 | 1 679 tok/s | **4 607,2** tok/s | **18 480,9** tok/s | llama.cpp **4,0x** |
| decode tg128 | | **317,2** tok/s | 246,4 tok/s | FORGE **1,29x** |

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

## Co zostało zrobione i co zostaje

ZROBIONE — ścieżka layer-major działa na AMD. Bramka `Vendor::Nvidia` zeszła z
`hybrid_prefill_t128_backend_capable` i z `hybrid_prefill_nvfp4_chunk_limit`;
warunkiem jest teraz JEDNOSTKA MACIERZOWA i fala 32, a o realnej dostępności
rozstrzygają artefakty. Dopisane kernele (wszystkie `# arch: amd:gfx11+`, każdy
z testem złotym wobec referencji CPU):
`gemm_nvfp4_gguf_wmma_f16_{bm32,bm128,bm128_bn32}` i `gemm_q8_0_wmma_triplet_bm64`.
Lista artefaktów T128 i limit chunka są teraz dwurodzinne — kernel NVIDII nie
zastępuje kernela AMD ani odwrotnie, co pilnuje test.
Zejście z flash-attention: `auto` wybierało Mojo FA HD256, która stoi na `mma`;
teraz przy braku artefaktu schodzi na `Exact`, ale JAWNE `...ATTN=fa` nadal jest
błędem — prośba o konkretny wariant nie ma schodzić po cichu.

ZOSTAJE, w kolejności wartości:
1. **decode** — 3,5x luki bez spekulacji i 1,52x z MTP; to teraz największa
   pojedyncza różnica i nie dotyczy jej nic z powyższego,
2. warianty strojone kafla NVFP4 (`sync1`, `prefetch`, `bn128`) nie mają
   odpowiedników WMMA — dispatch AMD używa jednego kafla na zakres,
3. `gemm_q4_k_dot4_*` (OLMoE i pozostałe GGUF K-quant) wciąż na instrukcjach dot.

## Zastrzeżenia metodyczne

- `llama-bench tg128` dekoduje z PUSTYM kontekstem, a `forge bench` po prompcie,
  więc porównanie decode jest przechylone NA KORZYŚĆ llama.cpp. Przy 26x i 3,5x
  różnicy nie zmienia to wniosku, ale przy 1,29x na 0,8B — może.
- Porównanie MTP-do-MTP zestawia `llama-cli --spec-type draft-mtp` (prompt ~10
  tokenów) z `forge bench --speculative mtp` (prompt 128). Oba są zdominowane
  przez decode, ale nie jest to ten sam kształt.
- OLMoE-1B-7B Q4_K_M NIE wczytał się w llama.cpp z `-ngl 99` (na CPU wchodzi),
  więc dla MoE nie ma tu punktu odniesienia.
