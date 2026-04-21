# Iroh Spike — kryteria akceptacyjne (Tasks #14-#22)

Throwaway evaluation crate. Cel: ocenić iroh jako alternatywę dla quinn.

## Status

| # | Kryterium | Status | Plik testu | Uwaga |
|---|-----------|--------|-----------|-------|
| a | accept/connect działa | scaffold | `tests/criterion_a_accept_connect.rs` | Real network bind |
| b | Ed25519+PIN pairing <5s | TODO | `tests/criterion_b_pairing.rs` | Wymaga porting pairing logic |
| c | Epoch rotation 24h+7d, zero-drop | TODO | `tests/criterion_c_rotation.rs` | Multi-day soak |
| d | Sliding-window replay | TODO | `tests/criterion_d_replay.rs` | Adversarial test |
| e | TrustRevoked + TrustedKeysSync ALPN | TODO | `tests/criterion_e_trust.rs` | Multi-peer |
| f | 100 conn / 1000 msg/s / 30min | TODO | `tests/criterion_f_throughput.rs` | Load test rig |
| g | 4h soak RSS/FD <5% drift | TODO | `tests/criterion_g_soak.rs` | 4h wall-clock |

## Decision (#22)

**Adopt iroh** if a-g all pass. Plan integration into `tentaflow-core/src/mesh/quic_mesh.rs`.

**Cut iroh** if any criterion fails or pre-1.0 stability concerns surface during testing.
Keep current quinn-based mesh.

Decision deadline: po ukonczeniu 4h soak test (g). Bez kompletu danych nie mergować.

## Uruchamianie

```bash
cd tentaflow-iroh-spike
cargo test --test criterion_a_accept_connect -- --ignored --nocapture
```

Soak/throughput tests uruchamiać po godzinach i logować RSS/FD do `target/spike-logs/`.

## Cleanup po decyzji

- **Adopt:** kod z `src/` portuje sie do `tentaflow-core/src/mesh/`, ten katalog usuwany.
- **Cut:** caly katalog `tentaflow-iroh-spike/` usuwany razem z iroh dep.
