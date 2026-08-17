# Łącze RoCE między dwoma GB10: ile daje i co z tego wynika

**Data:** 2026-08-04

**Węzły:** spark-001 (10.10.10.24) i spark-002 / rig25 (10.10.10.25), oba NVIDIA
GB10, 121 GiB pamięci zunifikowanej.

**Łącze:** RoCE v2, ConnectX, HCA `roceP2p1s0f0` port 1, `PORT_ACTIVE`,
`link_layer: Ethernet`. Drugi zestaw portów (`rocep1s0f0`) na osobnej podsieci
10.10.11.0/24.

**Narzędzie:** `cargo run -p forge-rdma --example link_probe --release`

## Wynik

| co | wynik |
|---|--:|
| opóźnienie 64 B, w jedną stronę | **2,18 µs** |
| przepustowość zapisu, 1 MiB | 12,3 GB/s |
| przepustowość zapisu, 4 MiB | 12,3 GB/s |
| przepustowość zapisu, 16 MiB | 12,3 GB/s |
| przepustowość zapisu, 64 MiB | **12,3 GB/s** |

12,3 GB/s to około 98 Gb/s, czyli łącze pracuje jako 100 GbE. Przepustowość jest
płaska od 1 MiB w górę, więc powyżej tego rozmiaru nie ma już czego zbierać
większymi transferami — liczy się tylko liczba przekroczeń granicy.

## Co z tego wynika dla podziału modelu

DeepSeek V4 Flash ma `hidden_size` 4096 i 43 warstwy, więc aktywacja na granicy
warstwy to 8 KiB na token w bf16. Przy powyższych liczbach:

| | dekod (1 token) | prefill (4096 tokenów) |
|---|--:|--:|
| podział warstwowy, 1 granica | 2,85 µs | 2,73 ms |
| podział tensorowy, 43 warstwy × 2 fazy | 244,8 µs | 234,8 ms |

**Podział warstwowy jest tu jedyną sensowną opcją** — 86× tańszy komunikacyjnie.
234 ms samej komunikacji na prefill 4096 tokenów przekreśla podział tensorowy na
tym łączu, mimo że w dekodowaniu obie opcje mieszczą się w budżecie. Warstwowy
jest przy okazji naturalnym podziałem pamięci: 156 GiB modelu nie mieści się w
121 GiB jednego węzła, a przecięcie po warstwach dzieli i wagi, i pracę.

To jest dokładnie ta decyzja, którą `topology::tensor_parallel_viable` podejmuje
ILOŚCIOWO; teraz ma pod nią zmierzone liczby tego łącza, a nie nazwę transportu.

## Czego pomiar NIE mówi

- Nie mierzy odbioru z udziałem GPU. Bufor jest zwykłymi stronami hosta;
  ścieżka produkcyjna musi alokować przez `cuMemHostAlloc`, bo `ibv_reg_mr`
  odrzuca `cuMemAlloc` i `cuMemAllocManaged` (patrz nagłówek `forge-rdma/src/lib.rs`).
- Nie mierzy obu par portów naraz. Każdy węzeł ma dwa aktywne HCA na osobnych
  podsieciach; zagregowanie ich to osobna praca i osobny pomiar.
- Jeden strumień, jedna kolejka. Wiele kolejek równolegle może dać więcej przy
  małych transferach; przy dużych limit 12,3 GB/s jest limitem łącza.
