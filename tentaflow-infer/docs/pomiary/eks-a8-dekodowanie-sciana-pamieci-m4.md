# EKS-A8 — ile jeszcze jest w dekodowaniu (Apple M4)

Zadanie: przebić MLX w dekodowaniu. **Nie udało się.** Ten dokument mówi,
dlaczego, i co dokładnie zostało wykluczone, żeby nikt nie próbował tego drugi
raz od zera.

## Ściana

Dekodowanie czyta CAŁĄ macierz wag, żeby policzyć jeden token.

| | |
|---|---:|
| wagi checkpointu | 4 206 804 396 B = **4,207 GB** |
| pasmo pamięci (EKS-A1) | 102,4 GB/s |
| **sufit dekodowania** | **24,34 tok/s** |

Oba silniki czytają te same bajty — format jest ten sam (nibble 4-bitowy plus
skala i przesunięcie bf16 na grupę 64, czyli 0,5625 B na wagę). Nie ma tu więc
przewagi do zdobycia przez czytanie mniej; jedyne pole gry to efektywność.

## Gdzie jesteśmy

Pomiar PRZEPLATANY (osobne przebiegi porównują temperaturę, nie kod):

| | mediana |
|---|---:|
| nasze | 21,2–21,3 tok/s |
| MLX (mlx-lm) | 21,3–21,9 tok/s |
| sufit | 24,34 |

Jesteśmy na **87% sufitu**, MLX na **88–90%**. Różnica jest rzędu kilku procent
i mieści się blisko szumu stanowiska, ale w większości rund MLX był wyżej.

## Co ogranicza: arytmetyka, nie pamięć

Pomiar rozstrzygający — ten sam ruch bajtów, ale bez rozpakowywania (wynik
celowo błędny, chodzi o zegar):

| | tok/s | GB/s |
|---|---:|---:|
| sam strumień, zero arytmetyki | **23,8** | 100,0 |
| pełne rozpakowanie i mnożenie | 21,3 | 89,7 |

Ścieżka pamięciowa osiąga więc **98% sufitu EKS-A1** — tam nie ma czego
poprawiać. Całe pozostałe **11% zjada arytmetyka rozpakowania**.

## Cztery próby, żadna nie przeszła

| zmiana | wynik |
|---|---|
| odczyt wag `uint4` zamiast `uint` (512 B na simdgrupę zamiast 128) | pozornie +2% na trzech rundach, **nierozróżnialne na czterech** (mediany 20,65 wobec 20,5) — wycofane |
| odczyt aktywacji `half4` zamiast skalarnego | bez zmiany (te odczyty i tak trafiają w cache) |
| dwa wiersze na simdgrupę (dwa strumienie wag w locie) | **gorzej**: 19,9 wobec 20,8 |
| skala czytana raz na czwórkę zamiast na słowo | **wyraźnie gorzej**: 19,0 wobec 21,3 |
| rozpakowanie na wektorach (`uchar4`/`float4` zamiast przesunięć skalarnych) | bez zmiany |

Nic z tego nie weszło do repo — zmiana bez zmierzonego zysku to sam koszt.

### Dwa wiersze na simdgrupę były do przewidzenia

EKS-A1 §2.1 zmierzył, że na Apple **więcej niezależnych łańcuchów akumulacji
obniża pasmo monotonicznie**: 103,3 → 99,2 → 90,9 → 81,1 GB/s dla 1, 2, 4 i 8
akumulatorów. Wariant dwuwierszowy tworzy drugi łańcuch, więc wynik był zapisany
w pomiarze sprzed miesięcy — sprawdzenie dokumentu oszczędziłoby próby.

To samo tłumaczy, dlaczego wektoryzacja z akumulatorem `float4` nie pomogła mimo
mniejszej liczby instrukcji: zysk na arytmetyce został zjedzony przez cztery
łańcuchy.

## Czego NIE zmierzono

Porównanie jest z **mlx-lm (Python)**, nie z **mlx-swift**. To nie to samo
odniesienie: mlx-swift ma mniejszy narzut na token po stronie hosta, więc w
dekodowaniu jest zapewne równy lub szybszy niż mlx-lm. Wniosek „jesteśmy blisko
MLX" dotyczy mlx-lm i nie wolno go przenosić na mlx-swift bez pomiaru.

## Co realnie zostało

**Kernel: 11%, ale trudne.** Sufit arytmetyczny jest znany (23,8 tok/s) i to on
wyznacza, ile da się jeszcze wziąć. Cztery oczywiste techniki odpadły, więc
kolejne podejście wymaga czegoś innego niż szersze odczyty i mniej instrukcji —
najbardziej obiecujące jest wyniesienie sumy aktywacji per grupa POZA kernel
(`acc = sc*Σxq + bi*Σx`), co usuwa jedno FMA na wagę, ale wymaga osobnego,
taniego przebiegu liczącego `Σx` raz na mnożenie zamiast raz na wiersz.

**Spekulacja: jedyna droga do dużego zysku.** Dekodowanie spekulatywne czyta
wagi RAZ i wypuszcza więcej niż jeden token, czyli omija ścianę zamiast się o
nią opierać — i to samo repo ma już n-gram oraz MTP po stronie CUDA. Trzeba
jednak powiedzieć wprost: porównywanie naszego dekodowania ze spekulacją do
zwykłego dekodowania MLX **nie jest porównaniem tej samej pracy** i nie wolno go
podawać jako „szybciej od MLX" bez tego zastrzeżenia.
