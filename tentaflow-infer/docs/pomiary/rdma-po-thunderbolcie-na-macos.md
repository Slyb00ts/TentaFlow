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

Deklarowane parametry: **80 Gb/s** i opóźnienia **5–9 us**, do **czterech
Maców** połączonych każdy z każdym (bez przełącznika).

## Której generacji Thunderbolta to wymaga — NIE USTALONE

Wtórne omówienia mówią o Thunderbolt 5. Nie udało się tego potwierdzić w nocie
źródłowej TN3205 (strona nie dała się pobrać), a użytkownik projektu podaje, że
działa również na Thunderbolt 4. **Ten dokument tego nie rozstrzyga** — 80 Gb/s
to parametr łącza TB5, ale to nie to samo co warunek działania.

Sonda na tej maszynie (Mac mini M4, macOS 26.5.2, magistrale 40 Gb/s = TB4)
zwraca `ibv_get_device_list` = 0 urządzeń. Jest to jednak NIEROZSTRZYGAJĄCE:
nic nie jest podłączone drugim końcem kabla, a urządzenie RDMA ma prawo pojawić
się dopiero przy istniejącym łączu.

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
