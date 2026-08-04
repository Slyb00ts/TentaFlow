# RDMA po Thunderbolcie na macOS — jest, wbrew temu, co tu wcześniej napisano

Twierdziłem w tej sesji dwukrotnie, że macOS nie ma RDMA i że Thunderbolt daje
wyłącznie IP. **To było błędne.** Poniżej stan faktyczny, sprawdzony w SDK tej
maszyny, a nie z pamięci.

## Co macOS ma

Apple dodał **RDMA over Thunderbolt w macOS 26.2** (nota TN3205). Nie jest to
warstwa emulacyjna ani protokół na gniazdach — wystawione jest **API zgodne z
RDMA Verbs**, czyli ten sam interfejs, którym mówi się do kart InfiniBand i
RoCE.

Zweryfikowane lokalnie w SDK (`xcrun --show-sdk-path`):

| co | gdzie |
|---|---|
| nagłówek | `usr/include/infiniband/verbs.h` |
| biblioteka | `usr/lib/librdma.tbd` |
| symbole | 47 wpisów `ibv_*` / `rdma_*`, m.in. `ibv_alloc_pd`, `ibv_cmd_create_qp`, `ibv_poll_cq` |

Deklarowane parametry: **80 Gb/s** i opóźnienia **5–9 us** przy Thunderbolt 5,
do **czterech Maców** połączonych każdy z każdym (bez przełącznika).

## Czego ta maszyna nie zrobi

Mac mini M4, na którym pracujemy: macOS **26.5.2**, ale magistrale Thunderbolt
raportują **40 Gb/s**, czyli Thunderbolt 4. RDMA po Thunderbolcie wymaga
**Thunderbolt 5**, więc tutaj skompiluje się i zalinkuje, ale nie zobaczy
urządzenia. Do realnego uruchomienia potrzebny jest Mac z TB5 (M4 Pro/Max i
nowsze) — i drugi taki sam.

## Dlaczego `forge-rdma` się nie linkował

Nie dlatego, że macOS nie ma RDMA. Dlatego, że **ta sama biblioteka nazywa się
inaczej**:

```
ld: library 'ibverbs' not found
```

Nagłówek jest ten sam (`infiniband/verbs.h`), symbole te same (`ibv_*`), ale na
Apple mieszkają w `librdma`, a nie w `libibverbs`. Poprawka to jeden warunek w
`build.rs` — po niej cały workspace, łącznie z `forge-rdma`, buduje się i
przechodzi testy na macOS.

## Co z tego wynika dla projektu

Transport RoCE napisany dla Sparków **nie jest ślepą uliczką na Apple**. Ten sam
kod verbs ma szansę działać między Macami z TB5, co otwiera klaster Maców jako
realną topologię, a nie tylko Thunderbolt Bridge po IP. Nie zostało to
zmierzone — brak sprzętu — i dopóki nie zostanie, należy o tym mówić jako o
możliwości wynikającej ze zgodności API, nie jako o działającej ścieżce.

## Źródła

- TN3205: Low-latency communication with RDMA over Thunderbolt —
  https://developer.apple.com/documentation/technotes/tn3205-low-latency-communication-with-rdma-over-thunderbolt
- 1,5 TB VRAM na Mac Studio, RDMA po Thunderbolcie 5 (Jeff Geerling) —
  https://www.jeffgeerling.com/blog/2025/15-tb-vram-on-mac-studio-rdma-over-thunderbolt-5/
- Omówienie i ograniczenia topologii —
  https://news.ycombinator.com/item?id=46248644
