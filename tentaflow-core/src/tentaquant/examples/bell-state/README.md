@@ pl
## Co tu się dzieje

Bramka Hadamarda na kubicie 0 daje superpozycję `(|0⟩ + |1⟩)/√2`. CNOT z kubitu 0 na kubit 1 kopiuje tę niepewność na drugi kubit, ale nie jako dwie niezależne monety: powstaje stan Bella

`|Φ⁺⟩ = (|00⟩ + |11⟩)/√2`

którego nie da się zapisać jako iloczynu stanów pojedynczych kubitów.

## Czego się spodziewać

- wyniki `00` i `11` pojawiają się mniej więcej po połowie,
- `01` i `10` nie pojawiają się nigdy — to korelacja, a nie dwa niezależne rzuty monetą,
- przy skończonej liczbie shotów proporcja nie wynosi dokładnie 50/50; im więcej shotów, tym bliżej teorii.

## Co spróbować

Zmień `h` na `x` i sprawdź, że splątanie znika (zostaje jeden wynik). Dodaj `rz(0.3) q[0];` przed pomiarem i zobacz, że rozkład się nie zmienia — faza nie jest widoczna w pomiarze w bazie Z.

@@ en
## What happens here

A Hadamard gate on qubit 0 gives the superposition `(|0⟩ + |1⟩)/√2`. A CNOT from qubit 0 to qubit 1 copies that uncertainty to the second qubit — not as two independent coins, but as the Bell state

`|Φ⁺⟩ = (|00⟩ + |11⟩)/√2`

which cannot be written as a product of single-qubit states.

## What to expect

- the outcomes `00` and `11` appear about half of the time each,
- `01` and `10` never appear — that is a correlation, not two independent coin tosses,
- with a finite number of shots the split is not exactly 50/50; the more shots, the closer to the theory.

## What to try

Change `h` to `x` and see the entanglement disappear (a single outcome is left). Add `rz(0.3) q[0];` before the measurement and notice that the distribution does not change — a phase is invisible to a measurement in the Z basis.
