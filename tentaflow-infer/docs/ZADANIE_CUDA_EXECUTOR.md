# Zadanie: `CudaExec` — trzeci wykonawca tego samego kontraktu

Dla agenta na maszynie z NVIDIA (DGX Spark, GB10, `sm_121a`). Na maszynie, na
której powstały kroki 1–4, nie ma karty NVIDIA — i to jedyny powód, dla którego
to zadanie tu leży zamiast być zrobione.

Kontekst i uzasadnienie całości: `docs/ARCHITEKTURA_DOCELOWA.md`. Ten plik jest
tylko instrukcją wykonawczą.

## 1. Co już jest

| co | gdzie | stan |
|---|---|---|
| słownictwo operacji | `crates/forge-graph/src/lib.rs` | `Op`, `Act`, `WeightId`, `Executor`, `WeightStore`, `ExecSpec`, `Tile` |
| model gęsty | `crates/forge-model/src/dense.rs` | emituje `Vec<Op>`, ZERO buforów, zero HAL-a |
| wykonawca Metal | `crates/forge-kernels/src/dense_exec.rs` | wzór do naśladowania |
| wzorzec hostowy | `crates/forge-kernels/src/host_exec.rs` | **wyrocznia** — działa wszędzie, też na Sparku |
| rejestr wariantów | `crates/forge-kernels/src/variant.rs` | formy z predykatem i pomiarem; dziś tylko wpisy Apple |

Commity: `66d1c675` (forge-graph), `6b8e9045` (wykonawca pod granicę),
`d52c95d0` (wzorzec hostowy).

## 2. Kamień milowy 1 — i jego rozmiar

**`CudaExec` liczący JEDNĄ sekwencję, dla GGUF Q4_K_M, bez ani jednego nowego
kernela.** Nie wsad, nie stronicowane KV, nie spekulacja, nie tensor parallel.

Nowych kerneli nie trzeba, bo każda operacja słownictwa ma już launcher w
`crates/forge-kernels/src/launchers/`:

| `Op` | launcher |
|---|---|
| `MatMul` (Q4_K / Q6_K) | `gemv_q4_k_f16`, `gemv_q6_k_f16`, `gemm_q4_k_f16`, `gemm_q6_k_f16` (`gemm/quantized/k_quants.rs`) |
| `RmsNorm` | `rmsnorm_f16` (`norm.rs`) |
| `Rope` | `rope_neox_f16` (`attention.rs`) — nasz RoPE jest NeoX, patrz §5 |
| `Attention` | `attn_decode_f16` (`attention.rs`) |
| `SiluMul`, `Residual` | `elementwise.rs` |
| `Embed` | `gather_q4_k_row_f16` (`k_quants.rs`) |
| `argmax` | `sample.rs` |

Czyli praca to **wpięcie**, a nie pisanie matematyki. Jeśli okaże się inaczej,
to jest wynik wart zapisania — nie powód, żeby dopisywać kernele w tym kroku.

## 3. Decyzja, którą trzeba podjąć NAJPIERW

`Dense::load` przepuszcza dziś każdą wagę przez `to_affine_triple`, bo tego chcą
kernele Metalowe: trzy osobne tablice (nibble, skale, przesunięcia). **Kernele
CUDA chcą czegoś innego — bloków GGUF w oryginalnym układzie.**

Dwie drogi:

- **(A) Przepakowanie w wykonawcy.** `CudaExec::put_affine` składa trójkę z
  powrotem w bloki Q4_1/Q4_K. Nic nie zmienia w kontrakcie, kosztuje czas
  ładowania i JEST STRATNE dla Q6_K — sześciu bitów nie da się włożyć w Q4_1.
- **(B) `WeightStore` przyjmuje postać ŹRÓDŁOWĄ** (bajty + `QuantKind` + wymiary),
  a każdy wykonawca sam decyduje, co z nią robi: Metal woła `to_affine_triple`,
  CUDA zostawia bloki. Model przestaje decydować o przepisaniu, czego i tak nie
  powinien robić.

**Rekomendacja: (B).** Jedna pułapka: permutacja wierszy Q/K dla GGUF-a
(`permute_rope_rows`) działa dziś na trójce afinicznej. Przy (B) trzeba jej
odpowiednika na układzie natywnym — dla Q4_K to przestawienie CAŁYCH wierszy
bloków, więc należy do `forge-formats`, nie do wykonawcy. Bez tego model
generuje płynne bzdury i żaden test kształtu tego nie złapie.

## 4. Jak to sprawdzić — i to jest właściwa część zadania

Wyrocznia jest w repo i **działa na Sparku bez żadnej karty**:

```bash
cd tentaflow-infer
cargo test --release -p forge-model --test host_vs_mlx -- --nocapture
```

Potrzebuje checkpointu w `<repo>/.runtime/models/` (patrz ścieżki w
`crates/forge-model/tests/common/mod.rs`). Na Apple daje:

```
krok 1: 3.34% rozpiętości, argmax 24666
krok 2: 1.17% rozpiętości, argmax 15625
```

Ścieżka Metalowa na TEJ SAMEJ wyroczni daje 2,71% i 1,21%, z tymi samymi
tokenami. **To są liczby, w które ma trafić CUDA.**

Test do napisania — `crates/forge-model/tests/cuda_vs_reference.rs`:

1. ten sam checkpoint, `Dense::load(path, |spec| CudaExec::new(device, spec))`,
2. dwa kroki wobec fikstury mlx-lm (`common::spread_error`, `common::top_k`) —
   argmax i czołowa trójka MUSZĄ się zgadzać, błąd < 5% rozpiętości,
3. ten sam `Dense` na `HostExec` w tym samym przebiegu, logit po logicie.
   Wzorzec liczy w f32, więc próg jest luźniejszy niż bitowy — ale rozjazd
   formuły wychodzi z niego natychmiast.

Punkt 3 jest ważniejszy niż punkt 2: fikstura ma pięć kroków jednego promptu,
a wzorzec odpowie na dowolne wejście.

Regresja: `cargo test -p forge-engine` musi zostać zielone. `CudaExec`
**dokłada** ścieżkę, niczego dziś nie zastępuje.

## 5. Rzeczy, które już raz kosztowały płynny, zły tekst

Wszystkie dały poprawnie wyglądające zdania i żadna nie dała awarii:

- **Konwencja RoPE.** Nasze kernele obracają połówki (NeoX). GGUF trzyma wiersze
  Q/K w kolejności llama.cpp (przeplatane pary). Warunek jest w
  `dense.rs`: architektura mówi, że wymaga, źródło mówi, że jeszcze nie zrobiło.
  Nałożenie permutacji dwa razy wygląda tak samo źle jak zero razy.
- **Grupa kwantyzacji to własność WAGI, nie modelu.** Q4_K_M ma 32 na większości
  wag i 16 na `attn_v`/`ffn_down`/głowie. Jeden model, dwie grupy i dwie
  szerokości kodu naraz.
- **Typ skal to co innego niż typ wag normalizacji.** W MLX oba są bf16, w GGUF
  skale f16 a normy f32. Zlanie ich w jedno wygląda na uproszczenie.
- **Długość kontekstu to co innego niż pojemność cache'u.** Wzięcie jednej za
  drugą adresuje głowice od złego wiersza i nie zmienia rzędu wielkości wyniku.

## 6. Czego NIE robić w tym kroku

- **Nie dotykać `forge-engine/src/model/arch/dense.rs`.** Nic tam jeszcze nie
  kasujemy. Kasowanie ma sens dopiero, gdy `Op` wyrazi lane'y wsadu i
  stronicowane KV, a fuzja stanie się passem — czyli po kamieniu milowym 2.
- **Nie rozszerzać `Op` „na zapas".** Szersze słownictwo pozwala modelowi wyrazić
  rzeczy, których żaden backend nie liczy, a kompilator tego nie powie. Nowy
  wariant dochodzi wtedy, gdy jest wykonawca, który go wykonuje, i test, który
  to pokazuje.
- **Nie wpisywać wariantów do `variant.rs` bez POMIARU.** Każda forma niesie
  liczbę, która ją tam postawiła. Wpis bez pomiaru to zgadywanie z pozorem
  danych.

## 7. Po kamieniu milowym 1

W tej kolejności, każdy krok z własnym testem:

1. **Lane'y wsadu** w `Op` (`tokens` → `[B, T]`) — bo `prefill_forward_lanes`
   silnika i `batched_decode` bez tego nie mają jak powstać.
2. **Stronicowane KV** jako kontrakt wykonawcy, nie modelu: `KvAppend` i
   `Attention` już dziś biorą tylko numer warstwy i pozycję, więc stronicowanie
   może zostać po stronie `CudaExec`. Sprawdzić, czy naprawdę może.
3. **Fuzja jako pass nad `Vec<Op>`** — `gemv_norm_*`, `gemv_norm_silu_*`,
   `gemv_residual_*` istnieją i są tym, czym silnik dziś ręcznie składa trzy
   łańcuchy dekodowania. Pass rozpoznaje parę `RmsNorm`+`MatMul` i
   `MatMul`+`Residual` i podmienia je na operację scaloną.
4. **Dopiero teraz** silnik może zacząć chudnąć, bo dopiero teraz kontrakt
   wyraża to, co on robi.

## 8. Konwencje repozytorium

- Komentarze, nazwy, testy, commity: **angielski**. Format: `[typ]: opis`.
- **Żadnej atrybucji AI** — ani w commitach, ani w PR, ani w kodzie.
- Commit na `main` i `git push` po każdym.
- `cargo run -p xtask -- lint` musi zostać na 36 naruszeniach i 0 nieaktualnych
  wpisach. Zapadka może tylko spadać.
- Nie zgłaszać niczego jako zrobione bez uruchomienia. Wynik niesprawdzony
  opisać jako niesprawdzony.
