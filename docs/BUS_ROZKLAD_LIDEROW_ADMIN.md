# TentaBus: rozkład prowadzenia topików między nody — poradnik administratora

Stan implementacji: 2026-11-23 (TB-04, kroki K4–K8: commity 6beca1a65, 864d4279c, 91b1a7afe, f4e121df0, 111999003 oraz poprawki po przekliku K10).

Ten dokument opisuje, który node prowadzi (przyjmuje zapisy) który topik TentaBus, jak prowadzenie się rozkłada, jak je bezpiecznie przenosić i co widzi użytkownik, który trafi na node nieprowadzący topiku. Zachowanie sprawdzono na żywo na dwóch nodach (dwa procesy na jednym serwerze, build K8) i opisano po poprawkach z tego przeklikania (K10). Tam, gdzie coś jest wnioskiem z kodu, a nie obserwacją, jest to zaznaczone. Scenariusz do przećwiczenia na 2–3 nodach: `docs/BUS_ROZKLAD_LIDEROW_DEMO.md`.

Słownictwo jest takie jak w aplikacji: „node”, „topik”, „prowadzenie”. Nody są w ekranach nazwane nazwą hosta maszyny (nie ma pola do jej ustawienia w panelu; zmienia się ją w systemie); node bez nazwy, albo z nazwą wspólną z innym nodem (np. dwa procesy na jednym serwerze), jest pokazany jako pierwsze 8 znaków id, a pełne id jest w podpowiedzi po najechaniu. Ekrany opisuje sekcja „Ekrany w panelu TentaBus”.

## Pojęcia

- **Lider partycji** (node prowadzący): jedyny node, który przyjmuje zapisy partycji i obsługuje jej odbiorców. Pozostałe nody z listą replik trzymają kopie.
- **Node preferowany**: pierwszy node na liście replik partycji (`replicas[0]`). To node, do którego topik wraca po awarii. Kolejność replik przeżywa elekcję i przeniesienia, więc nie potrzeba osobnego pola ani migracji bazy.
- **Jednostka = cały topik razem z jego DLQ (`__dlq.<topik>`).** Wszystkie partycje topiku i jego DLQ mają jednego lidera. Topik prowadzony przez dwa nody nie da się czytać (odbiorca wymaga lidera wszystkich partycji), dlatego przeniesienia, rozkład i powrót zawsze ruszają topik w całości. Rozkład partycji jednego topiku na wiele nodów nie jest częścią tej funkcji.
- **Node z funkcją przekazywania** (`forward_capable`): node, który rozumie przekazywane żądania (zapis, odczyt REST i przeniesienie prowadzenia). Od tego zależy, czy może być liderem z rozkładu (patrz niżej).

## Jak wybierany jest lider nowego topiku (K4)

Przy `create_topic` lider topiku to **najmniej obciążony** z kandydatów, a nie node, który tworzy topik.

- **Kandydaci:** node tworzący oraz osiągalne nody tego samego środowiska, które ogłaszają przekazywanie zapisu i odczytu (`BUS_FEATURES_FORWARDING`, wszystkie trzy bity naraz). Node, który odpowie, że nie hostuje tej instancji, nie jest jej członkiem: nie bywa kandydatem i nie blokuje rozkładu.
- **Obciążenie** to liczba partycji instancji, których dany node jest **preferowanym** liderem (`replicas[0]`), liczona po wszystkich organizacjach i wraz z topikami wewnętrznymi (`__*`). Nie jest to bieżący lider, więc node, który przejął partycje po awarii, nie dostaje przy tym nowych topików.
- **Remis** rozstrzyga pierścień nodów posortowany po id, zaczynany od pozycji `hash(nazwa topiku) mod liczba kandydatów`. Ten sam stan klastra daje ten sam wynik, a różne topiki nie trafiają zawsze na ten sam node.
- Wszystkie partycje topiku mają jednego lidera i ten sam node preferowany. Dotyczy to także topików z RF=1. Pozostałe repliki są rozkładane po kolei (round-robin po numerze partycji) z nodów tego samego środowiska.
- **DLQ idzie za źródłem:** partycja `__dlq.<topik>` o numerze `p` dostaje lidera i listę replik partycji `p` topiku źródłowego. Partycje DLQ bez odpowiednika układane są według partycji źródła.
- **`update_topic` z nowymi partycjami:** nowe partycje dostają lidera i kolejność replik istniejących partycji (wybiera się lidera większości i jego node preferowany), niezależnie od tego, który node jest akurat osiągalny. Topik nie zostaje przez rozkład rozdzielony między dwa nody; martwego lidera odzyska zwykła elekcja.
- **Wersje mieszane — lider zostaje na nodzie tworzącym** (jak przed TB-04), gdy:
  - dowolny node z tego samego środowiska nie ogłasza przekazywania, albo nie da się tego stwierdzić (brak odpowiedzi w 1 s, `Unknown`);
  - node jest teraz rozłączony i nie wiadomo, że przekazuje. Stary node po powrocie nie mógłby pisać do topiku, którego liderem jest inny node.
  Nody, od których nie przyszedł jeszcze `Hello`, są pytane równolegle, łącznie najwyżej 1 s (`PLACEMENT_PROBE_BUDGET`). Odpowiedź odmowna jest pamiętana 60 s (`PROBE_MISS_TTL`).
- Wpis audytu `bus.topic.create` ma w szczegółach `leader=<node> leader_rf=<RF>` przed listą replik.

Pierwszy zapis twórcy do topiku prowadzonego zdalnie nie kończy się błędem: lider, który nie zdążył zmaterializować wiersza topiku, czeka na niego do 0,5 s, a nadawca ponawia z rosnącą przerwą (50 ms, podwajaną do 800 ms) w granicach czasu oczekiwania na lidera. Zapis ze starszą generacją topiku (topik usunięto i utworzono ponownie) jest odrzucany i nigdy nie trafia do nowej inkarnacji.

## Przekazywanie zapisu i odczytu (K2–K3, bramka funkcji)

Żądanie, które trafi na node nieprowadzący partycji, jest przekazywane do lidera zamiast odrzucane. Dzieje się to w ramkach replikacji na ALPN `tentaflow-bus/v1`, po jednym strumieniu na żądanie.

| Co | Przekazywane? |
| --- | --- |
| Zapis (`publish`: REST `POST`, blok `bus_publish`, addon `bus_publish_v1`, DLQ, kwarantanna, rollup metryk) | tak, do lidera każdej partycji; jedno żądanie na lidera |
| Odczyt REST `GET .../records` (otwarcie konsumenta, pobranie, commit) | tak, w całości do lidera, ale tylko gdy **jeden** node prowadzi wszystkie partycje topiku |
| Przeniesienie prowadzenia | tak, do bieżącego lidera partycji |
| Konsument addonu (`bus_consume_open_v1`, uchwyt stanowy) | **nie**: odmowa `NoAvailableTarget` z nazwą lidera w audycie `bus.consume.open` (patrz `tentaflow-core/docs/ADDON_HOST_FUNCTIONS.md`, §18) |
| Reaktor flow `bus_consume` | **nie**: pracuje tylko node prowadzący; na pozostałych stan odbiorcy to `not_leader` (patrz niżej) |

Zasady:

- Lider wykonuje pełny potok zapisu: autoryzację, polityki pól, schemat, kwarantannę, dedup, limity i nagłówki `tf.*`. Oryginalny wywołujący jest autoryzowany **jeszcze raz na liderze**, jego własnym autoryzatorem i własną kopią ledgera (w tym członkostw w organizacji).
- Aktor systemowy brokera (`SYSTEM_ACTOR`) jest przyjmowany w przekazanym **zapisie** wyłącznie dla topików `__*` (DLQ, metryki); w przekazanym odczycie nigdy.
- Żądanie przekazane **nigdy nie jest przekazywane dalej**: odbiorca, który przestał być liderem, odpowiada `NotLeader`, więc żądanie nie krąży między nodami.
- Odmowa na liderze trafia do audytu razem z nodem, który przekazał żądanie (`forwarded_from=<node>`).
- Nadawca czeka na lidera (trwa elekcja, albo prowadzenie się właśnie zmienia) do 3 s (`LEADER_WAIT`) i ponawia **raz** po `NotLeader` otrzymanym przed zapisem. Żądanie, które wyszło z noda i nie dostało odpowiedzi, **nie jest ponawiane**: lider mógł je zapisać.
- Zapis obejmujący partycje prowadzone przez kilka nodów może się **częściowo udać**: pierwsza porażka kończy wywołanie, a partycje już przyjęte zostają w logu (`PartialPublish`).
- Odczyt przekazany do lidera jest **at-most-once**, gdy odpowiedź zginie (lider zatwierdza offset przed jej wysłaniem). Odczyt obsłużony na własnym nodzie pozostaje at-least-once. `max_records` i `max_bytes` obowiązują także odczyt przekazany; odpowiedź jest przycinana do jednej ramki, a lider zatwierdza tylko rekordy, które zwraca.

### Bramka funkcji (wersje mieszane)

Każdy node ogłasza obsługiwane rodzaje przekazywania w polu `bus_features` ramek `Hello`/`HelloAck`:

| Bit | Znaczenie |
| --- | --- |
| `1 << 0` | `ProduceForward` (zapis) |
| `1 << 1` | `ConsumeForward` (odczyt REST) |
| `1 << 2` | `TransferForward` (przeniesienie prowadzenia) |

Do prowadzenia z rozkładu i do udziału w przenoszeniu prowadzenia node musi ogłaszać **wszystkie trzy** bity. Node, od którego nie przyszedł jeszcze `Hello` (np. nie ma repliki), jest przed pierwszym żądaniem pytany o swoje bity (`LeoQuery` bez partycji). Wynik niezerowy jest zapamiętywany; odmowa jest pamiętana 60 s. Node, który nie odpowiada na pytanie albo nie zna bitów, nie dostaje żądania (starszy node porzuciłby je bez odpowiedzi, co wyglądałoby jak nieznany wynik):

- zapis wraca do wywołującego jako dotychczasowe `NotLeader` (z nazwą lidera);
- przeniesienie prowadzenia kończy się `bus.node_without_forwarding`;
- odczyt REST kończy się 503 `bus_not_leader`;
- pole `forward_capable` w `BusReplicaNodeWire` pokazuje stan noda (pole opcjonalne; `false` także od starszego noda, który go nie raportuje).

### Odpowiedzi REST

Dotyczy `POST|GET /v1/bus/instances/{instance_id}/topics/{topic}/records?org_id=...` (oraz dawnej ścieżki bez segmentu instancji). Przed TB-04 każda z poniższych sytuacji dawała 500 `internal_error`.

| HTTP | `error.code` | `Retry-After` | Znaczenie i co robić |
| --- | --- | --- | --- |
| 503 | `bus_not_leader` | `1` | Żaden lider nie może teraz przyjąć żądania (elekcja, przeniesienie w toku, topik rozdzielony między nody, starszy node bez przekazywania). Nic nie zapisano ani nie odczytano; ponowienie jest bezpieczne. |
| 503 | `bus_not_enough_replicas` | `1` | Za mało replik w ISR, by przyjąć zapis. Nic nie zapisano; można ponowić. |
| 503 | `bus_partition_unavailable` | `1` | Partycja chwilowo niedostępna (brak przypisania). Można ponowić. |
| 504 | `bus_forward_outcome_unknown` | brak | Żądanie wyszło do lidera i nie wróciła odpowiedź. Przy awarii lidera zdarza się to regularnie (w teście 4 z 1314 zapisów w oknie przejęcia; żaden z nich nie trafił do logu), więc producent musi ponawiać. Zapis mógł trafić do logu; **przed ponowieniem użyj klucza idempotencji lub tożsamości producenta**. Przy odczycie rekordy mogły zostać zatwierdzone dla grupy, choć wywołujący ich nie zobaczył; sprawdź offsety grupy. |
| 504 | `bus_ack_timeout` | brak | Repliki nie potwierdziły zapisu w czasie. Zapis mógł się udać; jak wyżej. |
| 502 | `bus_leader_failure` | brak | Lider partycji zawiódł po swojej stronie (dysk, silnik). Stan nie mija sam; szczegóły są w logu lidera. |
| 500 | `internal_error` z `error.acked` | brak | Część wielopartycyjnej paczki już trafiła do logu. `error.acked` to lista `{partition, base_offset, accepted}`; **tych rekordów nie wolno wysyłać ponownie bez klucza idempotencji**. |

Treść 503 i 504 jest stała: nie zawiera identyfikatorów nodów, epok ani powodów. Błędy odczytu lidera (np. `OffsetOutOfRange`) docierają do wywołującego jako ten sam błąd, jaki dałby odczyt na własnym nodzie.

**Klucze API i flow.** Klucz API działa **tylko na nodzie, który go wydał**: na drugim nodzie daje 401, więc wywołanie przez każdy node wymaga klucza wydanego na tym nodzie. Flow zapisujący do topiku potrzebuje **klucza topiku z prawem** w „Dostęp” › „Macierz dostępu (per klucz)”: zapis z kluczem użytkownika jest odmawiany (audyt `bus.produce.denied`), rekord nie trafia do topiku, a flow bez strumieniowania kończy się błędem 500 `flow.failed`. Flow ze strumieniowaniem (`"stream":true`) zachowuje status 200, bo strumień już ruszył, i kończy się zdarzeniem błędu SSE `{"error": {...}}` z `flow.failed`, po którym idzie `[DONE]`: klient strumienia musi czytać treść, nie sam status.

Sukces zapisu to 200 z `published`, `schema_rejected`, `schema_dropped`, `correlation_id`, `correlation_ids`. Odczyt: 200 z `records`. Odczyt przez REST ma `wait_ms` do 5000 ms.

### Addon i blok flow

- Addon: `bus_publish_v1` zwraca `Backpressure` dla „partycja bez lidera” (nic nie zapisano, ponów za chwilę) i `Timeout` dla nieznanego wyniku zapisu przekazanego do lidera albo braku potwierdzenia replik (zapis mógł się udać). Szczegóły: `tentaflow-core/docs/ADDON_HOST_FUNCTIONS.md`, §18.
- Blok `bus_publish` podaje w powodzie błędu: „partition of topic … has no leader at the moment”, „… got no confirmation in time; it may or may not have been stored” albo „the node that owns topic … failed while serving the write”.

## Bezpieczne przenoszenie prowadzenia (K5)

Przeniesienie wykonuje **bieżący lider partycji**. Żądanie, które trafi na inny node, jest do niego przekazywane (`TransferForward`); prosić może każdy node tej samej instancji i środowiska, także taki, który nie trzyma repliki. Lider zna swój żywy ISR i postęp każdej repliki, więc to on rozstrzyga.

### Gwarancje

1. **Cel w żywym ISR.** Cel musi być repliką partycji i być w **żywym** ISR lidera (nie w ISR z ledgera, który zmienia się tylko przy elekcjach i rekonfiguracjach). W przeciwnym razie odpowiedź to `bus.not_a_replica`.
2. **Zamknięcie zapisów i dogonienie.** Lider zamyka zapisy w samej partycji; po tym żaden zapis nie ląduje. Zapis, który był już za bramką, ląduje w logu przed zamknięciem albo jest odrzucony. Potem lider czeka najwyżej **ok. 2 s**, aż cel potwierdzi cały log do końca, i dopiero wtedy proponuje nowy wiersz przypisania. Dzięki temu cel nie przejmuje logu bez rekordów, które lider potwierdził producentowi (`acks=leader`), a stary lider nie obcina własnego ogona. Testy: 300 zapisów `acks=leader` tuż przed przeniesieniem nie ginie; opóźniony cel sprawdzono na poziomie menedżera.
3. **Cel w tyle** → `bus.transfer_target_behind`, zapisy są otwierane z powrotem i nic się nie zmienia.
4. **Zapis w trakcie przenoszenia** trafia na lidera i czeka; nadawca co 100 ms sprawdza, gdzie partycja jest prowadzona, więc zwykle czeka krócej niż 0,5 s, a najwyżej 3 s (`LEADER_WAIT`). Jeśli przeniesienie trwa dłużej, zapis jest odrzucany jako `bus.not_leader` (do ponowienia), nigdy przepuszczany. Producent przez przekazywanie (patrz wyżej) trafia potem do nowego lidera.
5. **Brak odpowiedzi o przyjęciu wiersza.** Lider czeka do 5 s, aż nowy wiersz przyjmie większość replik, a potem do 5 s czyta wiersz jeszcze raz. Żądanie przekazane do lidera czeka na jego odpowiedź do 12 s. Jeśli nadal nie wiadomo, gdzie partycja jest prowadzona, wynik to `bus.transfer_outcome_unknown`, a stary lider **zostaje zatrzymany** (nie przyjmuje zapisów), dopóki ledger nie rozstrzygnie. Przed ponowieniem sprawdź, który node prowadzi partycję.
6. **Cały topik albo żadna partycja.** Najpierw każda partycja topiku i jego DLQ jest sprawdzana u swojego lidera (na sucho). Dopiero potem partycje są przenoszone po kolei. Gdy któraś zawiedzie, partycje już przeniesione wracają do poprzedniego lidera, z dokładnie pierwotną listą replik (cofnięcie trwa najwyżej 15 s na partycję; wpisy audytu `reason=rollback`).
7. **Przeniesienie nie rozmija się z rzeczywistością.** `TransferForward` niesie oczekiwanego lidera i jego epokę. Lider odmawia, gdy wiersz partycji jest już inny niż ten, z którego liczono ruch (`bus.not_leader`). Cofnięcie oddaje partycję tylko wtedy, gdy nadal prowadzi ją ten, kto ją wziął.
8. **Pauza grupy blokuje przeniesienie** (`bus.transfer_blocked`, powód `consumer_group_paused`; gdy stanu pauz nie da się odczytać: `pause_state_unreadable`). Pauza jest wprawdzie replikowana (patrz „Odczyty z repliki”), ale topik z wstrzymaną grupą nie jest przenoszony ani automatycznie, ani ręcznie.
9. **Cel musi przyjmować przekazywane żądania** (`bus.node_without_forwarding`), inaczej producenci na pozostałych nodach straciliby zapisy. Dotyczy też przeniesień ręcznych. Node, który przyjmuje żądanie, jest zawsze uznawany za zdolny.
10. **Node preferowany po przeniesieniu.** Przeniesienie na inny node niż preferowany robi z celu node preferowany (`replicas[0]`), żeby automatyczny powrót nie cofnął ruchu po 5 minutach. Powrót na node preferowany nie zmienia kolejności replik, a przejęcie po awarii nigdy jej nie zmienia.

### Kody błędów

Kod jest pierwszym tokenem komunikatu błędu (`bus.<kod>: ...`), tak jak w pozostałych odpowiedziach busa. W wynikach Apply ten sam kod jest w polu `error_code`.

| Kod | `ProtocolErrorCode` | Kiedy | Co robić |
| --- | --- | --- | --- |
| `bus.not_leader` | Conflict | Wiersz partycji jest już inny niż ten, z którego liczono ruch (ktoś lub elekcja przeniosły partycję), albo epoka przeszła. | Zaplanuj ponownie. Tekst komunikatu ma ten sam format co przy zapisie i odczycie: `bus.not_leader: leader_node_id=<id> leader_epoch=<n>` (bez lidera: `bus.not_leader: epoch=<n>`). Wyjątek: odmowa z powodu epoki brzmi „epoch fenced have=… requested=…” i nie zawiera `leader_node_id=`/`leader_epoch=` (patrz „Uwagi o komunikatach błędów”). |
| `bus.transfer_target_behind` | Conflict | Cel jest w ISR, ale nie dogonił lidera w ok. 2 s. Sprawdzenie na sucho (Plan, pierwsza ocena każdej partycji) uznaje cel za niezdolny do dogonienia, gdy brakuje mu ponad 50 000 rekordów albo ma ponad 4 MiB wysłanych i niepotwierdzonych bajtów; to wskazówka, bo lider nie zna szybkości łącza. | Poczekaj, aż replika dogoni; powtórz. Nic się nie zmieniło. |
| `bus.not_a_replica` | BadRequest | Cel nie jest repliką partycji albo nie jest w żywym ISR lidera. | Wybierz node z listy ISR. |
| `bus.transfer_blocked` | Conflict | Pauza grupy konsumentów topiku. Powód jest w nawiasie komunikatu: `consumer_group_paused` albo `pause_state_unreadable`. | Wznów grupę, potem powtórz. |
| `bus.node_without_forwarding` | Conflict | Cel (albo lider partycji, do którego trzeba przekazać żądanie) nie ogłasza przekazywania. | Zaktualizuj node. Do tego czasu przeniesienie na niego jest niemożliwe. |
| `bus.transfer_refused` | Conflict | Lider odrzucił samo żądanie przekazania. Powód w nawiasie: `unknown_instance`, `environment_mismatch` albo `handler_failed`. | `unknown_instance`/`environment_mismatch`: żądanie trafiło do noda spoza instancji lub środowiska. `handler_failed`: błąd po stronie lidera, sprawdź jego log. |
| `bus.transfer_outcome_unknown` | Conflict | Żądanie wyszło do lidera i nie wróciła odpowiedź, a wiersz nie pokazał, gdzie partycja jest. | **Sprawdź, który node prowadzi partycję, zanim zapytasz ponownie.** Stary lider jest zatrzymany do rozstrzygnięcia. W wyniku Apply: `failed_partial`. |
| `bus.rebalance_busy` | Conflict | Na tym nodzie trwa inny rozkład, ręczne przeniesienie, jednorazowy rozkład po aktualizacji albo powrót. Zamek jest per node. | Poczekaj i powtórz. |
| `bus.plan_changed` | Conflict | Plan policzony przy Apply różni się od przesłanego (patrz „Plan i Apply”). | Pobierz plan jeszcze raz. Nic się nie ruszyło. |
| `bus.permission_denied` | PolicyDenied | Brak prawa administratora na topiku. | Nadaj prawo albo użyj konta z prawem. |
| `bus.replication_disabled` | NotAvailable | Ten node nie ma replikacji. | — |

`TransferOutcomeUnknown` to nazwa wariantu błędu w kodzie (`ReplError`); klient widzi go jako `bus.transfer_outcome_unknown`.

### Przeniesienie ręczne (`LeaderTransferRequest`)

`LeaderTransferRequest { topic, partition, target_node_id }` przenosi **cały topik** (wszystkie partycje i `__dlq.<topik>`), a nie jedną partycję; `partition` służy tylko do wyboru epoki w odpowiedzi. `LeaderTransferResponse { leader_epoch }` niesie nową epokę wskazanej partycji (a gdy cel już ją prowadził, jej dotychczasową). `target_node_id` nie może być pusty ani zawierać spacji, `=` i znaków sterujących. Wymaga tego samego co Apply (patrz niżej). Wpisy audytu: `bus.leader.transfer` dla każdej przeniesionej partycji (`reason=manual`). Wynik niepełny jest raportowany w komunikacie błędu: „no partition changed node”, „partitions that had moved were handed back” albo „these partitions could not be handed back and stay on the target: …; where these are led is not known: …”.

## „Rozłóż prowadzenie”: Plan i Apply (K5)

Ekran: zakładka „Kopie i nody”, karta „Rozkład prowadzenia” (patrz „Ekrany w panelu TentaBus”).

Dwie wiadomości w pakiecie `BusPayload` (ramka `BusBody` z `instance_id`; ten sam kanał co reszta panelu, nie REST `/v1`):

| Żądanie | Odpowiedź | Zmienia stan? |
| --- | --- | --- |
| `LeaderBalancePlanRequest { topic?: string }` | `LeaderBalancePlanResponse { moves, per_node, blockers }` | nie |
| `LeaderBalanceApplyRequest { moves }` | `LeaderBalanceApplyResponse { results }` | tak |

**Uprawnienia** (obie wiadomości): `bus.admin` w macierzy uprawnień instancji **oraz** rola Admin w organizacji (`org.admin`). Apply wymaga ponadto prawa administratora do **każdego** topiku, który nazywa (`bus.permission_denied` przed jakimkolwiek ruchem). Plan: topik bez prawa trafia do blokad jako `no_admin_right`; topik, którego wołający nie może nawet czytać, nie jest w ogóle nazwany.

### Plan

- `moves`: lista `{ topic, partitions, from_node_id, to_node_id, returns_to_preferred }`. `from_node_id` to node, który prowadzi większość topiku. `partitions` to partycje topiku; partycje jego DLQ idą razem. `returns_to_preferred = true` oznacza powrót na node preferowany (nie zmienia `replicas[0]`); każdy inny ruch robi z celu node preferowany.
- `per_node`: `{ node_id, partitions_before, partitions_after }` — prowadzone partycje przed i po. „Prowadzona” znaczy bieżący lider (`leader_node_id`), nie preferowany. `per_node` dotyczy organizacji wołającego; partycje innych organizacji ważą na bilansie, ale nie są raportowane.
- Algorytm jest deterministyczny (to samo wejście daje ten sam plan; wszystko sortowane po nazwie). Najpierw wracają na swój node preferowany topiki, które go nie mają; potem plan przenosi cały topik z najbardziej do najmniej obciążonego noda, dopóki ruch zmniejsza różnicę, wybierając ruch zostawiający najmniejszą różnicę. To algorytm zachłanny, **nie dowód minimalnej liczby ruchów**. Każdy topik rusza najwyżej raz na plan.
- Różnica „najwyżej waga najmniejszego topiku” nie zawsze jest osiągalna (topiki 8, 3 i 1 dają co najwyżej 4). Gdy zostaje szersza i nic jej nie tłumaczy, plan wskazuje topik z powodem `would_not_improve`.
- Każdy ruch w planie przeszedł **sprawdzenie na sucho u lidera** każdej partycji (żywy ISR, dogonienie celu). Ruch, który lider odrzucił, wypada z planu jako blokada, a plan liczy się od nowa. Pytanie liderów trwa najwyżej 20 s; ruchy niesprawdzone do tego czasu wypadają jako `leader_unavailable`. ISR z ledgera, które nie jest żywe, nie blokuje ruchu: ocenia go lider.
- Powrót zablokowany tylko **chwilowo** (cel w tyle, poza ISR, niedostępny) jest liczony przy nodzie preferowanym, bo automatyczny powrót (K6) wykona go sam. Plan nie przenosi wtedy innego topiku na ten node. Powrót zablokowany **trwale** (pauza grupy, node bez przekazywania) zostawia topik tam, gdzie jest. Przy wyłączonym wyłączniku powrotu (patrz niżej) takie topiki też są liczone tam, gdzie stoją.

**Blokady** (`blockers[].reason`, opcjonalnie `node_id`):

| Powód | Znaczenie |
| --- | --- |
| `node_unreachable` | node prowadzący topik albo cel nie odpowiada |
| `node_without_forwarding` | lider albo cel nie rozumie przekazywanych przeniesień |
| `target_outside_isr` | cel nie jest repliką któregoś z partycji jednostki albo jest poza żywym ISR |
| `target_behind` | cel jest w ISR, ale nie dogoni w czasie, na jaki przeniesienie wstrzymuje zapisy |
| `consumer_group_paused` | grupa topiku (lub jego DLQ) jest wstrzymana |
| `no_admin_right` | wołający nie ma prawa administratora na topiku |
| `leader_unavailable` | lidera którejś partycji nie dało się zapytać albo już jej nie prowadzi |
| `would_not_improve` | przeniesienie nie wyrównałoby liczb (`would_not_improve` nie jest wymienione w komentarzu pola `BusLeaderBlockerWire.reason`) |

Pauza grupy jest czytana z lokalnej bazy planującego noda; pauzy, których ten node nie zna, wracają jako odmowy liderów (`transfer_blocked`).

### Apply

- Apply **liczy plan jeszcze raz** dla całej organizacji i porównuje go z przesłanymi `moves`. Jeśli plan się różni, a wszystkie przesłane ruchy dotyczą jednego topiku, porównuje jeszcze z planem samego tego topiku (takim, jaki zwraca `LeaderBalancePlanRequest { topic }`). Różnica daje `bus.plan_changed` i nic się nie rusza. Prześlij więc dokładnie `moves` z odpowiedzi planu.
- Maksymalnie 4096 ruchów w jednym żądaniu (`bus.invalid_argument` powyżej).
- **Jeden rozkład naraz na node** (`bus.rebalance_busy`). Rozkłady na różnych nodach nie są wykluczane, ale każdy ruch czyta wiersze od nowa, a lider odmawia przeniesienia partycji, której już nie prowadzi, więc dwa rozkłady nie zrobią topiku rozdzielonego; drugi dostanie `bus.plan_changed` (lub `bus.not_leader` w wyniku ruchu).
- Topiki idą **po kolei, pierwszy błąd zatrzymuje resztę**. Pozostałe dostają `not_attempted`.

**Wynik per ruch** (`results[]`: `topic`, `partitions`, `from_node_id`, `to_node_id`, `status`, `error_code?`, `detail?`):

| `status` | Znaczenie | Gdzie jest topik |
| --- | --- | --- |
| `moved` | wszystkie partycje topiku i DLQ prowadzi cel | na celu |
| `failed` | odmowa przed ruchem albo błąd bez przeniesienia | bez zmian |
| `failed_rolled_back` | część partycji się przeniosła i wróciła | bez zmian |
| `failed_partial` | nie wszystkie przeniesione partycje dało się oddać (albo nie wiadomo, gdzie są): topik może być rozdzielony | **wymaga administratora**: sprawdź, kto prowadzi partycje z `detail` |
| `not_attempted` | wcześniejszy ruch się nie powiódł i zatrzymał przebieg | bez zmian |

**Częściowy sukces** dotyczy więc całego Apply: kilka pierwszych topików może mieć `moved`, jeden `failed*`, reszta `not_attempted`. Zawsze czytaj `results` w całości. `failed_partial` z kodem `bus.transfer_outcome_unknown` oznacza brak odpowiedzi lidera: partycja mogła się przenieść. Wynik `failed_partial` jest naprawiany automatycznie jako „zebranie” przy następnym przeglądzie reguły powrotu (patrz niżej), o ile wyłącznik jest włączony.

**Audyt:** `bus.leader.transfer` dla każdej przeniesionej partycji (i każdego cofnięcia) oraz jeden `bus.leader.rebalance` na organizację (patrz „Audyt”).

## Jednorazowy rozkład po aktualizacji (K5, P4)

Istniejące topiki po aktualizacji są rozkładane **automatycznie, raz na instancję**, tym samym mechanizmem co Plan/Apply:

- Rozkład zaczyna się, gdy **wszystkie** nody instancji ogłaszają przekazywanie. Wykonuje go node o najniższym id wśród nich. Pierwsze sprawdzenie następuje ok. 60 s po starcie noda, potem co 30 s, a node, który rozkładu nie wykonuje (jednowęzłowa instalacja, nie ten o najniższym id), sprawdza stan co 10 min.
- Plan obejmuje wszystkie organizacje, bez ograniczeń praw (aktor `system`). Topik, który zawiedzie, **nie zatrzymuje pozostałych** (inaczej niż przy ręcznym Apply). Te same blokady co w planie (m.in. wstrzymana grupa, ISR niedogoniony).
- Każdy topik zostawiony na miejscu ma wpis `bus.leader.rebalance_skipped` z powodem (`source=post_upgrade_spread`).
- **Znacznik wykonania** to klucz ustawień lokalnego noda `bus.leader_spread:<instancja>` = `done`. Powstaje tylko wtedy, gdy nic nie zostało z powodu przejściowego (cel w tyle lub poza ISR, node niedostępny, nieznany wynik). W przeciwnym razie rozkład jest ponawiany z przerwą 1, 2, 4 … min (najwyżej 30 min). Powód trwały (wstrzymana grupa, node bez przekazywania, brak prawa, `would_not_improve`) znacznika nie wstrzymuje; do takich topików służy ręczne „Rozłóż prowadzenie”.
- Znacznik **nie jest replikowany**: node o niższym id, który dołączy później, może uruchomić rozkład ponownie. Przy zrównoważonych topikach nic wtedy nie przeniesie.
- Wyłącznik powrotu nie wstrzymuje rozkładu po aktualizacji, ale zmienia sposób liczenia: przy wyłączonym wyłączniku topiki z powrotem zablokowanym chwilowo są liczone tam, gdzie stoją (jak w planie).

## Automatyczny powrót do noda preferowanego (K6)

Po awarii topik prowadzi inny node niż preferowany. Gdy node preferowany wróci, **lider topiku sam oddaje mu topik**, bez koordynatora. Decyzję podejmuje wyłącznie bieżący lider, więc dwa nody nie przerzucą tej samej partycji jednocześnie.

### Warunki (wszystkie naraz)

- Node preferowany jest w **żywym ISR** lidera i nie jest dalej niż **100 rekordów** za końcem logu (`PREFERRED_MAX_LAG_RECORDS`) nieprzerwanie od co najmniej **5 minut** (`stable_for`), dla każdej partycji topiku i jego `__dlq.*`.
- Node preferowany przyjmuje przekazywane żądania.
- Nic nie blokuje przeniesienia: wstrzymana grupa konsumentów zostawia topik na miejscu.
- Stan ISR jest próbkowany co 5 s, decyzje zapadają co 30 s, więc wypadnięcie z ISR zauważone przez próbkę zaczyna odliczanie od nowa (wypadnięcie krótsze niż 5 s może umknąć). Odliczanie dotyczy epoki lidera i generacji topiku: utrata i odzyskanie prowadzenia, ponowne utworzenie topiku albo restart noda zaczyna je od zera. Node niestabilny (ISR przerywany) nigdy nie dostaje topiku z powrotem.

### Tempo

- **Najwyżej jeden topik na 30 s na node** (`decision_interval`).
- Powrót używa tego samego przeniesienia całego topiku co Plan/Apply (sprawdzenie każdej partycji, dogonienie celu, cofnięcie przy błędzie) i **nie zmienia `replicas[0]`**. Idzie przez ten sam zamek wykonawcy co ręczne przeniesienie, ręczny rozkład i rozkład po aktualizacji: gdy któryś trwa, krok jest pomijany (bez błędu) i powtarzany przy następnej próbce.
- Powrót, który się nie udał i niczego nie przeniósł, zaczyna odliczanie topiku od nowa.

### Topik rozdzielony po awarii („zebranie”)

Po awarii partycje wybierają liderów po kolei, więc jeden topik może trafić pod dwa nody, a taki topik nie da się czytać. Zbiera go **jeden node: ten o najniższym id wśród liderów jego partycji**, bez czekania 5 minut:

- jeśli liderzy partycji mówią (sprawdzenie na sucho), że node preferowany może teraz przejąć wszystko, topik idzie na niego (`reason=preferred_restore`);
- w przeciwnym razie topik idzie na node, który prowadzi największą część jednostki (`reason=split_gather`; przy remisie najniższe id). Takie zebranie **nie przepisuje `replicas[0]`**: naprawia rozdzielenie, a powrót na node preferowany nastąpi potem zwykłą regułą.

Przeniesienie, które zostawiło topik rozdzielony (`failed_partial`), jest ponawiane przy następnym przeglądzie jako zebranie. Nieudane zebranie czeka dwa przeglądy (60 s).

### Wyłącznik

Wyłącznik jest **per instancja** i domyślnie **włączony**: klucz ustawień `bus.leader_return:<instancja>` z wartością `off`, żeby wyłączyć (`on` włącza; akceptowane są też `true`/`false`, `1`/`0`).

```
SettingsUpdateRequest {
  entries: [ { key: "bus.leader_return:tentabus-1a2b3c4d", value: "off", is_secret: false } ]
}
```

(`tentabus-1a2b3c4d` to przykładowy identyfikator instancji; nie jest to wynik uruchomienia.)

- Wymaga roli Admin (polityka `Admin` handlera ustawień). Obsługa odrzuca (`bad_request`): instancję, której ten node nie hostuje, wartość inną niż powyższe i ustawienie oznaczone jako sekret.
- Klucz jest **replikowany Sync Ledgerem do wszystkich nodów** (przedrostek `bus.leader_return:` na liście `SHARED_SETTING_KEY_PREFIXES`, bez migracji): wyłączenie na jednym nodzie wyłącza powroty i zebrania na wszystkich. Node ze starszą wersją nie zna klucza, zapisuje jego operację jako konflikt bez skutków i działa domyślnie, czyli z włączonym powrotem. **Dostarczenie wyłącznika na drugi node nie było sprawdzone end-to-end** (są testy jednostkowe przechwycenia i reseed).
- Wartość nieczytelna (albo klucz, którego nie da się odczytać) jest traktowana jak `off`, z ostrzeżeniem w logu najwyżej co 5 minut: nic nie rusza na domysł.
- Wyłączenie zeruje odliczanie; po ponownym włączeniu 5 minut liczy się od nowa.
- Z wyłącznikiem `off` **wyłączone jest też zebranie topików rozdzielonych po awarii**, więc po awarii z rozdzieleniem topik trzeba zebrać ręcznie (przeniesienie ręczne albo Apply).
- `settings.update` w audycie zapisuje tylko liczbę kluczy (`N keys`), bez nazw. Zmiana wyłącznika **nie jest więc widoczna w dzienniku audytu z nazwy klucza**.

Z jednowęzłowej instancji reguła nie robi nic (sprawdza stan co 5 min).

## Odczyty z repliki (K7)

Ekrany tylko do odczytu działają na **każdej replice**, nie tylko na liderze: przeglądarka wiadomości (`peek`), lista DLQ, lag grup (`group_lag`, lista i szczegóły grupy, szczegóły topiku) i próbkowanie lagu.

### Ograniczenia świeżości

- **Granica odczytu:** replika czyta własny log, ale tylko do `min(hw, committed_offset)`. Przy `acks=leader` (i przy RF=2) hw lidera wyprzedza większość, a replika może zostać obcięta z powrotem do `committed_offset`, więc rekord za tą granicą mógłby zniknąć po przełączeniu. Odczyt nie przekracza granicy także wtedy, gdy leży ona wewnątrz paczki (obcięcie jest co do rekordu). Strona zwraca tę granicę jako `high_watermark`.
- **Widok może pozostawać w tyle za liderem.** Odpowiedź zaznacza to polem `from_replica = true`. Brak pola to `false`. Pole jest w: partycjach przeglądarki i DLQ (`BusBrowsePartitionInfoWire`), podsumowaniu i szczegółach grupy (`BusGroupSummaryWire`, `BusGroupDetailWire`), podsumowaniu lagu topiku (`BusGroupLagSummaryWire`) i seriach historii lagu (`BusGroupLagSeriesWire`). `from_replica` oznacza, że node trzyma replikę **co najmniej jednej** partycji topiku, więc liczby są jego własne.
- **Lag na replice** to jej granica odczytu minus offset grupy, który do niej doszedł. Offsety grup są replikowane (`ReplOffsets`), łączone w okno **500 ms** (`offsets_coalesce_interval`). Lider wysyła pełny stan offsetów partycji przy każdym otwarciu strumienia oraz po utracie powiadomień, więc replika, która była nieobecna, nadrabia je po powrocie. Lag z repliki różni się od lagu lidera o to, co zatwierdzono w ostatnim oknie i o rekordy między jej granicą odczytu a hw lidera.
- Replika stosuje offsety tylko do przodu: po cofnięciu offsetu grupy (reset w tył) pokazuje dotychczasowy, niższy lag, aż grupa zatwierdzi dalej.
- **ACL i polityki pól (ukrywanie pól) replika stosuje według własnej kopii ledgera.** Są tak świeże, jak ta kopia: prawo cofnięte na liderze może jeszcze pozwalać na odczyt na replice, dopóki zmiana do niej nie dojdzie. Dotyczy to autoryzacji i projekcji rekordów w przeglądarce i na liście DLQ.
- Odczyt, który przeciął się z obcięciem logu repliki, jest powtarzany raz; przy drugim obcięciu odmawia (`bus.partition_unavailable`) zamiast zwrócić stronę z mieszanych stanów.
- Follower **bez znanego lidera** (zdegradowany były lider, którego log może być gałęzią, której nikt inny nie ma) niczego nie udostępnia. Node, który nie trzyma żadnej repliki partycji, nadal odpowiada `bus.not_leader` bez nazwy lidera.
- Przeglądarka lidera i odbiorca (`fetch`) też nie wychodzą poza hw: lider tnie paczkę na hw co do rekordu (wcześniej przy RF>1 dostawały całe paczki, także z rekordami, których repliki jeszcze nie mają).
- Metryki konsumenckiego lagu liczą tylko partycje prowadzone przez dany node, żeby replika nie dublowała lagu w sumie po nodach.

### Pauza grupy jest replikowana

Wiersze grup (`bus_groups`: tryb commitu, pauza) są replikowane razem z offsetami (`ReplOffsets.groups`): lider wysyła je przy zmianie i w pełnym stanie przy otwarciu strumienia. Replika zapisuje je według (epoka lidera, `updated_at_ms`) — nowsze wygrywa — i unieważnia podręczny stan grupy. Lista i szczegóły grupy na replice pokazują grupy i ich pauzę, a node przejmujący partycję zachowuje pauzę.

**Granica:** pauza, która nie zdążyła dojść do repliki w jednym oknie łączenia (500 ms), **ginie wraz z poprzednim liderem** (po jego awarii nowy lider uruchomi grupę, którą administrator wstrzymał chwilę wcześniej).

### Co zostaje tylko na liderze

Wszystko, co zmienia stan: ponowienie i odrzucenie rekordu DLQ, „Ponów wszystkie”, pauza i wznowienie grupy, każdy reset offsetu, otwarcie konsumenta, `fetch` i `commit`. Na replice każda taka operacja kończy się odmową `bus.not_leader` (kod Conflict) i niczego nie zmienia. „Ponów wszystkie” odmawia, zanim cokolwiek przeczyta; reset offsetu sprawdza lidera przed wierszem grupy. Ponowienie rekordu DLQ, gdy partycję DLQ prowadzi ten node, a partycji źródłowej inny, kończy się `bus.dlq_retry_source_not_leader` z nazwą lidera.

### Format `bus.not_leader`

| Sytuacja | Komunikat |
| --- | --- |
| Lider znany | `bus.not_leader: leader_node_id=<id> leader_epoch=<n>` |
| Lider nieznany (np. trwa elekcja, albo zdegradowany follower) | `bus.not_leader: epoch=<n>` — bez nazwy, żeby UI nie wziął pustego identyfikatora za node |

Pierwszy token po kodzie zaczynający się od `leader` jest nodem, który UI wskazuje wywołującemu („Prowadzi ją teraz: …”).

### Stan odbiorcy flow

`BusConsumerStatus` przy `reason = not_leader` podaje `leader_node_id` i `leader_node_name` (nazwa wyświetlana, gdy node ją zna). Gdy partycje topiku prowadzą różne nody, jest to node prowadzący ich najwięcej (przy remisie o mniejszym id); w trakcie wyborów pola są puste. Node i nazwę dostaje **tylko sesja administratora**; inne sesje widzą samo `reason = not_leader`. Tekst w Flow Builderze: `flows_config.consumer_reason_not_leader`.

## Przejęcie po awarii lidera

Gdy node prowadzący przestaje odpowiadać, node ze zgodną kopią przejmuje prowadzenie po wygaśnięciu dzierżawy lidera i krótkim dodatkowym oczekiwaniu na ewentualną odpowiedź lidera (przy RF≤2 3 s), żeby wolne łącze nie wyglądało na awarię: **przejęcie trwa ok. 6–12 s** (test dwóch procesów: 8 partycji zabitego lidera przeszło na drugi node po 8,0 s). Węzeł, który dopiero wstał, **nie staje do elekcji, dopóki od startu procesu nie dogonił lidera** (jego log nie doszedł do znacznika, który lider zgłosił), nawet gdy zapisany przydział nadal wymienia go w ISR: ISR skurczone przez lidera nie trafia do zapisanego przydziału, a przejęcie przez nie dogonioną replikę kasowałoby zapisy potwierdzone przez lidera. Jeśli lider w ISR padnie, a druga replika jeszcze nie nadrobiła, partycja czeka bez lidera na replikę z pełnym logiem (w logu: „not standing — this node has not caught up”). Węzeł, który od startu procesu usłyszał lidera, a potem go stracił, i ten, który od startu nie usłyszał lidera, nie stają do elekcji przez 60 s od startu procesu (do 120 s, dopóki mesh pokazuje lidera jako połączonego i świeżo słyszanego albo lider odpowiedział jako lider w ciągu 15 s). **Pomiar na dwóch nodach (starsza wersja, bez dodatkowego oczekiwania): 4–11 s od awarii** (`kill -9`, dwa przebiegi), przy czym przejęcie jest **rozłożone w czasie**: partycje wybierają liderów po kolei, więc przez ok. 6 s topiki bywają podzielone między nody. Ekran nadąża po 1–2 odpytaniach (10–20 s). W tym czasie zapisy przez żywy node trwają, a kilka w oknie przejęcia dostaje 504 `bus_forward_outcome_unknown` (producent ponawia, patrz „Odpowiedzi REST”). Odczyt grupy przy RF=2 nie zatrzymuje się: lider, który został sam w zbiorze zgodnych replik, dalej posuwa granicę odczytu (high watermark). Zebranie topiku rozdzielonego (`split_gather`) w pomiarach nie wystąpiło.

Fałszywe przejęcie `lease_expired` tuż po ręcznym przeniesieniu zaobserwowano jednokrotnie (po przeniesieniu jedna partycja i jej DLQ dostały wpis „przestał odpowiadać”, choć node działał, a ich node preferowany został inny niż reszty topiku). Topik, którego partycje nie zgadzają się co do noda preferowanego, jest pomijany przez automatyczny powrót, więc po ręcznym przeniesieniu sprawdź pinezki w „Partycje i kopie”.

### Strumień replikacji zerwany, mesh żywy (RF=2)

Przy RF=2 węzeł działa sam tylko wtedy, gdy drugi jest poza grą: potwierdził, że nie stanie na tej epoce, albo jest nieosiągalny bez przerwy przez `T_alone` (5 s): bez połączenia mesh, albo z połączeniem, które od 2 s nie niesie heartbeatu i nie odpowiada na sondę kanałem zapytań. Liczenie biegnie od ostatniego znaku życia, a ponowne połączenie, heartbeat lub odpowiedź na sondę je zeruje. Krótkie migotanie łącza nie rusza żadnej ze stron. Gdy strumień replikacji milczy, a drugi węzeł jest w mesh:

- **follower nie staje do elekcji**, na jak długo lider jest osiągalny. Stanie dopiero, gdy ma ważną flagę „doścignięty” (nie cofniętą komunikatem `IsrShrink`) i lider jest nieosiągalny przez `T_alone`. Odpowiedź lidera `leading: true` z bieżącej epoki też go wstrzymuje. Nie ma limitu czasu typu „po 60 s staje mimo wszystko”;
- **lider nie potwierdza zapisów sam.** Zawężony do siebie, wysyła followerowi komunikat `IsrShrink` (kanałem zapytań, który działa przy martwym strumieniu). Do potwierdzenia przez followera albo `T_alone` nieosiągalności: zapisy `acks=all` dostają 503 `bus_not_enough_replicas` (ponawialny; także zapis przyjęty tuż przed zawężeniem), a `hw` nie rusza. Zapisy `acks=leader` są nadal przyjmowane i potwierdzane przy dopisaniu, lecz nie są zatwierdzane (nie przesuwają `hw`, więc konsumenci ich nie widzą). To świadoma decyzja: `acks=leader` działa jak `acks=1` w Kafce, czyli potwierdzenie oznacza dopisanie u lidera, a niezatwierdzony ogon może zostać obcięty przy zmianie lidera (każde obcięcie zapisuje `bus.leader.tail_discarded`). Trwałość daje `acks=all`.
- **nowy lider RF=2 startuje zablokowany** (po elekcji, przeniesieniu, odbudowie, restarcie): zgodę na zatwierdzanie samemu daje potwierdzenie `IsrShrink`, `T_alone` nieosiągalności albo rejestracja followera. Follower, który potwierdził zawężenie, odzyskuje flagę dopiero po heartbeacie lidera z nim w ISR i po dojściu do pozycji z tego heartbeatu. Węzeł bez wpisu o partycji, bez tej instancji albo z zamkniętym menedżerem potwierdza zawężenie od razu;
- **odpowiedź na sondę „nie obsługuję tej partycji”** (a dla lidera: „nie prowadzę jej w tej epoce”) nie liczy się jako znak życia: heartbeat mesh przestaje wtedy ręczyć za węzeł, gdy takie odpowiedzi o danej partycji trwają nieprzerwanie przez `T_alone` i są odnawiane (seria zaczęta po ostatniej ramce strumienia od węzła; odpowiedź „obsługuję”, połączenie lub rozłączenie mesh albo ramka strumienia ją kończą), więc follower nie czeka bez końca na lidera z martwą szyną, a lider nie czeka na followera z martwą szyną. Brak odpowiedzi w ogóle niczego nie przesądza (łącze może być przyczyną), dlatego węzeł osiągalny w mesh, którego strumień i zapytania nie działają, nadal wstrzymuje drugi.

Gdy odmowa trwa dłużej niż 30 s, a drugi węzeł jest w mesh, lider zapisuje błąd w logu i wiersz audytu `bus.leader.alone_refused` (szczegóły: instancja, partycja, peer, `refused_s`). Wstrzymanie dotyczy RF≤2; od RF=3 decyduje większość.

Ubity proces zostawia połączenie mesh stojące do limitu bezczynności transportu (30–40 s), więc o jego śmierci mówi cisza heartbeatu i sondy. Przejęcie trwa `T_alone` plus elekcja: w teście `process_three_node_bus_failover` (osiem partycji) zmierzono 8,8–9,2 s od ubicia lidera do przejęcia wszystkich partycji. W logu follower pisze na poziomie debug „not standing — the leader has not been out of reach of the mesh for the alone interval”. Ostrzeżenie „not standing — this node has not caught up” powtarza się co 60 s na partycję, dopóki wstrzymanie trwa.

**Zatrzymanie noda i restart.** Follower, który zatrzymuje się łagodnie, mówi liderom RF=2, że odchodzi, więc lider zatwierdza sam od razu (a nie po `T_alone`). Gdy node padnie bez zatrzymania (`kill -9`) i wróci na ten sam adres w kilka sekund, połączenia szyny do dawnego procesu są zamykane przy pierwszym połączeniu mesh nowego, więc przekazywanie zapisów i replikacja wracają bez czekania na limit bezczynności QUIC. Czas przejęcia w historii liczy się od ostatniego sygnału życia poprzedniego lidera.

**Kolejność aktualizacji RF=2.** Zabezpieczenie `IsrShrink` działa tylko, gdy lider i follower mają nową wersję: lider sprzed tej zmiany nie wysyła komunikatu (follower nie zdejmuje flagi), a follower sprzed niej nie potwierdza go (nowy lider czeka wtedy `T_alone`). Aktualizuj najpierw followery, potem liderów; w oknie mieszanym obowiązuje dawne zachowanie.

**Ograniczenie RF=2: prawdziwa symetryczna awaria sieci między dwoma węzłami, trwająca dłużej niż `T_alone`, może zgubić potwierdzone zapisy.** Obie strony widzą wtedy drugą jako martwą, każda może prowadzić i po zrośnięciu zostaje łańcuch jednej z nich (`bus.leader.tail_discarded` pokazuje, ile rekordów odrzucono). Zerowa utrata wymaga RF=3, bo o liderze decyduje większość (albo świadka, poza zakresem). To świadomy wybór: przy RF=2 dostępność (lider pisze dalej po `T_alone`) ma pierwszeństwo przed zerową utratą. Dwie instancje na jednym serwerze rozłączają się praktycznie tylko przez zatrzymanie procesu, nie przez sieć.

### Przeniesienie a przejęcie

Lider, który oddał partycję w przeniesieniu (ręcznym, powrocie do noda preferowanego albo rozkładzie), ma po nim ten sam log co cel i może przejąć partycję, gdy nowy lider padnie od razu.

### Zamknięcie węzła

Węzeł zamykany łagodnie (SIGTERM) oddaje najpierw partycje, które prowadzi, zgodnej replice (do 5 s łącznie, po jednej partycji; każde oczekiwanie jest przycięte do pozostałego budżetu, po jego wyczerpaniu nie zaczyna się kolejne przeniesienie), więc druga strona nie czeka na wygaśnięcie dzierżawy. W logu: „handed the bus partitions this node led to a replica before stopping”. Partycja bez zgodnej repliki zostaje i przejęcie trwa jak po awarii.

## Audyt

| Akcja | Kiedy | Szczegóły |
| --- | --- | --- |
| `bus.topic.create` | utworzenie topiku | m.in. `leader=<node> leader_rf=<RF>` i lista replik |
| `bus.leader.transfer` | każda partycja, która zmieniła lidera przez akcję admina, rozkład, powrót lub cofnięcie | `instance_id`, `org_id`, `partition`, `from_node`, `from_epoch`, `target_node_id`, `leader_epoch`, `duration_ms`, `reason` |
| `bus.leader.rebalance` | jeden wpis na organizację na przebieg (Apply, rozkład po aktualizacji, każdy powrót lub zebranie) | `instance_id`, `org_id`, `actor` (id użytkownika albo `system`), `reason`, `moves`, `moved`, `result`; przy błędzie `failed_topic`, `error` |
| `bus.leader.rebalance_skipped` | topik zostawiony na miejscu przez rozkład po aktualizacji | `instance_id`, `org_id`, `reason` (powód blokady albo kod błędu), `node_id`, `source=post_upgrade_spread` |
| `bus.leader.failover` | zwykłe przejęcie po awarii (istniejące, bez zmian w TB-04) | źródło historii przejęć prowadzenia |
| `bus.leader.alone_refused` | lider RF=2 odmawia zatwierdzania sam od ponad 30 s, choć drugi węzeł jest w mesh, a strumień replikacji nie wraca (powtarzane co 30 s) | `instance_id`, `org_id`, `partition`, `peer`, `refused_s` |

Dziennik audytu jest **per node**: wpis leży na nodzie, na którym wykonano akcję (przeniesienie kliknięte na B jest tylko w audycie B). Lista „Przejęcia prowadzenia” w „Kopie i nody” pyta o historię **każdy osiągalny node**, łączy odpowiedzi i usuwa powtórzenia (ten sam topik, partycja, epoka i chwila), więc **gdy nody łączą się bezpośrednio**, pokazuje to samo niezależnie od tego, na którym nodzie jest otwarta; node, który nie odpowie, jest pomijany. W przekliku zapytania do drugiego noda kończyły się błędem `PolicyDenied: no assertion key … known for node …`, więc każdy node pokazywał tylko własne wpisy (znane ograniczenie). Filtr „Akcja” w dzienniku audytu wymaga pełnej nazwy (`bus.leader.transfer`; samo `bus.leader` daje „Brak wpisów”).

**Wartości `reason`** w `bus.leader.transfer` i `bus.leader.rebalance`:

| `reason` | Skąd | Aktor |
| --- | --- | --- |
| `manual` | `LeaderTransferRequest` („Przenieś prowadzenie”) | użytkownik; tylko wpisy `bus.leader.transfer` (ręczne przeniesienie nie zapisuje `bus.leader.rebalance`) |
| `rebalance` | Apply planu „Rozłóż prowadzenie” | użytkownik |
| `post_upgrade_spread` | jednorazowy rozkład po aktualizacji (w UI: „rozkład po aktualizacji”) | `system` |
| `preferred_restore` | automatyczny powrót do noda preferowanego, także zebranie topiku rozdzielonego, gdy preferowany może go przejąć | `system` |
| `split_gather` | zebranie topiku rozdzielonego na node, który prowadzi największą część (bez zmiany `replicas[0]`) | `system` |
| `rollback` | cofnięcie partycji po nieudanym przeniesieniu topiku (tylko `bus.leader.transfer`) | jak przebieg, który cofa |

Odmowy na liderze po przekazanym zapisie lub odczycie zapisują `forwarded_from=<node>`.

## Ekrany w panelu TentaBus

Wszystkie wartości są odświeżane na miejscu co 10 s (odpytanie listy replik), bez przebudowy ekranu.

**Nazwy nodów.** Każde miejsce, w którym ekran nazywa node (chip w nagłówku, karty nodów, tabela rozkładu, „prowadzą: …”, historia przejęć, okna, notka „Czeka”, wiersz kreatora, pinezki partycji, komunikaty), pokazuje tę samą nazwę: nazwę z listy nodów, a bez niej pierwsze 8 znaków id (pełne id jest w podpowiedzi po najechaniu). Nazwa pochodzi z hosta maszyny; nody o tej samej nazwie dostają skrócone id, także w notce „Czeka” (nazwa wysłana przez serwer jest tam użyta tylko dla noda spoza listy i tylko gdy żaden inny node jej nie nosi). Ta sama zasada działa w tekstach błędów (np. „Prowadzi go teraz: Serwer B”).

**„Kopie i nody”:**

- Karty nodów: nazwa, stan, „Prowadzi partycji”, „Trzyma kopii”, „Zgodne kopie”, „Ostatni sygnał”; chip „starsza wersja” przy nodzie bez przekazywania.
- Karta „Rozkład prowadzenia” (tylko przy dwóch i więcej nodach): tabela per node („Prowadzi” z liczbą partycji i topików, „Od równego podziału”, „Topiki poza preferowanym nodem”), ostrzeżenie nad tabelą, przycisk „Rozłóż prowadzenie” (dla administratora instancji) i przełącznik „Automatyczny powrót do preferowanego nodu” (zmienia go administrator platformy). Przycisk jest nieaktywny, gdy drugi node nie odpowiada albo ma starszą wersję, i mówi dlaczego w podpowiedzi. Ostrzeżenie zachęca do „Rozłóż prowadzenie” tylko wtedy, gdy przycisk jest aktywny. Gdy node nie odpowiada, ostrzeżenie zaczyna od tego („Node X nie odpowiada — N topików prowadzi inny node”), a zdanie „kopie … jeszcze doganiają” jest tylko dla noda, który odpowiada. Topik, którego partycje albo DLQ prowadzą różne nody, jest „rozdzielony”: karta liczy go w „poza preferowanym” przy jego preferowanym nodzie i ostrzega. Czas „odpowiada od …” jest zaokrąglany w dół, a poniżej minuty ostrzeżenie mówi „odpowiada dopiero przed chwilą”.
- „Partycje, które wymagają uwagi” z przyciskiem „Przenieś prowadzenie” oraz „Partycje według topiku” i „Przejęcia prowadzenia” (powód każdej zmiany, czas trwania; trwające krócej niż sekundę pokazują „<1 s”).
- **Jedna reguła liczenia partycji.** Karta noda, tabela rozkładu i podsumowanie w oknie „Rozłóż prowadzenie” liczą partycje topików użytkownika razem z ich DLQ i nigdy `__bus.*` ani innych topików wewnętrznych. Reguła jest ta sama co w planie po stronie serwera; podsumowanie okna liczy partycje na podstawie tych samych danych co tabela, nie z sum podanych w odpowiedzi planu.
- **Widok na nodzie, który nie prowadzi.** Własne dane takiego noda o zgodnych kopiach spóźniają się za liderem. Dlatego ekran pyta node prowadzący o jego własną listę replik i to jej odpowiedź decyduje o kopiach i o tym, które przyciski „Przenieś prowadzenie” są aktywne (wszystkie partycje topiku dostają ten sam werdykt). Do czasu odpowiedzi przycisk jest nieaktywny z komunikatem „Sprawdzam…”, a nie z „Żaden inny node nie ma teraz zgodnej kopii”; odpowiedź leży zwykle w ok. 0,1 s, więc ten stan jest ledwo widoczny.
- Przed utworzeniem pierwszego topiku lista nodów (budowana z kopii topików) bywa pusta albo ma jeden node, różny na różnych nodach; ekran nie pokazuje wtedy liczby nodów, a „Kopie i nody” wyjaśnia, że nody pojawią się z pierwszym topikiem. Dwa i więcej nodów jest pokazane zawsze.
- Lista topików jest odświeżana razem z listą replik: topik utworzony na innym nodzie pojawia się w ciągu jednego odpytania, bez przeładowania strony.

**„Przenieś prowadzenie”.** Okno mówi o całym topiku (razem z DLQ) i oferuje tylko node z zgodną kopią; pozostałe mają powód (kopia w tyle o …, kopia niezgodna, node nie odpowiada, starsza wersja). Pinezka oznacza node preferowany. Na nodzie, który topiku nie prowadzi, kliknięcie od razu pokazuje toast „Sprawdzam u nodu prowadzącego stan kopii…”, a okno otwiera się po odpowiedzi lidera, najpóźniej po 2 s; gdy lider nie odpowie, okno otwiera się z widokiem tego noda i notką o braku odpowiedzi. Wynik: „Topik … prowadzi teraz node …”.

**„Rozłóż prowadzenie”.** Jedno okno w trzech stanach:

1. *Podgląd*: tabela „Przeniesienia” (topik, partycje, z → na, dlaczego), sekcja „Pominięte” z blokadami, „Co się stanie po rozłożeniu” i stopka o audycie.
2. *Postęp*: ostrzeżenie „Rozkładam prowadzenie…” z „Przeniesiono n z m topików” i stan każdego wiersza („przeniesiono 4 z 4”, a dla topiku rozdzielonego np. „przeniesiono 3 z 4”; „czeka na kopie”, „przenoszę”, „w kolejce”; w przekliku stanów „czeka na kopie” i „w kolejce” nie widziano, bo cztery przeniesienia trwały 411 ms) odświeżany z odpytań co 10 s. Brak odpowiedzi w terminie to „Nie wiadomo jeszcze, jak skończyło się rozkładanie”: okno dalej obserwuje listy replik i nie raportuje błędu.
3. *Wynik*: „Rozłożono n z m topików” z częściowym sukcesem i powodami błędów. Przy szybkim przebiegu (0,24–0,41 s w teście) postęp jest ledwo widoczny.

**Odbiorca topiku prowadzonego przez inny node.** Stan pokazuje „Czeka: ten topik prowadzi node <nazwa> (tam działa odbiór)” i notkę, że liczby pochodzą z kopii. „Wstrzymaj”, „Wznów” i „Przesuń” (miejsce czytania) są na takim nodzie nieaktywne, a podpowiedź mówi, który node prowadzi topik. Komunikat odmowy `bus.not_leader` mówi „Ten node nie prowadzi tego topiku. Prowadzi go teraz: <nazwa>.”.

**Kreator topiku.** Krok 3 ma wiersz „Node prowadzący”: „dobierze instancja — node preferowany dla najmniejszej liczby partycji (teraz <nazwa>: <n> partycji)”. Liczone są partycje, dla których node jest *preferowany* (nie te, które prowadzi chwilowo), jak w wyborze po stronie serwera. Przy remisie liczby partycji wiersz wymienia obu kandydatów („jeden z nodów: X albo Y”), bo ekran nie liczy skrótu nazwy, który serwer wykorzystuje do rozstrzygnięcia. Przy pierwszym topiku instancji, zanim ekran pozna nody, wiersz mówi tylko, jak node jest wybierany.

## Znane ograniczenia

- **Zakres przeklikania.** Sprawdzono na żywo dwa nody: rozkład przy tworzeniu, przekazywanie zapisu i odczytu, przeniesienie podczas zapisów (0 błędów), awarię i automatyczny powrót (przejęcie 4–11 s, powrót po 5 min 27 s od dołączenia noda do ISR i co 30 s na topik), przełącznik powrotu (ok. 1 s na drugi node), ręczny „Rozłóż prowadzenie” (241 ms i 411 ms) i zgodność logów obu nodów. **Nie sprawdzono:** blokady planu, trzeciego noda, rozkładu po aktualizacji, resetu offsetu i ponowienia DLQ na nodzie nieprowadzącym oraz widoku telefonu okien przeniesienia. Czasy pozostałe (ok. 2 s, 3 s) pochodzą ze stałych w kodzie. Scenariusz: `docs/BUS_ROZKLAD_LIDEROW_DEMO.md`.
- **Fałszywe przejęcia (przeklik, po jednym wystąpieniu).** (1) Po restarcie noda B zdrowy lider A stracił część partycji (7 wpisów `bus.leader.failover reason=lease_expired` przy żywym A; zdarzyło się raz na trzy restarty). (2) Rozkład ręczny `result=ok` został cofnięty przez fałszywe przejęcie, a topik i jego DLQ zostały prowadzone przez różne nody (DLQ powstał w trakcie restartu B). Karta „Rozkład prowadzenia” pokazuje taki topik jako rozdzielony. Bez danych utraconych (hashe logów zgodne). Częstości nie mierzono.
- **Przeniesienia pojedyncze są czyste**, rozkład ręczny nie zawsze (patrz wyżej).
- **Zapytania z dashboardu do drugiego noda** (`targetNodeId`) kończyły się w przekliku błędem `PolicyDenied: no assertion key … known for node …` po ok. 5 s: historia przejęć nie jest scalana, a okno przeniesienia z noda nieprowadzącego otwiera się z notką o braku odpowiedzi lidera. Przyczyna leży po stronie serwera, nie ekranu.
- **Konsument addonu nie jest przekazywany** (uchwyt stanowy): działa na nodzie prowadzącym topik. Po zmianie lidera `next`/`commit` zwraca `NoAvailableTarget`; addon zamyka uchwyt i otwiera nowy na nodzie lidera, a niezatwierdzone rekordy są dostarczane ponownie.
- **Odczyt REST jest przekazywany tylko dla topiku prowadzonego przez jeden node.** Topik rozdzielony po awarii daje 503 do czasu zebrania (reguła powrotu, która musi być włączona).
- **Pauza grupy** blokuje przenoszenie topiku i może zginąć z poprzednim liderem, jeśli nie zdążyła się zreplikować (do 500 ms).
- **ACL i polityki pól na replice** są tak świeże jak jej kopia ledgera.
- **Zapis wielopartycyjny** do kilku liderów może się udać częściowo (`PartialPublish`); „wszystko albo nic” między nodami nie jest wspierane.
- **Opóźnienie zapisu spoza lidera** to dodatkowe przejście przez sieć; pomiaru nie wykonano (plan TB-04, ryzyko 4).
- Znacznik rozkładu po aktualizacji jest lokalny dla noda (patrz wyżej).
- Statystyki tempa (`msgs_in_per_sec`) liczy node, który przyjmuje zapis; po przekazywaniu tempo widać tylko na liderze (z planu; nie sprawdzono, czy UI sumuje je z kilku nodów).
- Dostarczenie wyłącznika `bus.leader_return:<instancja>` na drugi node nie zostało sprawdzone end-to-end.

## Uwagi o komunikatach błędów

- `bus.not_leader` w odpowiedziach na przeniesienie i Apply ma ten sam format co przy zapisie i odczycie (`leader_node_id=… leader_epoch=…`, a bez znanego lidera `epoch=…`); wyjątkiem jest odmowa z powodu epoki, „epoch fenced have=… requested=…”. Rozpoznawaj kod, nie treść.
- `bus.transfer_refused` nie oznacza ogólnej „odmowy lidera”: powstaje tylko przy odmowie warstwy przekazywania (`unknown_instance`, `environment_mismatch`, `handler_failed`). Merytoryczne odmowy lidera (cel w tyle, poza ISR, pauza) mają własne kody.
- Cel poza żywym ISR zwraca `bus.not_a_replica` (komunikat „is not a replica of …”), a nie osobny kod „poza ISR”; powód `target_outside_isr` występuje tylko w blokadach planu.


## Partycja czeka zamiast tracić dane

Węzeł, który wstał i nie dogonił jeszcze lidera, nie staje do elekcji, jeśli któraś replika nie odpowiada: nie ma dowodu, że jego log zawiera wszystko, co lider potwierdził. Partycja zostaje wtedy bez lidera, dopóki nie wróci replika z pełnym logiem. W logu pojawia się jednorazowe ostrzeżenie „not standing — this node has not caught up with a leader since it started and a replica did not answer”. To świadomy wybór: czekanie zamiast cichej utraty zapisów potwierdzonych kodem 200. Gdy wszystkie repliki odpowiadają i żadna nie prowadzi (restart całego zbioru replik), węzeł staje normalnie.

Jeśli mimo to zrzekający się lider odrzuci rekordy ponad znacznikiem zatwierdzonym (rozdwojenie RF=2 po rozłączeniu sieci), zapisuje to w logu na poziomie error i w audycie `bus.leader.tail_discarded` (liczba odrzuconych rekordów, nowy koniec logu). Sprawdź audyt po każdej przerwie sieci między węzłami RF=2. Ten sam wpis zapisuje też follower, któremu nowy lider obciął log, więc szukaj go na obu węzłach.

Węzeł, który po restarcie nie dogonił lidera, staje do elekcji przy RF<=2 tylko wtedy, gdy wszystkie pozostałe repliki odpowiedziały prawdziwym logiem (węzeł, którego szyna jeszcze nie wstała, się nie liczy). Od RF=3 decyduje większość logów.

Węzeł, który po restarcie dostaje wiersz wskazujący go jako lidera i ma rekordy w partycji, nie przyjmuje zapisów, dopóki nie potwierdzi, że żadna replika nie prowadzi nowszej epoki (zwykle do jednej rundy pytań, 250 ms po tym, jak odpowiedzą pozostałe repliki). Publikacje w tym czasie dostają ponawialny `NotLeader`. Jeśli druga replika jest wyłączona, a restartujący się węzeł ma rekordy, zapisy czekają na jej powrót. Czas otwarcia zapisów znajdziesz w logu `replication: leader writes opened after the restart check`.

Znane ograniczenia: (1) ocalały węzeł, który po (ponownym) połączeniu nie dostał żadnej ramki od lidera, nie staje sam do elekcji, więc lider, który padnie w ciągu około 500 ms od powrotu repliki, zostawia partycję bez lidera do swojego powrotu; (2) w trakcie aktualizacji partycja RF=2 może stać, gdy jeden węzeł jest stary, a drugi nowy. Aktualizuj węzły po jednym i przed kolejnym restartem sprawdź, że oba działają na nowej wersji.
