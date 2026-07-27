# Dokładanie nowej karty do FORGE

Procedura jest ta sama dla RDNA4 (`gfx1201`, Radeon AI PRO R9700), Blackwella
(`sm_120`, `sm_121`) i każdej kolejnej. Cztery kroki, każdy z własną bramką.

## 0. Skąd się bierze podział na architektury

Artefakt kernela jest inny w każdej rodzinie:

| rodzina | artefakt | przenośność |
|---|---|---|
| NVIDIA | PTX (tekst) | **w górę** — sterownik kompiluje JIT, zestaw `sm_89` działa na `sm_121` |
| AMD | HSACO (code object) | **żadna** — obraz jest związany z konkretnym ISA |

Stąd asymetria w `select_embedded_set` (`crates/forge-kernels/src/registry.rs`):
NVIDIA dostaje najwyższy zestaw nie nowszy od karty, AMD wyłącznie dokładne
dopasowanie. Cubiny (SASS) są wyjątkiem po stronie NVIDII — wymagają dokładnie
`sm_89`, bo nie niosą przenośnego PTX.

## 1. Zbuduj katalog kerneli na tej karcie

```bash
cd tentaflow-infer/kernels/mojo
# Wybór karty: HIP_VISIBLE_DEVICES dla AMD, CUDA_VISIBLE_DEVICES dla NVIDII.
HIP_VISIBLE_DEVICES=1 pixi run python scripts/build_kernel_catalog.py
```

Skrypt sam wykrywa architekturę (`ctx.arch_name()`) PRZED kompilacją, odsiewa
kernele spoza zasięgu i publikuje `build/<arch>/` atomowo.

**Jeżeli build padnie na pojedynczym kernelu**, masz do rozstrzygnięcia jedną
rzecz: czy ten kernel MA działać na tej karcie.

- **Nie ma** (stoi na `mma`/`ldmatrix` NVIDII, na WMMA, na FP8) → to nie jest
  usterka, tylko brakująca deklaracja. Dopisz ją w `build_kernels_catalog.mojo`
  nad rejestracją:

  ```mojo
      # arch: amd:gfx11+
      _ = ctx.compile_function[
          gemm_q8_0_wmma_64x128, dump_asm=Path("gemm_q8_0_wmma_64x128.ptx")
      ]()
  ```

  Gramatyka: lista po przecinku, człon to `nvidia` / `amd` albo
  `nvidia:sm_89+` / `amd:gfx11+` (`+` = to pokolenie i nowsze, bez `+` =
  dokładnie to). Brak komentarza znaczy PRZENOŚNY i taki kernel musi zbudować
  się wszędzie. AMD porównuje się po POKOLENIU: `gfx1030` i `gfx1036` to jedno
  (RDNA2), `gfx1100` to następne.

- **Ma działać, a nie działa** → to usterka portu, nie zasięg. Na czas bring-upu
  `FORGE_KERNEL_BUILD_PARTIAL=1` zapisuje takie kernele do
  `build/<arch>/unsupported.txt` i publikuje niepełny zestaw. Ta lista jest
  BACKLOGIEM, nie stanem docelowym — port jest domknięty, gdy build przechodzi
  BEZ tej flagi.

Nie zgaduj zasięgu z tego, że coś się nie kompiluje. To dwie różne rzeczy i
mylenie ich jest dokładnie tym, co ten mechanizm ma wykluczyć.

## 2. Wpisz listę artefaktów do binarki

```bash
pixi run python scripts/sync_embedded_arch.py gfx1201
```

Przepisuje listę wprost z manifestu do `registry.rs`, więc zestaw nie może
rozjechać się z buildem.

## 3. Dopisz wiersz do `EMBEDDED_SETS`

W `crates/forge-kernels/src/registry.rs`:

```rust
EmbeddedSet {
    arch: "gfx1201",
    manifest: EMBEDDED_MANIFEST_GFX1201,
    artifacts: EMBEDDED_GFX1201,
    name: "EMBEDDED_GFX1201",
},
```

plus `const EMBEDDED_MANIFEST_GFX1201` obok pozostałych. To wszystko — reszta
wyboru jest tabelaryczna.

Dla NVIDII nowszej od `sm_89` **nie trzeba nic robić**: PTX dociera tam sam.
Własny zestaw ma sens dopiero, gdy chcesz kerneli zawężonych do tej generacji
(np. FP4 Blackwella).

## 4. Bramki

```bash
# zasięg i zgodność katalogu z KAŻDYM zbudowanym manifestem
cd kernels/mojo/scripts && python3 -m unittest test_build_kernel_catalog

# wybór zestawu, asymetria NVIDIA/AMD
cargo test --release -p forge-kernels --lib registry

# kernele na realnej karcie
HIP_VISIBLE_DEVICES=1 cargo test --release -p forge-kernels \
    --no-default-features --features hip
```

`test_catalog_matches_committed_manifest` sprawdza KAŻDY katalog w `build/`
przeciw zasięgowi jego architektury, więc dodanie kernela bez przebudowania
któregoś z zestawów zapala się od razu.

## 5. Czego NIE zakładać o nowej karcie

Zmierzone na RX 7900 XT (RDNA3), gdy dokładaliśmy ją obok RX 6900 XT (RDNA2) —
oba punkty kosztowały pół dnia i żadnego nie dałoby się przewidzieć z papieru:

- **Nowsza karta bywa WOLNIEJSZA w instrukcji, na której stoi silnik.** int8
  `v_dot4_i32_i8` spadł z 97 do 43 TOPS między RDNA2 a RDNA3, bo RDNA3 przenosi
  ciężar na WMMA. Zmierz `bench-amd/bench_dot4_i8.mojo`, `bench_dot2_f16.mojo`
  i `bench_wmma_gfx11.mojo` ZANIM uznasz, że port jest zrobiony.
- **Instrukcja może się złożyć i policzyć co innego.** Asembler przyjmuje
  `v_dot4_i32_i8` na gfx11 i wykonuje ją jako wariant BEZ ZNAKU: iloczyn
  `(-1,-2,-3,-4)·(4,3,2,1)` dawał 2540 zamiast −20, bez jednego ostrzeżenia.
  Każdy mikrobenchmark instrukcji MUSI mieć przypadek z wartościami ujemnymi —
  pomiar samej przepustowości przechodził przy zepsutej instrukcji.

Dobra bramka na taki rozjazd: ten sam prompt na dwóch kartach musi dać
IDENTYCZNĄ sumę SHA wygenerowanych tokenów (`forge bench --prefix-cache off`).
