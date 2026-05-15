# Host Functions — Dokumentacja ABI addonow WASM

Addony TentaFlow dzialaja jako moduly WASM (WebAssembly) uruchamiane w sandboxie.
Komunikacja z hostem (Core) odbywa sie wylacznie przez zdefiniowane host functions.
Kazda funkcja operuje na pamieci liniowej guest (WASM) przez wskazniki i dlugosci.

---

## Konwencje ABI

- Parametry tekstowe: `(ptr: i32, len: i32)` — wskaznik i dlugosc UTF-8 w pamieci guest
- Bufor wyjsciowy: `(out_ptr: i32, out_capacity: i32, out_len_ptr: i32)` — wskaznik, pojemnosc, oraz wskaznik na u32 LE z faktycznym rozmiarem
- Zwracana wartosc `i32`: kod statusu (0 = sukces, !=0 = blad)
- Zwracana wartosc `i64` (packed): `(status << 32) | data_length` — status w gornych 32 bitach, dlugosc danych w dolnych

### Limity payloadu (`PayloadKind`)

Kazda host function ktora przyjmuje payload od addona MUSI sprawdzic rozmiar przez `enforce_payload_size`. Przekroczenie → `ABI_ERR_PAYLOAD_TOO_LARGE` (21).

| Kategoria | Max | Uzycie |
|-----------|-----|--------|
| `ServiceCall` | 8 MB | `service_call(alias, method, payload)` |
| `SqlCombined` | 4 MB | `sql_exec/query` — query + params zlozone |
| `VectorItem` | 1 MB | `vector_upsert` per item |
| `UiRender` | 2 MB | `ui_render` — drzewo komponentow |
| `Secret` | 64 KB | `secret_set/get` — wartosc |

### out_cap retry pattern

Kazda host function zwracajaca dane w buforze wyjsciowym uzywa ujednoliconej semantyki retry:

1. Caller alokuje bufor o rozmiarze `out_cap`, przekazuje pointer + capacity + out_len_ptr.
2. Host function probuje pisac:
   - Jesli `actual_size <= out_cap` → zapisuje, `*out_len_ptr = actual_size`, returns `ABI_OK` (0).
   - Jesli `actual_size > out_cap` → NIE pisze, `*out_len_ptr = actual_size` (wymagany rozmiar), returns `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` (6).
3. Caller realokuje bufor do `actual_size` (z marginesem +10%) i powtarza wywolanie.
4. Max retry: 1 (drugi `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` → addon bug, audit anomaly).

Przyklad uzycia w SDK (Rust):

```rust
const INITIAL_CAP: usize = 4096;
let mut buf = vec![0u8; INITIAL_CAP];
let mut out_len: u32 = 0;
let status = unsafe {
    host_fn(..., buf.as_mut_ptr() as i32, buf.len() as i32, &mut out_len as *mut u32 as i32)
};
if status == ABI_ERR_OUTPUT_BUFFER_TOO_SMALL {
    let need = (out_len as usize) + (out_len as usize / 10); // +10% margin
    buf = vec![0u8; need];
    let status2 = unsafe {
        host_fn(..., buf.as_mut_ptr() as i32, buf.len() as i32, &mut out_len as *mut u32 as i32)
    };
    // Drugi blad = bug addona; audit_anomaly zostal juz zapisany przez host.
    assert_eq!(status2, ABI_OK);
}
buf.truncate(out_len as usize);
```

### Domyslne timeouty per kategoria

| Kategoria | Timeout |
|-----------|---------|
| `service_call` | 30 s |
| `sql_*` (exec/query) | 30 s |
| `vector_*` (upsert/search) | 5 s |
| `camera_*` (probe/connect) | 15 s |
| `recording_*` (save/get_url) | 60 s |

Przekroczenie → `ABI_ERR_TIMEOUT` (4).

---

## Globalne kody bledow ABI

Wartosci dodatnie (1..24) wprowadzone w F1a (M0.W2) sa kanoniczne dla nowych host functions (SQL, Alias, Camera, Streaming, Recording). Pre-F1a host functions (`storage_*`, `http_*`, `llm_*`, `ui_*`, `events_*`, `secret_*`) uzywaja starych stalych ujemnych (`ABI_ERR_PERMISSION = -1`, ...) — zachowanych dla wstecznej kompatybilnosci. Mapowanie jest jednoznaczne po znaku wartosci.

| Kod | Stala (F1a) | Opis |
|-----|-------------|------|
| 0  | `ABI_OK` | Operacja zakonczona pomyslnie |
| 1  | `ABI_ERR_PERMISSION` | Brak wymaganych uprawnien |
| 2  | `ABI_ERR_NOT_FOUND` | Zasob nie znaleziony |
| 3  | `ABI_ERR_NO_AVAILABLE_TARGET` | Brak dostepnego targetu dla aliasu |
| 4  | `ABI_ERR_TIMEOUT` | Przekroczono limit czasu |
| 5  | `ABI_ERR_OPERATION` | Ogolny blad operacji |
| 6  | `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` | Bufor wyjsciowy za maly (out_len_ptr ma wymagany rozmiar) |
| 7  | `ABI_ERR_CONFLICT` | Konflikt stanu (duplikat) |
| 8  | `ABI_ERR_SQL_SYNTAX` | Bledna skladnia SQL |
| 9  | `ABI_ERR_SQL_CONSTRAINT` | Naruszenie constraint SQL (UNIQUE/NOT NULL/FK/CHECK) |
| 10 | `ABI_ERR_SQL_NO_RESULT` | Zapytanie SQL nie zwrocilo wyniku |
| 11 | `ABI_ERR_QUOTA_EXCEEDED` | Przekroczono kwote zasobow |
| 12 | `ABI_ERR_CAMERA_UNREACHABLE` | Kamera niedostepna |
| 13 | `ABI_ERR_CAMERA_AUTH_FAILED` | Bledne dane uwierzytelniajace kamery |
| 14 | `ABI_ERR_CAMERA_VENDOR_UNSUPPORTED` | Vendor kamery nieobslugiwany |
| 15 | `ABI_ERR_STREAM_NOT_FOUND` | Strumien nie znaleziony |
| 16 | `ABI_ERR_STREAM_CLOSED` | Strumien zamkniety |
| 17 | `ABI_ERR_BACKPRESSURE` | Addon nie nadaza za strumieniem |
| 18 | `ABI_ERR_RECORDING_NOT_FOUND` | Nagranie nie znalezione |
| 19 | `ABI_ERR_RECORDING_PURGED` | Nagranie wyczyszczone (retention) |
| 20 | `ABI_ERR_RECORDING_TIME_OUT_OF_RING` | Timestamp poza zakresem ring-buffera |
| 21 | `ABI_ERR_PAYLOAD_TOO_LARGE` | Payload przekroczyl limit wielkosci |
| 22 | `ABI_ERR_GATE_NOT_SATISFIED` | Gate niespelniony — operacja zablokowana |
| 23 | `ABI_ERR_FRAME_TOKEN_INVALID` | PickupToken/FrameToken nieprawidlowy lub wygasly |
| 24 | `ABI_ERR_FRAME_PURGED` | Frame zostal wyczyszczony |

Stare kody pre-F1a (zachowane dla `storage_*`, `http_*`, `llm_*`, `ui_*`, `events_*`, `secret_*`):

| Kod | Stala | Opis |
|-----|-------|------|
| -1 | `ABI_ERR_PERMISSION` (legacy) | Brak uprawnien |
| -2 | `ABI_ERR_OPERATION` (legacy) | Blad operacji |
| -3 | `ABI_ERR_TIMEOUT` (legacy) | Timeout |
| -4 | `ABI_ERR_RATE_LIMIT` (legacy) | Rate limit |
| -5 | `ABI_ERR_NOT_FOUND` (legacy) | Nie znaleziono |
| -6 | `ABI_ERR_BUFFER_TOO_SMALL` (legacy) | Bufor za maly |

---

## Versioning ABI

### Konwencja nazw

Nowe host functions F1a uzywaja sufiksu `_v1` (np. `sql_exec_v1`, `alias_get_v1`, `camera_add_v1`). Sufiks otwiera droge do `_v2` przy lamiacych zmianach bez breaking caller'ow `_v1`.

### Manifest

Addon moze zadeklarowac wymagana wersje SDK rdzenia:

```toml
[addon]
sdk_version = ">=0.2.0, <1.0"  # semver VersionReq
```

Pole jest opcjonalne — brak deklaracji = zakladamy kompatybilnosc.

### Mechanizm rejekcji

Rdzen eksportuje `CORE_SDK_VERSION = "0.2.0"` (stala kompilacji w `addon::sdk_version`). Przy instalacji `lifecycle::install` parsuje `manifest.sdk_version` jako `semver::VersionReq` i sprawdza dopasowanie do `CORE_SDK_VERSION`. Mismatch → install rolled back z `AbiError::Operation` (kod 5) i czytelnym komunikatem (`Addon 'X' wymaga SDK 'Y', rdzen ma 'Z'`).

Bumpujemy `CORE_SDK_VERSION` przy:
- usunieciu host function,
- zmianie ABI signatury istniejacego `_v1`,
- zmianie semantyki zwracanych kodow bledow.

Dodanie nowej host function lub nowego pola manifestu nie wymaga bumpu (addony nie deklaruja czego nie potrzebuja).

---

## Audit log — risk_class

Wpisy `audit_log` od F1a maja kolumne `risk_class` klasyfikujaca operacje wg RODO. Indeks partial `idx_audit_risk_class` (WHERE risk_class IN ('B','C')) umozliwia szybkie kwerendy zgodnosciowe.

| Klasa | Kiedy uzyc |
|-------|------------|
| `A` | Operacje administracyjne / techniczne bez danych osobowych (start/stop, config) |
| `B` | Operacje na danych osobowych zwyklych (RODO art. 6 — kontakty, identyfikatory) |
| `C` | Dane wrazliwe / biometryczne / decyzje automatyczne (RODO art. 9, art. 22 — rozpoznanie twarzy, ADR) |
| `unclassified` | Backward compat — wpisy sprzed F1a; domyslna wartosc kolumny |

Preferowane API host-side: `audit_log_with_risk(state, action, resource_type, resource_id, risk_class, related_claim_id, request_id, result, error_message)`. Stara funkcja `audit_log(...)` deleguje z `RiskClass::Unclassified` — istniejacy kod nie wymaga zmian.

`related_claim_id` (powiazany claim, F2) i `request_id` (korelacja wielu wpisow w obrebie jednego service_call) sa opcjonalne — przekazuj `None` gdy nie dotycza.

---

## 1. Storage API

Trwale przechowywanie danych key-value per addon. Dane izolowane miedzy addonami.

**Wymagane uprawnienie:** `storage`

### `storage_get(key_ptr, key_len, out_ptr, out_capacity, out_len_ptr) -> i32`

Pobiera wartosc powiazana z kluczem.

| Parametr | Typ | Opis |
|----------|-----|------|
| `key_ptr` | `i32` | Wskaznik na klucz (UTF-8) |
| `key_len` | `i32` | Dlugosc klucza w bajtach |
| `out_ptr` | `i32` | Wskaznik na bufor wyjsciowy |
| `out_capacity` | `i32` | Pojemnosc bufora wyjsciowego |
| `out_len_ptr` | `i32` | Wskaznik na 4 bajty — zostanie zapisana dlugosc wyniku (LE) |

**Zwraca:** `ABI_OK` (0) przy sukcesie, `ABI_ERR_NOT_FOUND` (-3) jesli klucz nie istnieje.

### `storage_set(key_ptr, key_len, val_ptr, val_len) -> i32`

Zapisuje wartosc pod kluczem (upsert).

| Parametr | Typ | Opis |
|----------|-----|------|
| `key_ptr` | `i32` | Wskaznik na klucz (UTF-8) |
| `key_len` | `i32` | Dlugosc klucza |
| `val_ptr` | `i32` | Wskaznik na wartosc (bajty) |
| `val_len` | `i32` | Dlugosc wartosci |

**Zwraca:** `ABI_OK` (0) przy sukcesie.

### `storage_delete(key_ptr, key_len) -> i32`

Usuwa klucz ze storage.

**Zwraca:** `ABI_OK` (0) przy sukcesie, `ABI_ERR_NOT_FOUND` (-3) jesli klucz nie istnieje.

### `storage_list(prefix_ptr, prefix_len, out_ptr, out_capacity) -> i64`

Listuje klucze z podanym prefixem. Wynik jako JSON array stringow.

**Zwraca:** packed `i64` = `(status << 32) | json_length`

---

## 2. HTTP API

Wykonywanie requestow HTTP/HTTPS z sandboxa. Wbudowana ochrona SSRF.

**Wymagane uprawnienie:** `http`

### `http_request(method_ptr, method_len, url_ptr, url_len, headers_ptr, headers_len, body_ptr, body_len, out_ptr, out_capacity) -> i64`

Wykonuje request HTTP.

| Parametr | Typ | Opis |
|----------|-----|------|
| `method_ptr/len` | `i32` | Metoda HTTP (GET, POST, PUT, DELETE) |
| `url_ptr/len` | `i32` | URL docelowy |
| `headers_ptr/len` | `i32` | Naglowki jako JSON object `{"key": "value"}` |
| `body_ptr/len` | `i32` | Body requestu (bajty, moze byc puste — len=0) |
| `out_ptr/capacity` | `i32` | Bufor na odpowiedz JSON |

**Zwraca:** packed `i64` = `(status << 32) | response_length`

**Odpowiedz JSON:**
```json
{
  "status": 200,
  "headers": {"content-type": "application/json"},
  "body": "..."
}
```

**Ochrona SSRF:**
- Blokowanie adresow prywatnych (RFC 1918): `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`
- Blokowanie loopback: `127.0.0.0/8`, `::1`
- Blokowanie link-local: `169.254.0.0/16`, `fe80::/10`
- Blokowanie metadata chmurowych: `169.254.169.254`
- Dozwolone domeny ograniczone przez `[permissions.http]` w manifescie

---

## 3. Secrets API

Bezpieczne przechowywanie sekretow (tokeny API, hasla). Szyfrowanie AES-256-GCM per addon per user.

**Wymagane uprawnienie:** `secrets`

### `secret_get(key_ptr, key_len, out_ptr, out_capacity) -> i64`

Pobiera odszyfrowany sekret.

**Zwraca:** packed `i64` = `(status << 32) | secret_length`

### `secret_set(key_ptr, key_len, val_ptr, val_len) -> i32`

Zapisuje zaszyfrowany sekret.

**Zwraca:** `ABI_OK` (0) przy sukcesie.

**Bezpieczenstwo:**
- Sekrety szyfrowane AES-256-GCM z unikalnym kluczem per addon per user
- Klucze szyfrujace nigdy nie opuszczaja Core — addon otrzymuje tylko odszyfrowane dane
- Sekrety usuwane przy deinstalacji addonu

---

## 4. Log API

Logowanie diagnostyczne z poziomu addonu. Logi widoczne w panelu administracyjnym.

**Wymagane uprawnienie:** `log`

### `log_info(msg_ptr, msg_len)`

Loguje wiadomosc na poziomie INFO.

### `log_warn(msg_ptr, msg_len)`

Loguje wiadomosc na poziomie WARN.

### `log_error(msg_ptr, msg_len)`

Loguje wiadomosc na poziomie ERROR.

**Uwagi:**
- Wiadomosci sa automatycznie tagowane addon_id i instancja
- Maksymalna dlugosc wiadomosci: 4096 bajtow (obcinane przy przekroczeniu)
- Logi dostepne przez `GET /api/audit` z filtrem `action=addon.log.*`

---

## 5. Events API

Komunikacja miedzy addonami i z systemem przez event bus (pub/sub).

**Wymagane uprawnienie:** `events`

### `event_subscribe(topic_ptr, topic_len) -> i32`

Subskrybuje topic — eventy beda dostarczane do `on_event()`.

| Parametr | Typ | Opis |
|----------|-----|------|
| `topic_ptr/len` | `i32` | Nazwa topicu (UTF-8, np. "chat.message", "addon.my_addon.custom") |

**Zwraca:** `ABI_OK` (0) przy sukcesie.

### `event_publish(topic_ptr, topic_len, data_ptr, data_len) -> i32`

Publikuje event na topic.

| Parametr | Typ | Opis |
|----------|-----|------|
| `topic_ptr/len` | `i32` | Nazwa topicu |
| `data_ptr/len` | `i32` | Dane eventu (JSON) |

**Zwraca:** `ABI_OK` (0) przy sukcesie.

**Topiki systemowe (tylko subskrypcja):**
- `chat.message` — nowa wiadomosc w czacie
- `chat.response` — odpowiedz LLM
- `user.login` / `user.logout` — sesja uzytkownika
- `addon.installed` / `addon.uninstalled` — cykl zycia addonow

---

## 6. UI API

Renderowanie paneli UI w dashboardzie. Addon deklaruje komponenty jako JSON (deklaratywny UI).

**Wymagane uprawnienie:** `ui`

### `ui_register_panel(name_ptr, name_len, html_ptr, html_len) -> i32`

Rejestruje panel UI addonu.

| Parametr | Typ | Opis |
|----------|-----|------|
| `name_ptr/len` | `i32` | Nazwa panelu (unikalna per addon, np. "main", "settings") |
| `html_ptr/len` | `i32` | Definicja UI jako JSON (deklaratywny format komponentow) |

**Zwraca:** `ABI_OK` (0) przy sukcesie.

**Format JSON UI:**
```json
{
  "type": "column",
  "children": [
    {"type": "text", "props": {"content": "Tytul", "variant": "heading"}},
    {"type": "button", "props": {"label": "Kliknij", "action_id": "my_action"}}
  ]
}
```

---

## 7. User API

Informacje o aktualnie zalogowanym uzytkowniku.

**Wymagane uprawnienie:** `user_info`

### `user_get_current(out_ptr, out_capacity) -> i64`

Pobiera dane aktualnego uzytkownika jako JSON.

**Zwraca:** packed `i64` = `(status << 32) | json_length`

**Odpowiedz JSON:**
```json
{
  "user_id": 1,
  "username": "jan",
  "display_name": "Jan Kowalski",
  "is_admin": false,
  "groups": ["developers", "testers"]
}
```

### `user_check_permission(perm_ptr, perm_len) -> i32`

Sprawdza czy uzytkownik ma dane uprawnienie.

**Zwraca:** `1` jesli ma uprawnienie, `0` jesli nie, `<0` przy bledzie.

---

## 8. LLM API

Generowanie tekstu przez modele LLM dostepne w systemie.

**Wymagane uprawnienie:** `llm`

### `llm_generate(prompt_ptr, prompt_len, model_ptr, model_len, out_ptr, out_capacity) -> i64`

Generuje tekst za pomoca modelu LLM.

| Parametr | Typ | Opis |
|----------|-----|------|
| `prompt_ptr/len` | `i32` | Prompt (UTF-8) |
| `model_ptr/len` | `i32` | Nazwa modelu (np. "default", "gpt-4", pusty = domyslny) |
| `out_ptr/capacity` | `i32` | Bufor na wygenerowany tekst |

**Zwraca:** packed `i64` = `(status << 32) | text_length`

**Limity:**
- Tokeny per minuta ograniczone przez `addon_resource_limits.llm_tokens_per_min`
- Model musi byc dostepny i skonfigurowany w systemie

---

## 9. Network API (proxy TCP/UDP)

Proxy sieciowe — addon nie laczy sie bezposrednio z siecia. Core proxy waliduje reguly,
zatwierdzenia, sprawdza DNS/IP (SSRF) i loguje kazda operacje.

**Wymagane uprawnienie:** `network`

**Wymagana deklaracja:** sekcja `[[network_rules]]` w manifescie

**Wymagane zatwierdzenie:** admin musi zatwierdzic kazda regule przed uzyciem

### `net_connect(rule_id_ptr, rule_id_len) -> i32`

Nawiazuje polaczenie TCP/UDP wedlug reguly z manifestu.

| Parametr | Typ | Opis |
|----------|-----|------|
| `rule_id_ptr/len` | `i32` | Identyfikator reguly sieciowej (UTF-8, np. "my_database") |

**Zwraca:** `conn_id` (>0) przy sukcesie, kod bledu (<0) przy niepowodzeniu.

**Przebieg:**
1. Odczytaj rule_id z pamieci guest
2. Sprawdz uprawnienie `network`
3. Znajdz regule w manifescie addonu
4. Sprawdz zatwierdzenie reguly w DB (`approved=1`)
5. Sprawdz limit polaczen (max 10 per instancja)
6. DNS resolve + walidacja IP (SSRF — blokowanie adresow prywatnych)
7. Nawiaz polaczenie TCP/UDP z timeoutami
8. Zapisz w ConnectionManager, zwroc conn_id

### `net_send(conn_id, data_ptr, data_len) -> i32`

Wysyla dane przez aktywne polaczenie.

| Parametr | Typ | Opis |
|----------|-----|------|
| `conn_id` | `i32` | Identyfikator polaczenia (z net_connect) |
| `data_ptr/len` | `i32` | Dane do wyslania (bajty) |

**Zwraca:** liczbe wyslanych bajtow (>0) lub kod bledu (<0).

### `net_recv(conn_id, out_ptr, out_capacity) -> i64`

Odbiera dane z aktywnego polaczenia.

| Parametr | Typ | Opis |
|----------|-----|------|
| `conn_id` | `i32` | Identyfikator polaczenia |
| `out_ptr/capacity` | `i32` | Bufor na odebrane dane |

**Zwraca:** packed `i64` = `(status << 32) | bytes_read`

Timeout odczytu (60s) nie jest bledem krytycznym — zwraca 0 bajtow.

### `net_close(conn_id) -> i32`

Zamyka aktywne polaczenie. Usuniecie z ConnectionManager powoduje drop socketu.

**Zwraca:** `ABI_OK` (0) przy sukcesie.

### Kody bledow sieciowych

| Kod | Stala | Opis |
|-----|-------|------|
| -8 | `ABI_ERR_NETWORK_RULE_NOT_FOUND` | Regula sieciowa nie znaleziona w manifescie addonu |
| -9 | `ABI_ERR_NETWORK_RULE_NOT_APPROVED` | Regula sieciowa nie zostala zatwierdzona przez admina |
| -10 | `ABI_ERR_MAX_CONNECTIONS` | Przekroczono limit polaczen per addon (max 10) |
| -11 | `ABI_ERR_CONNECTION_NOT_FOUND` | Polaczenie o podanym ID nie istnieje |
| -12 | `ABI_ERR_CONNECTION_FAILED` | Nie udalo sie nawiazac polaczenia (DNS, timeout, odmowa) |

### Stale konfiguracyjne

| Stala | Wartosc | Opis |
|-------|---------|------|
| `MAX_CONNECTIONS_PER_ADDON` | 10 | Maksymalna liczba jednoczesnych polaczen per instancja |
| `CONNECT_TIMEOUT` | 30s | Timeout nawiazywania polaczenia TCP |
| `RECV_TIMEOUT` | 60s | Timeout odczytu danych z socketu |
| `SEND_TIMEOUT` | 30s | Timeout wysylania danych |

### Ochrona SSRF

Walidacja IP przy kazdym polaczeniu — blokowane adresy:
- Loopback: `127.0.0.0/8`, `::1`
- Prywatne RFC 1918: `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`
- Link-local: `169.254.0.0/16`, `fe80::/10`
- Metadata chmurowe: `169.254.169.254`
- Unique local IPv6: `fd00::/8`
- IPv4-mapped IPv6: `::ffff:x.x.x.x` — sprawdzany wewnetrzny adres IPv4

---

## Manifest — kompletny format

Manifest addonu (`manifest.toml`) definiuje metadane, uprawnienia, narzedzia, reguly sieciowe i konfiguracje.

### `[addon]` — metadane addonu

```toml
[addon]
id = "moj-addon"                    # Unikalny identyfikator (a-z, 0-9, '.', '-', '_', max 128 zn.)
name = "Moj Addon"                  # Nazwa wyswietlana
version = "1.0.0"                   # Wersja semver
description = "Opis addonu"         # Krotki opis
author = "Autor"                    # Autor/firma
license = "MIT"                     # Licencja
min_core_version = "0.1.0"         # Minimalna wersja Core
wasm_file = "addon.wasm"           # Sciezka do pliku WASM (domyslnie "addon.wasm")
```

### `[platforms]` — obsługiwane platformy

```toml
[platforms]
targets = ["linux_x86_64", "linux_aarch64", "macos_x86_64", "macos_aarch64", "windows_x86_64", "android", "ios"]
```

### `[permissions]` — uprawnienia systemowe

```toml
[permissions]
# Wymagane — bez nich addon nie zadziala
[[permissions.required]]
type = "storage"        # Typ uprawnienia
access = "rw"           # Poziom dostepu
reason = "Przechowywanie stanu"  # Uzasadnienie dla admina

# Opcjonalne — admin moze przyznac lub odmowic
[[permissions.optional]]
type = "http"
resource = "*"          # Wzorzec zasobu (opcjonalny)
access = "rw"
reason = "Wykonywanie requestow HTTP"
```

**Dostepne typy uprawnien:**
`llm`, `llm_model`, `embeddings`, `rag`, `storage`, `http`, `events`, `ui`,
`audio`, `audio_capture`, `audio_play`, `tts`, `stt`, `camera`, `notifications`,
`background`, `secrets`, `user_info`, `timer`, `addon_communicate`, `log`, `network`

### `[[addon_permissions]]` — granularne uprawnienia deklarowane przez addon

```toml
[[addon_permissions]]
id = "manage_users"
name = "Zarzadzanie uzytkownikami"
description = "Pozwala addonowi zarzadzac uzytkownikami"
category = "admin"
```

### `[config_schema]` — schemat konfiguracji (JSON Schema)

```toml
[config_schema]
type = "object"
properties.api_key = { type = "string", title = "Klucz API", description = "Klucz do zewnetrznego serwisu" }
properties.language = { type = "string", title = "Jezyk", enum = ["pl", "en"], default = "pl" }
properties.max_results = { type = "integer", title = "Maks. wynikow", default = 10, minimum = 1, maximum = 100 }
```

### `[tools]` — narzedzia LLM (tool calling)

```toml
[tools]
[[tools.list]]
name = "search"
description = "Wyszukuje dane w zewnetrznym serwisie"
parameters = '{"type":"object","properties":{"query":{"type":"string","description":"Fraza wyszukiwania"}},"required":["query"]}'
```

### `[[network_rules]]` — reguly sieciowe TCP/UDP

```toml
[[network_rules]]
id = "my_database"          # Unikalny identyfikator reguly (uzywany w net_connect)
protocol = "tcp"            # Protokol: "tcp" lub "udp"
host = "db-server.local"   # Host docelowy (DNS lub IP)
port = 5432                 # Port docelowy
description = "Baza PostgreSQL"  # Opis dla admina
required = true             # true = addon nie zadziala bez tej reguly

[[network_rules]]
id = "metrics_collector"
protocol = "udp"
host = "statsd.local"
port = 8125
description = "Metryki StatsD"
required = false            # false = addon dziala bez tej reguly
```

**Wazne:** Kazda regula wymaga zatwierdzenia admina (`approved=1`) przed uzyciem.
Niezatwierdzone reguly zwracaja `ABI_ERR_NETWORK_RULE_NOT_APPROVED` (-9) przy probie polaczenia.

### `[resources]` — limity zasobow

```toml
[resources]
max_instances = 5           # Maks. jednoczesnych instancji
cpu_millicores = 100        # Limit CPU (0 = bez limitu)
ram_mb = 64                 # Limit RAM w MB
storage_mb = 10             # Limit storage w MB
rate_limit_rps = 10         # Limit requestow per sekunde
```

### `[lifecycle]` — timeouty cyklu zycia

```toml
[lifecycle]
install_timeout_ms = 10000  # Timeout on_install()
start_timeout_ms = 5000     # Timeout on_start()
stop_timeout_ms = 3000      # Timeout on_stop()
```

### `[ui]` — deklaracja paneli UI

```toml
[ui]
has_settings_panel = true       # Addon ma panel ustawien
has_dashboard_widget = false    # Addon ma widget na dashboardzie
```

---

## Przyklad — kompletny manifest

```toml
[addon]
id = "db-monitor"
name = "Monitor bazy danych"
version = "1.0.0"
description = "Monitoruje wydajnosc bazy PostgreSQL i wyswietla metryki"
author = "TentaFlow"
license = "MIT"
min_core_version = "0.1.0"

[platforms]
targets = ["linux_x86_64", "macos_aarch64"]

[permissions]
[[permissions.required]]
type = "storage"
access = "rw"
reason = "Przechowywanie historii metryk"

[[permissions.required]]
type = "log"
access = "ro"
reason = "Logowanie diagnostyczne"

[[permissions.required]]
type = "network"
access = "rw"
reason = "Polaczenie z baza danych"

[[permissions.optional]]
type = "ui"
access = "rw"
reason = "Panel metryk na dashboardzie"

[[permissions.optional]]
type = "notifications"
access = "rw"
reason = "Alerty o problemach wydajnosci"

[[network_rules]]
id = "postgres_main"
protocol = "tcp"
host = "db-server.local"
port = 5432
description = "Glowna baza PostgreSQL"
required = true

[config_schema]
type = "object"
properties.interval_sec = { type = "integer", title = "Interwat sprawdzania (s)", default = 30 }
properties.alert_threshold_ms = { type = "integer", title = "Prog alertu (ms)", default = 1000 }

[tools]
[[tools.list]]
name = "db_status"
description = "Zwraca aktualny status bazy danych (polaczenia, latency, cache hit ratio)"
parameters = '{}'

[lifecycle]
install_timeout_ms = 10000
start_timeout_ms = 5000
stop_timeout_ms = 3000

[ui]
has_settings_panel = true
has_dashboard_widget = true
```

---

## 10. Nowe API w F1a (planowane do implementacji)

Sekcja informacyjna — pelna dokumentacja per API zostanie dodana wraz z
implementacja w odpowiednich tygodniach planu F1a. Zarys w
`notes/tentavision-plan.md` §6 (ABI kontrakty).

| API | Funkcje | Tydzien |
|-----|---------|---------|
| **SQL** | `sql_exec_v1`, `sql_query_v1`, `sql_query_one_v1`, `sql_transaction_v1` | M0.W2 stub, M1.W5 pelne |
| **Aliases (readonly)** | `alias_get_v1`, `alias_list_owned_v1` | M1.W5 |
| **Camera** | `camera_add_v1`, `camera_list_v1`, `camera_get_v1`, `camera_snapshot_v1`, `camera_discover_v1`, `camera_test_connection_v1`, `camera_health_v1`, `camera_credentials_rotate_v1`, `camera_remove_v1`, `camera_update_v1` | M0.W2 stub, M2 pelne |
| **Streaming** | `stream_subscribe_v1`, `stream_next_v1`, `stream_close_v1` | M0.W2 stub, M2 pelne |
| **Recording** | `recording_save_segment_v1`, `recording_save_snapshot_v1`, `recording_get_url_v1`, `recording_stats_v1`, `frame_url_v1` | M0.W2 stub, M2 pelne (basic) |
| **service_call (extended)** | `service_call_v1(alias, method, payload)` — istniejace `service_call` rozszerzone o parametr `method` | M0.W2 stub, M2 pelne |
| **Vector** | `vector_upsert_v1`, `vector_search_v1`, `vector_count_v1`, `vector_delete_v1` | F2 |
| **Claims/Gates** | `claim_add_v1`, `claim_check_v1`, `claim_revoke_v1`, `gate_check_v1`, `gate_enforce_v1` | F2 |
| **Flow** | `flow_invoke_v1`, `flow_status_v1`, `flow_list_v1`, `flow_get_v1` | F2 |
| **Audit** | `audit_log_with_risk_v1`, `audit_query_v1`, `audit_export_v1`, `audit_verify_v1` | M0.W3 risk_class, F2 export |

**Konwencja nazewnictwa:** wszystkie nowe host functions koncza sie na `_v1`
(versioning ABI). Kolejne wersje (`_v2`, ...) bedzie wprowadzane bez usuwania
starszych, dopoki istnieje addon korzystajacy ze starej wersji.

**Kody bledow:** nowy enum `AbiError` z 24 kodami z `tentavision-plan.md` §6.2.Y
zostanie dodany w M0.W2 (`src/addon/errors.rs`). Najwazniejsze nowe kody:
`ABI_ERR_PAYLOAD_TOO_LARGE`, `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` (retry semantics
z `*out_len_ptr` = required size), `ABI_ERR_GATE_NOT_SATISFIED`,
`ABI_ERR_RATE_LIMITED`.

**Status M0.W1 (manifest parser):** zaimplementowany. Dokumentacja sekcji
manifestu: `docs/ADDON_MANIFEST.md`. Testy: `tests/addon_manifest_parsing.rs`.

---

## 11. SQL API (F1a M1.W4 — zaimplementowane)

Per-addon SQLite. Kazdy addon dostaje wlasny plik bazy `~/.tentaflow/addons/<addon_id>/data.db`
z WAL mode, `foreign_keys=ON`, `synchronous=NORMAL`, `busy_timeout=5s`. Izolacja
przez FS sandbox (`addon/fs_sandbox.rs`) + walidacje `addon_id` regex
`^[a-z0-9][a-z0-9-]{0,63}$`. DDL (CREATE/ALTER/DROP/VACUUM/PRAGMA itp.) jest
zablokowane w runtime — schemat zmienia sie wylacznie przez migracje z bundle
addona.

### Wymagania manifestu

Addon musi zadeklarowac:

```toml
[storage]
sql = true
sql_backends = ["sqlite"]
sql_dialect = "sqlite"        # opcjonalnie, default "ansi"
migrations_dir = "migrations" # opcjonalnie, default "migrations"
encryption = "none"           # F1a: "at-rest" akceptowane ale nie wymuszone (F8: SQLCipher)

[[permission]]
id = "sql.read"
display_name = "Odczyt SQL"
risk = "medium"

[[permission]]
id = "sql.write"
display_name = "Zapis SQL"
risk = "medium"
```

Bez `[storage] sql = true` wszystkie host functions SQL zwracaja
`AbiError::Permission` (1). Bez uprawnien `sql.read`/`sql.write` analogicznie.

### Migracje

Pliki `*.sql` w `<bundle>/<migrations_dir>/` z nazewnictwem `NNN_lowercase_name.sql`
(>=3 cyfry numerujace + podkreslnik + nazwa). Aplikowane leksykograficznie przy
`install_addon`. Kazda migracja runuje w transakcji per-addon SQLite
(`execute_batch` w `BEGIN; ...; COMMIT;`); fail dowolnego statementu w pliku =
rollback calej migracji.

Idempotencja: tabela core DB `addon_migrations_applied` przechowuje
`(addon_id, migration_name, migration_hash)`. Re-install z tym samym hash =
skip. Hash mismatch (recznie zmodyfikowany plik po apply) = install fail
+ audit anomaly. Status `failed` umozliwia retry przy kolejnym install.

Przyklad `migrations/001_init.sql`:

```sql
CREATE TABLE alarms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id TEXT NOT NULL,
    ts INTEGER NOT NULL,
    severity TEXT NOT NULL CHECK(severity IN ('info','warning','critical')),
    note TEXT
);
CREATE INDEX idx_alarms_camera_ts ON alarms(camera_id, ts);
```

### DDL block w runtime

Zapytania zaczynajace sie (po whitespace) od `CREATE`, `ALTER`, `DROP`,
`TRUNCATE`, `REINDEX`, `VACUUM`, `ATTACH`, `DETACH`, `PRAGMA` sa odrzucane z
`AbiError::Permission`. Cel: addony nie moga uciec sandboxowi schematu
(np. dodajac kolumne pomijajaca aplikacja constraintu). Schemat zmienia sie
wylacznie przez migrations, ktore sa wersjonowane przez hash.

### SQL injection protection

Wszystkie parametry sa bindowane przez `rusqlite::params_from_iter` — nigdy
string concat. Wartosc `"'; DROP TABLE x;--"` jako parametr zostanie zapisana
literalnie do TEXT kolumny, bez interpretacji jako SQL.

### Limity i timeouts

| Wlasciwosc | Wartosc |
|------------|---------|
| Payload combined (query + params) | 4 MB (`PayloadKind::SqlCombined`) |
| Query timeout | 30 s (watchdog na `InterruptHandle::interrupt()`) |
| Connection pool per addon | 5 polaczen (`r2d2`) |
| Pool get timeout | 10 s |
| SQLite `busy_timeout` | 5000 ms |

### Encryption at-rest

Deklaracja `encryption = "at-rest"` w manifescie jest akceptowana w F1a, ale
NIE wymusza szyfrowania (SQLCipher integration planowane w F8). Runtime
loguje warning przy install. Addon dziala normalnie — plik `data.db` jest
plain SQLite.

### sql_exec_v1

DML (`INSERT`, `UPDATE`, `DELETE`). Uprawnienie: `sql.write`.

ABI:

```
sql_exec_v1(
    query_ptr: i32, query_len: i32,
    params_json_ptr: i32, params_json_len: i32,
    out_ptr: i32, out_cap: i32, out_len_ptr: i32,
) -> i32
```

Input `params_json` — JSON array (pusty `[]` = brak parametrow). Wartosci:

| JSON | SQLite |
|------|--------|
| `null` | `NULL` |
| `true`/`false` | `INTEGER 1/0` |
| Integer | `INTEGER` |
| Real (float) | `REAL` |
| String | `TEXT` |
| `{"$bytes": "<base64>"}` | `BLOB` |

Output JSON: `{"rows_affected": <u64>, "last_insert_id": <i64>}`.

Kody bledow: `0` OK, `1` Permission (brak `sql.write` / brak `[storage] sql=true` / DDL),
`4` Timeout (>30s), `5` Operation (params parse fail), `8` SqlSyntax,
`9` SqlConstraint (UNIQUE/FK/CHECK/NOT NULL), `21` PayloadTooLarge.

### sql_query_v1

`SELECT`, `WITH`, `EXPLAIN`. Uprawnienie: `sql.read`. Pisaca komenda
zwraca `AbiError::Permission` z komunikatem audit "use sql_exec for writes".

ABI identyczne jak `sql_exec_v1`. Output JSON:

```json
{
  "columns": ["id", "ts", "severity"],
  "rows": [
    [1, 1715515200, "warning"],
    [2, 1715515210, "info"]
  ]
}
```

Wartosci BLOB w wierszach zwracane jako `{"$bytes": "<base64>"}`.

### sql_query_one_v1

Jak `sql_query_v1`, ale zwraca pierwszy wiersz lub `null`. Output:
`{"row": [<v1>, <v2>, ...]}` lub `{"row": null}`. Gdy wynik > 1 wiersz =
audit warning, zwraca pierwszy.

### sql_transaction_v1

Atomic batch DML. Uprawnienie: `sql.write`.

ABI:

```
sql_transaction_v1(
    statements_json_ptr: i32, statements_json_len: i32,
    out_ptr: i32, out_cap: i32, out_len_ptr: i32,
) -> i32
```

Input:

```json
{
  "statements": [
    {"query": "INSERT INTO alarms (camera_id, ts, severity) VALUES (?, ?, ?)",
     "params": ["cam-1", 1715515200, "warning"]},
    {"query": "INSERT INTO alarms (camera_id, ts, severity) VALUES (?, ?, ?)",
     "params": ["cam-1", 1715515210, "info"]},
    {"query": "UPDATE cameras SET last_alarm_ts = ? WHERE id = ?",
     "params": [1715515210, "cam-1"]}
  ]
}
```

Wszystkie statementy w jednej transakcji. Fail dowolnego = rollback wszystkich.
Output: `{"rows_affected_total": <i64>}`. DDL w ktoremkolwiek statemencie
przed startem transakcji = `AbiError::Permission`.

### Przyklad uzycia (Rust SDK)

```rust
use tentaflow_addon_sdk::prelude::*;

fn save_alarm(camera_id: &str, ts: i64, severity: &str) -> Result<i64, i32> {
    let res = sql_exec(
        "INSERT INTO alarms (camera_id, ts, severity) VALUES (?, ?, ?)",
        &[
            SqlValue::String(camera_id.to_string()),
            SqlValue::I64(ts),
            SqlValue::String(severity.to_string()),
        ],
    )?;
    Ok(res.last_insert_id)
}

fn recent_alarms(camera_id: &str, since: i64) -> Result<Vec<(i64, i64)>, i32> {
    let rows = sql_query(
        "SELECT id, ts FROM alarms WHERE camera_id = ? AND ts > ? ORDER BY ts DESC LIMIT 100",
        &[
            SqlValue::String(camera_id.to_string()),
            SqlValue::I64(since),
        ],
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match (row.get(0), row.get(1)) {
            (Some(SqlValue::I64(id)), Some(SqlValue::I64(ts))) => Some((*id, *ts)),
            _ => None,
        })
        .collect())
}

fn batch_update(rows: &[(String, i64)]) -> Result<u64, i32> {
    let stmts: Vec<(&str, &[SqlValue])> = rows
        .iter()
        .map(|(id, ts)| {
            (
                "UPDATE cameras SET last_seen = ? WHERE id = ?",
                vec![SqlValue::I64(*ts), SqlValue::String(id.clone())].leak() as &[SqlValue],
            )
        })
        .collect();
    sql_transaction(&stmts)
}
```

### Audit

Kazde wywolanie SQL host function pisze do `audit_log` przez `audit_log_with_risk`:

- `action` = `sql.exec` / `sql.query` / `sql.query_one` / `sql.transaction`
- `resource_type` = `sql`
- `resource_id` = pierwsze 16 znakow hex SHA256(query) (dla compliance bez ujawniania pelnej tresci)
- `risk_class` = `A` (operacyjne)
- `result` = `ok` / `denied` / `error`

---

## 12. Aliases (readonly)

Readonly query metadanych aliasow AI w globalnej tabeli `model_aliases`.
Addon **nie tworzy ani nie deaktywuje** aliasow przez ABI w runtime —
zarzadzanie cyklem zycia aliasow nalezy do core (lifecycle hooks):

- **install** → `install_manifest_aliases` czyta `[[alias]]` z manifestu i
  zapisuje aliasy w `model_aliases` z `owner = addon:<addon_id>` plus rekordy
  `model_alias_visibility` i `model_alias_consumers` na podstawie pol
  `visibility` / `allowed_consumers`.
- **uninstall** → `deactivate_aliases_owned_by_addon` ustawia `is_active=0`
  na wszystkich aliasach z `owner_id = <addon_id>`. Wiersze pozostaja
  (admin moze je trwale usunac z poziomu M16).
- **upgrade** (F1b/F2) → diff manifestu starego vs nowego: nowe aliasy
  dodawane, znikajace deaktywowane.

Permission wymagana przez ponizsze host functions: `alias.read`
(uprzednio nazywane `alias.manage`).

### `alias_get_v1(alias_id_ptr, alias_id_len, out_ptr, out_cap, out_len_ptr) -> i32`

Zwraca metadane aliasu jako TOML. **Pola statystyczne**
(`last_used_target`, `last_used_at`, `calls_24h`, `fallback_calls_24h`) sa
stripowane gdy caller nie jest ownerem aliasu — chroni przed wyciekiem
wzorcow uzycia miedzy addonami.

| Parametr | Typ | Opis |
|----------|-----|------|
| `alias_id_ptr/len` | `i32` | Identyfikator aliasu (UTF-8) |
| `out_ptr/out_cap/out_len_ptr` | `i32` | Bufor wyjsciowy z retry semantyka (sekcja "out_cap retry pattern") |

**Output (TOML, AliasInfo):**

```toml
id = "teams-stt"
display_name = "Teams meeting STT"
owner = "addon:teams-bot"
visibility = "restricted"
current_target = "whisper-large-v3"
fallback_targets = ["whisper-medium", "vosk-pl"]
strategy = "first_available"
is_active = true
# nizsze pola tylko gdy caller == owner
last_used_target = "whisper-large-v3"
last_used_at = 1715515200
calls_24h = 412
fallback_calls_24h = 3
```

**Errors:**
- `ABI_OK` (0) — sukces
- `ABI_ERR_PERMISSION` (1) — brak `alias.read`
- `ABI_ERR_NOT_FOUND` (2) — alias nie istnieje
- `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` (6) — `out_cap` za maly (`*out_len_ptr` ma wymagany rozmiar)

### `alias_list_owned_v1(out_ptr, out_cap, out_len_ptr) -> i32`

Listuje wszystkie aliasy, ktorych ownerem jest wywolujacy addon. Wynik to
TOML array `AliasInfo` (z pelnymi statystykami, bo zawsze owner).

**Errors:**
- `ABI_OK` (0)
- `ABI_ERR_PERMISSION` (1) — brak `alias.read`
- `ABI_ERR_OUTPUT_BUFFER_TOO_SMALL` (6)

### SDK API (Rust)

```rust
use tentaflow_addon_sdk::prelude::*;

pub fn alias_get(id: &str) -> Result<AliasInfo, AbiError>;
pub fn alias_list_owned() -> Result<Vec<AliasInfo>, AbiError>;
```

### Przyklad — addon sprawdza w `on_tick` czy jego alias jest aktywny

Admin moze w panelu M16 deaktywowac alias mimo ze addon jest zainstalowany.
Addon powinien wykrywac taki stan i graceful-fallback:

```rust
fn on_tick(_ts: i64) -> i32 {
    match alias_get("teams-stt") {
        Ok(info) if info.is_active => {
            // alias dostepny — normalna sciezka
            run_stt_pipeline();
        }
        Ok(_) => {
            log_warn("alias 'teams-stt' jest nieaktywny — pomijam tick STT");
        }
        Err(AbiError::NotFound) => {
            // wariant defensywny: alias zostal usuniety przez admina (M16)
            log_error("alias 'teams-stt' nie istnieje juz w core");
        }
        Err(e) => return e.into(),
    }
    ABI_OK
}
```

## 13. Camera API (F1a M1.W6 — TentaVision)

Camera ingest layer dla addonow video (TentaVision). Wszystkie host functions
sa gated za cargo feature `camera`, ktore wciaga zalezosci GStreamer. Payload
input/output to TOML (nie JSON).

**Scope F1a:** wylacznie vendor `fake_file` (mp4 loop via GStreamer
`filesrc`). RTSP, ONVIF discovery i rotacja credentialow przyjda w F1b/F1c —
host functions sa zaimplementowane juz teraz jako noop zeby stabilizowac
ABI dla SDK.

**Uprawnienia (manifest `[[permission]].id`):**

| Permission         | Risk | Funkcje                                                                     |
|--------------------|------|-----------------------------------------------------------------------------|
| `cameras.read`     | B    | `camera_list_v1`, `camera_get_v1`, `camera_health_v1`                       |
| `cameras.write`    | A    | `camera_add_v1`, `camera_update_v1`, `camera_remove_v1`, `camera_discover_v1`, `camera_test_connection_v1`, `camera_credentials_rotate_v1` |
| `cameras.snapshot` | A    | `camera_snapshot_v1`                                                        |

**Ownership guard:** wszystkie operacje (read i write) sa zakreslone do
kamer nalezacych do wywolujacego addona (`owner_addon_id = caller.addon_id`).
Cudzy `camera_id` zwraca `NotFound` — nie `Permission` — zeby nie wyciekac
przez side-channel istnienia kamer innych addonow.

### Sygnatury ABI

```text
camera_add_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_list_v1(out_ptr, out_cap, out_len_ptr) -> i32
camera_get_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_update_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_remove_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_snapshot_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_health_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_discover_v1(out_ptr, out_cap, out_len_ptr) -> i32
camera_test_connection_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
camera_credentials_rotate_v1(input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32
```

### Schematy TOML — `camera_add`

Input:
```toml
display_name = "Front gate"
vendor = "fake_file"            # F1a: tylko 'fake_file'
url = "file:///abs/path/sample.mp4"
target_fps = 30                 # 1..=60, default 30
resolution_width = 1280         # opcjonalne
resolution_height = 720         # opcjonalne
retention_class = "C"           # A/B/C/Unclassified, default C
profile = "default"             # default 'default'
```

Output:
```toml
camera_id = "cam_<uuid>"
status = "starting"
```

Bledy: `CameraVendorUnsupported` (vendor poza whitelist), `Operation`
(target_fps poza zakresem, pusty display_name, niewlasciwy retention_class,
zly TOML), `Permission`, `Conflict` (kolizja na partial unique index —
zazwyczaj wewnetrzny rollback rozwiazuje), `CameraUnreachable`
(file_not_found / symlink na ścieżce).

### Schematy TOML — `camera_list` / `camera_get`

`camera_list` (bez wejscia) zwraca:
```toml
[[camera]]
camera_id = "cam_xyz"
display_name = "Front gate"
vendor = "fake_file"
url = "file:///abs/path"
target_fps = 30
resolution_width = 1280
resolution_height = 720
status = "online"
status_message = ""
fps_actual = 29.8
last_frame_at = 1715789000
retention_class = "C"
profile = "default"
```

`camera_get { camera_id = "cam_xyz" }` zwraca pojedynczy `[camera]` ze
struktury powyzej (bez wrappera tablicy).

Runtime metryki (`status`, `fps_actual`, `last_frame_at`, `status_message`)
pochodza z supervisora; gdy session nie zyje (np. po restarcie hosta),
fallback na wartosci z DB.

### Schematy TOML — `camera_update`

```toml
camera_id = "cam_xyz"
# Wszystkie ponizsze pola opcjonalne — pomin pole zeby zostawic bez zmian.
display_name = "Front gate v2"
target_fps = 25
resolution_width = 1920
resolution_height = 1080
retention_class = "B"
profile = "high_quality"
```

`vendor` i `url` NIE mogą byc updateowane — zmiana wymaga `camera_remove` +
`camera_add`. F1a: runtime config supervisora nie jest rebuiltowany —
nowy target_fps wchodzi po remove+add. Bledy jak w `camera_add`.

### Schematy TOML — `camera_remove`

Input:
```toml
camera_id = "cam_xyz"
```
Output:
```toml
removed = true
```
Soft-delete (stamps `removed_at`). Re-add tego samego `camera_id` jest
dozwolony (partial unique index na `camera_id WHERE removed_at IS NULL`).

### Schematy TOML — `camera_snapshot`

Input:
```toml
camera_id = "cam_xyz"
```
Output:
```toml
camera_id = "cam_xyz"
width = 1280
height = 720
pixel_format = "rgb24"
timestamp_unix_ms = 1715789000123
data_b64 = "<base64 RGB24 bytes>"
```

F1a: inline base64. Limit `PayloadKind::ServiceCall` = 8 MB. 1280x720 RGB24
(2.76 MB raw, ~3.7 MB base64) miesci sie; 1920x1080 (~8.3 MB base64)
przekroczy limit → `PayloadTooLarge`. F1c wprowadzi `SnapshotRef` z M1.W7
LRU frame storage.

### Schematy TOML — `camera_health`

Input:
```toml
camera_id = "cam_xyz"
```
Output:
```toml
camera_id = "cam_xyz"
status = "online"        # offline/starting/online/error/stopping
status_message = ""
fps_actual = 29.8
last_frame_at = 1715789000
frames_total = 12345
frames_dropped = 0
```

### Schematy TOML — `camera_discover`

Brak inputu. F1a output:
```toml
discovered = []
```
F1b doda RTSP + ONVIF probe.

### Schematy TOML — `camera_test_connection`

Input:
```toml
vendor = "fake_file"
url = "file:///path/file.mp4"
```
Output:
```toml
ok = true
message = "fake_file path readable"
```
Albo `ok = false` z `message` opisujacym przyczyne (symlink, brak pliku,
nieprawidlowy URL).

### Schematy TOML — `camera_credentials_rotate`

Input:
```toml
camera_id = "cam_xyz"
new_credentials_b64 = "..."      # opcjonalne
```
F1a output (noop dla `fake_file`):
```toml
rotated = false
reason = "f1a_noop_fake_file_has_no_credentials"
```

### Przyklad — addon dodaje kamere i co tick odswieża metryki

```rust
use tentaflow_addon_sdk::prelude::*;

fn on_start() -> Result<(), AbiError> {
    let spec = CameraAddSpec {
        display_name: "Front gate".into(),
        vendor: "fake_file".into(),
        url: "file:///opt/tentaflow/samples/gate.mp4".into(),
        target_fps: 25,
        retention_class: "C".into(),
        ..Default::default()
    };
    let added = camera_add(&spec)?;
    store_set("front_gate_id", &added.camera_id).ok();
    Ok(())
}

fn on_tick(_ts: i64) -> i32 {
    let id = match store_get("front_gate_id").ok().flatten() {
        Some(v) => v,
        None => return AbiError::Ok.as_i32(),
    };
    if let Ok(h) = camera_health(&id) {
        if h.frames_dropped > 0 {
            log_warn(&format!("front_gate dropped {} frames", h.frames_dropped));
        }
    }
    AbiError::Ok.as_i32()
}
```

