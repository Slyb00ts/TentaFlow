# tentaflow-sdk-spec

Source of truth dla TentaFlow addon SDK typed schemas. Patrz `docs/ADDON_BINARY_PROTOCOL_v1.md` i `docs/ADDON_UI_COMPONENT_CATALOG_v1.md`.

Status: **chunk 1.1 done** — control-channel typed payloads + Envelope live in `src/protocol/`. UI, host_fn, stream channels and codegen annotations land in following chunks of Faza 6.

## Aktualna zawartość

- `src/lib.rs` — public re-exports
- `src/protocol/envelope.rs` — `Envelope<P>`, `Channel`, `Priority`, `Flags`, `ProtocolVersion`
- `src/protocol/ids.rs` — `SessionId`, `TraceId`, `NodeId`, `DeviceId`, `ClientActionId` (bstr 16) and `Hash32` (bstr 32)
- `src/protocol/value.rs` — generic CBOR `Value` (rejects indefinite-length items, semantic tags, undefined)
- `src/protocol/control.rs` — full §5 control payloads (handshake, lifecycle, flow-control) + `CborMap` with canonical tstr key sort

## Planowana zawartość (kolejne chunki)

- `src/protocol/ui.rs` — UI channel messages (§6) — chunk 1.2/1.3
- `src/protocol/host_fn.rs` — host function channel (§host_fn doc) — later
- `src/protocol/stream.rs` — stream channel (§7) — later
- `src/ui/components/*.rs` — typed Component structs per `ADDON_UI_COMPONENT_CATALOG_v1.md`
- `src/sdk/*.rs` — annotations for `tentaflow-sdk-gen` (`#[derive(SdkType)]`, `#[sdk_host_fn]`)
- `catalog-manifest/v1.cbor` — generated canonical manifest, SHA-256 used in handshake `Capability { name: "ui_v1", hash: <bstr32> }`
- `catalog-manifest/v1.hash.hex` — hex form for diagnostics

## Generowanie manifestu

```bash
cargo run -p tentaflow-sdk-gen -- --emit catalog-manifest \
  --output tentaflow-sdk-spec/catalog-manifest/v1.cbor \
  --hash-out tentaflow-sdk-spec/catalog-manifest/v1.hash.hex
```

Manifest jest **committed do repo** — każda zmiana spec wymaga regeneracji + commit obu artefaktów. CI sprawdza spójność (rebuild deterministic, hash matches).

## Wersjonowanie

`catalog_version` w handshake odpowiada major+minor stop tej spec. Bump tylko po zmianie wire schema (nowe komponenty, nowe pola, zmiany ABI).
