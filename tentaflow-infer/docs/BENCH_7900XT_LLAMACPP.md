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

| | FORGE (stan wyjściowy) | **FORGE (teraz)** | llama.cpp | |
|---|--:|--:|--:|---|
| prefill p1024 | 27,3 tok/s | **843,9** tok/s | 716,3 tok/s | **FORGE 1,18x** |
| decode tg128, bez spekulacji | 9,5 tok/s | **26,4** tok/s | **33,0** tok/s | llama.cpp 1,25x |
| decode tg128, MTP po obu stronach | 47,1 tok/s | **51,4** tok/s | **73,4** tok/s | llama.cpp 1,43x |

**Prefill urósł 30,9x i wyprzedził llama.cpp**, decode 2,8x. Wyjście jest przez
całą tę drogę BITOWO IDENTYCZNE — ta sama suma SHA 128 tokenów co przed
pierwszą zmianą (`0bf2b86b…`).

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

### Optymalizacja po profilu (rocprofv3)

**Decode, 2,8x.** Profil pokazał, że 82% czasu GPU to JEDEN kernel:
`gemv_nvfp4_gguf_f16` przy 133 GB/s z dostępnych 674. Trzy wady naraz: workgroup
256 wątków na wiersz, choć wiersz ma ~3 kB (redukcja przez cały blok kosztowała
tyle co liczenie), wagi i aktywacje czytane BAJT PO BAJCIE, i droga
dekwantyzacja. Przepisany na falę na wiersz z 16-bajtowymi odczytami i
dekwantyzacją przez konstrukcję bitów: **542 GB/s, 4,8x**. Pułapka e2m1: jedyna
wartość subnormalna (0,5) jest w f16 NORMALNA z zerową mantysą, więc bit mantysy
wolno przepuścić tylko dla E>0 — pierwsza wersja dawała 0,75 zamiast 0,5.

**Prefill, dodatkowe 1,68x (503 → 844).** Podmiana wagi na stałą w kernelu
pokazała, że sama dekwantyzacja to POŁOWA czasu GEMM-u (25 wobec 48 TFLOPS), a
każda z fal wzdłuż tokenów rozpakowywała DOKŁADNIE TE SAME kolumny. Wagi idą
teraz raz na blok do LDS (8 KiB na kafel BN x 64), a sweep kafli wybrał
BM256/BN64 na ośmiu falach: **52 TFLOPS wobec 25**.

ZMIERZONE I ODRZUCONE (wszystkie brzmiały sensownie):
- aktywacja w LDS w GEMV — 493 → 326 GB/s; 32 KiB LDS zabija zajętość, a x i tak
  siedzi w cache,
- dwa wiersze na falę w GEMV — płasko lub gorzej,
- tania dekwantyzacja bitowa w GEMM-ie WMMA — 503 → 439 tok/s; to, co pomaga
  kernelowi ograniczonemu pamięcią, szkodzi ograniczonemu jednostką macierzową.

ZOSTAJE, w kolejności wartości:
1. **decode** — 1,25x luki; NVFP4 GEMV chodzi po 542 z 674 GB/s, czyli jest już
   blisko roofline, a reszta różnicy to narzut **1193 uruchomień kerneli na
   token** (w tym 427 kopii D2D),
2. wąskie projekcje w GEMV trzymają 247-259 GB/s wobec 542 na szerokich —
   ograniczenie latencji przy krótkiej pętli,
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
