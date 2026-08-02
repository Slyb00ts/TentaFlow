# Wyrocznia numeryczna MLX

Generator wektorów wzorcowych dla dekodera kwantyzacji MLX (`crates/forge-formats/src/mlx.rs`).
Wartości oczekiwane liczy **sama biblioteka MLX**, nie nasza implementacja wzoru — inaczej
test sprawdzałby wyłącznie to, że kod zgadza się sam ze sobą.

## Po co

Kolejność bitów w upakowaniu jest jedyną własnością formatu, której **nie da się wyczytać
z `config.json`**. Zły odczyt daje wagi wyglądające sensownie i model, który liczy śmieci
bez jednego błędu — dokładnie ta klasa usterki, która przy DeepSeeku V4 kosztowała pół dnia
(`weight_scale_2` mnoży, `weight_global_scale` dzieli, MLX-owe `biases` dodaje).

## Użycie

```bash
python3 -m venv .venv && ./.venv/bin/pip install 'mlx==0.31.2'
./.venv/bin/python gen_fixtures.py \
    <katalog-checkpointu-MLX> \
    ../../crates/forge-formats/tests/fixtures/mlx_affine_bielik.bin
```

Wersja MLX jest przypięta do tej samej, którą śledzi `MLXBridge` w drzewie głównym.

## Co trafia do fikstury

Siedem tensorów z `Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit`, po dwa pełne wiersze:
projekcje uwagi, FFN, `down_proj` o innej liczbie grup, oraz **skwantyzowany embedding
i głowa** — te dwa mają inną ścieżkę użycia niż projekcje i dlatego są w zestawie.

Dla każdego tensora zapisywane są dwa niezależne zestawy wartości oczekiwanych:

1. **samo rozpakowanie** (skala 1, przesunięcie 0) — przypina kolejność bitów, porównanie
   jest bit w bit;
2. **pełny dequant afiniczny** — przypina kierunek działania przesunięcia; porównanie jest
   bit w bit **po jednym zaokrągleniu do bf16**, bo dekoder liczy w f32, a MLX zwraca typ
   checkpointu.

Fikstura to ~600 kB wycinków wag i jest wersjonowana razem z testem.
