# Polityki SLA i limity usług — poradnik administratora

Stan implementacji: 2026-11-18 (TB-03, commit 277a279d8).

Ten dokument opisuje, jak działają polityki SLA przypisywane do przepływów (flow) oraz limity równoległości serwisów AI. Opisane zachowanie pochodzi z kodu; tam, gdzie coś jest wnioskiem z logiki, a nie z uruchomienia, jest to zaznaczone.

## Czym jest polityka SLA

Polityka SLA to nazwany zestaw limitów i progów należący do organizacji. **Polityki przypisuje się wyłącznie do przepływów.** Serwisy AI nie dostają polityki SLA; mają własny limit wątków w sekcji „Limity” edycji serwisu (opis niżej). Przepływ ma najwyżej jedną politykę, a ponowne przypisanie zastępuje poprzednią. Przepływów systemowych (`is_system`) nie obejmuje się polityką.

Polityki zarządza się w menu „Flow Builder”, zakładka „Polityki SLA” (obok zakładki „Przepływy”). Lista polityk ma kolumny „Nazwa”, „Wątki (na węźle)” (np. „2 / kolejka 2, czekanie do 10 s, potem 429”), „Limit / okno (na węźle)” (np. „60 / minuta, ostrzeżenie przy 80 %”), „Progi” oraz „Usługi” (liczba przypisanych przepływów, np. „1 przepływ”). Przypisanie robi się ikoną tarczy „Przypisz politykę SLA” w wierszu przepływu; zdjęcie polityki to wybór „—” w tym samym oknie (przycisk nadal nazywa się „Przypisz”). Kolumna „Polityka SLA” na liście przepływów pokazuje stan: OK, ostrzeżenie, naruszenie lub brak odpowiedzi. **Lista przepływów nie ma stanu „wstrzymany limitem”**: wstrzymanie odbioru z TentaBus otwiera naruszenie „Limit wywołań osiągnięty”, więc wiersz pokazuje „naruszenie” z „limit wywołań N/M”. Stan „wstrzymany limitem” istnieje tylko w karcie „Stan SLA” (patrz niżej).

Przepływ udostępnia się jako model w API `/v1` w Flow Builderze: przycisk „Ustawienia” w pasku u góry (obok „Zmienne”) otwiera okno „Ustawienia przepływu” z przełącznikiem „Udostępnij jako model w API /v1”, polem „Nazwa modelu” i polem tylko do odczytu „Adres wywołania”. Pusta nazwa albo wyłączony przełącznik odpublikowuje flow; nazwa już zajęta daje błąd przy polu. Opublikowany flow ma na liście chip „/v1 · <model>”. Katalog `/v1` obejmuje tylko flow o statusie Aktywny.

Nazwa polityki jest unikalna w obrębie organizacji (`UNIQUE(org_id, name)`, bez `COLLATE NOCASE`, więc wielkość liter ma znaczenie: „Api” i „API” to dwie różne polityki).

## Uprawnienia

- **Odczyt** (lista polityk, stan, naruszenia) wymaga kontekstu organizacji. Bez uprawnienia do zmian widać zakładkę w trybie „Tylko podgląd”.
- **Tworzenie, zmiana i usuwanie polityki** wymagają uprawnienia `sla.manage` („Zarządzanie politykami SLA”). Domyślnie ma je tylko rola `org_admin` (migracja 213).
- **Przypisanie i zdjęcie polityki** wymagają `sla.manage` ORAZ prawa do edycji przepływów (konto `power_user` albo `admin`). Bez tego przycisk jest nieaktywny z podpowiedzią „Przypisanie wymaga prawa do edycji przepływów”.
- **Izolacja organizacji.** Polityki są per organizacja; polityka innej organizacji zwraca `not_found`. W odczytach tylko administrator platformy (super-admin) może wskazać `org_id` innej organizacji. `sla.manage` nadane w jednej organizacji nie daje prawa zarządzania w innej.
- **„Inna organizacja”.** Przepływy nie należą do organizacji, więc każdy, kto może je edytować, może je przypisać. Jeśli przepływ ma już przypisanie nadane przez inną organizację, Twoja organizacja zobaczy w kolumnie komórkę „Inna organizacja” (z jej stanem), a próba przypisania lub zdjęcia polityki zwróci błąd `governed_elsewhere` (podpowiedź „Politykę nadała inna organizacja”).

Błędy zapisu: `invalid_value` (z nazwą pola), `name_taken`, `policy_in_use` (z liczbą przypisań; usuwanie jest odrzucane, dopóki polityka jest przypisana), `system_flow`, `governed_elsewhere`, `not_found`.

## Pola polityki i zakresy

| Pole (etykieta w UI) | Zakres | Znaczenie |
| --- | --- | --- |
| Nazwa | 1–200 znaków po przycięciu | unikalna w organizacji |
| Opis | do 2000 znaków | opcjonalny |
| Maks. liczba wątków na węźle (`max_concurrency`) | 1–1024 | ile wykonań naraz; puste = bez limitu |
| Kolejka ponad limit wątków (`max_queue`) | 0–10000 | puste = tyle, ile `max_concurrency` (bez limitu wątków: 0) |
| Maks. czas w kolejce (ms) (`queue_timeout_ms`) | 1–300000 | puste = 10 000 ms |
| Limit wywołań na węźle (`call_limit`) | co najmniej 1 | ustawiany razem z oknem |
| Okno limitu (`call_window_secs`) | 1, 60, 3600, 86400 s (sekunda, minuta, godzina, doba) | ustawiane razem z limitem albo oba puste |
| Ostrzeżenie przy wykorzystaniu limitu (%) (`warn_at_pct`) | 1–99 | próg ostrzeżenia o limicie wywołań |
| Timeout wykonania (ms) (`timeout_ms`) | 100–86 400 000 | twardy limit czasu jednego wykonania |
| Próg p95 czasu odpowiedzi (ms) (`latency_p95_ms`) | co najmniej 1 | próg naruszenia „Czas odpowiedzi p95” |
| Próg odsetka błędów (%) (`error_rate_pct`) | 1–100 | próg naruszenia „Odsetek błędów” |
| Kolejność przy równoległym odbiorze z TentaBus (`ordering`) | `partition` (domyślnie, „per partycja”) lub `none` („bez kolejności”) | dotyczy tylko flow z węzłem „Odbiór z magistrali” |

Okno limitu jest stałe i wyrównane do zegara (np. 10:15:00–10:15:59 dla minuty), a nie przesuwne.

## Limity są liczone na każdym węźle osobno

Liczniki i kolejka bramki żyją w pamięci procesu każdego węzła. Dla ruchu `/v1` efektywny limit klastra to limit × liczba węzłów obsługujących ruch. **Restart węzła zeruje okna i histogramy.** Zmiana polityki działa od razu bez restartu; przy zmianie długości okna licznik okna startuje od nowa, a wykonania będące w toku nie są przerywane.

Odbiór z TentaBus działa tylko na liderze partycji, więc limity przepływów z odbiorem z magistrali obejmują de facto cały klaster (stan pokazuje „limit dla całego klastra”).

Podprzepływy i aliasy (głębokość wywołania większa od zera) nie zajmują osobnego slotu; liczy się przepływ wejściowy. Przepływ bez polityki nie jest limitowany. Jeśli odczyt polityk się nie powiedzie, bramka używa ostatniej znanej konfiguracji (przy zimnym starcie: bez limitu).

## Co dostaje wywołujący

Gdy wszystkie wątki są zajęte, żądanie czeka w kolejce (do `max_queue` miejsc, nie dłużej niż `queue_timeout_ms`), a potem jest odrzucane. Odmowy z powodu wątków mają `Retry-After` 1 s. Odmowa z powodu limitu wywołań przychodzi od razu, a `Retry-After` to czas do początku następnego okna (co najmniej 1 s). Odrzucone z powodu wątków wywołania są zwracane do licznika okna, więc okno liczy tylko wywołania przyjęte.

Powody odmowy: `calls_limit`, `concurrency_queue_full`, `concurrency_queue_timeout`.

- **`/v1` (OpenAI i Anthropic):** odmowa SLA to `429` (typ `rate_limit_exceeded`) z `Retry-After` (zaokrąglone w górę do sekund, minimum 1). Zajęty serwis AI to `503` (`service_unavailable`) z `Retry-After`, także dla wbudowanego OCR i rerank (przy limicie wątków 1 pojedyncze żądanie przechodzi; wcześniejsza wersja odmawiała 500 po upływie timeoutu). Przekroczenie `timeout_ms` polityki to `504` (`timeout_error`) bez `Retry-After`.
- **WebSocket:** komunikaty tekstowe z kodami `sla.overloaded: <powód>, retry after N ms`, `sla.timeout: flow ... exceeded its N ms timeout` oraz `backend.busy: retry after N ms`.
- **BPMN:** zadanie nie kończy się błędem, tylko jest odraczane kodem `FLOW_OVERLOADED` na `retry_after_ms`. Incydent powstaje dopiero po 10 minutach nieudanych prób (`DEFER_DEADLINE_MS`). Zadania BPMN nie czekają w kolejce bramki.

**Uwaga o 504 i ponowieniach.** SDK OpenAI domyślnie ponawia odpowiedzi 504 dwukrotnie. Kosztowny przepływ z krótkim `timeout_ms` może więc wykonać się nawet 3 razy dla jednego wywołania klienta. Przy przekroczeniu `timeout_ms` wykonanie jest anulowane (z 2-sekundowym okresem łaski `CANCEL_GRACE`), a jego wiersz w historii zamykany jako `error`. Przy odpowiedziach strumieniowych slot jest trzymany do końca strumienia.

## TentaBus: limit tylko zwalnia odbiór

Limit nigdy nie odrzuca wiadomości z magistrali: nie ma DLQ ani utraty. Gdy bramka odmawia, odbiorca śpi przez `retry_after_ms` (co najwyżej 30 s), a wiadomości czekają w topiku. Widać to jako rosnący lag oraz alert „Wstrzymano limitem do …” (`throttled_until`, pokazywany tylko, gdy czas jest w przyszłości).

Inaczej jest z przekroczeniem `timeout_ms` polityki i błędem przepływu: te przechodzą przez ustawienie węzła „Przy błędzie” (DLQ, pomiń, zatrzymaj subskrypcję).

Zachowanie odbiorcy:

- Bez polityki albo z `max_concurrency` puste lub równym 1 odbiór działa szeregowo, jak dawniej.
- Przy `max_concurrency` N > 1 w jednym cyklu pobieranych jest do N paczek (po `batch_size` rekordów z partycji). `ordering = partition` wykonuje paczki jednej partycji po kolei, a różnych partycji równolegle. `ordering = none` wykonuje wszystkie paczki równolegle i nie daje żadnych gwarancji kolejności.
- **Zatwierdzanie ciągłego prefiksu.** Per partycja zatwierdzany jest tylko ciągły początek obsłużonych paczek. Obsłużona znaczy: przepływ zakończony sukcesem, `on_error = skip` albo wszystkie rekordy trafiły do DLQ. Paczka, która jeszcze trwa lub zawiodła, blokuje zatwierdzenie późniejszych, więc po awarii nic się nie gubi (możliwe jest ponowne przetworzenie).
- W trybie „Najwyżej raz” (`at_most_once`) cykl najpierw rezerwuje sloty (pierwszy staje w kolejce, kolejne tylko jeśli są wolne), pobiera nie więcej rekordów niż slotów i oddaje niewykorzystane. Zarezerwowane sloty są trzymane przez long-poll, ograniczony do mniejszej z wartości: „Maks. czas oczekiwania” i połowa `queue_timeout_ms`.
- Klucz rekordu decyduje o partycji; rekordy bez klucza są rozdzielane po partycjach rotacyjnie.

**Format wiadomości.** Odbiorca flow (`bus_consume`) odrzuca do DLQ każdy rekord, którego payload nie jest poprawnym JSON-em (`dlq.reason=schema_violation`), niezależnie od polityki i od rodzaju treści topiku. To nie jest skutek limitu, ale łatwo to pomylić z awarią. Dotyczy to też topików binarnych.

Strona odbiorcy w TentaBus (TentaBus → Odbiorcy → szczegóły) pokazuje kafelek „Równoległość” (w toku/maks.), alert „Wstrzymano limitem do …” oraz kartę „Polityka SLA” z odnośnikiem „Zobacz Polityki SLA”; sekcja „Miejsce czytania” pokazuje offsety zatwierdzone per partycja. Prawa konta usługowego do topiku nadaje się w szczególe topiku (ACL topiku), nie w kreatorze klucza ogólnego.

Karta „Stan SLA” przepływu z TentaBus (Flow Builder → ikona zegara „Historia wykonań” w wierszu) w wierszu węzła prowadzącego odbiór pokazuje „prowadzi odbiór” i „wstrzymano do …”. Chip węzła „wstrzymany limitem” jest widoczny, gdy wstrzymanie trwa, a wszystkie otwarte naruszenia dotyczą limitów; stopka mówi „odbiór ruszy sam o …”. Podpis odmów to „w ostatnich 5 min · dziś N”, a nagłówek panelu zawiera nazwę przepływu.

## Naruszenia i ocena

Co 30 s każdy węzeł ocenia swoje bramki na podstawie ostatnich 5 minut. Rodzaje naruszeń:

| Rodzaj (etykieta) | Kiedy |
| --- | --- |
| Czas odpowiedzi p95 (`latency`) | szacowane p95 przekracza próg |
| Odsetek błędów (`errors`) | odsetek błędów jest co najmniej równy progowi |
| Przekroczenia czasu wykonania (`timeouts`) | co najmniej jedno przekroczenie `timeout_ms` w oknie (próg 0) |
| Wykorzystanie limitu wywołań (`calls_warn`) | szczyt wywołań rozpoczętych w oknie limitu osiąga `warn_at_pct` limitu |
| Limit wywołań osiągnięty (`calls_limit`) | szczyt osiąga limit; liczone z wywołań rozpoczętych, nie z odmów, więc ten sam rodzaj pojawi się też dla przepływu z TentaBus |
| Wszystkie wątki zajęte (`concurrency_saturated`) | zajęte wszystkie wątki na każdym ticku przez co najmniej 60 s |

Naruszenie jest otwierane, aktualizowane (szczyt) i zamykane. Zamyka się samo przy pierwszej ocenie, w której warunek już nie zachodzi (dla rodzajów opartych na oknie to około 5 czystych minut), albo gdy polityka lub przypisanie zniknie. Jeśli nie da się odczytać polityk, nic nie jest zamykane. Stan przepływu: ok, ostrzeżenie (otwarte tylko `calls_warn`), naruszenie (cokolwiek innego).

Ocena z 5 minut porównuje dawne szczyty z bieżącym limitem, więc zmniejszenie limitu może od razu otworzyć naruszenie (w teście po zmianie limitu 60 na 10 otwarło się „limit wywołań” ze szczytem 40/10). Wiersze historii pokazują próg zapisany przy otwarciu każdego naruszenia, więc dawne wpisy nie są przeliczane na nowy limit. W karcie „Stan SLA” podpis „w ostatnich 5 min” przy odmowach dotyczy okna oceny, a nie bieżącej minuty okna limitu.

Retencja: próbki minutowe 7 dni, zamknięte naruszenia 90 dni. Historia jest lokalna dla węzła i pokazywana per węzeł. Lista naruszeń: domyślnie 50 pozycji, maksymalnie 200, od najnowszych.

**„Dziś”.** Licznik odmów „dziś” liczy od lokalnej północy węzła (zapisane próbki minutowe plus jeszcze niezapisana część), z możliwym rozjazdem około 30 s przy zmianie doby. Liczy się strefa czasowa procesu; kontenery zwykle chodzą w UTC, więc „doba” kończy się wtedy o północy UTC.

## Dziennik audytu

Zmiany polityk: `sla.policy.create`, `sla.policy.update`, `sla.policy.delete`, `sla.policy.assign`, `sla.policy.unassign` (w szczegółach JSON przed i po).

Zdarzenia okienkowe (zasób `flow`, bez użytkownika; ważność warning, a dla zamknięcia info): `sla.calls.rejected`, `sla.concurrency.rejected`, `sla.breach.raised`, `sla.breach.resolved`. Pierwsze wystąpienie danej akcji dla danego przepływu w oknie 60 s jest zapisywane od razu, kolejne są składane w jeden wiersz z `count=N`.

Zmiana limitów serwisu zapisuje `service.update` (szczegóły niżej).

## Zabbix

Endpoint `GET /v1/metrics/zabbix` z nagłówkiem `Authorization: Bearer <monitoring_zabbix_token>` działa tylko przy włączonym `monitoring_zabbix_enabled`. Szablon: `GET /v1/metrics/zabbix/template` (bez uwierzytelnienia). Metryki SLA (bez etykiet, per węzeł):

- `tentaflow_sla_breaches_open` — otwarte naruszenia na węźle (wszystkie organizacje),
- `tentaflow_sla_rejected_total` — odmowy (limit wywołań plus wątki) od startu procesu,
- `tentaflow_sla_timeouts_total` — przekroczenia czasu od startu,
- `tentaflow_sla_inflight` — wykonania w toku.

Sumy są licznikami w pamięci i szablon przechowuje wartości surowe (bez kroku delta). Mogą spaść, gdy bramka zniknie (np. po odpięciu polityki) albo po restarcie.

## Limity serwisów AI („Limity”)

W edycji serwisu sekcja „Limity” ma dwa pola:

- **Maks. liczba wątków (równoległych wykonań)**: puste lub 0 = bez limitu, zakres 1–1024.
- **Timeout (ms)**: 100–86 400 000 (domyślnie 30 000). To także czas oczekiwania na wolny slot.

Limit dotyczy jednego węzła i jednego serwisu (węzła, na którym serwis działa) i obejmuje HTTP, QUIC, silniki wbudowane oraz `/v1/infer`, `/v1/rerank`, `/v1/ranking` i generowanie obrazów. Kolejne żądania czekają na slot najwyżej przez timeout, a kolejka ma długość równą limitowi. Potem dostają `503` z `Retry-After` (czas oczekiwania zaokrąglony w górę do sekund, minimum 1). W łańcuchu zapasowych serwisów kandydaci inni niż ostatni są próbowani bez czekania. Odmowa nie jest liczona jako błąd przez wyłącznik obwodu.

Zmiana działa od razu, a zapis zawierający wyłącznie limity nie restartuje serwisu (audyt pokazuje `restarted=false`). Restart jest potrzebny tylko przy zmianie pozostałych pól. Okno edycji ma jednak domyślnie zaznaczone „Restart po zapisie” i przycisk „Zapisz i restartuj”, także gdy zmieniono tylko limity. Jeśli węzeł hostujący jest starszy i nie obsługuje zdalnej zmiany limitów, zapis zostanie odrzucony komunikatem, że węzeł jest za stary i trzeba go zaktualizować. Zapis lokalny zostawia wpis audytu `service.update` (`service_id`, `restarted`, `max_concurrent`, `timeout_ms`). Zmiana przekazana do innego węzła kończy się tam przed zapisem audytu.

## Znane ograniczenia

- Okno wywołań jest per węzeł i w pamięci; restart je zeruje.
- Długie wywołanie z TentaBus jest zaliczane do okna, w którym się rozpoczęło.
- Nasycenie wątków jest próbkowane na tickach co 30 s, więc krótkie piki mogą być niewidoczne.
- „Dziś” ma rozjazd około 30 s przy północy i zależy od strefy procesu.
- Sumy Zabbix mogą się cofać.
- Na liście przepływów brak stanu „wstrzymany limitem”; wstrzymanie widać tam jako naruszenie limitu wywołań.
- Filtr „Akcja” w Dzienniku audytu wymaga pełnej nazwy akcji (np. `sla.policy.update`); wyszukiwanie po prefiksie `sla.` nic nie zwraca.
- Przy `ordering = none` stopka karty „Stan SLA” może nadal wspominać o partycjach.
- Przypadek „`sla.manage` bez prawa edycji przepływów” nie był sprawdzany w teście end-to-end.
