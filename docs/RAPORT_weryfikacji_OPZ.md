# Raport weryfikacji wymagań OPZ na działającej aplikacji

**Data:** 2026-07-03 · **Środowisko:** rig24 (spark-001) + rig25 (spark-002), instancja produkcyjna użytkownika, dane w `/opt/TentaFlow/.runtime/`
**Metoda:** Playwright (Chromium) — desktop 1440×900 i mobile (iPhone 13), zrzuty ekranu z realnej przeglądarki + testy `/v1` przez curl + inspekcja SQLite obu węzłów. Login: `admin/admin` (domyślne hasło działa).
**Zakres:** przeszedłem 36 widoków (desktop + mobile) i przetestowałem funkcjonalnie sekcje OPZ. Roboty (ROB) pominięte na prośbę użytkownika.

Uwaga: to instancja produkcyjna, nie czysta — nie czyściłem danych. Wszystkie artefakty testowe (3 aliasy `opz-*`, klucz `opz-test`, ręczne wpisy uprawnień) zostały posprzątane; klucz odwołany.

---

## Podsumowanie

Aplikacja jest w bardzo dobrym stanie — zdecydowana większość deklarowanych funkcji **realnie działa**, także mobilnie (żaden z 36 widoków nie miał poziomego przewijania). To nie jest fasada z mockupów. Znalazłem jednak **4 realne błędy funkcjonalne** (2 poważne), kilka defektów UI i drobne braki i18n.

| Priorytet | Liczba | Czego dotyczą |
|-----------|--------|---------------|
| 🔴 Krytyczne (blokują wymaganie OPZ) | 4 | fallback aliasu /v1, klucz na flow-as-model, Tłumacz GUI, generowanie RODO |
| 🟠 Poważne UI/UX | 4 | chat domyślny, Scheduler layout, przecieki modali, rola admina |
| 🟡 Drobne | 6 | i18n bez PL-znaków, health-check port, HSTS na 429, dashboard kafelek, sync jednego flow, nazwa addona |

---

## 🔴 Błędy krytyczne

### 1. Fallback aliasu nie działa przez `/v1` gdy węzeł-cel jest offline (RTG-02)
**To najpoważniejsze — podważa sztandarową funkcję „aliasy z automatycznym fallbackiem".**

`authorize_model` (`api/openai/server.rs:162-173`) przy autoryzacji aliasu iteruje `once(primary).chain(fallbacks)` i **odmawia całego aliasu (HTTP 404)**, jeśli KTÓRYKOLWIEK cel łańcucha nie jest aktualnie reklamowany w katalogu (bo węzeł-właściciel jest offline).

Dowód (testy na żywo):
- alias `primary=deepseek(offline) + fallback=qwen(live)` → **404** (nie skorzystał z żywego fallbacku)
- alias `primary=qwen(live) + fallback=deepseek(offline)` → **404** (offline fallback zablokował żywy primary)
- alias `primary=qwen + fallback=chat-flow` (oba live) → **200**, rozwiązany do qwen ✓

Czyli dla zewnętrznego API alias przestaje działać **dokładnie w scenariuszu, dla którego fallback istnieje** (padł węzeł z modelem). Komentarz w kodzie robi to świadomie („deny rather than silently skip"). Opis RTG-02 w OPZ („przy niepowodzeniu transportowym przechodzi automatycznie do kolejnej pozycji") nie jest spełniony na ścieżce `/v1`.
_Zastrzeżenie:_ fallback runtime dla żywego-ale-padającego celu (błąd transportu) oraz ścieżka dashboardu mogą działać inaczej — ale API OpenAI-compat gubi obietnicę.

### 2. Klucz API nadany na „flow-as-model" z GUI zawsze zwraca 404 (API-05)
Kreator kluczy zapisuje uprawnienie do flow jako `resource_id = UUID flowa`, ale autoryzator `/v1` sprawdza po **published_model_name** (np. `chat-flow`). Efekt: każdy klucz nadany na przepływ przez GUI → 404 przy wywołaniu.
Dowód: po ręcznym wstawieniu `flow|chat-flow` wywołanie flow-as-model działa (187 ms). Dodatkowo GUI kreatora pokazuje nazwę flowa („Chat Flow qwen"), a nie nazwę do użycia w API („chat-flow") — klient i tak nie wie, czego użyć.

### 3. Tłumacz nie działa z poziomu GUI (UI-03)
Backend jest OK (`translateRequest` zwraca poprawne tłumaczenie qwen), ale UI nigdy nie wysyła żądania. `tf-textarea._onInput()` emituje `CustomEvent('input',{detail})`, lecz **natywny** `input` z wewnętrznego `<textarea>` też bąbelkuje do hosta; handler robi `e.detail?.value ?? ''` → drugie (natywne) zdarzenie **zeruje** tekst źródłowy, licznik spada do 0/10000 i tłumaczenie jest anulowane. Wynik: pole „Tłumaczenie pojawi się tutaj" pozostaje puste na zawsze.
**Uwaga:** to dotyczy potencjalnie WSZYSTKICH konsumentów `tf-textarea`. Fix: `stopPropagation` natywnego `input` w komponencie albo fallback `e.detail?.value ?? e.target.value`.

### 4. Generowanie dokumentu RODO zawsze pada (ZGD-05)
`legalDocumentGenerateRequest` zawsze zwraca `path_traversal_blocked`. Przyczyna: `rodo_generator.rs:416` wymaga `org_id` w formacie UUIDv4, a domyślna organizacja to `org-default` (nie UUID). GUI nie pokazuje błędu (cichy fail — lista dalej „0 dokumentów"). Na domyślnej instalacji moduł generowania klauzul RODO jest niedostępny.

---

## 🟠 Poważne problemy UI/UX

### 5. Chat: domyślne przepływy nie odpowiadają
- Domyślny „Agent Run" → `routing error: conversation_history adapter: no session_id`
- „Default Chat" → `model '' not found in catalog` (węzeł LLM bez ustawionego modelu)
- Działa tylko „Chat Flow qwen". Nowy użytkownik po wejściu w Chat i wpisaniu wiadomości dostaje błąd zamiast odpowiedzi.
- Picker modeli to surowy `<select>` (łamie konwencję `tf-*` z CLAUDE.md) i pokazuje wewnętrzne przepływy RAG (`rag-…—retrieval-round`), które nie są modelami czatu.

### 6. Scheduler — rozjechany layout (WYRAŹNIE brzydki)
W panelu „Konfiguracja" opis funkcji addonu zawija się **pionowo, po jednym słowie w linii** (flex bez `min-width:0`). Formularz się rozsypuje. To jedno z miejsc, o których pisałeś „zlepione/nieczytelne".

### 7. Przecieki modali między widokami
Modal „Nowy klucz API" i dialog „Generuj RODO" **przetrwały nawigację** na inny ekran (wisiały nad Tłumaczem i ML Studio, blokując kliknięcia). `Router.navigate` nie sprząta otwartych okien.

### 8. Users: błędne metadane admina
Konto admin ma rolę wyświetlaną „**USER**" (a jest adminem), nagłówek „0 admin", ostatnie logowanie „**NaN mies. temu**".

---

## 🟡 Drobne

- **i18n:** część widoków bez polskich znaków — miks „Odswiez / Skrocony / pelna klauzula" (Audyt, RODO, opisy Eureki).
- **Health-check:** `config.toml` deklaruje `[monitoring] health_check_bind = 0.0.0.0:8888`, ale **nic nie nasłuchuje na 8888**. Endpoint z konfiguracji nie działa. `/v1/health` i `/health` wymagają klucza API (401) — probe monitoringu zwykle powinien być publiczny.
- **HSTS na ścieżce błędu:** odpowiedź 429 nie ma nagłówka HSTS (200 ma). OPZ/CLAUDE.md mówi „HSTS unconditional".
- **Dashboard:** kafelek „Węzły MESH" utknął na „Ładowanie…" (agregat VRAM=0).
- **Sync — jeden flow nie propaguje:** flow „Chat Flow qwen" (utworzony na rig24) nie pojawił się na rig25, mimo że `flows` są w rejestrze sync, a `api_keys` synchronizują się w ~4 s. Warte zbadania (utworzony przed sparowaniem? kolizja published-name?).
- **Addon Eureka** wyświetla się jako nazwa „**e**" (zgubiona nazwa wyświetlana).
- **Flow Builder z menu** (bez ID) → toast „Brak ID flow – wracam do listy" zamiast pustego płótna. Edytor otwiera się tylko przez „Edytuj" konkretnego przepływu.

---

## ✅ Co potwierdzone jako działające

**Bezpieczeństwo / API (rozdz. 18, 20):**
- TLS 1.3 wymuszony; TLS 1.2 odrzucany (`alert protocol version`). Szyfr AES-256-GCM ✓ (BEZ-01)
- `/v1`: chat direct (133 ms), streaming SSE, Anthropic `/v1/messages`, `/v1/models` per-klucz ✓
- ACL: model nienadany i nieistniejący → **404 bez ujawniania istnienia**, zły klucz → 401 ✓ (API-04)
- Rate-limit: pod obciążeniem **429 + `retry-after: 1`** ✓ (API-07); `last_used_at` klucza aktualizowany
- Kreator kluczy 3-krokowy (typ → zasoby → token), token pokazywany raz + HMAC w bazie ✓
- Filtr PII w przepływie realnie maskuje dane (`[IMIE_NAZWISKO]`) ✓ (ZGD-07)
- Macierz uprawnień default-DENY, klucze scoped/rotowalne ✓ (ADM)

**Sieć i synchronizacja (rozdz. 4, 5):**
- Parowanie rig24 ↔ rig25 dwukierunkowe, `trust_state=2`, cała sieć 12 węzłów ✓ (MSH)
- **Sync Ledger działa:** klucz API utworzony na rig24 pojawił się na rig25 w ~4 s ✓ (SYN)
- Klastry z load-balancingiem/failoverem, chipy transportu (Ethernet 100G), sondy ✓ (KLA)

**Modele / studia / przepływy:**
- Katalog usług, kreator wdrożenia (wykrywanie GPU, HF Hub, wybór kart) ✓ (MOD)
- Rejestr modeli sieciowy (12 węzłów, backend/rozmiar/lokalizacja) ✓
- Flow Builder (edytor DAG, autozapis, wersje), lista przepływów ✓ (FLW)
- **ML Studio:** kreator 4-krokowy + widok FT LLM z metodami **SFT/DPO/KD × QLoRA/LoRA/DoRA**, modele bazowe (Qwen/Llama/HF), szacowanie VRAM — bardzo dobre ✓ (TRN)
- **Benchmark Studio:** benchmark, historia runów, trend decode t/s ✓ (BEN)
- Analityka modeli: 1.2 M tok., TTFT p50 53 ms, decode p50 78 tok/s, 0.2% błędów ✓
- Agenci (rejestr + Przebiegi), Skills, Prompty, Reguły TTS/PII/Fast-path ✓ (AGT)
- Meeting Bot (Teams/Meet/Zoom, STT whisper-large-v3, diaryzacja pyannote, AI summary) ✓ (SPT)
- Audyt: 5.5 M wpisów, filtry, eksport CSV, severity, na żywo — łańcuch hash w kodzie ✓ (ADM/BEZ)
- Addony: 5 aktywnych (wasmtime/wasmi), per-addon uprawnienia, OAuth, install ZIP ✓ (ADD)
- Notatki (create/persist), Scheduler (37 funkcji, cron/interval/once) ✓

**Responsywność mobilna:** hamburger, tabele→karty (Serwisy, Przepływy), kalendarz, ML Studio, Benchmark — czytelne, **zero poziomego overflow** na 36 widokach. Znacznie lepiej niż zakładano.

---

## Rekomendacje (kolejność napraw)

1. **Fallback aliasu /v1** (#1) — zmienić `authorize_model`: nie odrzucać aliasu, gdy tylko część łańcucha jest offline; autoryzować po dostępnych celach.
2. **Klucz na flow-as-model** (#2) — kreator/handler ma zapisywać flow po `published_model_name`; GUI pokazywać tę nazwę.
3. **tf-textarea double-input** (#3) — jedna poprawka komponentu odblokowuje Tłumacza i chroni resztę konsumentów.
4. **Generowanie RODO** (#4) — obsłużyć `org-default` (mapować na deterministyczny UUID lub złagodzić walidację dla nazw org).
5. **Chat domyślny** (#5) — naprawić `session_id` w Agent Run i model w Default Chat, żeby świeży użytkownik dostawał odpowiedź.
6. Scheduler layout (#6), przecieki modali (#7), rola admina (#8), i18n, health-check.
