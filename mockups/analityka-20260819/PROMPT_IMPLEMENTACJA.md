# Prompt: implementacja ekranu Analityka wg mockupów

Zaimplementuj w TentaFlow nowy, zunifikowany ekran **Analityka** dokładnie według
`mockups/analityka-20260819/SPEC.md` (decyzje D1–D10 są wiążące) i mockupów
`mockups/analityka-20260819/m01–m06.html`. Zastępuje on OBA istniejące ekrany:
„Analityka modeli" (`model-metrics.js`) i „Zużycie tokenów" (`token-usage.js`).

Fan out sub-agents i ultracode. Podziel pracę na niezależne strumienie i daj każdy
osobnemu sub-agentowi:

1. **Core/Rust** — `display_name` w wire rows (append-only, `#[serde(default)]`),
   join nazw user/grupa/node/model w dispatch, liveness nodów z `sync_nodes.last_seen_at`.
2. **TfCartesianChart** — animacje wejścia (transform/opacity, prefers-reduced-motion),
   crosshair + tooltip domyślnie, stacked bary z zaokrąglonym szczytem, viewBox z realnej
   szerokości kontenera; adaptacyjne osie przez `fmtCompact`.
3. **Moduł analytics.js + analytics.css** — 6 zakładek, lepkie filtry z auto-przeładowaniem,
   drill-down z breadcrumbem, KPI z count-upem, wyłącznie komponenty `tf-*` i tokeny `--tf-*`.
4. **fmtCompact w utils.js** — 842 / 12,4 tys / 121 mln / 3,2 mld, pełna wartość w `title`;
   Rozliczenia zawsze dokładnie.
5. **i18n** — namespace `analytics.*` w KOMPLECIE 5 locale, formy mnogie `{count|...}`,
   kasacja `token_usage.*`/`model_metrics.*`.
6. **Kasacja** — `token-usage.js`, `model-metrics.js`, `model-metrics.css`, stare wpisy nav/Router;
   zero martwego kodu po sobie.
7. **Bramka /v1 (SPEC §5, G1–G2)** — passthrough `response_format.json_schema` + flatten
   nieznanych pól w `ChatCompletionRequest`, flaga `supports_structured_output`; cert TLS
   per instalacja (rcgen, SAN-y: localhost + hostname + lokalne IP + `extra_sans` z configu).
   Weryfikacja krytyka tu nie-wizualna: test integracyjny wysyła `json_schema` przez bramkę
   do vLLM i dostaje odpowiedź zgodną ze schematem; `openssl x509` na wygenerowanym cercie
   pokazuje IP SAN-y.

/loop na każdym elemencie UI osobno: po każdej iteracji uruchom dashboard, zrób screeny
Playwrightem w 1680px (desktop) ORAZ 390px (telefon, deviceScaleFactor 2, fullPage) dla
każdej zakładki i drill-downu, i przekaż je osobnemu sub-agentowi **krytykowi wizualnemu**.
Krytyk ma być naprawdę bezlitosny: porównuje screen implementacji ślepo, obok siebie,
z odpowiadającym screenem mockupu (`m01`–`m06`, wersje desktop i mobile) i mówi, który
wygląda lepiej i CO dokładnie odstaje — marginesy, typografia, animacje, jednostki liczb,
zawijanie na mobile, sklejone stopki, obcięte kolumny, UUID zamiast nazwy, cokolwiek.
Jeśli implementacja nie jest nieodróżnialna od mockupu albo lepsza — iteruj dalej.
Nie przechodź do następnego elementu, dopóki krytyk nie powie wprost, że w ślepym
porównaniu wybrałby implementację.

Bramki twarde (krytyk odrzuca bez dyskusji, gdy złamane):
- jakikolwiek widoczny UUID/64-hex bez ludzkiej nazwy jako tytułu,
- liczba ≥ 10 000 bez adaptacyjnej jednostki (poza Rozliczeniami),
- poziomy scroll CAŁEJ strony na 390px (scroll wolno mieć tylko wewnątrz karty),
- awatar-inicjały przy userze lub grupie,
- status noda niezgodny z `last_seen_at`,
- surowy `<button>/<input>/<select>` zamiast komponentu `tf-*`,
- brak któregoś klucza `analytics.*` w którymkolwiek z 5 locale,
- animacja na property innym niż transform/opacity albo ignorująca prefers-reduced-motion.

Na końcu osobny sub-agent robi przejście E2E (Playwright): wszystkie zakładki, drill-down
i powrót breadcrumbem, zmiana każdego filtra (auto-reload bez klikania „Odśwież"),
edycja limitu, zapis cennika, eksport CSV — na desktopie i na 390px. `cargo check` i
testy muszą przechodzić, `cargo check` nie może zostawić żadnego unused-warninga po kasacji
starych modułów. /loop until it's utterly perfect.
