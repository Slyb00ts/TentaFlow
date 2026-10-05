@@ pl
## Co tu się dzieje

Stan GHZ to uogólnienie stanu Bella na `n` kubitów:

`|GHZ⟩ = (|00…0⟩ + |11…1⟩)/√2`

Hadamard na kubicie 0 tworzy superpozycję, a `n − 1` bramek CNOT z kubitu 0 przenosi ją na pozostałe. Pomiar daje albo same zera, albo same jedynki — nigdy nic pomiędzy.

## Czego się spodziewać

- tylko dwa wyniki: `00…0` i `11…1`, po połowie,
- im więcej kubitów, tym dłużej liczy symulator: stan to `2ⁿ` amplitud, a każdy kubit więcej podwaja pamięć,
- liczba bramek rośnie liniowo z `n`, ale koszt symulacji wykładniczo.

## Co spróbować

Przesuń suwak liczby kubitów od 3 do 28 i porównaj czas runu na T0 i T1. Wstaw `z q[0];` zaraz po bramce `h`: powstaje `(|00…0⟩ − |11…1⟩)/√2` — inny stan, a rozkład wyników pomiaru jest ten sam. Różnicę widać dopiero w widoku stanu w Studio obwodów.

@@ en
## What happens here

A GHZ state generalises the Bell state to `n` qubits:

`|GHZ⟩ = (|00…0⟩ + |11…1⟩)/√2`

A Hadamard on qubit 0 creates the superposition and `n − 1` CNOT gates from qubit 0 spread it over the rest. A measurement gives either all zeros or all ones — never anything in between.

## What to expect

- only two outcomes, `00…0` and `11…1`, half of the time each,
- the more qubits, the longer the simulator takes: the state is `2ⁿ` amplitudes and every extra qubit doubles the memory,
- the gate count grows linearly with `n`, the cost of simulating them exponentially.

## What to try

Move the qubit count from 3 to 28 and compare the run time on T0 and T1. Insert `z q[0];` right after the `h` gate: you get `(|00…0⟩ − |11…1⟩)/√2` — a different state with the same measurement distribution. The difference only shows in the state view of the circuit Studio.
