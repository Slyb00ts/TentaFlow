# Stany: empty / loading / error / offline

Zasada 8 w [`../README.md`](../README.md): „każda lista ma stan pusty, każda
operacja sieciowa stan ładowania i błędu”. Ten dokument definiuje jak te
stany wyglądają, kiedy dokładnie się pojawiają i jaki ton ma ich treść.

## Cztery obowiązkowe stany — przegląd

| Stan | Komponent/mechanizm | Kiedy |
|---|---|---|
| Empty | `<tf-empty-state icon title message>` + CTA w domyślnym slocie | Zapytanie się powiodło, zwróciło zero rekordów |
| Loading | `<tf-skeleton>` (layout znany) lub `<tf-spinner>` (layout nieznany) | Trwa pierwsze zapytanie / operacja bez danych do pokazania w tle |
| Error | Alert inline + przycisk retry | Zapytanie się nie powiodło |
| Offline/reconnecting | `connection-overlay.js` (globalny) lub `createConnectionOverlay()` (per-moduł) | Utracone połączenie WS z demonem (globalne) albo z konkretnym węzłem (lokalne) |

## Empty

```html
<tf-empty-state icon="cluster" title="Brak klastrów"
                 message="Dodaj pierwszy klaster, aby zacząć.">
  <tf-button variant="primary" icon="plus">Nowy klaster</tf-button>
</tf-empty-state>
```
`tf-empty-state.js`: ikona/tytuł/wiadomość z atrybutów, akcja(-e) z
domyślnego slotu (przechwycone i przeniesione do `.tf-empty-state-actions`
przy `_build()`; ukrywane automatycznie gdy pusty).

Rozróżniaj **dwa różne empty**, jak w `users.js`:

```js
const empty = users.length === 0
  ? I18n.t('users.no_users')  // naprawdę brak danych — pokaż CTA „Dodaj”
  : I18n.t('users.no_match'); // filtr/wyszukiwanie nic nie znalazło — pokaż „wyczyść filtr”
```
Pierwszy przypadek dostaje CTA tworzenia; drugi — wskazówkę zmiany
filtru/wyszukiwania (nigdy tego samego CTA co pierwszy, bo dodanie kolejnego
rekordu nie rozwiąże „nic nie pasuje do filtra”).

## Loading

- **Layout znany** (wiadomo z góry, ile wierszy/kart się pojawi, jaki mają
  kształt) → `<tf-skeleton variant="text|circle|rect" lines width height>`.
  Przykład z `cluster-detail.js` (`renderSkeleton()`): nagłówek jako
  `<span class="skeleton" style="width:240px;height:24px">`, dwie karty
  140 px wysokie — kształt skeletonu kopiuje kształt docelowej treści, nie
  jest generycznym paskiem.
- **Layout nieznany / operacja bez reprezentacji wizualnej** (submit
  formularza, akcja w toku) → `<tf-spinner size="sm|md|lg">`, zwykle obok
  tekstu stanu:
  ```html
  <div class="cluster-deploy-progress">
    <tf-spinner size="sm"></tf-spinner>
    <span>Wdrażanie w toku…</span>
  </div>
  ```
  (`cluster-detail.js`, `renderDeploySection`).
- **Pierwsze pełne ładowanie ekranu** (Router w trybie render/mount) ma
  wbudowany fallback w `router.js`: `content.innerHTML =
  '<div style="padding:48px;text-align:center;">Ładowanie…</div>'` —
  zastępowane wynikiem `render()` natychmiast po rozwiązaniu. Nie polegaj
  wyłącznie na tym dla własnych zapytań asynchronicznych wewnątrz `mount()`
  — ten fallback pokrywa tylko czas budowy samego `render()`.
- **Minimalny czas wyświetlenia (anti-flash)**: jeśli operacja może
  zakończyć się w < 300 ms, pokazanie i natychmiastowe schowanie
  spinnera/skeletonu miga i jest gorsze niż brak wskaźnika. **Nie znaleziono
  w repo współdzielonego helpera wymuszającego minimalny czas
  wyświetlenia** — to zalecenie ogólne (branżowe), nie potwierdzony wzorzec
  z kodu. Przy nowej implementacji: opóźnij pokazanie wskaźnika ładowania o
  ~150 ms (`setTimeout`) i, jeśli już pokazany, trzymaj go minimum 300 ms
  zanim zniknie — zamiast pokazywać/chować natychmiast przy każdej
  odpowiedzi.

## Error

Wzorzec z `router.js` (fallback dla `render()`/`show()` który rzucił) —
alert inline, nigdy pusty ekran:

```js
content.innerHTML = `<div style="padding:32px;">
  <h3 style="color:var(--danger);">Błąd ładowania widoku</h3>
  <pre style="color:var(--text-2);font-family:monospace;">${escapeHtml(e.message)}</pre>
</div>`;
```

Wzorzec na poziomie sekcji/karty (nie całego ekranu), `settings.js`:
```js
host.innerHTML = `<div class="empty-big" style="padding:24px;color:var(--danger);">
  ${escapeHtml(err.message || String(err))}
</div>`;
```
Reguła: kolor tekstu błędu = `semantic.critical` (`--danger`/`--tf-danger`,
patrz [`forms.md`](forms.md)); treść błędu (`err.message`) pokazana wprost —
serwer zwraca komunikaty już przeznaczone do wyświetlenia usera, nie surowe
stack trace'y.

**Retry**: audytowane moduły odzyskują się głównie przez **przycisk
odświeżenia sekcji**, obecny obok tytułu karty niezależnie od tego, czy
poprzednie ładowanie się powiodło (`settings.js`: `#general-refresh`,
`#sso-refresh`, `#registries-refresh` — zawsze widoczne, nie pojawiają się
tylko po błędzie). Jeśli dodajesz dedykowany błąd-z-retry, dołącz przycisk
„Spróbuj ponownie” wprost w bloku błędu, nie każ userowi szukać ogólnego
odświeżenia gdzie indziej na stronie.

**Toasty a błędy**: `toast(msg, 'error')` (`utils.js`) informuje o wyniku
POJEDYNCZEJ akcji (np. „Nie udało się zapisać”) i deduplikuje identyczne
komunikaty (`×N` licznik zamiast stosu identycznych toastów) — nie zastępuje
trwałego stanu błędu sekcji, gdy user ma coś poprawić i spróbować ponownie
(to idzie do `.form-error`, patrz [`forms.md`](forms.md)).

## Offline / reconnecting

Dwa poziomy, oba oparte o jeden mechanizm (`connection-overlay.js`):

1. **Platformowy** (`ConnectionOverlay.init()`, wołane raz w `app.js`
   `bootstrap()`) — przeglądarka straciła demona, **nic** na stronie nie
   może z niczym rozmawiać. Reaguje na cykl życia `ApiBinary`, przyciemnia
   całą aplikację.
2. **Modułowy** (`createConnectionOverlay(config)`) — ta sama karta
   (nagłówek z pulsującą kropką, ikona w pierścieniu, pierścień odliczania
   1 Hz, log ze znacznikiem czasu, stopka z akcjami), dla modułu zgłaszającego
   INNĄ utratę połączenia niż platformowa (Code Studio: węzeł-właściciel
   nieosiągalny mimo że platforma działa).
   - **`isPlatformDown()` rozstrzyga pierwszeństwo** — nakładka modułu nie
     może przykryć platformowej („demon zniknął” wygrywa z „jeden węzeł
     zniknął”).
   - Utrata połączenia to **degradacja, nie awaria** — węzeł nadal wykonuje
     swoją pracę — dlatego karta nosi ton `warning`, nie `critical`.
   - Karta zawsze odpowiada na trzy pytania w tej kolejności: CZYJE
     połączenie padło (to urządzenie czy węzeł), CO dzieje się z pracą, która
     akurat trwała, KIEDY nastąpi kolejna próba.

## Częściowe / nieaktualne dane (stale) i aktualizacje optymistyczne

- **Stale data**: gdy strumień danych przestaje napływać, ale ostatnia
  wartość wciąż jest wyświetlana, oznacz ją jawnie — wzorzec z `robots.js`
  (LiDAR): świeżość liczona jako `now - frame.timestampUs`; klatka starsza
  niż `LIDAR_STALE_AFTER_MS` (1500 ms — przy ~5 fps to około 7 brakujących
  klatek) przełącza odznakę na „nieaktualne”; sweep co
  `LIDAR_STALE_SWEEP_MS` (500 ms) re-renderuje odznakę **bez** dodatkowego
  ruchu sieciowego — to lokalny zegar, nie poll.
- **Reconnect z backoffem**: po zakończeniu strumienia z powodu
  `subscriber_lagged`, `robots.js` czeka `LIDAR_RESUBSCRIBE_DELAY_MS`
  (1000 ms) i próbuje **dokładnie raz** ponownie — świadomie bez pętli
  ponawiania, żeby UI nie migotało.
- **Aktualizacje optymistyczne**: nie znaleziono w audytowanych modułach
  wzorca „zmień UI natychmiast, cofnij jeśli serwer odrzuci” — operacje
  (np. `setShareEnabled` w `tentanas/shares.js`) czekają na odpowiedź
  serwera i dopiero wtedy odświeżają widok (`onDone`/`refresh()`). Wyjątek
  częściowy: `users.js`, segment uprawnień (`tf-segmented[data-resource-type]`)
  **cofa** kontrolkę do ostatniej potwierdzonej wartości przy błędzie zapisu
  (`seg.value = prev`), co jest de facto odwrotnością optymistycznej
  aktualizacji — kontrolka reaguje natychmiast na klik (optymistycznie), ale
  jawnie się wycofuje przy porażce, z osobnym toastem błędu. Traktuj to jako
  jedyny potwierdzony wzorzec optymistyczny w repo; kopiuj go (zmiana +
  rollback + toast), nie wymyślaj nowego.

## Wskazówki tekstowe (copy)

- Przyjazny, zorientowany na akcję ton: „Dodaj pierwszy klaster, aby
  zacząć” (co robić), nie „Brak danych” (fakt bez wyjścia).
- Treść błędu = to, co naprawdę poszło nie tak (`err.message` z serwera),
  nie generyczne „Wystąpił błąd” gdy serwer dał konkretniejszy komunikat.
- Wszystkie teksty stanów przez i18n (`I18n.t('…')`), we wszystkich pięciu
  językach — pusty/loading/error to najczęściej odwiedzane stany strony,
  brakujący klucz tu jest widoczny natychmiast.

## Tabela decyzyjna

| Sytuacja | Stan | Komponent |
|---|---|---|
| Pierwsze wejście na listę, zapytanie w locie | Loading | `tf-skeleton` (kształt znany) |
| Zapytanie zwróciło `[]`, brak filtrów aktywnych | Empty (z CTA tworzenia) | `tf-empty-state` |
| Zapytanie zwróciło `[]`, filtr/szukajka aktywne | Empty (z podpowiedzią zmiany filtra) | `tf-empty-state` (inny tekst, bez CTA tworzenia) |
| Zapytanie rzuciło błąd sieci/serwera | Error | alert inline + retry |
| WS do demona padł | Offline (globalny) | `connection-overlay.js` (`init()`) |
| WS do demona żyje, konkretny węzeł/zasób nieosiągalny | Offline (modułowy) | `createConnectionOverlay()`, `isPlatformDown()` sprawdzone najpierw |
| Dane są, ale strumień ucichł | Stale | odznaka „nieaktualne” liczona z lokalnego zegara, nie poll |
| Trwa akcja bez reprezentacji wizualnej (zapis, wdrożenie) | Loading (spinner) | `tf-spinner` + tekst statusu |

## Checklist

- [ ] Każda lista ma `tf-empty-state` z CTA — i osobny tekst dla „naprawdę
      pusto” vs „nic nie pasuje do filtra”.
- [ ] Każde zapytanie sieciowe ma stan loading dobrany do tego, czy kształt
      docelowej treści jest znany (`tf-skeleton`) czy nie (`tf-spinner`).
- [ ] Błąd renderuje się inline z treścią `err.message`, nigdy jako pusty
      ekran; jest przycisk odświeżenia/retry w zasięgu wzroku.
- [ ] Moduł z własnym połączeniem (poza platformowym WS) używa
      `createConnectionOverlay()` i sprawdza `isPlatformDown()` przed
      pokazaniem własnej nakładki.
- [ ] Dane strumieniowe, które mogą ucichnąć, mają odznakę „nieaktualne”
      liczoną z lokalnego zegara (nie z kolejnego pollu).
- [ ] Wszystkie teksty stanów przez `I18n.t()`, sprawdzone w 5 językach.
