@@ pl
Przygotuj kubit tak, by po pomiarze dawał `0` z prawdopodobieństwem 0,7 i `1` z prawdopodobieństwem 0,3.

Obrót `ry(θ)` przeprowadza |0⟩ w `cos(θ/2)|0⟩ + sin(θ/2)|1⟩`, więc prawdopodobieństwo jedynki to `sin²(θ/2)`. Dobierz kąt, a potem zmierz kubit. Sprawdzenie uruchamia obwód 1 024 razy; zalicza odległość wariacyjną (TVD) od rozkładu {0: 0,7, 1: 0,3} poniżej 0,05.

@@ en
Prepare a qubit so that measuring it gives `0` with probability 0.7 and `1` with probability 0.3.

The rotation `ry(θ)` takes |0⟩ to `cos(θ/2)|0⟩ + sin(θ/2)|1⟩`, so the probability of one is `sin²(θ/2)`. Pick the angle, then measure the qubit. The check runs the circuit 1,024 times and passes a total variation distance (TVD) from {0: 0.7, 1: 0.3} below 0.05.
