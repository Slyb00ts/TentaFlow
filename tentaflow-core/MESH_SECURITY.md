# Bezpieczenstwo Mesh Networking — TentaFlow.AI

## 1. Przeglad

Mesh networking w TentaFlow.AI laczy nody (Router, Desktop, Mobile) w siec peer-to-peer. System zabezpieczen chroni przed:

- **Nieautoryzowanym dolaczeniem** — tylko sparowane nody moga komunikowac sie w mesh
- **Podsluchem komunikacji** — payload szyfrowany ChaCha20-Poly1305 wewnatrz tunelu QUIC/TLS
- **Podszywaniem sie** — podpisy Ed25519 weryfikuja tozsamosc noda
- **Manipulacja danymi** — AEAD (Authenticated Encryption with Associated Data) gwarantuje integralnosc

### Warstwy zabezpieczen

```
┌─────────────────────────────────────────────────┐
│  Warstwa 1: mDNS Discovery (nieszyfrowane)      │  <- Celowo otwarte — tylko wykrywanie
├─────────────────────────────────────────────────┤
│  Warstwa 2: Parowanie PIN (jednorazowe)         │  <- 6-cyfrowy PIN, 60s TTL
├─────────────────────────────────────────────────┤
│  Warstwa 3: QUIC/TLS 1.3 (transport)            │  <- Szyfrowany tunel sieciowy
├─────────────────────────────────────────────────┤
│  Warstwa 4: ChaCha20-Poly1305 (payload)         │  <- Szyfrowanie end-to-end per para nodow
└─────────────────────────────────────────────────┘
```

---

## 2. Kryptografia

### Ed25519 — podpisy cyfrowe

- **Cel**: Weryfikacja tozsamosci noda, podpisywanie wiadomosci
- **Generowanie**: `SigningKey::generate(&mut OsRng)` — kryptograficznie bezpieczny CSPRNG
- **Przechowywanie**: Klucz prywatny jako hex w tabeli `settings` pod kluczem `node_private_key`
- **Rozmiar**: 32 bajty klucz prywatny, 32 bajty klucz publiczny, 64 bajty podpis

### X25519 — wymiana kluczy Diffie-Hellman

- **Cel**: Uzgodnienie wspolnego sekretu (shared secret) miedzy dwoma nodami
- **Generowanie**: `StaticSecret::random_from_rng(OsRng)`
- **Przechowywanie**: Klucz prywatny jako hex w `settings` pod kluczem `node_x25519_private_key`
- **Rozmiar**: 32 bajty klucz prywatny, 32 bajty klucz publiczny

### HKDF-SHA256 — derywacja klucza

- **Cel**: Wyprowadzenie klucza symetrycznego z wyniku Diffie-Hellman
- **Wejscie**: 32-bajtowy raw shared secret z X25519 DH
- **Kontekst**: `b"tentaflow-mesh-chacha20-key"` — unikalny per protokol
- **Wyjscie**: 32-bajtowy klucz symetryczny dla ChaCha20-Poly1305
- **Uwaga**: Raw wynik DH NIE jest uzywany bezposrednio jako klucz — zawsze przechodzi przez HKDF

### ChaCha20-Poly1305 — szyfrowanie symetryczne AEAD

- **Cel**: Szyfrowanie payloadu wiadomosci miedzy zaufanymi nodami
- **Klucz**: 32-bajtowy klucz wyderywowany z HKDF-SHA256 (nie raw DH)
- **Nonce**: 12 bajtow, losowy per wiadomosc (`rand::random()`)
- **Tag**: 16 bajtow — Poly1305 MAC (autentykacja)

### Schemat kryptograficzny

```
Node A                                              Node B
──────                                              ──────

Ed25519 priv ──> Ed25519 pub ─────────────────────> Zapisz w trusted_keys
X25519 priv  ──> X25519 pub  ─────────────────────> X25519 DH
                                                        │
                              <─────────────────── Ed25519 pub <── Ed25519 priv
                       X25519 DH <──────────────── X25519 pub  <── X25519 priv
                           │                            │
                           v                            v
                    raw_shared A                 raw_shared B
                    (identyczne — DH)            (identyczne — DH)
                           │                            │
                           v                            v
                    HKDF-SHA256                  HKDF-SHA256
                    (context: "tentaflow-         (context: "tentaflow-
                     mesh-chacha20-key")         mesh-chacha20-key")
                           │                            │
                           v                            v
                    derived_key A                derived_key B
                    (identyczne)                 (identyczne)
                           │                            │
                           v                            v
                   ChaCha20-Poly1305            ChaCha20-Poly1305
                   encrypt(payload)             decrypt(payload)
```

### Format klucza publicznego

Klucz publiczny noda to konkatenacja Ed25519 + X25519 w hex:

```
[Ed25519 pub — 32B = 64 hex][X25519 pub — 32B = 64 hex] = 128 hex znakow
```

Przyklad: `a1b2c3...64znakow...d4e5f6...64znakow...` (128 znakow laczenie)

**Walidacja**: Klucz publiczny MUSI miec dokladnie 128 znakow hex. Krotszy klucz jest odrzucany przy parowaniu — parowanie nie moze sie udac bez pelnego klucza (Ed25519 + X25519).

---

## 3. Proces parowania

### Diagram flow

```
   Node A (inicjator)                    Node B (odbiorca)
   ─────────────────                    ─────────────────
         │                                     │
   [1]   │  mDNS: Odkrycie Node B              │
         │─────────────────────────────────────>│
         │                                     │
   [2]   │  UI: Klik "Sparuj" na Node B        │
         │  generate_pin() => "483921"         │
         │  Zapis: pending_pairings            │
         │    (outgoing, 60s TTL)              │
         │                                     │
   [3]   │  QUIC: PairingRequest               │
         │  { from: A, pin: "483921" }         │
         │─────────────────────────────────────>│
         │                                     │  [4] Zapis: pending_pairings
         │                                     │      (incoming, 60s TTL)
         │                                     │
         │                                     │  [5] UI: Wyswietl PIN "483921"
         │                                     │      Uzytkownik zatwierdza
         │                                     │
         │  QUIC: PairingConfirm               │
         │  { from: B, public_key: B_pub }     │
         │<─────────────────────────────────────│  [6]
         │                                     │
   [7]   │  confirm_pairing():                 │
         │  - Sprawdz TTL (< 60s)              │
         │  - Parsuj klucz Ed25519             │
         │  - Zapisz do trusted_nodes          │
         │  - Oblicz shared_secret (X25519 DH) │
         │  - Usun pending_pairings            │
         │                                     │
   [8]   │  QUIC: PairingConfirm               │
         │  { from: A, public_key: A_pub }     │
         │─────────────────────────────────────>│
         │                                     │  [9] confirm_pairing():
         │                                     │      (ten sam proces co krok 7)
         │                                     │
   [10]  │  QUIC: TrustedKeysSync              │
         │  { keys: [(C, C_pub), (D, D_pub)] } │
         │─────────────────────────────────────>│
         │                                     │  [11] add_trusted_key() dla
         │  CRDT: AddTrustedNode               │       kazdego noda z listy
         │  (propagacja do C, D, ...)          │
         │<────────────────────────────────────>│
         │                                     │
   ✅ SPAROWANE — pelna komunikacja            ✅ SPAROWANE
```

### Opis krokow

| Krok | Node | Opis |
|------|------|------|
| 1 | A | mDNS discovery wykrywa Node B w sieci lokalnej. Node B widoczny jako "discovered" (szary). |
| 2 | A | Uzytkownik klika "Sparuj" w UI. System generuje 6-cyfrowy PIN (100000-999999) i zapisuje w `pending_pairings` z TTL 60 sekund. |
| 3 | A→B | Wyslanie `PairingRequest` przez QUIC z PIN-em. Discriminant: `0x20`. |
| 4 | B | Zapis zadania parowania jako incoming w `pending_pairings`. |
| 5 | B | UI wyswietla PIN i prosi uzytkownika o zatwierdzenie. |
| 6 | B→A | Po zatwierdzeniu — `PairingConfirm` z kluczem publicznym B (128 hex). Discriminant: `0x21`. |
| 7 | A | Weryfikacja: TTL nie przekroczony, klucz poprawny. Zapis do `trusted_nodes`, obliczenie shared secret. |
| 8 | A→B | `PairingConfirm` z kluczem publicznym A. |
| 9 | B | Ten sam proces weryfikacji i zapisu. |
| 10 | A→B | `TrustedKeysSync` — wyslanie kluczy publicznych wszystkich zaufanych nodow. Discriminant: `0x24`. |
| 11 | B | Dodanie otrzymanych kluczy przez `add_trusted_key()`. |

### PIN

- **Generowanie**: `OsRng.gen_range(100_000..999_999)` — 6 cyfr
- **TTL**: 60 sekund od wygenerowania
- **Weryfikacja**: Porownanie z zapisanym w `pending_pairings` + sprawdzenie `expires_at`
- **Po uzyciu**: Rekord usuwany z `pending_pairings`
- **Czyszczenie**: `cleanup_expired()` automatycznie co 30 sekund (task w pipeline) — usuwa wygasle rekordy z `pending_pairings`

### Automatyczna propagacja kluczy

Po zatwierdzeniu parowania node A wysyla do B klucze publiczne wszystkich swoich zaufanych nodow (`TrustedKeysSync`). Node B dodaje je przez `add_trusted_key()` — pomijajac wlasny klucz i juz znane nody.

Dodatkowo operacja `AddTrustedNode` w CRDT propaguje informacje o nowym zaufanym nodzie do calego mesh przez gossip.

---

## 4. Stany nodow

### Wykryty (discovered) — szary

- Node widoczny przez mDNS discovery
- Znany: adres IP, port, rola (z wlasciwosci mDNS)
- **Nieznany**: hostname, OS, CPU, RAM, GPU, kontenery
- **Brak** szyfrowanej komunikacji
- **Brak** wymiany danych CRDT, heartbeatow, forwardingu

### Oczekujacy na parowanie (pairing) — zolty

- PIN wygenerowany i wyswietlony na jednym z nodow
- Rekord w tabeli `pending_pairings` z kierunkiem (`outgoing`/`incoming`)
- TTL: 60 sekund — po tym czasie parowanie automatycznie wygasa
- Mozliwosc odrzucenia (`PairingReject`, discriminant `0x22`)

### Zaufany (trusted) — zielony

- Klucz publiczny zapisany w tabeli `trusted_nodes`
- Klucz weryfikujacy (Ed25519) w pamieciowej mapie `trusted_keys`
- Shared secret (ChaCha20) w pamieciowej mapie `shared_secrets`
- **Pelna komunikacja**: heartbeaty, CRDT sync, forwarding requestow, NodeInfo
- Pole `is_active = 1` w bazie

### Odlaczony (revoked)

- Usuniety z `trusted_nodes` w bazie
- Usuniety z `trusted_keys` i `shared_secrets` w pamieci
- Wiadomosc `TrustRevoked` (discriminant `0x23`) wyslana do mesh
- Operacja `RemoveTrustedNode` propagowana przez CRDT
- Node wraca do stanu "discovered" — wymaga ponownego parowania

---

## 5. Szyfrowanie i filtrowanie komunikacji

### Trojwarstwowe filtrowanie (defense in depth)

Kazda wiadomosc jest filtrowana na trzech poziomach:

1. **Warstwa wysylania** (`quic_mesh.rs`): `send_heartbeat_to_all`, `broadcast_node_info`, `broadcast_crdt_delta`, `send_node_info`, `forward_request` — sprawdzaja `is_trusted()` PRZED wyslaniem. Niezaufani peery sa pomijani.
2. **Warstwa odbierania** (`quic_mesh.rs`): `handle_uni_stream`, `handle_bidi_stream` — odrzucaja wiadomosci od niezaufanych (oprocz parowania 0x20-0x22).
3. **Warstwa pipeline** (`pipeline.rs`): event handler — dodatkowy safety net, ignoruje eventy od niezaufanych.

### Co jest szyfrowane (ChaCha20-Poly1305 na payloadzie)

- Operacje CRDT (synchronizacja stanu: serwisy, modele, aliasy, uzytkownicy, grupy)
- NodeInfo (hostname, OS, CPU, RAM, GPU)
- Gossip membership (Join, Leave, Ping)
- Forwarding requestow/odpowiedzi
- Heartbeaty z metrykami
- FullState (poczatkowa synchronizacja po polaczeniu)

### Co NIE jest szyfrowane (celowo)

- **mDNS discovery** — nody musza sie wykryc zanim moga sie sparowac. Informacja widoczna: node_id, port, rola. To samo co broadcast w sieci lokalnej.
- **Wiadomosci parowania** (0x20, 0x21, 0x22) — przesylane plaintext wewnatrz tunelu QUIC/TLS, bo shared secret jeszcze nie istnieje.

### Co jest ODRZUCANE od niezaufanych peerow

- Heartbeaty (0x10) — odrzucone, peer nie widzi naszych metryk
- CRDT delta (0x11) — odrzucone, peer nie moze synchronizowac stanu
- FullState (0x12) — nie wysylamy, peer nie dostaje poczatkowego stanu
- Forward request (0x13) — odrzucone, peer nie moze forwardowac requestow
- NodeInfo (0x18) — odrzucone, peer nie widzi hostname/OS/GPU
- Bidi stream (forward) — zamykany natychmiast

### Brak plaintext fallback

Jesli deszyfrowanie ChaCha20-Poly1305 nie powiedzie sie (zmanipulowany ciphertext, bledny klucz, replay attack), wiadomosc jest **ODRZUCANA** — nie ma fallbacku na plaintext. Dotyczy to zarowno wysylania (blad szyfrowania = nie wyslij) jak i odbierania (blad deszyfrowania = odrzuc).

### Format zaszyfrowanej wiadomosci

```
┌──────────────┬──────────────────────────────────┐
│  Nonce (12B) │  Ciphertext + Poly1305 Tag (16B) │
└──────────────┴──────────────────────────────────┘
│<─────── encrypt_for_node() ─────────────────────>│
```

- **Nonce**: 12 bajtow, losowy (`rand::random()`) — generowany per wiadomosc
- **Ciphertext**: zaszyfrowany payload
- **Tag**: 16 bajtow Poly1305 MAC — dolaczony automatycznie przez `chacha20poly1305::Aead`
- **Minimalny rozmiar**: 12 (nonce) + 16 (tag) = 28 bajtow dla pustego payloadu

### Podwojna warstwa szyfrowania

```
┌────────────────────────────────────────────────────────────┐
│  QUIC / TLS 1.3 (warstwa transportowa)                     │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  ChaCha20-Poly1305 (warstwa aplikacyjna)              │  │
│  │  ┌──────────────────────────────────────────────┐    │  │
│  │  │  Payload: CRDT ops / NodeInfo / Heartbeat     │    │  │
│  │  └──────────────────────────────────────────────┘    │  │
│  └──────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

- **TLS 1.3 (QUIC)**: Chroni tunel transportowy. Certyfikaty self-signed.
- **ChaCha20-Poly1305**: Dodatkowa warstwa — nawet gdyby TLS zostal zlamany (np. MITM z wlasnym cert), atakujacy nie odczyta payloadu bez shared secret.

---

## 6. API Reference

Wszystkie endpointy zwracaja JSON. Prefix: `/api/mesh/`.

### GET /api/mesh/peers

Lista wszystkich nodow widocznych w mesh (discovered + trusted).

**Odpowiedz (200):**
```json
[
  {
    "node_id": "550e8400-e29b-41d4-a716-446655440000",
    "addresses": ["192.168.1.10"],
    "port": 4433,
    "role": "router",
    "status": "connected",
    "quic_connected": true,
    "hostname": "tentaflow",
    "os_info": "Linux 6.8",
    "cpu_count": 16,
    "ram_total_mb": 32768,
    "cpu_usage_percent": 23.5,
    "ram_used_mb": 8192,
    "gpu_info": [],
    "containers": [],
    "networks": []
  }
]
```

### GET /api/mesh/trusted

Lista zaufanych (sparowanych) nodow.

**Odpowiedz (200):**
```json
[
  {
    "node_id": "550e8400-e29b-41d4-a716-446655440000",
    "public_key": "a1b2c3...128_hex_znakow",
    "hostname": "tentaflow",
    "approved_by": "admin",
    "approved_at": "2026-03-22 10:15:00",
    "is_active": 1
  }
]
```

### GET /api/mesh/pending

Lista oczekujacych parowan. Automatycznie czysci wygasle rekordy.

**Odpowiedz (200):**
```json
[
  {
    "remote_node_id": "node-xyz",
    "pin_code": "483921",
    "direction": "outgoing",
    "expires_at": "2026-03-22 10:16:00",
    "created_at": "2026-03-22 10:15:00"
  }
]
```

### POST /api/mesh/pair/:node_id

Rozpocznij parowanie z nodem — generuje PIN.

**Odpowiedz (200):**
```json
{
  "pin": "483921",
  "node_id": "node-xyz",
  "expires_in_seconds": 60
}
```

### POST /api/mesh/pair/:node_id/confirm

Potwierdz parowanie — przekaz klucz publiczny.

**Request body:**
```json
{
  "public_key": "a1b2c3...128_hex_znakow",
  "hostname": "moj-desktop"
}
```

**Odpowiedz (200):**
```json
{ "ok": true, "node_id": "node-xyz" }
```

**Bledy (400):**
```json
{ "error": "Parowanie wygaslo — wygeneruj nowy PIN" }
{ "error": "Brak oczekujacego parowania z nodem node-xyz" }
```

### POST /api/mesh/pair/:node_id/reject

Odrzuc parowanie.

**Odpowiedz (200):**
```json
{ "ok": true }
```

### DELETE /api/mesh/trust/:node_id

Cofnij zaufanie — node wraca do stanu "discovered".

**Odpowiedz (200):**
```json
{ "ok": true }
```

### GET /api/mesh/identity

Klucz publiczny tego noda.

**Odpowiedz (200):**
```json
{
  "public_key": "a1b2c3...128_hex_znakow",
  "ed25519_key": "a1b2c3...64_hex_znakow",
  "x25519_key": "d4e5f6...64_hex_znakow"
}
```

---

## 7. Scenariusze

### Dodanie pierwszego noda do sieci

1. Node A startuje — generuje klucze Ed25519 + X25519 (lub wczytuje z bazy)
2. Node A rejestruje sie w mDNS jako `_tentaflow-mesh._udp.local`
3. Node B startuje — to samo
4. mDNS discovery: A wykrywa B i B wykrywa A
5. Oba widoczne w UI jako "discovered" (szary)
6. Uzytkownik na A klika "Sparuj" przy B — PIN generowany
7. Proces parowania (sekcja 3)
8. Po zatwierdzeniu — oba nody zaufane, pelna komunikacja

### Dodanie kolejnego noda (automatyczna propagacja)

1. Node C dolacza do sieci — wykryty przez mDNS
2. Uzytkownik paruje C z A (PIN)
3. Po zatwierdzeniu A wysyla do C `TrustedKeysSync` z kluczem B
4. C automatycznie dodaje B do zaufanych (`add_trusted_key()`)
5. CRDT operacja `AddTrustedNode` propaguje klucz C do B
6. B oblicza shared secret z C — pelna komunikacja miedzy wszystkimi

### Cofniecie zaufania

1. Uzytkownik wywoluje `DELETE /api/mesh/trust/:node_id`
2. `revoke_trust()`: Usuniecie z `trusted_nodes` w DB, z `trusted_keys` i `shared_secrets` w pamieci
3. Wiadomosc `TrustRevoked` (discriminant `0x23`) wyslana do mesh
4. CRDT operacja `RemoveTrustedNode` propagowana do wszystkich peerow
5. Pozostale nody usuwaja klucz odwolnaego noda
6. Node wraca do stanu "discovered" — moze byc ponownie sparowany

### Zmiana IP noda

1. UUID noda jest staly — generowany raz i zapisany w bazie
2. IP moze sie zmienic (DHCP, restart sieci)
3. mDNS automatycznie aktualizuje adres IP w sieci lokalnej
4. QUIC reconnect do nowego adresu — polaczenie wznawiane
5. Klucze kryptograficzne bez zmian — zaufanie oparte na UUID + kluczu publicznym, nie na IP

### Node offline i powrot

1. Node traci polaczenie — status zmienia sie na "disconnected"
2. mDNS wyrejestrowanie (Drop)
3. Po powrocie: mDNS re-discovery
4. QUIC reconnect z exponential backoff (1s base, 30s max)
5. Klucze i shared secrets nadal wazne (w pamieci lub wczytane z DB)
6. Delta CRDT sync — tylko operacje nowsze niz version vector peera

---

## 8. Bezpieczenstwo — analiza zagrozen

### Podsluch mDNS

**Zagrożenie**: Atakujacy w tej samej sieci lokalnej widzi broadcasty mDNS.

**Co wycieka**: node_id (UUID), port QUIC, rola noda (router/desktop/mobile).

**Co NIE wycieka**: Klucze kryptograficzne, dane CRDT, hostname, metryki.

**Ryzyko**: **Niskie**. Informacja o istnieniu noda nie daje mozliwosci komunikacji — bez parowania nie ma wymiany kluczy ani shared secret.

### Przechwycenie PIN

**Zagrożenie**: Atakujacy podsluchuje PIN podczas transmisji PairingRequest przez QUIC.

**Ochrona**: PairingRequest przesylany przez QUIC (TLS 1.3) — PIN jest szyfrowany na warstwie transportowej. Atakujacy musialby zlamac TLS zeby odczytac PIN.

**Dodatkowa ochrona**: PIN wazny 60 sekund. Po jednorazowym uzyciu usuwany. Wymaga fizycznego potwierdzenia na obu nodach (UI).

**Ryzyko**: **Niskie** — wymaga zlamania TLS 1.3 ORAZ fizycznego dostepu do UI w ciagu 60 sekund.

### Wyciek klucza prywatnego

**Zagrożenie**: Atakujacy uzyskuje dostep do bazy SQLite i odczytuje klucz prywatny z tabeli `settings`.

**Skutki**:
- Moze podszywac sie pod node (podpisy Ed25519)
- Moze odszyfrowac cala komunikacje z tym nodem (shared secrets z X25519)
- Moze odszyfrowac przeszla komunikacje (brak forward secrecy)

**Srodki zaradcze**:
1. Cofnac zaufanie dla skompromitowanego noda na wszystkich pozostalych nodach
2. Wygenerowac nowe klucze na skompromitowanym nodzie (usunac rekordy z `settings`)
3. Ponownie sparowac nody

**Ryzyko**: **Wysokie** jesli atakujacy ma dostep do pliku bazy danych.

### Ograniczenia i znane ryzyka

| Ograniczenie | Opis |
|---|---|
| **Brak forward secrecy** | Klucze X25519 sa statyczne (nie efemeryczne). Kompromitacja klucza prywatnego pozwala odszyfrowac przeszla komunikacje. |
| **Klucze w plaintext w DB** | Klucze prywatne przechowywane jako hex w SQLite bez dodatkowego szyfrowania. Zabezpieczenie zalezy od uprawnien plikowych. |
| **PIN 6-cyfrowy** | Przestrzen 899999 kombinacji. Przy brute-force przez siec: rate limiting QUIC + 60s TTL = bezpieczne. Przy offline ataku: nie dotyczy (PIN jednorazowy, nie kryptograficzny). |
| **Self-signed TLS** | QUIC uzywa certyfikatow self-signed — mozliwy MITM na warstwie TLS. Dlatego dodatkowa warstwa ChaCha20-Poly1305 na payloadzie. |
| **Siec lokalna** | System zaprojektowany na sieci lokalne (LAN). mDNS nie dziala przez internet. Uzycie w WAN wymaga dodatkowej konfiguracji (VPN, tunel). |

---

## Pliki zrodlowe

| Plik | Odpowiedzialnosc |
|---|---|
| `mesh/security.rs` | Generowanie kluczy, parowanie, szyfrowanie/deszyfrowanie ChaCha20, HKDF, podpisy Ed25519 |
| `mesh/quic_mesh.rs` | Transport QUIC — filtrowanie wg trusted, szyfrowanie/deszyfrowanie payloadu, send/receive wiadomosci |
| `mesh/pipeline.rs` | Inicjalizacja MeshSecurity, integracja z mDNS i QUIC, filtrowanie eventow, czyszczenie wygaslych parowan |
| `api/dashboard/api_mesh.rs` | Endpointy REST API dla mesh security, wysylanie PairingRequest/Confirm/Reject przez QUIC |
| `api/dashboard/api_agents.rs` | Filtrowanie danych agentow wg trust_status, blokowanie 403 dla niezaufanych |
| `db/migrations.rs` | Migracja #15: tabele `trusted_nodes` i `pending_pairings` |
| `mesh/crdt.rs` | Operacje `AddTrustedNode` / `RemoveTrustedNode` — propagacja zaufania |
| `Protocol/src/mesh.rs` | Stale discriminantow QUIC: `PairingRequest` (0x20), `PairingConfirm` (0x21), `PairingReject` (0x22) |
