# Milvus standalone for TentaFlow

This directory contains a Milvus **standalone** stack (etcd + minio + milvus) to
back the external `milvus` vector backend. The embedded **zvec** backend is the
default and needs nothing here; deploy this only when an admin wants Milvus for
specific addons (large collections, shared external store).

## Files

| File | Description |
|------|-------------|
| `stack.yml` | Compose/Portainer stack with the three services. |

## Requirements

- Docker + Compose (or Portainer).
- Open ports: `19530/tcp` (gRPC API) and `9091/tcp` (health/metrics). The minio
  ports `9000/9001` are bound to `127.0.0.1` (internal to the stack).
- Disk for the named volumes `milvus-etcd`, `milvus-minio`, `milvus-data`.

Pinned to Milvus **2.5.x** — required for sparse vectors and online add-field,
which the TentaFlow hybrid search and schema reconciliation paths use.

## Deploy

```bash
docker compose -f stack.yml up -d
# wait ~60-90 s on first start, then:
curl -f http://localhost:9091/healthz     # -> 200 when ready
```

Or paste `stack.yml` into Portainer > Stacks > Add stack.

## Wire it into TentaFlow

The backend is selected **per addon** via reserved `addon_config` keys (set in
the dashboard under the addon's Settings — they appear automatically for addons
that declare a `[[vector_namespace]]`):

| Key | Value |
|-----|-------|
| `__vector_backend` | `milvus` |
| `__milvus_uri` | `http://<host>:19530` |
| `__milvus_user` | (only if auth is enabled) |
| `__milvus_password` | (only if auth is enabled) |

Every other addon keeps the embedded zvec backend. One namespace maps to one
Milvus collection (`v_<org>_<addon>_<namespace>`); declared metadata fields
become scalar columns, and `sparse = true` namespaces get a sparse-float field
with an inverted index (`IP` metric) for hybrid search.

## Notes

- The default minio credentials (`minioadmin`) are fine for a single-host
  internal deployment; change them (and enable Milvus auth) for anything
  exposed beyond localhost.
- Data survives restarts via the named volumes. To wipe, `docker compose -f
  stack.yml down -v`.
- This is the same standalone topology the official Milvus docs ship; for a
  clustered/HA Milvus, deploy via the Milvus Helm chart instead and just point
  `__milvus_uri` at it.
