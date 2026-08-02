# EKS-A1 / EKS-A3 — harness pomiarowy Apple

Dwa eksperymenty rozstrzygające z `docs/PLAN_NAPRAWY.md` §7.7:

- **EKS-A1** — jaki procent katalogowej przepustowości pamięci oddaje kernel strumieniowy,
  ze sweepem po liczbie akumulatorów (ILP) i po rozmiarze siatki.
- **EKS-A3** — koszt dyspozycji wewnątrz command buffera, koszt osobnego command buffera
  i koszt powrotu na hosta. To rozstrzyga, czy fuzja kerneli jest na Apple dźwignią.

## Uruchomienie

```bash
./run.sh
```

Wymaga wyłącznie Xcode Command Line Tools (`swiftc` + framework Metal). Harness wypisuje
wynik w markdownie, gotowy do wklejenia do raportu w `docs/pomiary/`.

## Protokół

Zgodny z `PLAN_NAPRAWY.md` §9 N0: rozgrzewka 300 iteracji na tym samym kształcie co pomiar,
proces ciepły, 5 przebiegów z odrzuceniem pierwszego, mediana i IQR, znacznik `ważny` przy
`IQR/mediana ≤ 3%`. Stan termiczny zapisywany przed i po — to odpowiednik `pp_dpm_mclk`
z protokołu AMD i pomiar wykonany po throttlingu nie jest porównywalny.

Wyniki z 2026-08-02 (Apple M4, 10 rdzeni GPU): `docs/pomiary/eks-a1-a3-apple-m4.md`.
