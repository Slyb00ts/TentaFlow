# Host Functions — Dokumentacja ABI addonow WASM

Addony TentaFlow dzialaja jako moduly WASM (WebAssembly) uruchamiane w sandboxie.
Komunikacja z hostem (Core) odbywa sie wylacznie przez zdefiniowane host functions.
Kazda funkcja operuje na pamieci liniowej guest (WASM) przez wskazniki i dlugosci.

---

## Konwencje ABI

- Parametry tekstowe: `(ptr: i32, len: i32)` — wskaznik i dlugosc UTF-8 w pamieci guest
- Bufor wyjsciowy: `(out_ptr: i32, out_capacity: i32)` — wskaznik i pojemnosc bufora
- Zwracana wartosc `i32`: kod statusu (0 = sukces, <0 = blad)
- Zwracana wartosc `i64` (packed): `(status << 32) | data_length` — status w gornych 32 bitach, dlugosc danych w dolnych

---

## Globalne kody bledow ABI

| Kod | Stala | Opis |
|-----|-------|------|
| 0 | `ABI_OK` | Operacja zakonczona sukcesem |
| -1 | `ABI_ERR_PERMISSION` | Brak wymaganych uprawnien |
| -2 | `ABI_ERR_OPERATION` | Blad operacji (ogolny) |
| -3 | `ABI_ERR_NOT_FOUND` | Zasob nie znaleziony |
| -4 | `ABI_ERR_TIMEOUT` | Przekroczono limit czasu |

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
