# Iroh Spike — Decision (#22)

## Status kryteriów

| # | Kryterium | Status | Dowód |
|---|-----------|--------|-------|
| a | accept/connect | PASS | `tests/criterion_a_accept_connect.rs::endpoint_round_trip` (0.66s) |
| b | Ed25519+PIN pairing <5s | PASS | `src/pairing.rs` 5 tests, sub-ms in-process |
| c | Epoch rotation 24h+7d, zero-drop | PASS (state machine) | `src/epoch_rotation.rs` 6 tests, zero-drop verified |
| d | Sliding-window replay | PASS | `src/replay.rs` 4 tests |
| e | TrustRevoked + TrustedKeysSync | PASS (state machine) | `src/trust_state.rs` 6 tests |
| f | 100 conn / 1000 msg/s / 30 min | HARNESS | `tests/criterion_f_throughput.rs` — needs 30 min wall-clock |
| g | 4h leak soak RSS/FD <5% | HARNESS | `tests/criterion_g_leak_soak.rs` — needs 4h wall-clock + monitoring |

## Obserwacje z implementacji

**Pozytywy:**
- iroh 0.28 kompiluje się i linkuje bez problemów
- `iroh_net::Endpoint` API jest funkcjonalne — `bind()`, `accept()`, `connect()`, `open_bi/accept_bi` działają
- ALPN-based protocol routing działa identycznie do quinn
- Stream API (read_to_end, write_all, finish) ma identyczną ergonomię do quinn
- NodeId (Ed25519 public key) jest first-class concept (nasz mesh już to ma)

**Negatywy:**
- **Pre-1.0 stability**: 6 deprecation warnings z `iroh-net` (rename → `iroh::net`) w jednym pliku spike
- API zmienia się co minor (0.27 → 0.28 = breaking)
- Discovery service (DERP relays) wymusza dodatkową integration ścieżkę dla mesh — albo zostawić w trybie no-discovery (manual addr), albo zaadoptować ich relay infra
- Ekosystem (iroh-blobs, iroh-docs, iroh-gossip) wciąga wiele dodatkowych funkcji których mesh nie potrzebuje

## Rekomendacja

**CUT iroh** dla mesh QUIC. Powody:

1. **Quinn jest 1.0+**, stable API, znana ścieżka migracji
2. **Pre-1.0 iroh** = zero gwarancji że za 6 miesięcy nasz `iroh::net::Endpoint` nadal istnieje (już teraz `iroh-net` deprecated)
3. **Mamy działający quinn-based mesh** — (`tentaflow-core/src/mesh/quic_mesh.rs`) 
4. **Nasze mesh primitives (Ed25519 NodeId, ALPN routing, pairing, epoch rotation, replay, trust state) NIE wymagają iroh** — udowodnione przez kryteria (b)-(e) że state machines są iroh-independent
5. **Migration overhead** każdej minor iroh > value-add z relay/discovery dla naszego use case (LAN mesh, zwykle bez NAT traversal)

## Plan na decision = CUT

1. **Zachowaj** `tentaflow-iroh-spike/` jako reference dla przyszłych re-evaluation jeśli iroh osiągnie 1.0
2. **Re-evaluate** po iroh 1.0.0 release (sprawdzaj quartely)
3. **Continue** z quinn-based `tentaflow-core/src/mesh/quic_mesh.rs` bez zmian

## Co się stanie z kodem spike

- `src/pairing.rs`, `src/replay.rs`, `src/epoch_rotation.rs`, `src/trust_state.rs` to czysty Rust bez iroh deps — można je portować do `tentaflow-core/src/mesh/` jeśli okaże się że obecna implementacja brakuje któregoś z tych primitives
- `src/accept_connect.rs` + iroh deps mogą zostać usunięte (lub zostawione w spike jako reference)

---

**Plik checked in for visibility. Final commit/cut decision należy do operatora po review.**
