@@ pl
Zbuduj obwód na dwóch kubitach, który wytwarza stan Bella `|Φ⁺⟩ = (|00⟩ + |11⟩)/√2`, i zmierz oba kubity.

CNOT sam w sobie nie splątuje stanów bazowych — potrzebuje superpozycji na kubicie sterującym. Sprawdzenie uruchamia obwód 1 024 razy i liczy TVD od rozkładu {00: 0,5, 11: 0,5}. Zaliczenie: TVD poniżej 0,05 i zero zliczeń dla 01 oraz 10.

@@ en
Build a two-qubit circuit that produces the Bell state `|Φ⁺⟩ = (|00⟩ + |11⟩)/√2` and measure both qubits.

CNOT alone does not entangle basis states: it needs a superposition on the control qubit. The check runs the circuit 1,024 times and computes the TVD from {00: 0.5, 11: 0.5}. To pass: TVD below 0.05 and zero counts for 01 and 10.
