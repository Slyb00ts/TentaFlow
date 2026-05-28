# Benchmark: redb vs Fjall vs RocksDB dla Sync Ledger

## Cel

Porownanie `redb 4.1.0`, `fjall 3.1.4` i `rocksdb 0.24.0`
(`librocksdb-sys 0.17.3+10.4.2`) jako kandydatow na storage techniczny Sync
Ledger.

Benchmark zostal uruchomiony poza repo w `/tmp/tf-kv-bench`, zeby nie dodawac
zaleznosci do TentaFlow przed decyzja architektoniczna.

## Biblioteki

| Biblioteka | Typ | Profil |
|------------|-----|--------|
| redb 4.1.0 | pure Rust, copy-on-write B-tree, ACID/MVCC | bardzo dobre odczyty i scan, wiekszy koszt update |
| fjall 3.1.4 | pure Rust, log-structured LSM-tree | bardzo dobre zapisy/update, partycje jak column families |
| rocksdb 0.24.0 | C++ RocksDB przez Rust wrapper | bardzo mocny LSM, najmniejszy dysk w testach, trudniejszy cross-build |

## Scenariusz

Kazdy silnik dostal ten sam workload:

```text
insert: sekwencyjny zapis w batchach
random_read: losowe odczyty istniejacych kluczy
full_scan: pelny range scan
update: losowe nadpisywanie istniejacych kluczy w batchach
```

Durability:

```text
redb: Durability::Immediate
fjall: PersistMode::SyncAll na batch
rocksdb: WriteBatch z WAL i WriteOptions::sync(true)
```

## Wyniki: 300k rekordow, wartosc 256 B

```text
records=300000 reads=120000 updates=120000 value_bytes=256 batch_size=10000
```

| Metryka | redb | fjall | Wygrywa |
|---------|------|-------|---------|
| insert | 302.586 ms, 991k ops/s | 153.941 ms, 1.95M ops/s | fjall 1.97x |
| random_read | 97.593 ms, 1.23M ops/s | 171.414 ms, 700k ops/s | redb 1.76x |
| full_scan | 25.068 ms, 11.97M rows/s | 62.301 ms, 4.82M rows/s | redb 2.49x |
| update | 400.091 ms, 300k ops/s | 113.413 ms, 1.06M ops/s | fjall 3.53x |
| disk | 257.00 MiB | 168.59 MiB | fjall 1.52x mniejszy |

Po dodaniu RocksDB:

| Metryka | redb | rocksdb | fjall/default | fjall/ledger-tuned | Wygrywa |
|---------|------|---------|---------------|--------------------|---------|
| insert | 298.843 ms | 146.152 ms | 148.398 ms | 149.741 ms | rocksdb minimalnie |
| random_read | 97.614 ms | 95.231 ms | 134.466 ms | 76.803 ms | fjall/ledger-tuned |
| full_scan | 26.082 ms | 19.830 ms | 50.811 ms | 36.597 ms | rocksdb |
| update | 404.395 ms | 157.033 ms | 108.951 ms | 126.581 ms | fjall/default |
| disk | 257.00 MiB | 77.77 MiB | 168.59 MiB | 179.61 MiB | rocksdb |

## Wyniki: 1M rekordow, wartosc 256 B

```text
records=1000000 reads=250000 updates=250000 value_bytes=256 batch_size=10000
```

| Metryka | redb | fjall | Wygrywa |
|---------|------|-------|---------|
| insert | 1.025 s, 975k ops/s | 543.997 ms, 1.84M ops/s | fjall 1.88x |
| random_read | 227.967 ms, 1.10M ops/s | 480.648 ms, 520k ops/s | redb 2.11x |
| full_scan | 93.977 ms, 10.64M rows/s | 236.800 ms, 4.22M rows/s | redb 2.52x |
| update | 1.177 s, 212k ops/s | 219.880 ms, 1.14M ops/s | fjall 5.35x |
| disk | 1028.00 MiB | 381.25 MiB | fjall 2.70x mniejszy |

Po dodaniu RocksDB:

| Metryka | redb | rocksdb | fjall/default | fjall/ledger-tuned | Wygrywa |
|---------|------|---------|---------------|--------------------|---------|
| insert | 1.009 s | 494.247 ms | 533.739 ms | 514.870 ms | rocksdb |
| random_read | 229.617 ms | 584.391 ms | 450.250 ms | 559.770 ms | redb |
| full_scan | 93.891 ms | 176.233 ms | 207.069 ms | 164.220 ms | redb |
| update | 1.178 s | 266.175 ms | 215.970 ms | 231.342 ms | fjall/default |
| disk | 1028.00 MiB | 311.85 MiB | 381.25 MiB | 392.43 MiB | rocksdb |

## Wyniki: 500k rekordow, wartosc 1 KiB

```text
records=500000 reads=150000 updates=150000 value_bytes=1024 batch_size=10000
```

| Metryka | redb | fjall | Wygrywa |
|---------|------|-------|---------|
| insert | 1.615 s, 310k ops/s | 736.591 ms, 679k ops/s | fjall 2.19x |
| random_read | 202.105 ms, 742k ops/s | 328.016 ms, 457k ops/s | redb 1.62x |
| full_scan | 206.621 ms, 2.42M rows/s | 222.518 ms, 2.25M rows/s | redb 1.08x |
| update | 1.026 s, 146k ops/s | 259.634 ms, 578k ops/s | fjall 3.95x |
| disk | 2056.00 MiB | 731.13 MiB | fjall 2.81x mniejszy |

## Wyniki: 300k rekordow, wartosc 1 KiB, z RocksDB

```text
records=300000 reads=100000 updates=100000 value_bytes=1024 batch_size=10000
```

| Metryka | redb | rocksdb | fjall/default | fjall/ledger-tuned | fjall/no-compression | Wygrywa |
|---------|------|---------|---------------|--------------------|----------------------|---------|
| insert | 955.298 ms | 513.942 ms | 450.070 ms | 440.686 ms | 445.441 ms | fjall/ledger-tuned |
| random_read | 123.302 ms | 208.032 ms | 190.369 ms | 238.781 ms | 177.251 ms | redb |
| full_scan | 165.281 ms | 125.702 ms | 127.894 ms | 75.578 ms | 131.946 ms | fjall/ledger-tuned |
| update | 529.413 ms | 236.220 ms | 169.245 ms | 194.842 ms | 189.654 ms | fjall/default |
| disk | 1028.00 MiB | 376.61 MiB | 409.71 MiB | 461.06 MiB | 463.87 MiB | rocksdb |

## Interpretacja

`redb` ma bardzo mocny profil read-heavy:

- szybsze losowe odczyty,
- szybszy pelny scan przy malych wartosciach,
- prosty model ACID/MVCC,
- ale wysoki koszt update i wyraznie wiekszy rozmiar plikow w tym workloadzie.

`fjall` ma profil dobry dla Sync Ledger:

- wyraznie szybszy insert,
- wyraznie szybszy update,
- duzo mniejszy rozmiar na dysku w testowanym workloadzie,
- model LSM pasuje do append-only logu, outbox/inbox, ACK i cursorow,
- partycje pasuja do planowanych partycji ledgera.

`rocksdb` ma profil bardzo konkurencyjny:

- bardzo szybki insert,
- najlepszy albo prawie najlepszy full scan w czesci testow,
- najmniejszy rozmiar danych na dysku,
- slabszy od Fjall w update-heavy path w tych przebiegach,
- najwieksze ryzyko operacyjne: C++ build, linkowanie, Android/iOS/macOS/Windows matrix.

## Wniosek

Decyzja projektowa: glowny Sync Ledger idzie w `fjall` jako docelowy embedded
storage.

`redb` warto zostawic jako opcje dla lokalnych, read-heavy metadanych albo
prostych tabel KV, ale nie jako pierwszy wybor dla append/update-heavy ledgera.

`rocksdb` wygrywa rozmiarem na dysku i ma bardzo mocne ogolne wyniki, ale niesie
koszt C++ toolchaina i trudniejszy build matrix. Nie jest domyslnym wyborem dla
Sync Ledger; zostaje punktem odniesienia wydajnosci i moze wrocic tylko jako
osobna decyzja server-only.

`fjall` jest slabszy od RocksDB w rozmiarze na dysku i czesci scanow, ale
wygrywa update-heavy path w tych przebiegach, jest pure Rust i lepiej pasuje do
platform mobile oraz wbudowanego storage bez zewnetrznego toolchaina C++.

## Strojenie Fjall

Fjall ma ustawienia globalne bazy:

```text
cache_size
worker_threads
max_cached_files
max_journaling_size
manual_journal_persist
journal_compression
```

Oraz ustawienia per keyspace:

```text
max_memtable_size
data_block_size_policy
data_block_compression_policy
index_block_compression_policy
filter_policy
expect_point_read_hits
data_block_hash_ratio_policy
with_kv_separation
compaction_strategy
```

Sprawdzone warianty:

```text
default
ledger-tuned:
  cache_size = 128 MiB
  worker_threads = 8
  max_journaling_size = 1 GiB
  max_memtable_size = 128 MiB
  data_block_size = 16 KiB
  data_block_hash_ratio = 4.0
  bloom = 12 bits/key
  expect_point_read_hits = true
no-compression:
  max_memtable_size = 128 MiB
  data/index compression disabled
```

Wynik dla 500k rekordow po 256 B:

| Wariant | Insert | Random read | Full scan | Update | Dysk |
|---------|--------|-------------|-----------|--------|------|
| fjall/default | 260.801 ms | 206.926 ms | 88.932 ms | 133.007 ms | 178.20 MiB |
| fjall/ledger-tuned | 251.468 ms | 167.679 ms | 77.472 ms | 129.373 ms | 171.11 MiB |
| fjall/no-compression | 253.041 ms | 196.779 ms | 89.470 ms | 128.763 ms | 171.50 MiB |

Wynik dla 300k rekordow po 1 KiB:

| Wariant | Insert | Random read | Full scan | Update | Dysk |
|---------|--------|-------------|-----------|--------|------|
| fjall/default | 449.373 ms | 192.348 ms | 123.951 ms | 189.784 ms | 409.71 MiB |
| fjall/ledger-tuned | 446.238 ms | 242.213 ms | 72.257 ms | 214.722 ms | 461.02 MiB |
| fjall/no-compression | 442.263 ms | 183.013 ms | 123.471 ms | 193.981 ms | 464.36 MiB |

Interpretacja:

- `ledger-tuned` pomaga przy malych wartosciach 256 B: szybszy odczyt losowy,
  szybszy scan, lekko szybszy insert/update i mniejszy dysk.
- Przy wartosciach 1 KiB `ledger-tuned` bardzo przyspiesza scan, ale pogarsza
  random read, update i rozmiar.
- Wylaczenie kompresji nie daje jednoznacznego zysku. Przy losowych danych
  potrafi przyspieszyc minimalnie insert/random read, ale zwieksza rozmiar.

Rekomendacja dla TentaFlow: nie ustawiamy jednego profilu globalnie dla wszystkiego.
Sync Ledger powinien miec profile per partycja:

```text
append_log: wiekszy memtable, umiarkowany block size, kompresja wlaczona
cursor_ack: mniejszy block size, hash ratio/bloom pod point reads
snapshot_metadata: domyslne albo read-heavy
large_payload_refs: rozwazyc kv_separation lub trzymanie blobow poza KV
```

Nastepny krok techniczny: po dodaniu `fjall` do `tentaflow-core` implementujemy:

```text
FjallSyncLedgerStore jako domyslny i docelowy storage Sync Ledger
profile Fjall per partycja ledgera zamiast jednego globalnego profilu
RocksDB tylko jako historyczny benchmark i ewentualna przyszla opcja server-only po osobnej decyzji
```
