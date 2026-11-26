# Changelog

Najważniejsze zmiany w TentaFlow.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) /
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Przekazywanie zapisu i odczytu do lidera partycji (TB-04)

- REST rekordów działa na każdym węźle, także na tym, który nie prowadzi
  topiku. `POST .../records` przekazuje zapis do lidera partycji, a `GET
  .../records` przekazuje cały odczyt (otwarcie konsumenta, pobranie i commit)
  do węzła prowadzącego topik. Lider autoryzuje oryginalnego wywołującego
  jeszcze raz (własnym autoryzatorem i własną kopią członkostw w organizacji;
  aktora systemowego brokera przyjmuje w przekazanym zapisie tylko dla
  topików `__*`) i niczego nie przekazuje dalej. Każda odmowa na liderze trafia do audytu razem z węzłem, który
  przekazał odczyt (`forwarded_from`). Odczyt jest przekazywany tylko wtedy,
  gdy jeden węzeł prowadzi wszystkie partycje topiku; gdy partycje prowadzi
  kilka węzłów, odpowiedź to 503 `bus_not_leader` z `Retry-After` (stan
  przejściowy: przenosiny liderów obejmują cały topik).
- `max_records` obowiązuje także odczyt przekazany do lidera, a odpowiedź lidera
  jest przycinana tak, by mieściła się w jednej ramce (klucze, nagłówki i narzut
  też się liczą) i w `max_bytes` wywołującego. Lider zatwierdza offset tylko
  rekordów, które faktycznie zwraca; reszta zostaje na następny odczyt.
- Nowe odpowiedzi REST zamiast 500 `internal_error`:
  - 503 z nagłówkiem `Retry-After: 1` i kodem `bus_not_leader`,
    `bus_not_enough_replicas` albo `bus_partition_unavailable`: partycja
    chwilowo nie ma lidera, który przyjąłby wywołanie. Nic nie zostało
    zapisane (ani odczytane), więc ponowienie jest bezpieczne.
  - 504 z kodem `bus_forward_outcome_unknown` albo `bus_ack_timeout` i bez
    `Retry-After`: żądanie wyszło do lidera i nie wróciła odpowiedź albo repliki
    nie potwierdziły zapisu w czasie. Zapis mógł trafić do logu albo nie; przed
    ponowieniem trzeba użyć klucza idempotencji albo tożsamości producenta.
    Odczyt przekazany do lidera jest at-most-once, gdy odpowiedź zginie: lider
    zatwierdza offset przed jej wysłaniem, więc rekordy mogły zostać
    zatwierdzone dla grupy, choć wywołujący ich nie zobaczył (odczyt obsłużony
    na własnym węźle pozostaje at-least-once). Przed kolejnym odczytem warto
    sprawdzić offsety grupy.
  - 502 z kodem `bus_leader_failure` i bez `Retry-After`: lider partycji zawiódł
    po swojej stronie (dysk, silnik). Wcześniej taki błąd byłby przedstawiony
    jako chwilowy brak lidera; szczegóły zostają w logu lidera.
  - Treść 503 i 504 jest stała: nie zawiera identyfikatorów węzłów, epok ani
    powodów. Węzeł, do którego trafiło żądanie, jest tylko w logu. Błędy odczytu
    lidera (np. `OffsetOutOfRange`) docierają do wywołującego jako ten sam
    błąd, jaki dałby odczyt na własnym węźle.
- Zapis obejmujący partycje prowadzone przez kilka węzłów może się częściowo
  udać. Odpowiedź 500 `internal_error` zawiera wtedy w `error.acked` listę
  partycji, które już przyjęły rekordy (`partition`, `base_offset`,
  `accepted`); tych rekordów nie wolno wysyłać ponownie bez klucza
  idempotencji.
- Addon: odmowa „partycja bez lidera” przy `bus_publish_v1` dostaje kod ABI
  `Backpressure` (ponów za chwilę), a nieznany wynik zapisu przekazanego do
  lidera albo brak potwierdzenia replik kod `Timeout` (zapis mógł się udać).
  Konsument addonu jest stanowy i nie jest przekazywany: `bus_consume_open_v1`
  na węźle, który nie prowadzi topiku (i `next`/`commit` na uchwycie, którego
  temat zmienił lidera) zwraca `NoAvailableTarget`, a wpis audytu
  `bus.consume.open` nazywa węzeł prowadzący.
- Blok `bus_publish` podaje w powodzie błędu „partition of topic … has no
  leader at the moment”, „… got no confirmation in time; it may or may not
  have been stored” albo „the node that owns topic … failed while serving the
  write”.
- Kopia kwarantanny rekordu odrzuconego przez schemat trafia do lidera
  partycji `__dlq.*`, także gdy rekord przyszedł od innego węzła albo węzeł nie
  ma repliki tej partycji; wcześniej taki rekord był po cichu odrzucany.
  Kopia jest zapisywana dopiero po przyjęciu właściwego zapisu (lider, ISR,
  limity), więc odmowa `NotLeader` nie zostawia kopii, które ponowienie zapisze
  drugi raz. Zapis kopii przy publikacji przekazanej od innego węzła mieści się
  w czasie, który ten węzeł czeka na odpowiedź. Kopia o nieznanym wyniku (brak
  odpowiedzi lidera DLQ albo brak potwierdzenia replik) nie jest liczona jako
  `schema_dropped`: publikacja kończy się błędem `ForwardOutcomeUnknown` /
  `AckTimeout`, a nic poza ewentualną kopią nie zostało zapisane.
- Ramki `ConsumeForward`/`ConsumeForwardReply` (rodzaje 11 i 12) są ogłaszane
  osobnym bitem `bus_features`; węzeł bez tego bitu dostaje odczyt jako
  odmowę `bus_not_leader`, jak dotąd. Węzeł, od którego nie przyszedł jeszcze
  żaden `Hello` (np. nie ma repliki), jest przed pierwszym żądaniem pytany o
  swoje bity (`LeoQuery` bez partycji, odpowiedź niesie `bus_features`);
  wynik niezerowy jest zapamiętywany. Węzeł, który nie odpowiada na pytanie
  albo nie zna bitów, nie dostaje żądania i wywołujący widzi 503, nie 504.
- Nowy topik ma lidera na najmniej obciążonym węźle, a nie na węźle, który go
  tworzy. Kandydatami są ten węzeł i węzły z tego samego środowiska, które
  ogłaszają przekazywanie zapisu i odczytu; obciążenie to liczba partycji,
  których dany węzeł jest preferowanym liderem (pierwsza pozycja `replicas`,
  nie bieżący lider, więc węzeł, który przejął partycje po awarii, nie dostaje
  przy tym nowych topików). Remis rozstrzyga pierścień węzłów posortowany po
  id, zaczynany od `hash(nazwa topiku) mod liczba węzłów`. Wszystkie partycje
  topiku mają jednego lidera; topiki z RF=1 też są rozkładane. Wybór następuje
  dopiero po sprawdzeniu nazwy i duplikatu, więc odrzucone utworzenie nie pyta
  żadnego węzła.
- Wersje mieszane: dopóki jakikolwiek węzeł z tego samego środowiska nie
  ogłasza przekazywania, albo nie da się tego stwierdzić, lider zostaje na
  węźle tworzącym, jak dotąd. Dotyczy to też węzła chwilowo rozłączonego, o
  którym nie wiadomo, że przekazuje (stary węzeł po powrocie nie mógłby pisać
  do topiku). Węzły, od których nie przyszedł `Hello`, są pytane równolegle,
  łącznie nie dłużej niż 1 s; odpowiedź odmowna jest pamiętana przez 60 s.
  Węzeł, który odpowie, że nie hostuje tej instancji (`instance_unhosted` w
  odpowiedzi `LeoQuery`), nie jest jej członkiem: nie bywa kandydatem i nie
  blokuje rozkładu.
- Pierwszy zapis twórcy do topiku prowadzonego zdalnie nie kończy się już
  błędem: ramki `ProduceForward` i `ConsumeForward` niosą generację topiku i
  liczbę partycji nadawcy. Lider, który nie dostał jeszcze wiersza topiku
  (albo partycji dodanych przez `update_topic`), czeka na nie do 0,5 s, po
  czym odpowiada `NotLeader` bez lidera, a nadawca ponawia z rosnącą przerwą
  (50 ms, podwajaną do 800 ms) w granicach czasu oczekiwania na lidera.
  Zapis nadawcy ze starszą generacją niż ta u lidera (topik w międzyczasie
  usunięto i utworzono ponownie) jest odrzucany i nigdy nie trafia do nowej
  inkarnacji topiku.
- Dodatkowe partycje z `update_topic` dostają lidera i kolejność replik
  istniejących partycji (większościowy lider i preferowany węzeł), bez
  względu na to, który węzeł jest akurat osiągalny; martwego lidera odzyskuje
  zwykła elekcja, a topik nigdy nie ma przez rozkład dwóch liderów.
- Temat DLQ (`__dlq.*`) idzie za swoim źródłem: partycja DLQ trafia tam, gdzie
  jest ta sama partycja tematu źródłowego (lider i repliki); partycje bez
  odpowiednika są układane według partycji tematu źródłowego, nie samych
  partycji DLQ.
- Wpis audytu `bus.topic.create` ma w szczegółach
  `leader=<węzeł> leader_rf=<RF>` przed listą replik.
- Przenosiny prowadzenia obejmują cały topik razem z jego DLQ (`__dlq.*`), nie
  pojedynczą partycję: temat prowadzony przez dwa węzły nie byłby czytany przez
  żaden z nich. `leader_transfer_v1` dla partycji przenosi więc wszystkie
  partycje topiku. Najpierw sprawdza każdą z nich u jej lidera, potem przenosi
  po kolei; gdy któraś zawiedzie, te już przeniesione wracają do poprzedniego
  lidera (wpisy `bus.leader.transfer` z `reason=rollback`), a odpowiedź mówi,
  że żadna partycja nie zmieniła węzła. Powrót przywraca dokładnie pierwotną
  listę replik. Jeśli powrót się nie uda w 15 s, wynik to `failed_partial` z
  listą partycji, które zostały na celu. Cel musi przyjmować przekazywane
  żądania (`bus.node_without_forwarding`, dotyczy też przenosin ręcznych).
- Przenosiny wykonuje bieżący lider partycji. Żądanie, które trafi gdzie
  indziej, jest przekazywane do niego (nowa ramka replikacji, rodzaje 13 i 14,
  oraz bit `bus_features` `1<<2`); prosić może każdy węzeł tej samej instancji
  i środowiska, także taki, który nie trzyma repliki. Odmowa lidera dociera do
  wołającego z własnym kodem (`bus.transfer_refused`), nie jako błąd
  wewnętrzny. Ten kod pochodzi wyłącznie z warstwy przekazywania
  (`unknown_instance`, `environment_mismatch`, `handler_failed`); odmowy
  samego lidera (cel poza ISR, w tyle, wstrzymana grupa, inny lider) mają
  własne kody.
  Lider sprawdza, że cel jest w żywym ISR i nadąża. Potem zamyka zapisy w
  samej partycji (po tym żaden zapis nie ląduje), czeka na dogonienie celu do
  końca logu (najwyżej ok. 2 s) i dopiero wtedy proponuje wiersz. Gdy cel
  zostaje w tyle, wynik to `bus.transfer_target_behind`, zapisy są otwierane
  z powrotem i nic się nie zmienia. Zapis, który trafi w tym czasie na lidera,
  czeka (do 2 s); jeśli przeniesienie trwa dłużej, jest odrzucany jako
  `bus.not_leader` (do ponowienia), nigdy przepuszczany. Zapis, który był już
  za bramką, ląduje w logu przed zamknięciem albo jest odrzucony. Gdy odpowiedź
  o przyjęciu wiersza nie nadejdzie, lider czyta wiersz ponownie; jeśli nadal
  nie wiadomo, gdzie partycja jest prowadzona, wynik to
  `bus.transfer_outcome_unknown`, a stary lider zostaje zatrzymany (nie
  przyjmuje zapisów). Przeniesienie nie gubi potwierdzonych zapisów (test: 300
  zapisów `acks=leader` tuż przed przeniesieniem; opóźniony cel testowany na
  poziomie menedżera).
- Grupa konsumentów wstrzymana (pauza) blokuje przeniesienie topiku:
  `bus.transfer_blocked`. Pauza jest trzymana przez lidera i nie jest
  replikowana, więc nowy lider wznowiłby grupę.
- Przeniesienie na inny węzeł robi z niego węzeł preferowany (`replicas[0]`);
  powrót na węzeł preferowany nie zmienia kolejności replik; failover nigdy jej
  nie zmienia.
- „Rozłóż prowadzenie”: `LeaderBalancePlanRequest` pokazuje plan (które topiki
  zmienią lidera, ile partycji prowadzi każdy węzeł przed i po, co blokuje
  topik), `LeaderBalanceApplyRequest` go wykonuje. Plan przenosi cały topik z
  najbardziej do najmniej obciążonego węzła, dopóki ruch zmniejsza różnicę,
  wybierając ruch zostawiający najmniejszą różnicę (zachłannie, nie dowodliwe
  minimum); najpierw wracają topiki na swój węzeł preferowany. Różnica
  „najwyżej waga najmniejszego topiku” nie zawsze jest osiągalna (topiki 8, 3
  i 1 dają co najwyżej 4); gdy zostaje szersza i nic jej nie tłumaczy, plan
  wskazuje topik z powodem `would_not_improve`. Powrót zablokowany tylko
  chwilowo (cel w tyle, poza ISR, niedostępny) liczy obciążenie na węźle
  preferowanym, więc plan nie przenosi w tym czasie innego topiku na ten węzeł.
  ISR z księgi, które nie jest żywe, nie blokuje ruchu: ocenia go lider.
  „Prowadzona” znaczy: aktualny lider (`leader_node_id`), nie preferowany.
  `per_node` dotyczy organizacji wołającego.
  Wykonanie liczy plan jeszcze raz i przy różnicy odrzuca żądanie kodem
  `bus.plan_changed`. Wymaga roli Admin w organizacji, uprawnienia `bus.admin`
  i prawa administratora do każdego ruszanego topiku (`bus.permission_denied`).
  Topiki idą po kolei, pierwszy błąd zatrzymuje resztę (`not_attempted`).
  Jeden rozkład naraz na węzeł (`bus.rebalance_busy`); rozkłady na różnych
  węzłach nie są wykluczane, ale każdy ruch czyta wiersze od nowa, więc drugi
  dostaje `bus.plan_changed`. Wynik bez odpowiedzi to `failed_partial` z kodem
  `bus.transfer_outcome_unknown`.
  Audyt: `bus.leader.transfer` dla każdej partycji i jeden
  `bus.leader.rebalance` na organizację.
- `BusReplicaNodeWire.forward_capable` i
  `BusPartitionReplicaWire.preferred_leader_node_id` (pola opcjonalne,
  `SCHEMA_VERSION` bez zmian; stary klient je ignoruje, stary węzeł ich nie
  podaje).
- Jednorazowy rozkład po aktualizacji: gdy wszystkie węzły instancji ogłaszają
  nowe funkcje, węzeł o najniższym id raz rozkłada istniejące topiki (audyt
  `bus.leader.rebalance`, aktor `system`, `reason=post_upgrade_spread`).
  Znacznik wykonania to klucz ustawień lokalnego węzła
  `bus.leader_spread:<instancja>` (bez migracji; nie jest replikowany, więc
  węzeł o niższym id, który dołączy później, może uruchomić rozkład ponownie,
  co przy zrównoważonych topikach nic nie przenosi). Topik, który zawiedzie,
  nie zatrzymuje pozostałych. Każdy topik zostawiony na miejscu ma wpis
  `bus.leader.rebalance_skipped` z powodem. Znacznik powstaje tylko wtedy, gdy
  nic nie zostało z powodu przejściowego (cel w tyle lub poza ISR, węzeł
  niedostępny, nieznany wynik); w przeciwnym razie rozkład jest ponawiany z
  przerwą 1, 2, 4 ... min (najwyżej 30 min). Powód trwały (wstrzymana grupa,
  węzeł bez przekazywania) znacznika nie wstrzymuje; do takich topików służy
  „Rozłóż prowadzenie”. Instalacja jednowęzłowa i węzeł, który rozkładu nie
  wykonuje, sprawdzają stan rzadko (co 10 min).

- Automatyczny powrót topiku do węzła preferowanego: po awarii topik prowadzi
  inny węzeł niż `replicas[0]`. Gdy węzeł preferowany wróci, lider topiku
  oddaje mu go sam, bez koordynatora. Warunki, wszystkie naraz:
  - węzeł preferowany jest w ŻYWYM ISR tego lidera i nie jest dalej niż 100
    rekordów za końcem logu (to samo, co „bez lagu”, liczone z
    `replica_progress`) nieprzerwanie od co najmniej 5 minut, dla każdej
    partycji topiku i jego `__dlq.*` (jednostką jest cały topik z DLQ).
    Stan ISR jest próbkowany co 5 s, decyzje zapadają co 30 s, więc
    wypadnięcie z ISR zauważone przez próbkę zaczyna odliczanie od nowa
    (wypadnięcie krótsze niż 5 s może umknąć). Odliczanie dotyczy epoki
    lidera i generacji topiku: utrata i odzyskanie prowadzenia albo
    ponowne utworzenie topiku zaczyna je od zera, tak samo restart węzła;
  - węzeł preferowany przyjmuje przekazywane żądania;
  - nic nie blokuje przeniesienia: pauza grupy konsumentów (jak przy
    „Przenieś prowadzenie”, `bus.transfer_blocked`) zostawia topik na miejscu.
  Powrót używa tego samego przeniesienia całego topiku co „Rozłóż prowadzenie”
  (sprawdzenie każdej partycji u lidera, dogonienie celu, cofnięcie przy
  błędzie) i nie zmienia `replicas[0]`. Idzie przez ten sam zamek wykonawcy co
  ręczne przeniesienie, ręczny rozkład i rozkład po aktualizacji: gdy któryś
  trwa, ten krok jest pomijany (bez błędu) i powtarzany przy następnej próbce.
  Najwyżej jeden topik na 30 s na węzeł. Powrót, który się nie udał i niczego
  nie przeniósł, zaczyna odliczanie topiku od nowa. Audyt: wiersze
  `bus.leader.transfer` z `reason=preferred_restore` oraz jeden
  `bus.leader.rebalance` (aktor `system`, `reason=preferred_restore`)
  na powrót.
- Topik rozdzielony po awarii (partycje wybrały liderów po kolei, więc
  prowadzą go różne węzły, a taki topik nie da się czytać) zbiera jeden węzeł:
  ten o najniższym id wśród liderów jego partycji. Nie czeka 5 minut. Jeśli
  liderzy mówią, że węzeł preferowany może go teraz przejąć (sprawdzenie
  na sucho każdej partycji), idzie do niego (`reason=preferred_restore`),
  w przeciwnym razie na węzeł, który prowadzi największą część jednostki
  (`reason=split_gather`, przy remisie najniższe id). Takie zebranie NIE
  przepisuje `replicas[0]`: naprawia rozdzielenie, nie wybiera lidera,
  a powrót do węzła preferowanego następuje potem zwykłą regułą. Przeniesienie,
  które zostawiło topik rozdzielony (`failed_partial`), jest ponawiane
  przy następnym przeglądzie jako zebranie; nieudane zebranie czeka dwa
  przeglądy. Przełącznik automatycznego powrotu wyłącza także to zbieranie,
  nie tylko powrót po 5 minutach.
- Przeniesienia nie mogą się już minąć: `TransferForward` niesie
  oczekiwanego lidera i jego epokę (pole opcjonalne; starszy węzeł go nie
  wysyła i nie sprawdza), a lider odmawia (`bus.not_leader`), gdy wiersz
  partycji jest już inny niż ten, z którego liczono przeniesienie. To samo
  dotyczy przeniesień lokalnych. Cofnięcie po błędzie oddaje partycję tylko
  wtedy, gdy nadal prowadzi ją ten, kto ją wziął; wynik, który zostawił
  część partycji gdzie indziej, jest raportowany jako `failed_partial`.
  Ręczne „Przenieś prowadzenie” bierze ten sam zamek co plan i zwraca
  `Busy`, gdy inny przebieg trwa. Zamek jest per węzeł; wyścig między
  węzłami (ręczne przeniesienie na jednym, powrót na drugim) łapie sprawdzenie
  epoki u lidera partycji, a nie zamek.
- Wyłącznik powrotu jest per instancja i domyślnie WŁĄCZONY: klucz ustawień
  `bus.leader_return:<instancja>` o wartości `on`/`off` (także `true`/`false`,
  `1`/`0`), ustawiany zwykłym `SettingsUpdateRequest` (rola Admin). Obsługa
  odrzuca instancję, której ten węzeł nie hostuje, wartość inną niż te
  i ustawienie oznaczone jako sekret. Klucz jest replikowany przez Sync
  Ledger do wszystkich węzłów (lista przedrostków `SHARED_SETTING_KEY_PREFIXES`,
  bez migracji): wyłączenie na jednym węźle wyłącza powroty i zbieranie
  na wszystkich. Węzeł ze starszą wersją nie zna klucza i zapisuje jego
  operację jako konflikt bez skutków (działa domyślnie, czyli włączony).
  Wartość nieczytelna jest traktowana jak `off` (ostrzeżenie najwyżej co
  5 minut). Wyłączenie zeruje odliczanie.
- Plan „Rozłóż prowadzenie” zakłada, że powrót zablokowany tylko chwilowo (cel
  w tyle, poza ISR, niedostępny) nastąpi sam, czyli dokładnie to, co robi
  powyższa reguła: topik prowadzony z jednego węzła liczy się przy węźle
  preferowanym, a rozdzielony przy węźle, który prowadzi jego większość
  (tam zbierze go reguła). Węzłem preferowanym jednostki jest ten, który
  wymieniają jako pierwszy wszystkie jej wiersze; przy braku zgody nie ma
  go ani plan, ani reguła. Powód trwały (pauza, węzeł bez przekazywania)
  zostawia topik tam, gdzie jest, w planie i w regule. Przy wyłączonym
  wyłączniku takie topiki są liczone tam, gdzie stoją.
- Ekrany tylko do odczytu działają na każdej replice, nie tylko na liderze:
  przeglądarka wiadomości (`peek`), lista DLQ, lag grup (`group_lag`, lista i
  szczegóły grupy, szczegóły topiku) i próbkowanie lagu. Replika czyta własny
  log, ale tylko do `min(hw, committed_offset)`: przy `acks=leader` (i przy
  RF=2) hw lidera wyprzedza większość, a replika może zostać obcięta z
  powrotem do `committed_offset`, więc rekord za tą granicą mógłby zniknąć po
  przełączeniu. Odczyt nie przekracza granicy także wtedy, gdy leży ona
  wewnątrz paczki (zwracane są całe paczki, więc obcięcie jest co do rekordu),
  a strona zwraca tę granicę jako `high_watermark`. Odczyt, który przeciął
  się z obcięciem logu repliki, jest powtarzany raz, a przy drugim obcięciu
  odmawia (`bus.partition_unavailable`) zamiast zwrócić stronę z mieszanych
  stanów. Follower bez znanego lidera (zdegradowany były lider) niczego nie
  udostępnia. Widok z repliki może pozostawać w tyle za liderem, a odpowiedź to
  zaznacza polem `from_replica` (partycje przeglądarki i DLQ, podsumowanie i
  szczegóły grupy, podsumowanie lagu topiku, serie historii lagu; pola
  opcjonalne, `SCHEMA_VERSION` bez zmian; brak pola to `false`). Replika
  pokazuje ACL i reguły ukrywania pól tak świeże, jak jej kopia ledgera.
- Przeglądarka lidera i odbiorcy (`fetch`) też nie wychodzą poza hw: wcześniej
  przy RF>1 dostawali całe paczki, także z rekordami, których repliki jeszcze
  nie mają i które mogły zostać obcięte, a odbiorca mógł je skomitować. Teraz
  lider tnie paczkę na hw co do rekordu.
- Lag na replice to jej granica odczytu minus offset grupy, który do niej
  doszedł. Offsety grup są replikowane (`ReplOffsets`, łączone w okno
  `offsets_coalesce_interval`) i lider wysyła ich pełny stan dla partycji przy
  każdym otwarciu strumienia oraz po utracie powiadomień, więc replika, która
  była nieobecna albo nigdy nie widziała ostatniego commitu bezczynnej grupy,
  nadrabia je po powrocie, a promowana replika nie dziedziczy starego offsetu.
  Lag z repliki różni się od lagu lidera o to, co zatwierdzono w ostatnim
  oknie i o rekordy między jej granicą odczytu a hw lidera. Replika stosuje
  offsety tylko do przodu: po cofnięciu offsetu grupy (reset w tył) pokazuje
  dotychczasowy, niższy lag, aż grupa zatwierdzi dalej.
- Wiersze grup (`bus_groups`: tryb commitu, pauza) są replikowane razem z
  offsetami (`ReplOffsets.groups`, pole opcjonalne): lider wysyła je przy
  zmianie i w pełnym stanie przy otwarciu strumienia, a replika zapisuje je
  według (epoka lidera, `updated_at_ms`), nowsze wygrywa, i unieważnia
  podręczny stan grupy. Lista i szczegóły grupy na replice pokazują więc grupy
  i ich pauzę, a węzeł przejmujący partycję zachowuje pauzę; blokady K5/K6
  widzą ją tak samo (topik z wstrzymaną grupą nie jest przenoszony). Pauza,
  która nie zdążyła dojść do repliki (jedno okno łączenia), ginie wraz z
  poprzednim liderem. Metryki konsumenckiego lagu liczą tylko partycje
  prowadzone przez dany węzeł, żeby replika nie dublowała lagu w sumie po
  węzłach. Węzeł, który nie trzyma żadnej repliki partycji, nadal odpowiada
  `bus.not_leader` bez nazwy lidera (także dla listy DLQ, gdy nic na nią nie
  czeka).
- Wszystko, co zmienia stan, zostaje na liderze: ponowienie i odrzucenie
  rekordu DLQ, „Ponów wszystkie”, pauza i wznowienie grupy, każdy reset offsetu.
  Na replice każda z tych operacji kończy się odmową `bus.not_leader` z
  `leader_node_id=<id> leader_epoch=<n>` (kod Conflict) i niczego nie zmienia;
  „Ponów wszystkie” odmawia zanim cokolwiek przeczyta, zamiast zgłaszać każdy
  rekord jako nieudane ponowienie. Reset offsetu sprawdza lidera przed
  wierszem grupy. Komunikat nie drukuje już `Some("...")`/`None`, a pusty
  identyfikator lidera to brak lidera: `bus.not_leader: epoch=<n>`, bez nazwy,
  którą UI mógłby wziąć za węzeł.
- Stan odbiorcy flow (`BusConsumerStatus`) przy `reason = not_leader` podaje
  węzeł prowadzący topik: `leader_node_id` i `leader_node_name` (nazwa
  wyświetlana, gdy ten węzeł ją zna), ustawiane razem ze statusem. Gdy
  partycje topiku prowadzą różne węzły, jest to węzeł prowadzący ich
  najwięcej (przy remisie o mniejszym identyfikatorze); w trakcie wyborów
  pola są puste. Węzeł i nazwę dostaje tylko sesja administratora; inne sesje
  widzą sam `reason = not_leader`. Nazwy są rozwiązywane raz na żądanie.
  Pola opcjonalne, `SCHEMA_VERSION` bez zmian. Tekst w Flow Builderze
  (`flows_config.consumer_reason_not_leader`) dopisze K8.

- Panel TentaBus pokazuje rozkład prowadzenia. W „Kopie i nody” jest karta
  „Rozkład prowadzenia” (partycje i topiki na node, liczone razem z `__dlq`
  jak w planerze, odchylenie, topiki poza preferowanym nodem), przycisk
  „Rozłóż prowadzenie” tylko dla admina (nieaktywny z powodem, także gdy
  któryś node ma starszą wersję) i przełącznik automatycznego powrotu
  (tylko administrator platformy). Okno rozkładu pokazuje podgląd, blokady,
  postęp i wynik z częściowym sukcesem; gdy odpowiedź nie przyjdzie w czasie,
  okno nie ogłasza porażki, tylko sprawdza stan co 10 s. „Przejęcia
  prowadzenia” rozróżniają rozkład po aktualizacji, rozłożenie, powrót do
  preferowanego nodu, scalenie topiku i cofnięcie (wiersz audytu zachowuje
  powód zapisany przez system). Strona topiku ma znacznik preferowanego nodu,
  a „Przenieś prowadzenie” przenosi cały topik i bierze zaległość kopii od
  nodu prowadzącego, także gdy panel działa na innym nodzie. Stan odbiorcy
  mówi, który node prowadzi topik, a widoki z repliki są oznaczone.
- `BusReplicaNodeWire.forwarding` (opcjonalne: `capable`, `lacking`,
  `unknown`): node niedostępny albo jeszcze nie sprawdzony nie jest już
  pokazywany jako starsza wersja. `forward_capable` zostaje dla starych
  klientów.
- Poprawki po przeklikaniu dwóch węzłów (TB-04 K10):
  - Lider sam w ISR przy RF=2 i `acks=leader` (drugi węzeł nie żyje) udostępnia
    teraz konsumentom to, co przyjął: znacznik `hw` rośnie razem z lokalnymi
    zapisami, a nie tylko po potwierdzeniach i zmianach ISR. Wcześniej zapis
    dostawał 200, a nowa grupa czytała tylko rekordy sprzed awarii. Semantyka
    RF>=3 oraz `acks=quorum|all` bez zmian.
  - Ręczne przeniesienie topiku nie ściga się już z elekcją. Węzeł, którego
    księga ma już nowszy wiersz (przeniesienie, które sondowanie jeszcze nie
    zastosowało), nie staje do elekcji na starym widoku, a dotychczasowy lider
    odpowiada na `LeoQuery` epoką przeniesienia, ale tylko wierszem, który
    rzeczywiście pokazuje przeniesienie (cel na czele, epoka nie niższa), a
    kandydat tuż przed `propose` czyta księgę jeszcze raz i rezygnuje, gdy
    trzyma ona już claim na jego epoce lub wyżej, wystawiony przez inną
    operację (poprzednia próba tego węzła nie przeszkadza). Wiersz, który
    wygrał termin z późniejszym czasem, jest przejmowany na podstawie
    tożsamości operacji, nie kolejności replik. Skutek wcześniej: fałszywy
    wpis `bus.leader.failover reason=lease_expired` i nadpisana kolejność
    replik (część partycji nadal preferowała stary węzeł).
  - Węzeł preferowany topiku (z jego DLQ) to ten, który większość partycji
    wymienia jako pierwszy; remis rozstrzyga węzeł prowadzący najwięcej
    partycji, potem najniższe id. Dotyczy planera i automatycznego powrotu,
    więc topik, którego partycje się różnią, nie jest już pomijany na stałe.
    Topik prowadzony w całości przez swój węzeł preferowany, w którym część
    partycji nadal wskazuje inny węzeł, dostaje przepisaną kolejność replik
    tych partycji. Zmienia się tylko kolejność: wiersz jest przepisywany,
    gdy ma ten sam zestaw węzłów i tę samą epokę, z których liczono (węzeł
    usunięty w międzyczasie nie wraca, a wiersz, który się zmienił, jest
    pomijany po cichu). Każda taka zmiana to wpis audytu `bus.leader.align`
    (aktor systemowy, `reason=preferred_align`), bo zmienia się zapisany węzeł
    preferowany. Wynik częściowy jest zgłaszany jako `partial`.
  - Zebranie rozdzielonego topiku następuje od razu na węźle, który prowadzi
    go najwięcej (najmniej przeniesień, bez zmiany węzła preferowanego, czyli
    `keep_order`). Gdy wszystkie partycje wskazują jeden węzeł preferowany i
    każdy lider potwierdzi przeniesienie, zebranie idzie na ten węzeł, jak
    dotąd. Gdy partycje się różnią, nie ma węzła preferowanego do zebrania:
    powrót do węzła z większością głosów idzie zwykłą ścieżką z zegarem
    stabilności, bo ten węzeł mógł dopiero wstać.
  - Węzeł, który nie prowadzi partycji, pokazuje żywe ISR lidera. Heartbeat
    niesie nowe pole `isr` (dołączone na końcu ramki; starszy węzeł je
    ignoruje), a zmiana ISR trafia do followera w ciągu jednego heartbeatu.
    Wcześniej follower pokazywał ISR z wiersza księgi sprzed ostatniej zmiany
    rozmieszczenia.
  - Nazwy węzłów w widoku replik (`BusReplicaNodeWire.label`) to nazwa, którą
    aplikacja już pokazuje (`node_display_name`: hostname maszyny dla węzła
    lokalnego, hostname z mesh dla pozostałych, potem nazwa z rejestru sync).
    Gdy nazwy brak albo dwa węzły na jednej maszynie ją dzielą, etykieta jest
    pusta, a UI pokazuje krótkie id. Wcześniej etykietą było 64-znakowe id.
  - Odpowiedź flow, w którym padł węzeł (np. `bus_publish` odmówiony kluczowi
    użytkownika), jest błędem, a nie pustym 200, także gdy flow rozwiązuje
    wykonawca modeli, nie tylko `/v1/chat/completions`. Klient dostaje
    ogólny komunikat ze stabilnym kodem `flow.failed` (przekroczony czas
    to 504), a tekst węzła trafia do logu i audytu zgodności. Model dostępu
    bez zmian; nie ma osobnego błędu 403 dla odmowy węzła.
  - Lider startujący sam nad ogonem logu, którego żadna replika nie
    potwierdziła, udostępnia go od razu (bez nowego zapisu), a
    wywołanie `await_acks` na zamkniętym leaderze nie przelicza `hw`.
  - Przeniesienie nie przypina wpisu starego lidera do wiersza, który
    przegrał termin z innym claimem.
  - Lista replik: nazwy węzłów czytane są poza wątkiem obsługi (SQLite), a
    węzeł z własną, unikalną nazwą hosta nie traci jej, gdy nazwa
    zastępcza innego węzła jest taka sama.
  - `bus.not_leader` z przeniesienia i `reassign` podaje
    `leader_node_id=<id> leader_epoch=<n>` jak odmowa zapisu i odczytu (gdy
    lider nieznany: `epoch=<n>`). Ramka `TransferForwardReply` niesie epokę
    jako dodatkową liczbę kodu `bus.not_leader`. Wpis audytu `bus.topic.create`
    wymienia repliki w kolejności rozmieszczenia, lider pierwszy. Powód
    `would_not_improve` dopisano do opisu `BusLeaderBlockerWire.reason`.
  - Skoki epok lidera o 63-64 są zamierzone: każdy węzeł ma stały slot modulo
    64 (`election::EPOCH_SLOTS`) i podnosi epokę tylko w swoim slocie, żeby
    dwie niezależne elekcje nigdy nie wybrały tej samej liczby.
  - Węzeł, który wstał po restarcie albo któremu padło łącze, nie odbiera już
    prowadzenia żywemu liderowi. Przy RF≤2 jedna replika może działać sama
    (`availability_quorum`), więc cisza na łączu, które dopiero się podnosi
    (połączenie QUIC przez relay trzeba odbudować, trwa to sekundy, a budżet
    `LeoQuery` to 300 ms), wyglądała jak śmierć lidera i kończyła się fałszywym
    `lease_expired`. Teraz: (1) w pre-vote przy RF≤2 kandydat, któremu nikt
    nie odpowiedział, czeka jeszcze 3 s (łącznie 300 ms + 3 s od pierwszego
    dialu); próba, która kończy się od razu odmową połączenia (restartujący
    się peer), jest ponawiana co 250 ms do końca tego czasu, zamiast uznać
    ciszę za śmierć. Partycje pytające ten sam węzeł w tym samym czasie
    dzielą jeden dial na rundę (jedna próba na peera, jej wynik, także
    porażka, dostają wszystkie). Runda głosowania nie czeka. Awaria lidera
    jest więc przejmowana po ok. 6–12 s (dzierżawa 3 s, pre-vote,
    głosowanie), a nie po 3–9 s; (2) węzeł, który od startu procesu ani razu
    nie przyjął `Hello` od lidera i któremu nikt nie odpowiedział, nie staje
    do elekcji przez 60 s od startu procesu (nie od przebudowania managera);
    do 120 s, dopóki lider ma dowód życia: mesh pokazuje go jako połączonego
    i świeżo słyszanego (sygnał w ciągu 15 s) albo lider odpowiedział jako
    lider w ciągu 15 s. Bez takiego dowodu karencja kończy się po 60 s. Węzeł,
    który przestał prowadzić partycję, zaczyna od zera: nie liczy się jako
    „słyszał lidera”. Elekcję przerywa tylko lider, którego strumień do tego
    węzła żyje w chwili decyzji, sprawdzony pod tym samym zamkiem co
    rezerwacja obietnicy terminu; lider restartujący się w pętli, którego
    strumień już się skończył, nie wstrzymuje przejęcia na zawsze.
  - **Utrata potwierdzonych zapisów po restarcie repliki (krytyczne).** Węzeł,
    który wstał po przerwie dłuższej niż kilkadziesiąt sekund, przy RF=2
    stawał do elekcji po upływie karencji (120 s) nad żywym liderem, wygrywał
    bez niczyjej zgody (jedna replika działa sama) i wymuszał na liderze
    obcięcie logu do swojej, krótszej kopii. Zapisy przyjęte przez lidera w
    czasie jego nieobecności (potwierdzone kodem 200) znikały, a skróty logów
    na obu węzłach były równe. Bramka „jest w ISR” czytała zapisany wiersz
    przydziału, a skurczenie ISR przez lidera jest lokalne i do wiersza nie
    trafia, więc wiersz nadal wymieniał węzeł, który nie żył. Teraz węzeł
    staje do elekcji tylko wtedy, gdy od startu procesu dogonił lidera (jego
    log doszedł do znacznika high watermark, który lider zgłosił, albo sam
    prowadził partycję), a ostatni zbiór ISR zgłoszony przez lidera, jeśli
    taki był, wymienia ten węzeł. Zapis do wiersza nie jest możliwy w tej
    postaci: ta sama epoka i ten sam lider nie są przyjmowane przez
    materializer, a przy RF=2 większość to oba węzły, więc zmiana ISR nie
    mogłaby się potwierdzić właśnie wtedy, gdy replika nie żyje. Skutek:
    jeśli lider będący w ISR padnie, a drugi węzeł jeszcze nie nadrobił,
    partycja zostaje bez lidera do powrotu repliki, która ma pełny log (świadomy
    wybór zamiast cichej utraty danych). Obcięcie logu zrzekającego się
    lidera, które odrzuca zapisy powyżej znacznika zatwierdzonego, jest
    teraz zapisywane na poziomie error i w audycie `bus.leader.tail_discarded`.
    Nie ma odmowy po stronie starego lidera: `Hello` nie niesie pozycji logu
    nowego lidera, a leczenie rozdwojenia RF=2 opiera się na obcięciu.
  - **Audyt `bus.leader.tail_discarded`.** Gdy zrzekający się lider obcina log do
    znacznika zatwierdzonego i traci przy tym rekordy, które miał ponad nim,
    dostaje to wpis na poziomie error i wiersz audytu z liczbą odrzuconych
    rekordów i nowym końcem logu. Rekordy, których nie ma na żadnej innej
    replice, są wtedy stracone; wpis jest sposobem, by to zobaczyć. Ten sam
    zapis nie rozstrzyga, czy któryś z nich był potwierdzony kodem 200.
  - **Kompromis bramki „dogonił lidera”.** Węzeł, który nie dogonił lidera od
    startu procesu i nie może się skontaktować z którąś repliką, nie staje do
    elekcji: partycja czeka bez lidera na replikę z pełnym logiem, zamiast
    zgubić potwierdzone zapisy. Dotyczy to też RF=2, w którym drugi węzeł
    zniknął na dłużej niż do pierwszego kontaktu. Restart wszystkich replik
    naraz nie blokuje: gdy odpowiedzą wszystkie, a żadna nie prowadzi, węzeł staje.
  - **Bramka „dogonił lidera” – poprawki po przeglądzie.** Odpowiedź peera,
    który nie ma jeszcze zarejestrowanej instancji busa, wpisu w rejestrze albo
    jest po zamknięciu managera, ma zera wyglądające jak pusty log. Nie liczy
    się już jako odpowiedź: `ReplLeoReply` niesie pole `log_known`, a starszy
    peer bez niego też nie liczy się węzłowi, który nie dogonił. W głosowaniu
    odpowiedź musi być związana z tą epoką. To samo dotyczy okresu łaski po
    restarcie. Flaga „dogonił” jest przypisana do epoki lidera, w której
    została zdobyta, nie jest ustawiana przez odtworzenie starego wiersza po
    starcie i znika z wyjściem partycji z rejestru. Cel dogonienia jest
    zamrożony przy pierwszej ramce strumienia, więc węzeł dogania także pod
    stałym obciążeniem zapisami. Reguła „odpowiedziały wszystkie repliki”
    dotyczy tylko RF<=2; od RF=3 decyduje większość logów związanych z epoką.
    Ponowna inicjalizacja replikacji zeruje flagi, co jest akceptowalne.
  - **Zapisy lidera po restarcie są wstrzymane do potwierdzenia.** Węzeł, który
    po starcie procesu dostaje wiersz wskazujący go jako lidera (to może być
    stary wiersz odtworzony z księgi) i ma w partycji rekordy, nie przyjmuje
    zapisów, dopóki nie wie, że nikt nie prowadzi partycji w nowszej epoce:
    albo follower przyjął jego `Hello` w tej epoce, albo większość replik
    (liczył się on sam) odpowiedziała własnym logiem i żadna nie obiecała ani
    nie zna wyższej epoki. Do tego czasu publikacja kończy się ponawialnym
    `NotLeader`. Bez tego lider po restarcie przy RF=2 potwierdzał zapisy
    (acks=leader), które potem obcinał nowy lider. Pełny restart klastra nie
    blokuje: gdy wszystkie repliki odpowiedzą i nikt nie jest dalej, zapisy
    otwierają się przy pierwszej rundzie pytań (250 ms). Partycja bez rekordów
    jest prowadzona od razu. Czas od restartu do otwarcia zapisów zapisuje log
    `replication: leader writes opened after the restart check`.
  - **Przekazanie lidera czeka, aż follower zastosuje wiersze grup.** Pauza i
    wznowienie grupy konsumentów są wysyłane przed przekazaniem partycji
    (nie czekają na tick koalescencji), a follower potwierdza w `Ack` pole
    `groups_applied`, które zastosował (pole opcjonalne: starszy follower go nie
    ma, wtedy liczy się wysłanie). Czekanie jest ograniczone do 2 s; ostatnie
    wysłanie następuje tuż przed zatrzymaniem starego lidera. Zmiana wiersza
    grupy po tym ostatnim wysłaniu, a przed zatrzymaniem, jest replikowana jak
    każdy inny wiersz, bez gwarancji.
  - **Ograniczenia, o których trzeba wiedzieć.** Ocalały węzeł, który po
    (ponownym) połączeniu nie dostał żadnej ramki od lidera, nie staje do
    elekcji sam: lider, który umrze w ciągu około jednego heartbeatu (500 ms)
    od powrotu repliki, zostawia partycję bez lidera do jego powrotu. Podczas
    aktualizacji partycja RF=2 może stać, gdy jeden węzeł ma starą wersję, a
    drugi nową (pole `log_known`): aktualizować węzły po jednym i przed
    kolejnym restartem sprawdzić, że oba są na nowej wersji.
  - **Audyt przekazanych żądań wg listy.** Ograniczanie wpisów dotyczy tylko
    żądań z jawnej listy tylko-do-odczytu (`READ_ONLY_VARIANTS`); wszystko
    inne, w tym `ProjectStudioCasesBulkStatusRequest`, jest audytowane za
    każdym razem po obu stronach. Zastępuje to heurystykę po nazwie.
  - **Kroki lidera na jednym ticku.** Zrzeczenie się nieaktualnego przywództwa
    (blokujące ogrodzenie i obcięcie logu) działa w osobnym zadaniu na
    partycję, więc nie wstrzymuje elekcji ani sprawdzenia kworum.
  - **`bus.leader.tail_discarded` także po stronie followera.** Follower, któremu
    nowy lider obcina log (`Truncate`), zapisuje liczbę odrzuconych rekordów.
  - **Utrata potwierdzonych zapisów przy RF=2, gdy padł tylko strumień
    replikacji (krytyczne).** Przyczyna z przebiegu e2e (restart 7): połączenie
    mesh i przekazywanie zapisów działały, a strumień replikacji był zerwany.
    Follower uznawał lidera za martwego (dzierżawa strumienia wygasła) i stawał
    do elekcji, a lider, wyrzuciwszy follower z ISR, dalej potwierdzał zapisy
    sam. Po zwycięstwie followera te zapisy znikały (66 i 9 rekordów).
    Zasada przy RF=2: węzeł działa sam tylko wtedy, gdy drugi jest
    potwierdzony poza grą, albo nieosiągalny przez `T_alone` (5 s) bez
    przerwy: bez połączenia mesh albo z połączeniem, które od 2 s nie niesie
    heartbeatu i nie odpowiada na sondę (zapytanie kanałem zapytań, nie tym
    połączeniem). Licznik biegnie od ostatniego znaku życia, a każde
    ponowne połączenie, heartbeat lub odpowiedź na sondę go zeruje. Krótkie
    migotanie nie rusza żadnej ze stron, a zgubiony sam heartbeat przy
    odpowiadającym węźle też nie.
    - **Lider** zawężony do siebie przestaje potwierdzać `acks=all` (503
      `bus_not_enough_replicas`, ponawialny) i nie przesuwa `hw`, dopóki
      (a) follower nie potwierdzi komunikatu `IsrShrink` (lider wysyła go
      kanałem zapytań `LeoQuery`, osobnym od strumienia; follower zdejmuje
      wtedy swoją flagę „doścignięty” dla tej epoki i nie może na niej stanąć),
      albo (b) follower nie będzie nieosiągalny przez `T_alone`. Zapis przyjęty
      przed zawężeniem i czekający na ack follower nie zostaje potwierdzony
      przez samo zawężenie: kończy się `bus_not_enough_replicas`. Przy
      `acks=leader` obowiązuje dotychczasowa umowa (potwierdzenie przy
      dopisaniu do logu lidera), ale `hw` stoi, aż zajdzie (a) lub (b); takie
      niereplikowane zapisy mogą zginąć przy prawdziwym przejęciu. To jest
      świadoma decyzja: `acks=leader` działa jak `acks=1` w Kafce
      (potwierdzenie przy dopisaniu u lidera, niezatwierdzony ogon może zostać
      obcięty przy zmianie lidera); trwałość daje `acks=all`. Każde takie
      obcięcie zapisuje `bus.leader.tail_discarded`.
    - Nowy lider RF=2 (po elekcji, przeniesieniu, odbudowie i restarcie) startuje
      bez zgody na zatwierdzanie samemu: zgodę daje potwierdzenie `IsrShrink`,
      `T_alone` nieosiągalności albo rejestracja followera. Potwierdzenie jest
      przypisane do zawężenia, na które odpowiada (numer zawężenia w
      komunikacie): odpowiedź na wcześniejsze zawężenie nie zdejmuje
      blokady po późniejszym. Follower, który potwierdził zawężenie, odzyskuje
      flagę „doścignięty” dopiero po heartbeacie lidera wymieniającym go w ISR i
      po dojściu do pozycji lidera z tego heartbeatu. Heartbeat niesie numer
      zawężenia lidera (`shrink_seq`, pole dopisane, domyślnie 0) i liczy się
      tylko taki, którego numer nie jest niższy niż numer zawężenia zdejmującego
      flagę, więc heartbeat wysłany przed zawężeniem, a przetworzony po
      komunikacie, flagi nie przywraca. Numery zawężeń startują od wartości z zegara
      (`licznik uruchomień << 16`, gdzie licznik to zegar w ms, ale nigdy nie
      mniej niż zapisana w pliku `leader-incarnation` w katalogu partycji wartość plus 1,
      z fsync pliku i katalogu, na wątku blokującym obok stemplowania epoki; do czasu
      jego zakończenia numeracja idzie z samego zegara, a uchwyt zatrzymany albo
      zastąpiony nie pobiera licznika; błąd zapisu jest logowany i numeracja
      wraca do zegara) przy każdym
      uruchomieniu lidera, więc po restarcie lidera w tej samej epoce
      wszystkie numery są wyższe niż wcześniejsze, także gdy zegar cofnięto:
      komunikat z nowego uruchomienia zastępuje zdjęcie flagi z celem od zera, a
      heartbeat z wyższym numerem liczy się z własnym celem. Komunikat z
      wcześniejszej epoki nie zastępuje nowszego zdjęcia. Węzeł, który przestał
      być repliką partycji, zapomina też flagę. Seria odmów kończy się także
      ramką strumienia od węzła (również gdy strumienia już nie ma), a sondy
      idą tylko po partycjach, w których decyzja czeka na ten węzeł. Lider
      sprzed tej zmiany (716671606) nie wysyła `IsrShrink`, więc nic nie
      zdejmuje flagi follower. Flagę zdejmuje i przyznaje się pod jednym
      zamkiem, a `note_led` po przeniesieniu nie unieważnia zdjęcia z tej samej
      epoki. Follower nie potwierdza zawężenia,
      gdy sam prowadzi elekcję albo obiecał nowszą kadencję. Węzeł bez wpisu o
      partycji, bez tej instancji albo po zamknięciu menedżera potwierdza
      zawężenie od razu (nie ma flagi do zdjęcia). Odpowiedź na sondę „partycja
      nie jest tu obsługiwana” (albo, dla lidera, „nie prowadzę jej w tej
      epoce”) nie jest znakiem życia: heartbeat mesh przestaje wtedy
      ręczyć za węzeł, gdy takie odpowiedzi o tej partycji trwają
      nieprzerwanie przez `T_alone`, są odnawiane (ostatnia nie starsza niż
      4 s) i zaczęły się po ostatniej ramce strumienia od tego węzła.
      Odpowiedź „obsługuję”, połączenie albo rozłączenie mesh z węzłem oraz
      ramka strumienia kończą serię. Każda partycja RF=2 jest sondowana osobno.
      Brak jakiejkolwiek odpowiedzi niczego nie przesądza (seria wygasa).
    - **Follower** staje do elekcji sam tylko z ważną flagą „doścignięty”, nie
      cofniętą przez `IsrShrink`, i gdy lider jest nieosiągalny przez `T_alone`.
      Odpowiedź `leading: true` z bieżącej epoki też blokuje. Dawne 60 s
      „poddania się” nie ma: lider osiągalny w mesh zatrzymuje partycję na
      tak długo, jak jest osiągalny. Wstrzymanie dotyczy tylko RF≤2; od RF=3
      decyduje większość jak dotąd.
    - Lider, który odmawia dłużej niż 30 s, choć drugi węzeł jest w mesh, a strumień
      nie wraca, zapisuje błąd w logu i wiersz audytu
      `bus.leader.alone_refused`; zasada bezpieczeństwa się nie łamie.
    - Ubity proces zostawia połączenie mesh stojące do limitu bezczynności
      transportu (30–40 s), więc o jego śmierci mówi cisza heartbeatu i
      sondy; przejęcie trwa `T_alone` plus elekcja. Zmierzone w
      `process_three_node_bus_failover` (osiem partycji, bez RUST_LOG):
      8,8 s, 8,9 s i 9,2 s od ubicia lidera do przejęcia wszystkich partycji.
    Znane ograniczenie (decyzja: dostępność ponad spójność): **prawdziwa
    symetryczna awaria sieci między dwoma węzłami RF=2, trwająca dłużej niż
    `T_alone`, może zgubić potwierdzone zapisy `acks=all`** (obie strony widzą
    drugą jako martwą, lider dalej pisze sam, follower staje). Przy RF=2 bez
    świadka zerowa utrata w tym przypadku wymaga RF=3.
  - **Migotanie połączenia mesh po restarcie węzła (przyczyna pośrednia
    powyższego).** Węzeł, który wstał, dialował drugi, a ten trzymał martwe
    (zombie) połączenie sprzed restartu i zamykał świeże jako
    `tie-break-loser` (14 razy co ok. 13 s po restarcie A) albo `duplicate`
    (22–40 razy na minutę po restarcie B). Każde takie zamknięcie wysyłało
    `PeerDisconnected`, które kończy strumień replikacji followera
    (`mark_leader_disconnected`), więc follower tracił dzierżawę. Zapisane
    połączenie jest teraz zastępowane przez nowe połączenie tego samego peera
    tylko wtedy, gdy jest martwe, czyli peer nie otworzył na nim żadnego
    strumienia od 3 s (kilka heartbeatów); połączenie, które niesie ruch, nie
    jest zastępowane, jak stare by nie było. Dwa równoczesne dialowania nadal
    rozstrzyga tie-break. Strumienie replikacji jadą na osobnym połączeniu
    (`ALPN_BUS`), które tie-break mesh nie zamyka; psuło je wspomniane
    `PeerDisconnected`.
  - **Repliki nieobecne w wierszu ISR nie stawały po śmierci lidera.** Wiersz
    umieszczenia zapisuje ISR tylko przy zmianie lidera; replika, której nie
    było przy wyborze lidera, zostawała poza nim, choć od dawna była
    doścignięta, więc po śmierci lidera nie stawała (trzeci restart e2e:
    partycje czekały na powrót lidera, w logu nic). Elekcja liczy teraz
    ISR z wiersza razem z ISR, który lider ostatnio zgłosił w heartbeacie.
  - **Restart lidera w tej samej epoce a nieutrwalony ogon.** Lider, który po
    awarii wstaje w tej samej epoce, pisze nowe rekordy pod offsetami zgubionego
    ogona; follower, który ten ogon ma, miał rekordy o tym samym `(epoka,
    offset)` i innej treści, czego porównanie epok i offsetów nie widzi. Przy
    pierwszym uścisku dłoni z danym followerem w tym wcieleniu lider obcina go
    do końca swojego logu z chwili startu (o ile follower nie ma tam
    zatwierdzonych rekordów).
  - **Brak przejęcia po wcześniejszym przeniesieniu (krytyczne dla dostępności).**
    Lider, który oddał partycję w przenosinach (ręcznych, powrocie do noda
    preferowanego), zostawał followerem bez dowodu, że ma pełny log, więc gdy
    nowy lider padł zaraz po przenosinach (przed pierwszą ramką nowej epoki),
    partycja czekała na jego powrót (7–154 s, w logu nic, bo ostrzeżenie
    „not standing” pojawiało się raz na proces). Teraz przeniesienie, po którym
    cel był dogoniony (lider zamyka zapisy i czeka, aż cel ma cały log),
    oznacza dawnego lidera jako zgodnego w nowej epoce. Ostrzeżenie „not
    standing — has not caught up” powtarza się co 60 s na partycję, dopóki
    wstrzymanie trwa.
  - **Klucze API przestawały działać po restarcie węzła.** Dwa węzły
    parowane po osobnej instalacji mają każdy swój pepper kluczy API;
    synchronizacja sekretów (`SharedSecretsSync`) przyjmowała pepper
    nowszego i nadpisywała własny, więc klucze wydane na węźle, który
    przegrał, przestawały przechodzić weryfikację (401) na obu węzłach.
    Pepper zastąpiony przy przyjęciu trafia teraz na listę wycofanych
    (`api_key_pepper_retired`, szyfrowana, replikowana jako sekret z
    sumą mnogościową zamiast wersji; lista jest posortowana, więc węzły
    scalają ją do tej samej wartości w dowolnej kolejności, a ustawień API
    nie można jej zapisać). Wpisy listy mają postać `<hex>:<czas wycofania w ms>`,
    a przy przekroczeniu limitu 8 odpada pepper wycofany najdawniej, ale nigdy
    wycofany w ostatniej godzinie (na każdym węźle, według jego zegara, więc
    zegar węzła wycofującego spóźniony o mniej niż godzinę nikogo nie
    zawodzi; ochrona obowiązuje w całej flocie, dopóki rozjazd zegarów jest
    mniejszy niż godzina); ten sam pepper wycofany w różnych momentach
    zachowuje wcześniejszy czas, więc węzły zbiegają do tej samej listy.
    Nowy klucz ma postać `sk-<id peppera>-<losowe hex>` (id to 8 bajtów SHA-256
    z tagiem domeny, nie ujawnia peppera), a weryfikator w bazie to nadal goły
    HMAC całego tokenu, więc klucz wydany na nowym węźle działa też na węźle
    sprzed tej zmiany z tym samym pepperem. Token z id jest sprawdzany jednym
    HMAC-iem i jednym zapytaniem pod nazwanym pepperem; klucz sprzed
    identyfikatorów (`sk-<hex>`) jest próbowany pod pepperem bieżącym, potem
    wycofanymi. Węzeł wycofuje pepper, który sam
    zastępuje przy przyjęciu, więc klucz działa tam, gdzie go wydano; na węzeł,
    który nigdy nie miał starego peppera, lista dociera przy najbliższej
    synchronizacji sekretów (przy połączeniu i po przyjęciu), niezależnie od
    replikacji samych kluczy. Do tego czasu taki klucz jest odrzucany (401), nie
    przyjmowany błędnie. Wartości peppera nie trafiają do logów.
  - **`bus.leader.tail_discarded` nie liczy rekordów, które wracają.** Lider
    przy uścisku dłoni obcina follower do końca jego ostatniej epoki w logu
    lidera (jak `OffsetForLeaderEpoch` w Kafce), a nie do jego znacznika
    zatwierdzenia. Rekordy, które lider też ma, zostają, a liczą się tylko te,
    które naprawdę rozeszły się z łańcuchem lidera. Obejmuje to uścisk
    dłoni; elekcja, która sama obcina follower, nadal tnie do zatwierdzonego.
    Zastrzeżenie: reguła zakłada, że rekordy epoki zapisał jeden lider
    jednorazowo. Restart lidera w tej samej epoce (świeża epoka na każdy
    restart nie jest zaimplementowana) to łamie: follower z ogonem dawnej
    inkarnacji, który następnie uzgadnia z innym liderem niż zrestartowany,
    zachowuje ten ogon bez śladu w audycie. Dotyczy to także RF=3.
  - **Restart węzła w tym samym adresie nie zostawia martwych połączeń szyny
    (D1).** Gdy węzeł wraca na ten sam adres po `kill -9`, mesh podmienia
    zamilkłą sesję, ale połączenia szyny do dawnego procesu (przekazywanie
    zapisów, replikacja) zostawały otwarte do limitu bezczynności QUIC
    (47–55 s: 504 `bus_forward_outcome_unknown`, wiszące zapisy, późny powrót do
    ISR). Podmiana zamilkłej sesji zamyka teraz też połączenia szyny do tego
    węzła, które śledzono nie później niż sekundę po ostatnim odbiorze starej
    sesji (późniejsze, otwarte już do nowego procesu, zostają), więc pula
    przekazywania i pętle replikacji dzwonią od nowa.
  - **Zatrzymanie followera zwalnia lidera od razu (D2).** Zatrzymujący się
    węzeł najpierw przestaje obsługiwać i stawać do elekcji, potem mówi
    liderom RF=2, których śledzi (`ReplLeoQuery::leaving_epoch`, pole
    dopisane), że odchodzi; lider wyjmuje go z ISR i zatwierdza sam bez
    czekania `T_alone` (do 5–6 s odmów `acks=all` przy każdym restarcie).
    Dotyczy to także partycji, które węzeł właśnie oddał (jego wpis jest
    wtedy `NotReplica` do czasu pollu): wybór idzie po położeniu, nie po roli,
    a komunikat jest ponawiany co 150 ms w budżecie 2 s, ale tylko dopóki lider
    odpowiada epoką niższą niż w komunikacie (nie zastosował jeszcze
    wiersza); inna odmowa (starsza wersja, inna kadencja) kończy ponawianie.
    Węzeł, który prowadzi elekcję albo obiecał kadencję wyższą niż wiersz,
    nie wysyła komunikatu, a epoka i lider pochodzą z bieżącego wiersza
    ledgera. Zatrzymujący się węzeł odrzuca też `Hello` (jak
    `UnknownInstance`; sprawdzane ponownie przy rozstrzyganiu o wpisie i przed
    podpięciem strumienia, ale nie atomowo z anulowaniem: strumień przyjęty tuż
    przed nim jest zatrzymywany przy podpinaniu), a odpowiedź z epoką 0 (węzeł nic nie obsługuje)
    kończy ponawianie, więc lider po zakończeniu strumienia nie wciąga go z
    powrotem do ISR. Lider cofa zgodę na zatwierdzanie samemu przed
    usunięciem followera z ISR, nie po (publikacja czytająca ISR i zgodę nie
    widzi już ISR=1 przy nadal udzielonej zgodzie). Lider, który dostał spóźniony komunikat,
    kończy też strumień takiego followera, żeby żywy follower połączył się
    od nowa i wrócił do ISR. Reguła bezpieczeństwa bez zmian: lider działa
    dopiero po tym, jak węzeł przestał obsługiwać.
  - **Pierwszy klucz API na świeżej parze działa na obu węzłach (D3).**
    Wydanie albo rotacja klucza, które tworzy pepper, wypychają od razu
    sekrety floty do zaufanych węzłów; wcześniej drugi węzeł tworzył własny
    pepper przy pierwszym sprawdzeniu klucza i klucz działał tylko na
    wydającym do następnego połączenia.
  - **Odczyt wstrzymanej grupy konsumentów to 409 `bus_group_paused`, nie 500
    (D4).**
  - **Zapis w trakcie przenoszenia nie czeka całych 3 s (D5).** Gdy lider
    odmówił zapisu, a rola tego węzła nadal wskazuje jego (rolę zmienia dopiero
    poll, do sekundy po przyjęciu wiersza), nadawca idzie za wierszem w
    ledgerze, który już wskazuje innego lidera, i sprawdza go co 100 ms.
    Wcześniej grupa odmówiona przez tego samego lidera czekała na zmianę
    generacji do końca `LEADER_WAIT`. Dotyczy to też odmowy bez wskazania lidera (stary lider w trakcie
    przeniesienia) i odmowy lidera z wiersza, który widzi jeszcze starszą
    epokę (nie zastosował wiersza): nie liczą się jako druga próba. Wiersz
    jest śledzony tylko, gdy ma wyższą epokę niż rola; wskazujący ten węzeł
    nie jest śledzony: zapis czeka, aż rola nadgoni. Grupa wysłana
    według wiersza pozostaje ważna, dopóki wiersz (albo, po nadgonieniu, rola)
    nadal wskazuje ten węzeł: sprawdzenie ważności trwającego żądania nie
    wraca do roli, która nadal wskazuje lidera, który odmówił, więc zapis nie
    kończy się po 100 ms jako 504 `outcome unknown` (także po odmowie bez
    wskazania lidera). Grupy wybrane przez wiersz i przez rolę idą w osobnych
    żądaniach.
  - **Czas przejęcia (`duration_ms` rekordu failover) liczy się od ostatniego
    sygnału życia poprzedniego lidera (D6),** nie od zliczenia głosów, gdy ten
    moment jest znany; nazwa pola i format bez zmian.
  - **Zamknięcie węzła przekazuje partycje.** Przed zatrzymaniem replikacji
    węzeł oddaje partycje, które prowadzi, replice z zgodnym logiem
    (najwyżej 5 s łącznie, każde oczekiwanie przeniesienia jest przycięte do
    pozostałego budżetu, po jego wyczerpaniu nie zaczyna się kolejne, a
    liczba przeniesionych jest raportowana także wtedy, gdy budżet się skończył),
    więc druga strona nie czeka na wygaśnięcie dzierżawy. Partycja bez zgodnej
    repliki zostaje, jak dotąd.
  - **Polecenia mesh.** Limit czasu zależy od rodzaju polecenia: transfery
    modeli i wdrożenia nie są ucinane po 10 minutach (modele: do 25 h, tyle
    ile czeka wołający). Cofnięcie zaufania przerywa trwające polecenie, a
    peer nie dostaje nowych uprawnień, dopóki jego stare polecenia trzymają sloty.
  - **Odczyt tematu rozdzielonego między liderów.** Każdy lider dostaje swój
    udział w odczycie (nie głodzą go partycje wcześniej na liście), a konsument
    jest otwierany raz na wywołanie.
  - **Audyt przekazanych żądań.** Rozpoznanie żądań tylko do odczytu jest
    ścisłe: `...StatusSetRequest` i `...StatusUpdateRequest` zmieniają dane i
    nie są już ograniczane. Klucz ograniczania zawiera organizację.
  - **Zapomnienie partycji.** Wpis, którego nie ma w księdze, jest zapominany
    dopiero po 10 s nieobecności, nie po dwóch odpytaniach.
  - **Znacznik świeżej instalacji** nie jest ustawiany na węźle, który ma
    zaufane węzły, a jeszcze nie zsynchronizował wierszy.
  - **Uścisk dłoni strumienia replikacji ma limit czasu (10 s).** Lider czekał na
    `HelloAck` bez limitu; strumień, na którym druga strona milczy, wisiał do
    wygaśnięcia bezczynnego połączenia QUIC (40 s) plus 5 s ponownego łączenia,
    czyli cykl co 45 s widoczny w logach po restarcie. Teraz kończy się po 10 s i
    od razu łączy się ponownie. To usuwa wiszenie, ale nie wyjaśnia, dlaczego
    strona przyjmująca milczała: przyczyny po stronie węzła po restarcie nie
    ustalono.
  - **Odczyt topiku rozdzielonego między nody.** `GET .../records` na topiku,
    którego partycje prowadzą różne nody, nie kończy się już `503
    bus_not_leader`: każdy lider jest pytany o swoje partycje (nowe pole
    `only_partitions` w `ConsumeForward`), a odpowiedź zawiera rekordy
    wszystkich. Gdy któryś lider zawiedzie po tym, jak inny zwrócił rekordy,
    odpowiedź zawiera to, co już odczytano (ci liderzy zatwierdzili swój odczyt).
    Partycja bez znanego lidera nadal daje `bus_not_leader`.
  - **Zapis, który czekał na koniec przenosin lidera**, nie dostaje już 503: po
    przenosinach jest ponawiany raz na nowego lidera. Gdy przenosiny trwają
    dłużej niż 2 s, zapis nadal dostaje `bus_not_leader` bez nazwanego lidera
    (producent ponawia).
  - **Stan węzła w liście replik** (`reachable`) widzi węzeł, z którym łączy
    strumień replikacji, jako niedostępny po 5 s ciszy na strumieniu, a nie po
    15 s od ostatniego sygnału mesh (20-25 s od zabicia węzła).
  - **Po świeżej instalacji** rozkład „po aktualizacji” jest oznaczany jako
    wykonany od razu (przy pierwszym sprawdzeniu), a instalacja z topikami na
    jednym węźle sprawdza co 30 s, czy dołączył drugi (było 10 min).
  - **Audyt `app.route.execute`** dla żądań tylko do odczytu: jeden wiersz na
    użytkownika, węzeł i wariant w oknie 10 min, jak wiersz węzła źródłowego.
  - **Nazwy węzłów (D2):** nazwa z rejestru synchronizacji równa nazwie hosta
    któregokolwiek węzła jest traktowana jak ta nazwa i nie rozróżnia węzłów.
  - Każda elekcja działa we własnym zadaniu (najwyżej 16 naraz), a tyknięcie
    pętli dzierżaw tylko je startuje i nie czeka na ich koniec: wolna elekcja
    jednej partycji nie wstrzymuje startu elekcji innej, a partycja z trwającą
    elekcją jest pomijana. Kontrole `check_stale_leadership` i
    `check_quorum_leases` działają w tej samej pętli co start elekcji.
    Zrzeczenie się prowadzenia z powodu braku kworum dotyczy tylko tego
    prowadzenia, które utraciło kworum, nie późniejszego.
  - Wywołania dashboardu kierowane na inny węzeł (`targetNodeId`) kończyły się
    po 5 s błędem `no assertion key ... known for node`. Pętla zdarzeń mesh
    obsługiwała polecenie od peera w miejscu: `AppRouteOp` prosiło nadawcę o
    klucze asercji, a odpowiedź na to pytanie przychodzi tą samą pętlą, więc
    czekała, aż pytanie wygaśnie. Polecenia peerów działają teraz we własnych
    zadaniach, ograniczonych do 32 na peera i 256 na węzeł; zezwolenie jest
    trzymane przez cały czas trwania polecenia. Kolejne polecenie dostaje od
    razu odpowiedź „busy” z kodem błędu `busy` (nadawca odróżnia ją od błędu
    polecenia i może ponowić później). Odpowiedź „busy” jest wysyłana z
    osobnego zadania, najwyżej 8 naraz i do 2 s, więc nie blokuje pętli
    zdarzeń, a nadmiarowe są pomijane bez odpowiedzi; odmowy loguje się
    najwyżej raz na minutę na peera. Polecenie trwa do końca albo do 10 min
    (wtedy odpowiedź z błędem „command timed out”); rozłączenie peera go nie
    przerywa, a utrata zaufania pomija polecenia jeszcze nierozpoczęte i
    odpowiedzi tych w toku.
  - Audyt `app.route.forward` powstaje tylko dla przekazania, na które węzeł
    docelowy odpowiedział, z nazwą wariantu i wynikiem. Zapytania tylko do
    odczytu (`*List`, `*Get` i podobne) są audytowane raz na 10 min dla
    kombinacji użytkownik, węzeł, wariant, żeby odpytywanie co 10 s nie
    zalewało tabeli. Asercja, która nie przeszła weryfikacji (sfałszowana,
    wygasła, nieznany klucz), trafia na węźle wykonującym do logu na poziomie
    warn, najwyżej raz na minutę na parę (peer, użytkownik).
  - Strumieniowy `/v1/chat/completions` kończy flow, w którym padł węzeł,
    zdarzeniem błędu SSE w kształcie OpenAI (`data: {"error": {"message",
    "type", "code"}}`) z ogólnym komunikatem `flow.failed`, po którym idzie
    `[DONE]`. Status odpowiedzi pozostaje 200, bo strumień już ruszył;
    błąd jest w treści. Ta sama funkcja `client_message` obsługuje strumień
    Anthropic. Komunikaty błędów wewnętrznych dla klienta są po angielsku
    (`Internal error: ...`). Zakończenie flow strumieniowego jest logowane
    także wtedy, gdy klient rozłączył się wcześniej.
  - Powiadomienie konsumenta o liderze (`leader_node_name`) przechodzi przez tę
    samą logikę etykiet co lista replik, w odniesieniu do wszystkich zaufanych
    węzłów: gdy lider dzieli nazwę hosta z którymkolwiek z nich, nazwa jest
    pusta, a UI pokazuje krótkie id.

### Limity i SLA usług (TB-03)

- Migracje 211 i 212. 211 dodaje `service_sla_policies` i `service_sla_assignments`,
  replikowane przez Sync Ledger w zakresie organizacji: polityka jest wspólna
  dla całego klastra, a limity /v1 liczy każdy węzeł osobno. 212 dodaje lokalne
  tabele węzła `sla_breaches` i `sla_samples`.
- Nowy moduł `sla` z rejestrem polityk i bramką usługi. Bramka pilnuje limitu
  równoczesnych wywołań, krótkiej kolejki z limitem czasu oraz limitu wywołań
  w oknie zegarowym, a także zbiera p95 opóźnień. Zmiana polityki działa
  w miejscu i nie ucina trwających wywołań. Bramek nic jeszcze nie wywołuje,
  więc zachowanie aplikacji się nie zmienia.
- Jeśli dwa węzły nadadzą polityce tę samą nazwę, na każdym węźle wygrywa ta
  sama, starsza polityka, a druga dostaje przyrostek z identyfikatora.
- Polityka SLA działa przy uruchamianiu przepływu. Slot zajmuje tylko
  wywołanie najwyższego poziomu; wywołania z wnętrza przebiegu (bloki,
  addony, model opublikowany jako ten sam flow) slotu nie zajmują.
- Gdy limit jest wyczerpany:
  - `/v1` (OpenAI i Anthropic) zwraca 429 z `Retry-After` w sekundach;
  - WebSocket zwraca błąd z kodem `sla.overloaded`;
  - zadanie BPMN jest odkładane z kodem `FLOW_OVERLOADED` i ponawiane po
    oknie.
- Timeout z polityki anuluje przebieg i daje mu 2 s na zakończenie. Wiersz
  wykonania kończy się wtedy jako `error`. `/v1` zwraca 504, a WebSocket
  błąd `sla.timeout`. Klienci SDK OpenAI ponawiają 504 dwa razy, więc
  kosztowny przepływ może się wykonać do trzech razy.
- Reaktor TentaBus konsumuje flow zgodnie z jego polityką SLA. Przy
  `max_concurrency` = N przetwarza naraz do N paczek: przy `ordering =
  partition` partycja idzie po kolei, a różne partycje równolegle; przy
  `ordering = none` wszystkie paczki idą równolegle. Bez polityki albo przy
  N = 1 reaktor działa jak dotąd.
- Commit obejmuje tylko ciągły prefiks obsłużonych paczek w każdej partycji.
  Rekordy po porażce zostaną dostarczone ponownie; `on_error` (dlq, skip,
  halt) działa dla każdej paczki osobno.
- Wyczerpany limit tylko opóźnia przetwarzanie: nic nie trafia do DLQ i nic
  nie ginie, rośnie lag. Spowalnianie widać w lagu i jego alertach oraz w
  polu `throttled_until`, a nie w licznikach odmów.
- W trybie `at_most_once` reaktor najpierw rezerwuje sloty i pobiera najwyżej
  tyle paczek, ile zarezerwował. Czekanie na nowe rekordy jest wtedy skracane
  do połowy `queue_timeout` polityki.
- Status konsumenta pokazuje `in_flight`, `max_concurrency` i
  `throttled_until`. Starsze węzły odczytują go bez tych pól.
- Każdy węzeł co 30 s ocenia ostatnie 5 minut usług objętych polityką
  i otwiera naruszenia: p95 czasu odpowiedzi, odsetek błędów, timeouty,
  ostrzeżenie o wykorzystaniu limitu wywołań, osiągnięcie limitu oraz
  nasycenie wątków trwające co najmniej 60 s. Naruszenie otwiera się raz,
  aktualizuje, dopóki trwa, i zamyka przy pierwszej ocenie, w której warunek
  już nie zachodzi albo polityka zniknęła. Flow busa, który tylko zwalnia,
  też je otwiera, bo liczy się zapełnienie okna, a nie odmowy.
- Węzeł zapisuje próbki minutowe usług (7 dni) i historię naruszeń (90 dni
  po zamknięciu). Audyt `sla.calls.rejected`, `sla.concurrency.rejected`,
  `sla.breach.raised` i `sla.breach.resolved` jest zbierany w oknach 60 s
  ze stemplem organizacji polityki.
- Eksport Zabbix ma cztery nowe metryki bez etykiet:
  `tentaflow_sla_breaches_open`, `tentaflow_sla_rejected_total`,
  `tentaflow_sla_timeouts_total` i `tentaflow_sla_inflight`; szablon Zabbix
  zawiera pasujące pozycje.
- `max_concurrent` usługi AI jest teraz prawdziwym limitem równoczesnych
  wywołań na węźle. Obejmuje usługi HTTP, QUIC i silniki wbudowane
  (llama.cpp, MLX, sherpa) oraz endpointy `/v1/infer`, `/v1/rerank`,
  `/v1/ranking` i generowanie obrazów. Strumień trzyma slot do końca.
  Brak klucza oznacza brak limitu, jak dotąd; dozwolone wartości to 0–1024,
  gdzie 0 też znaczy brak limitu.
- Zajęte wywołanie czeka najwyżej `timeout_ms` usługi. Potem `/v1` zwraca 503
  z `Retry-After`, a WebSocket błąd `backend.busy`. Komunikat nie zawiera
  identyfikatorów usługi ani węzła.
- Przy kilku replikach zajęta replika nie blokuje wolnej: żądanie bez
  czekania przechodzi do następnej i czeka tylko na ostatniej. Odmowa z
  powodu zajętości nie liczy się jako awaria w circuit breakerze.
- Protokół ma nową rodzinę komunikatów `SlaBody`: lista, odczyt, zapis
  i usunięcie polityki, przypisanie i zdjęcie polityki z flow, stan SLA oraz
  stronicowana lista naruszeń. Starsze klienty ignorują nowe pola.
- Migracja 213 dodaje uprawnienie `sla.manage`, nadawane tylko roli
  `org_admin`. Zapis polityk wymaga `sla.manage`, a przypisanie do flow
  dodatkowo roli `power_user` lub `admin`, tak jak edycja flow. Odczyty
  dotyczą własnej organizacji; inną organizację może wskazać tylko
  super-admin.
- Odmowy, na które admin może zareagować, mają kody: `invalid_value`
  (z nazwą pola), `name_taken`, `policy_in_use` (z liczbą przypisań),
  `system_flow` i `governed_elsewhere`. Zmiany polityk i przypisań trafiają
  do audytu `sla.policy.*` ze stanem przed i po.
- Lista flow pokazuje przypisaną politykę i skrót stanu (ok, ostrzeżenie,
  naruszenie, zajęcie, wykorzystanie okna). Flow objęty polityką innej
  organizacji ma w skrócie znacznik `governed_elsewhere` z bieżącym stanem,
  bez nazwy i identyfikatora polityki.
- Strona Przepływy ma zakładkę „Polityki SLA” z listą, edytorem i oknem
  przypisania. Bez `sla.manage` zakładka jest tylko do odczytu. Usunięcie
  polityki przypisanej do flow jest wyłączone z podpowiedzią o liczbie
  przypisań.
- Lista flow ma kolumnę „Polityka SLA” ze stanem i zajęciem. Przypisanie jest
  wyłączone dla flow systemowych, dla flow objętych polityką innej
  organizacji i bez prawa edycji flow.
- Historia wykonań flow ma kartę „Stan SLA” z wierszem na każdy węzeł:
  zajęcie, okno limitu z godziną resetu, p95, błędy, odmowy (dziś i w oknie)
  oraz stronicowaną historię naruszeń. Flow zasilany z TentaBus pokazuje
  „Wstrzymania limitem” i węzeł prowadzący odbiór. Wolny węzeł nie blokuje
  karty.
- TentaBus, odbiorca: kafel „Równoległość”, alert „Wstrzymano limitem do”
  oraz karta polityki SLA w ustawieniach. Stan jest czytany z węzła, który
  prowadzi odbiór.
- Edycja serwisu ma sekcję „Limity”: maks. liczba wątków (0 lub puste pole
  = bez limitu) i czas czekania na wolny slot. Limity działają od razu po
  zapisie; zapis samych limitów nigdy nie restartuje serwisu. Węzeł ze
  starszą wersją odmawia zmiany limitów z prośbą o aktualizację, a okno
  pokazuje wtedy sekcję wyłączoną. Audyt `service.update` zapisuje faktyczny
  restart i nowe limity. Lista serwisów i zdarzenia jej zmian podają
  egzekwowane `max_concurrent` i `timeout_ms`.
- Stan SLA podaje odmowy od lokalnej północy węzła. Tuż po północy licznik
  może przez około 30 s obejmować odmowy z ostatnich sekund poprzedniego
  dnia albo je zgubić, bo próbka minutowa zapisana po północy ma znacznik
  poprzedniej minuty.
- Okna potwierdzenia wyświetlają treść komunikatu jako tekst. Nazwy
  przepływów, polityk i plików nie mogą już wstrzyknąć HTML.
- Przepływ można opublikować jako model `/v1` z poziomu Flow Buildera
  („Ustawienia” → „Udostępnij jako model w API /v1”); lista przepływów
  pokazuje znacznik `/v1 · <model>`. Edycja przepływu zapisuje teraz nazwę
  modelu (pusta nazwa wycofuje publikację), a zajęta nazwa daje błąd zamiast
  cichego pominięcia. Tworzenie przepływu przycina nazwę tak samo, a
  przywrócenie wersji odświeża katalog `/v1/models`.
- Wbudowany OCR na `/v1/infer` zajmuje jeden slot limitu serwisu zamiast
  dwóch, więc limit 1 już go nie blokuje. Zajęty serwis zwraca 503
  z `Retry-After` także dla OCR i reranku. Odpowiedzi błędów `/v1` nie
  zawierają już szczegółów wewnętrznych (identyfikatorów węzłów, treści
  odpowiedzi usługi); szczegóły trafiają do logu.
- Karta „Stan SLA” przepływu z TentaBus pokazuje węzeł prowadzący odbiór,
  czas wstrzymania i stan „wstrzymany limitem”; historia naruszeń używa
  progu zapisanego przy naruszeniu. Dziennik audytu pokazuje zapisaną
  ważność zdarzenia (`AuditLogEntry.severity`).
- Migracja 214 dodaje do `sla_breaches` limit wywołań i najwyższą liczbę
  wywołań. Wiersz ostrzeżenia pokazuje „ostrzeżenie przy 80 % (48/60)”,
  a szczyt jako „57/60”; starsze wiersze zachowują dotychczasowy opis.
- Kafel „Równoległość” odświeża się co 5 s. Nagłówek historii wykonań
  mówi „ostatnie 100 wykonań”, gdy lista jest pełna.
- Dokumentacja administratora: `docs/SLA_POLITYKI_ADMIN.md`.
- Okno „Edytuj serwis” zamyka się klawiszem Escape. Okno potwierdzenia
  otwarte nad nim samo obsługuje Escape. Ekran usług ma 64 brakujące
  tłumaczenia po niemiecku, hiszpańsku i francusku (kolumny, przyciski
  w wierszu, statusy, aliasy, aktualizacja serwisu), które wcześniej
  pokazywały się po angielsku.
- TentaBus: okruszki strony odbiorcy nazywają grupę klucza tak samo jak
  tytuł strony („Klucz Portal”), a nie technicznym `k:<id>`. Kreator
  tematu przy formacie innym niż JSON ostrzega, że przepływ czyta
  z magistrali tylko JSON, a wiadomość w innym formacie trafi do
  „Nieprzetworzone”.

### Zdarzenia i konta usługowe (TB-02)

- Przebiegi AI w dzienniku zdarzeń zawsze zapisują pełne wejście i wyjście
  (prompt, odpowiedź, argumenty i wyniki narzędzi, żądanie do modelu w każdej
  iteracji agenta) — bez zgody organizacji. Ustawienie
  `events.store_assistant_body:<org>` nie istnieje: wiersz `assistant_message`
  niesie treść odpowiedzi dla każdego przebiegu, a starsze wiersze z
  `not_enabled` i `no_organisation` nadal się odczytują. Przebiegi z magistrali
  (TentaBus, dane medyczne) nie zapisują żadnej treści — tylko oś czasu — i tego
  nie zmienia żadna ścieżka. Przebieg czatu bez modelu kończy się teraz jako
  `failed` (błąd dociera do strumienia zamiast pustej odpowiedzi), a przycisk
  „Anuluj” nie pojawia się przy przebiegach, których nie da się anulować.
- Każdy przebieg przepływu kończy się teraz jednym wierszem `request_finished`
  w dzienniku zdarzeń (status `completed`/`failed`/`cancelled` i czas, bez
  treści), także przebiegi z magistrali i przepływy bez wywołania LLM; widok
  Zdarzeń zamyka żądanie tym wierszem, a dla starszych przebiegów zostaje
  dotychczasowa reguła (odpowiedź albo błąd). Wiersz nie wymaga migracji, ale
  starsza wersja programu po obniżeniu wersji odrzuca strony zdarzeń zawierające
  `request_finished` (nieznany rodzaj zdarzenia).
- Odwołanie klucza API, który jest kontem usługi aktywnych przepływów
  `bus_consume`, ostrzega przed potwierdzeniem i wymienia te przepływy
  (`ServiceAccountUsageRequest`, tylko Admin, w obrębie organizacji wywołującego).

### Struktura organizacyjna

- Nieobecność nie ma już powodu: pole zniknęło z formularza, bazy (migracja 200
  usuwa kolumnę `org_absences.reason` oraz kopie w dzienniku synchronizacji) i
  widoczności („Powód nieobecności”). Pole `reason` zostaje w typach protokołu
  jako przestarzałe (pola są dopisywane, nie usuwane): serwer odmawia niepustego
  powodu i nigdy go nie wysyła. Węzeł, który kiedykolwiek zapisał lub odebrał
  nieobecność, po aktualizacji odtwarza księgę synchronizacji (jednorazowy reset
  bazowy); wpisy w kopiach zapasowych, w wolnych stronach sprzed aktualizacji i
  w operacjach na węzłach, które nie zostały zaktualizowane, mogą jeszcze
  zawierać stare powody. Dziennik audytu nigdy ich nie zawierał.
- Nieobecność albo zastępstwo z datą początku wcześniejszą niż dziś wpisuje tylko
  administrator; pozostali dostają odmowę `backdating_admin_only` także z
  `confirm_backdated`.

### Procesy (BPMN)

- Zadanie usługi może uruchamiać narzędzie addonu (B3.1): obok dotychczasowego
  przepływu model niesie `implementation` (`Flow` albo `AddonTool`). Zadanie z
  przepływem serializuje się bajt w bajt jak dotąd, więc opublikowane modele
  zachowują swoje sumy SHA-256. Narzędzie działa jako osoba, która uruchomiła
  instancję, wymaga uprawnienia „llm” do addonu i addonu z tej samej organizacji;
  odmowa przy starcie zadania otwiera zadanie dla człowieka
  (`TOOL_PERMISSION_DENIED`), a cofnięcie dostępu w trakcie kończy się
  `SOURCE_ACCESS_REVOKED`. Wywołanie WASM nie daje się przerwać: po przekroczeniu
  czasu wynik jest niepewny (`SERVICE_TIMEOUT`), a narzędzie dostaje klucz
  idempotencji `idempotency_key`. Publikacja przypina identyfikator addonu,
  narzędzia, skrót pakietu i schematu. Migracja 201 dopisuje do wywołań usług
  kolumnę `implementation_kind`. Eksport XML: `<tentaflow:service
  implementation="tool" addonId toolName>`; inspektor ma wybór implementacji
  (przepływ albo narzędzie addonu).
- Zadanie usługi może uruchomić pojedynczy blok Flow Buildera (B3.2):
  `implementation` dostaje wariant `Block { node_type, config }`. Serwer ma stałą
  listę dozwolonych bloków (`llm`, `embeddings`, `pii_filter`) i ścisły zestaw
  ustawień każdego z nich; `stt`, `tts` i `reranker` nie są na liście, bo ich
  porty niosą dźwięk albo JSON, którego proces nie ma jak podać ani odczytać.
  Publikacja przypina deterministyczny graf `trigger → blok → output` (JSON z
  posortowanymi kluczami i jego SHA-256) bez wiersza w `flows`, więc późniejsza
  zmiana kodu bloku nie zmienia przypięcia, a przypięcie musi być dokładnie grafem
  bloku z modelu. Blok działa jako osoba, która uruchomiła instancję (aktywny
  użytkownik w organizacji), przez `dispatch_pinned_synthetic`; nie zapisuje
  rekordu wykonania. Do bloku trafia wyłącznie tekst z mapowania `payload`
  (inny typ kończy się `BLOCK_INPUT_INVALID` przed granicą wysyłki). Błąd odczytu
  konta nie jest odmową: przed startem odkłada zadanie (`BLOCK_UNAVAILABLE`), a
  po wyniku nie unieważnia go. Wywołanie bloku miało zapis `flow` z
  `flow_id = block:<typ>`; od migracji 203 (B3.4a) ma własny
  `implementation_kind = block`. Eksport XML: `<tentaflow:service
  implementation="block" nodeType>` z `<tentaflow:blockConfig>`; inspektor
  pozwala wybrać blok i wpisać jego ustawienia jako JSON.
  Blok z modelem podlega tym samym zasadom co czat: inicjator musi mieć dostęp
  do modelu (przy publikacji, przy starcie i tuż przed uruchomieniem; odmowa to
  `SOURCE_ACCESS_REVOKED`), wyczerpany limit tokenów zatrzymuje zadanie przed
  granicą wysyłki (`TOKEN_QUOTA_EXCEEDED`), a wywołanie trafia do audytu zgodności
  z organizacją procesu; embeddingi procesu przechodzą teraz przez bramę AI.
  Pin jest czytany po strukturze, nie porównywany z tym, co zbudowałby dzisiejszy
  kod, a blok usunięty z listy kończy się odmową dostępu. Własne ponowienia bloku
  LLM są wyłączone (jedna próba), `stop` ma najwyżej 4 napisy, a
  `reasoning_effort` jest jedną z wartości `low`, `medium`, `high`, `xhigh`, `max`.
- Zadanie usługi może być krokiem agenta (B3.4a, ujście i wiązanie; wykonawca
  opisuje następny punkt): `implementation` dostaje wariant `Agent { agent_id,
  goal_expression, allowed_tools, max_iterations, output_schema }`, limit czasu
  takiego zadania to 1..=3600 s, a jego wynik pochodzi ze zgłoszenia agenta, nie
  z `result_expression` (model go odrzuca). Agent kończy krok narzędziem
  `core.task_complete` (`outcome` = `completed`, `error` albo `needs_human`, `code`,
  `summary`, `outputs`, `evidence`): krok, do którego należy, nie jest
  argumentem, więc model nie zgłosi cudzego zadania; wskazuje go wiązanie
  `process_activity` w metadanych przebiegu, ale metadane zapisze każdy węzeł
  przepływu (także blok addonu), więc same niczego nie dowodzą. Ujście ufa bazie:
  zgłasza przebieg, który silnik wykonuje (`ExecutionContext::request_id`, nie
  metadane), a jego wiersz w `agent_runs` musi być uruchomionym przebiegiem
  najwyższego poziomu agenta przypiętego w kroku, utworzonym dla tego wywołania
  (`correlation_id` = identyfikator wywołania). Do tego zgłoszenie jest sprawdzane
  na żywo (faza `may_have_executed`, próba i ogrodzenie, otwarta instancja,
  identyfikator przebiegu wyprowadzony z wywołania), mieści się w limitach wyniku
  kontraktowego, a `outputs` kroku `completed` muszą pasować do `output_schema`
  (podzbiór JSON Schema). Pierwsze zgłoszenie jest ostateczne, drugie dostaje
  `[TOOL_ERROR] already completed`; tabela `bpmn_agent_completions` odmawia
  drugiego wiersza i każdej zmiany. Narzędzia przebiegu to przecięcie
  `tools_json` agenta z `allowed_tools` kroku (w katalogu modelu, przy wywołaniu
  modelu i w blokach grafu `exec_command` oraz `patch_review`), plus samo ujście;
  wiązanie, którego nie da się odczytać, nie zostawia żadnego narzędzia, a
  podagent z `core.agent_spawn` dostaje najwyżej to przecięcie i żadnego wiązania.
  Zagnieżdżony blok `agent` zachowuje wiersz przebiegu wywołującego (głębokość,
  limity delegowania i dziennik pozostają jednym rachunkiem), ale zamiast wiązania
  dostaje znacznik `process_step_tools`: samą listę narzędzi kroku, która zawęża
  katalog, bramę wywołań, bloki grafu i delegowanie, nie otwierając ujścia.
  Blok grafu `spawn` przechodzi to samo sito co `core.agent_spawn`.
  Poza krokiem procesu ujście nie jest oferowane ani wybieralne w edytorze agenta.
  Zawężenie czyta wiązanie z metadanych, więc blok grafu uprawniony do zapisu
  metadanych może je usunąć; dlatego wykonawca (B3.4b) przypina graf harnessu.
  Publikacja wymaga agenta włączonego,
  z runtime `llm` (agent `cli` jest odrzucany), `allowed_tools` zawartych w jego
  narzędziach i `max_iterations` nie większego niż agenta. Kodek zapisu przesyła
  krok agenta dokładnie tak, jak go odczytał, w tym schemat wyjścia
  z kluczami autora. Eksport XML: `implementation="agent" agentId maxIterations`
  z `<tentaflow:agentConfig>`. Migracja 203 przebudowuje CHECK kolumny
  `implementation_kind` (dozwolone `flow`, `addon_tool`, `block`, `agent`, każde
  tylko z własnym prefiksem referencji) i tworzy `bpmn_agent_completions`.
- Krok agenta jest wykonywany i można go publikować (B3.4b). Wykonawca
  (`processes/agent_exec.rs`) sprawdza agenta i inicjatora przed granicą wysyłki,
  przypina wersję grafu harnessu (zapisana wersja przepływu równa grafowi z chwili
  startu kroku, więc późniejsza edycja nie zmieni trwającego przebiegu), a po
  granicy tworzy przebieg przez `spawn_with_run_id` z identyfikatorem wyprowadzonym
  z wywołania, bez rodzica, pod zasadą z korelacją równą wywołaniu i z wiązaniem w
  metadanych. Cel to wynik `goal_expression`; wejście aktywności dochodzi jako
  ogrodzone DANE, których znacznik końca jest w nich zneutralizowany. Budżet
  iteracji kroku może tylko obniżyć budżet agenta i jest czytany z modelu procesu. Wynikiem kroku jest wiersz
  w `bpmn_agent_completions` (nigdy ostatnia wiadomość), czytany po zakończeniu
  przebiegu, i ma pochodzenie `Contract`: błąd i eskalacja zgłoszone przez agenta
  uruchamiają zdarzenia brzegowe bez `result_expression`, a przyjęcie wyniku agenta
  wymaga, by był dokładnie zapisanym zgłoszeniem. Przebieg zakończony bez
  zgłoszenia, anulowany albo z wyczerpanym budżetem to incydent
  (`EXTERNAL_OUTCOME_UNCERTAIN`, powód `AGENT_NO_RESULT`), nigdy sukces; porzucenie
  kroku (limit czasu, anulowanie, utrata dzierżawy) anuluje przebieg. Bez menedżera
  przebiegów krok czeka (`AGENT_UNAVAILABLE`, ten sam ograniczony czas co zajęte
  narzędzie). Po restarcie zgłoszenie zapisane przed zatrzymaniem jest przyjmowane
  jako zaobserwowany wynik `Contract` bez ponownego uruchamiania agenta;
  przebieg bez zgłoszenia zostaje przy ścieżce `INTERRUPTED`. Zadania agentów mają
  własną klasę slotów (4 sloty agentów obok 4 ogólnych) także w zapytaniu o
  kandydatów, więc pełny zestaw długich przebiegów nie głodzi szybkich zadań, a
  nadmiar zadań agentów nie wypełnia okna kandydatów. Publikacja procesu z krokiem
  agenta jest otwarta (usunięto `refuse_agent_steps_until_runnable`), a model
  dopuszcza zdarzenie eskalacji także na kroku agenta. Projektant: czwarta
  podgrupa palety „Agenci” (`ProcessOptionsResponse.service_agents`, dopisane z
  `#[serde(default)]`; `SCHEMA_VERSION` zostaje 32), inspektor kroku agenta (agent,
  cel, narzędzia, limit iteracji, schemat wyniku) bez reguły wyniku, limit czasu
  do 3600 s i przyciski błędu oraz eskalacji dostępne bez `result_expression`.
- Poprawki po przeglądzie kroku agenta (B3.4b). Agent z `on_child_complete =
  continue` nie może być krokiem procesu, a przebieg z pochodzeniem `process` nie
  jest kontynuowany poza krokiem (kontynuacja ominęłaby zawężenie narzędzi i
  przeżyła krok). Porzucony krok anuluje całe drzewo przebiegów
  (`AgentRunManager::cancel_tree`), a zgłoszenie zapisane w
  `bpmn_agent_completions` kończy krok także przy limicie czasu, anulowaniu workera
  i utracie dzierżawy; samo zgłoszenie zatrzymuje przebieg, który pracuje dalej.
  Lista narzędzi, budżet iteracji i schemat wyniku kroku są czytane z opublikowanego
  modelu po identyfikatorze przebiegu, nie z metadanych, które zapisze każdy węzeł
  grafu; ujście sprawdza `outputs` względem tego schematu, a przyjęcie wyniku
  sprawdza je ponownie. Błąd bazy przy przypięciu harnessu odracza krok zamiast go
  kończyć, nieudane `await_run` i odczyt wiersza ujścia są ponawiane, a przycinanie
  historii przepływu pomija wersje trzymane przez oczekujące przebiegi. Liczniki
  slotów workera są niezależne według klasy. Projektant: limit iteracji to
  najwyżej 100 i najwyżej limit agenta, puste pole jest odrzucane, nieważny schemat
  wyniku zostaje widoczny z błędem po ponownym narysowaniu, a podpowiedzi celu i
  narzędzi opisują to, co system naprawdę robi.
- Druga runda poprawek po przeglądzie kroku agenta (B3.4b). Zagnieżdżony blok
  `agent` działa pod identyfikatorem przebiegu kroku, więc samo `request_id` nie
  odróżniało go od modelu kroku i `core.task_complete` mógł zakończyć krok:
  `ExecutionContext::nested_agent` (nieresetowalne, dziedziczone przez klony
  kontekstu) każe `sealed_meta` oddać zagnieżdżonemu agentowi tylko znacznik
  zawężenia odtworzony z modelu, nigdy wiązanie. Czasowniki delegowania i bloki
  grafu (`spawn`, `await_subagents`, `subagent_status`, `exec_command`,
  `patch_review`) budują wywołującego (`CallerRun::from_envelope`) i sito
  (`require_core_tool`) z zapieczętowanych metadanych, a dla kroku bierzemy
  identyfikator przebiegu z silnika, nie z metadanych: węzeł nie rozszerzy listy
  narzędzi dziecka ani nie wyprowadzi go spod `cancel_tree`. Zgłoszenie, które
  trafi do bazy między odczytem a `fail_claim`, jest obserwowane zamiast
  pogrzebane pod incydentem niepewności (cofnięcie dostępu nadal blokuje), a
  odroczone odzyskiwanie obejmuje też wyniki agentów. Strażnik anulowania
  uzbraja się przy każdym wyjściu (przebieg, który sam się zakończył, mógł zostawić
  podagentów), `cancel_tree` zatrzymuje rodziców przed dziećmi, powtarza przejście,
  aż nic nowego nie przybywa, ostrzega o ucięciu i nie blokuje wątku asynchronicznego.
  Błąd odczytu kontraktu odbiera narzędzia tylko przebiegowi, który deklaruje krok,
  nie zwykłemu agentowi czatu. Projektant: odrzucony szkic limitu iteracji lub
  schematu przeżywa zmianę zaznaczenia, a publikacja go wymienia; agent spoza listy
  nie pozwala podnieść limitu ponad obecny.
- Trzecia runda poprawek po przeglądzie kroku agenta (B3.4b). Odrzucony szkic
  limitu iteracji lub schematu jest kluczowany treścią, ciałem (proces i ścieżka
  podprocesów) oraz identyfikatorem węzła, a publikacja szuka go we wszystkich
  ciałach modelu, więc szkic zostawiony w podprocesie nadal blokuje publikację
  z głównego ciała. Spóźnione anulowanie nie przepisuje już zakończonego
  przebiegu na `cancelled`: `cancel` zmienia tylko wiersz w stanie aktywnym i
  tylko wtedy wysyła sygnał. `cancel_tree` po przekroczeniu granicy 1024 przebiegów
  przestaje odkrywać, ale sygnalizuje wszystko, co znalazł, a granica dotyczy
  tylko naprawdę nowych identyfikatorów. Zgłoszenie agenta, którego obserwacja
  nie przechodzi (ponad budżet historii, brak tożsamości żądania), jest próbowane
  w punkcie zapisu i nie cofa porażki zadania: następuje zwykły incydent
  niepewności. Strażnik anulowania zatrzymuje żywe przebiegi (korzeń i znanych
  potomków) synchronicznie z rejestru, a do `spawn_blocking` trafia tylko przejście
  po wierszach. Wywołania narzędzi biorą identyfikator przebiegu z `CallerRun`,
  nie z metadanych.
- Czwarta runda poprawek po przeglądzie kroku agenta (B3.4b). Odrzucony szkic
  pola kroku agenta blokuje publikację tylko, dopóki węzeł nadal jest krokiem
  agenta: po cofnięciu (Ctrl+Z) do zadania przepływu, po usunięciu węzła i po
  wczytaniu innego modelu (import XML, szkic z serwera, podgląd wersji) szkice są
  odrzucane. Nieczytelny wiersz zgłoszenia agenta (`result_json`) nie cofa już
  porażki zadania: jest traktowany jak zgłoszenie, którego nie da się
  zaobserwować, i kończy się incydentem niepewności; komunikat błędu dekodowania
  nie cytuje treści zgłoszenia, a dziennik niesie tylko identyfikatory i długość.
  Przebieg, którego wiersz anulowanie już ustawiło na `cancelled`, nie jest
  przepisywany na `completed` przez koniec jego własnego zadania: zapis końcowy
  zachowuje status, wynik i powód wyjścia, zapisuje tylko koszt, a kanał statusu
  mówi `Cancelled`. Lista stanów aktywnych i końcowych przebiegu jest jedna
  (`AGENT_RUN_ACTIVE_STATES`, `AGENT_RUN_TERMINAL_STATES`), a test pilnuje, by
  nowy stan trafił do jednej z nich.
- Piąta runda poprawek po przeglądzie kroku agenta (B3.4b). Wczytanie innego
  modelu (import XML, szkic, podgląd wersji) czyści też panel inspektora, więc
  nie zostaje w nim odrzucony tekst usuniętego węzła. Stan końcowy przebiegu,
  który słyszy rodzic, pochodzi z tej samej instrukcji `UPDATE ... RETURNING`,
  co zapis końcowy (bez drugiego odczytu, który mógł się nie udać), a zapis po
  anulowaniu zachowuje też czas zakończenia. Nieczytelne zgłoszenie agenta nie
  jest porażką odzyskiwania powtarzaną przy każdym starcie: krok idzie zwykłą
  ścieżką przerwanego zadania. Przejściowy błąd bazy przy odczycie zgłoszenia w
  trakcie porażki zadania jest ponawiany, a nie zamieniany w incydent
  niepewności. Listy stanów końcowych w czyszczeniu retencji, `wait_any` i
  odczycie generacji wynikają z `AGENT_RUN_TERMINAL_STATES`.
- Szósta runda poprawek po przeglądzie kroku agenta (B3.4b), z żywego krytyka GUI.
  Zapis i Wstecz nie gubią już odrzuconego pola inspektora (limit iteracji, schemat
  wyniku): zapis odmawia z nazwą węzła, a wyjście z edytora pyta jak przy
  niezapisanych zmianach. Publikacja odrzucona przez edytor nie zmienia napisu stanu
  zapisu na „Błąd zapisu” i nie towarzyszy jej „Zapisano”; problemy są w banerze po
  jednym w linii, a dwa kroki o tej samej nazwie mają numer. Limit iteracji pokazuje
  maksimum agenta i mówi, że wartość ucięto; limit czasu ma walidację w polu
  (1–600 s, agent do 3600 s) i zapis odmawia z nazwą węzła, także gdy odmawia
  serwer (komunikat modelu niesie id kroku). Agent wyłączony lub usunięty ma
  ostrzeżenie w inspektorze i znaczek na płótnie, a odmowa publikacji nazywa krok i
  agenta. Paleta i lista agentów są posortowane po nazwie. Znane ograniczenie:
  kolejność właściwości schematu wyniku jest po zapisie alfabetyczna (opis w planie).
- Siódma runda poprawek po przeglądzie kroku agenta (B3.4b), z drugiego przebiegu
  żywego krytyka GUI. „Wstecz” nie pyta już dwa razy: pytanie zadaje wyłącznie
  router (`canUnmount`), więc Wstecz, pasek boczny i adres mają jedno pytanie, a
  „Odrzuć zmiany” nie zapisuje niczego (autozapis jest wstrzymany, gdy pytanie jest
  otwarte). Otwarcie nieistniejącego procesu albo wyjście w trakcie ładowania nie
  rzuca już błędu z `processDraftProblems`. Baner odrzuconych pól podąża za
  poprawkami i znika z ostatnią; po odmowie edytor pokazuje pierwszy odrzucony krok
  z polem (wchodzi do treści i podprocesu), a kliknięcie banera prowadzi do
  następnego, więc dwa kroki o tej samej nazwie są rozróżnialne. Linie szkiców
  nazywają miejsce (treść, podprocesy), mają jeden format „Nazwa”: komunikat we
  wszystkich językach (po francusku ze spacją przed dwukropkiem; apostrofy w sekcji
  `bpmn` ujednolicone). Odmowa publikacji agentów nazywa wszystkie kroki naraz
  (serwer zbiera odmowy), z numerem wystąpienia przy powtórzonej nazwie. Zmiana
  agenta na mniejszy limit iteracji oraz zmiana kroku agenta na Flow z limitem
  czasu ponad 600 s ogłaszają obniżenie wartości. Nagłówek odmowy publikacji mówi
  „Nie opublikowano”, a komunikat w toaście zachowuje łamanie linii; odmowa serwera
  ma teraz toast jak odmowa edytora. `tf-alert` dostał atrybut `live` (rola
  `alert`), a `tf-input` i `tf-textarea` ustawiają `aria-invalid` i
  `aria-describedby` dla błędu pola.
- Ósma runda poprawek po przeglądzie kroku agenta (B3.4b), z trzeciego przebiegu
  żywego krytyka GUI. Pytanie „Niezapisane zmiany” ma trzy odpowiedzi: „Zostań w
  edytorze”, „Odrzuć zmiany” i „Zapisz i wyjdź”; Esc, przycisk × okna i klik w tło
  nie są odpowiedzią i znaczą „zostań” (edytor i zmiany zostają, a router
  przywraca adres, bez pętli historii). Dotyczy to też klasycznego buildera, który
  używa tego samego `canUnmount`. `TfWindow.choose` to nowe, ogólne okno pytania
  z dowolnymi odpowiedziami (`TfWindow.confirm` jest na nim zbudowane i nie
  zmienia wyniku). Obniżenie wartości (limit iteracji po zmianie agenta, ucięcie
  wpisanej liczby, limit czasu po zmianie na Flow) jest zapisane w podpowiedzi
  pola, a wspólny kontener toastów ma `role="status"` i `aria-live="polite"`.
  Podpowiedź po wpisaniu nieprawidłowej liczby nie zostaje przy „zmniejszono”.
  Dojście do następnego odrzuconego pola to przycisk `tf-button` w banerze
  (dostępny z klawiatury), a nie klikalny komunikat. Eksport, symulacja i
  uruchomienie nie chowają banera ani nie zmieniają napisu stanu, dopóki pole jest
  odrzucone; toast mówi, które pola pomijają. Po francusku cudzysłowy « » mają
  twarde spacje w całym pliku, a odmowa publikacji agenta używa półpauzy zamiast
  drugiego dwukropka we wszystkich językach.
- Weryfikacja kroku zadania usługi (B3.5): obok „Warunku” i „Człowieka” krok może
  mieć weryfikację „Test” (osobny przepływ sprawdzający wynik) albo „Krytyk”
  (osobny agent z kryteriami, tylko narzędzia odczytu, które krok i tak ma
  dozwolone). Odrzucenie kończy się błędem kroku (`VERIFICATION_FAILED`) albo
  zadaniem dla człowieka (`on_failure`). Wynik testu lub krytyka jest drugim etapem
  tego samego zadania; werdykt krytyka ma własny wiersz w
  `bpmn_verification_completions` (migracja 204 i 205) i znaczniki
  `tentaflow.verification:`, a historia dostaje zdarzenia `verification_passed` i
  `verification_withdrawn`. Krok nie przechodzi do „Zakończony” bez dowodu swojego
  weryfikatora: planista otwiera zadanie „niezweryfikowane” dla człowieka, a
  cofnięte zatwierdzenie jest wyliczane w tej samej transakcji akceptacji.
  Niedostępny weryfikator (`VERIFICATION_UNAVAILABLE`) i krytyk odmawiający pracy
  (`VERIFICATION_AGENT_REFUSED`) mają osobne kody incydentów. Flow nie może być
  własnym testem, a krytyk nie może być wykonawcą kroku. Projektant ma wybór flow
  testowego i agenta-krytyka, kryteria liczone w bajtach, tabelę tras Test/Krytyk
  z kodem odrzucenia, a odmowa publikacji wymienia każdy odrzucony weryfikator z
  nazwą kroku. Teksty w pięciu językach. `SCHEMA_VERSION` nadal 32.
- Paleta projektanta BPMN ma grupę „Bloki TentaFlow” (B3.3a): przepływy, bloki
  Flow Buildera (`llm`, `embeddings`, `pii_filter`) i narzędzia addonów
  upuszcza się na płótno jako gotowe zadania usługi, z nazwą pozycji i bez pytania
  o typ implementacji. `ProcessOptionsResponse` dostał pola `service_blocks` i
  `service_tools` (dopisane z `#[serde(default)]`, więc starsi klienci i serwery
  się nie rozjeżdżają). Narzędzia są filtrowane tą samą regułą co publikacja
  (addon działa, jest z organizacji użytkownika, użytkownik ma uprawnienie „llm”),
  a decyzja o addonie zapada raz na addon, nie raz na narzędzie. Lista narzędzi
  jest ucięta po 1000 pozycjach (stała kolejność addon, narzędzie); w inspektorze
  nadal można wpisać dowolne narzędzie. Agenci są oferowani od B3.4b.
  Pozycja narzędzia niesie nazwę zainstalowanego addonu (`addon_name`, dopisane z
  `#[serde(default)]`), więc dwie instancje jednego pakietu nie są nieodróżnialne
  („Notes › search_notes”). Katalog pomija narzędzia z identyfikatorami, które
  odrzuciłaby publikacja, a opisy i nazwy są ucinane do 512 znaków. Nieczytelny
  stan inicjatora kończy zapytanie o opcje błędem zamiast pustą listą narzędzi.
- Paleta BPMN (B3.3a, poprawki): grupa „Bloki TentaFlow” jest pierwsza, wiersze mają
  najwyżej dwie linie nazwy i opisu (pełny tekst w podpowiedzi), a przepływ pokazuje
  własny opis (`ProcessOptionFlow.description`, `#[serde(default)]`). Etykieta
  narzędzia addonu jest ucinana do 256 bajtów UTF-8 bez znaków sterujących, bo
  staje się nazwą węzła, którą serwer odrzuca powyżej tego limitu; dwie instancje o tej
  samej nazwie dostają id instancji, a instancja bez nazwy jest opisana swoim id.
  Dodanie z palety klawiszem lub kliknięciem układa kolejne węzły kaskadą zamiast
  na jednym punkcie, wyszukiwanie ignoruje białe znaki na brzegach, a po Enter fokus
  zostaje na wierszu. Nagłówki podgrup mają rolę `heading` i kontrast 11:1.
- Paleta i płótno BPMN (B3.3a, trzecia runda): dodanie bez pozycji szuka wolnego miejsca
  testem przecięcia prostokątów (z rzeczywistym rozmiarem węzła i odstępem), więc nowe
  węzły nie nakładają się na siebie ani na istniejące; upuszczenie zachowuje pozycję.
  Delete i Ctrl+Z działają także po kliknięciu Zapisz lub Enter na palecie (klawisze
  ignorują tylko pola tekstowe i okna). Obcięcie do dwóch linii dotyczy wyłącznie grupy
  „Bloki TentaFlow”. Etykieta z sufiksem id zachowuje nazwę addonu i nawias, a etykieta
  złożona z samych białych znaków wraca do id instancji. Opcje procesu przeglądają
  tabelę przepływów stronami, więc nieaktywne wiersze nie ukrywają dozwolonych przepływów.
- Paleta i płótno BPMN (B3.3a, czwarta runda): dodanie z palety do klasycznego Flow
  Buildera mierzy renderowany węzeł i powtarza szukanie wolnego miejsca z jego
  rzeczywistą wysokością (nadal jeden wpis cofania), a upuszczony węzeł jest
  wyśrodkowany na kursorze. Rozmiar węzła nie jest już dzielony przez zoom, prostokąty
  są mierzone raz na wyszukiwanie, a widok przesuwa się tak, by nowy węzeł był w całości
  widoczny. Skrócone id instancji zachowuje koniec, wiersz narzędzia pokazuje nazwę
  narzędzia i osobną linię z addonem, a nazwa złożona z niewidocznych znaków (zero-width)
  jest pusta po obu stronach. Opcje procesu przeglądają przepływy kursorem (nazwa, id).
  Przepływy z krawędziami `from`/`to` (lub `source`/`target`) otwierają się z narysowanymi
  połączeniami i zapisują jako `from_node`/`to_node`.
- Paleta i płótno BPMN (B3.3a, piąta runda): powtórne szukanie miejsca zaczyna od
  pierwotnego celu i używa tych samych prostokątów, a zewnętrzna etykieta zadania
  wlicza się do miejsca i przesunięcia widoku. Odstęp od krawędzi płótna to margines
  przesunięcia przeliczony przez zoom. Wiersze narzędzi o tej samej nazwie addonu
  zaczynają się od id instancji (wiersz bez nazwy pokazuje id raz). Kliknięcie
  płótna lub węzła zabiera fokus polu tekstowemu spoza płótna, więc Delete i Ctrl+Z
  działają po edycji nazwy. Serwer odrzuca w nazwie węzła znaki sterujące kierunkiem
  tekstu (bidi), projektant je usuwa, a znaki wypełniające (U+034F, U+115F, U+1160,
  U+3164, U+FFA0) liczą się po obu stronach jako puste.
- Inspektor zadania usługi BPMN (B3.3b): pola są w pięciu zakładkach (Akcja, Dane,
  Wynik, Czas i Więcej, z pełnymi nazwami w podpowiedzi), widocznych naraz w panelu
  o szerokości 340 px. Lista typów bloków i ich wymaganych ustawień pochodzi z opcji
  serwera (`serviceBlocks`, `requiredSettings`), a stała po stronie klienta zniknęła.
  Zakładka „Wynik” pokazuje tabelę „co się stanie” wyliczaną z grafu
  (`processResultRouting`): ukończone, odrzucone przez weryfikującego (`HUMAN_REJECTED`,
  `VERIFICATION_FAILED`), błąd z kodem lub bez, wymaga człowieka (także eskalacja
  nieprzerywająca, która zostawia zadanie), anulowane, odmowa uprawnienia narzędzia
  addonu (zadanie weryfikacji, niezależnie od reguły wyniku), awaria platformy i
  przekroczenie czasu (incydent `EXTERNAL_OUTCOME_UNCERTAIN`, powód `SERVICE_TIMEOUT`;
  przerywający timer brzegowy nadal kończy zadanie i go rozwiązuje), ze zdarzeniami
  brzegowymi przypiętymi do zadania, oraz ostrzeżenia (jedno na rodzaj problemu) i
  przyciski „Dodaj obsługę błędu / eskalacji”. Zdarzenie dodane przyciskiem leży na
  dolnej krawędzi zadania i nosi nazwę po kodzie („Błąd: INVALID_INPUT”). Tabela jest
  sprawdzana przeciwko silnikowi wspólnym plikiem `tests/fixtures/process_result_routing.json`,
  który czytają test projektanta i test silnika `processes::result_routing_tests`.
  Przy niskim oknie wiersz ustawień procesu jest zwinięty do znacznika stanu, a przy
  szerokości poniżej 1440 px paleta jest szufladą otwieraną z płótna, więc inspektor ma
  użyteczną wysokość, a płótno szerokość. Pusty warunek akceptacji jest zgłaszany z
  nazwą zadania przed zapisem. Węzeł zadania usługi ma plakietkę rodzaju (przepływ,
  blok, narzędzie addonu) i znacznik ostrzeżenia dla bloku bez wymaganego modelu. Nowa
  aktywność wywołująca bez dokładnej wersji ma znacznik na węźle i komunikat na górze
  inspektora, który wyjaśnia przyczynę; zapis nadal wymaga wybranej wersji.
- Silnik BPMN: powtarzane zadanie usługi, którego wynik jest nieobsłużonym Błędem,
  Anulowanym albo odrzuconym przez warunek (`VERIFICATION_FAILED`,
  `EXPRESSION_ERROR`), nie było nigdy zatwierdzane: plan nie nazywał incydentu w
  grupie powtórzeń, walidator go odrzucał, a wynik zostawał w stanie `observed` bez
  incydentu. Grupa wskazuje teraz swój incydent, a walidator przyjmuje taki plan tylko
  dla incydentu o kodzie i komunikacie, który wynika z przypiętej konfiguracji: wynik
  przyjęty przez warunek nie da się zablokować, a kod i komunikat odrzucenia walidator
  wylicza ponownie z przypiętego wyrażenia. Ordynał zablokowanego wyniku ma zawsze
  status `AcceptedBlocked` (nie `RetryableError`). Błąd z powtarzanego zadania w
  podprocesie nie wspina się do zakresów nadrzędnych (tak jak w silniku).
- Silnik BPMN: w równoległej grupie powtórzeń rodzeństwo zablokowanego ordynału nadal
  kończy się (incydent grupy nie blokuje wyników zadań będących w locie): drugi
  nieobsłużony wynik zgłasza własny incydent powiązany ze swoim zadaniem, a grupa zostaje
  przy pierwszym, więc żaden incydent wyniku zadania nie jest podmieniany ani osierocony
  (przerwanie grupy zamyka je przez zadanie), a wynik rodzeństwa nie zostaje w stanie
  `observed`. Incydenty bez zadania (błąd wejścia, błąd oceny skryptu) powstają najwyżej jeden na
  grupę, bo grupa z incydentem nie aktywuje kolejnych ordynałów (ordynał, który
  zawiódł, zostaje `Active`, następne `Pending`). Odrzucenie weryfikacji przez człowieka
  też nie blokuje decyzji o rodzeństwie. Wynik, którego plan silnik albo walidator
  odrzuca na stałe (`RESULT_REJECTED`, `TRANSITION_ERROR`), oraz wynik, który nie może
  się opublikować mimo ponowień (`REVISION_CONFLICT`), zgłasza od razu incydent
  `RESULT_RECOVERY_FAILED` (wynik zostaje zapisany), a okresowy przebieg odzyskiwania
  próbuje go przyjąć ponownie, także dla wyników przepływu i bloku (nie tylko
  narzędzi addonu), z opóźnieniem podwajanym po każdej nieudanej próbie (minuta, dwie,
  cztery, do godziny), i po sukcesie zamyka incydent; nie trzeba restartu. Przyjęcie trzymane przez inny worker, zajęty magazyn
  i przegrany wyścig rewizji nie są odrzuceniem planu: pierwsze zostawia wynik jego
  właścicielowi, pozostałe są ponawiane. Błąd zadania wewnątrz powtarzanego
  podprocesu trafia do zdarzenia brzegowego na tym podprocesie także w walidatorze.
- Silnik BPMN: nieudany odczyt migawki instancji podczas publikacji wyniku nie jest już
  cofnięciem uprawnień. Zajęta pula albo magazyn są ponawiane, każdy inny błąd odczytu
  kończy się `RESULT_ACCEPTANCE_FAILED` (wynik zostaje zapisany i da się go przyjąć), a
  tylko odmowa dostępu blokuje wynik jako `SOURCE_ACCESS_REVOKED`. To samo dotyczy
  odczytu uprawnień przy zapisie obserwacji i błędzie zadania: blokuje wyłącznie
  typowana odmowa (`ProcessAuthorityDenied`, także dla niezgodnego inicjatora wywołanego
  procesu), a błąd wejścia/wyjścia albo nieczytelny wiersz wersji, definicji czy instancji
  zostawia wynik zapisany i widoczny jako incydent. Powtarzane zadanie
  skryptu z wejściem IO, którego treść zawodzi, przestało przerywać start instancji:
  walidator odtwarza treść z zapisanymi wejściami i przyjmuje incydent ordynału bez źródła.
- Silnik BPMN: przyjęcie zapisanego wyniku ponawia się tylko po przegranym wyścigu rewizji
  albo zajętym magazynie. Każde inne niepowodzenie przyjęcia (brak zapisanej obserwacji,
  utracona generacja zadania, otwarty incydent odzyskiwania) jest stałe: wynik zostaje
  zapisany, a worker zgłasza incydent `RESULT_RECOVERY_FAILED` od razu (kod
  `RESULT_ACCEPTANCE_FAILED` w dzienniku), zamiast ośmiu prób i fałszywego
  `REVISION_CONFLICT`. Cofnięte uprawnienie właściciela albo źródła blokuje wynik jako
  `SOURCE_ACCESS_REVOKED`. Błąd zapisu (tylko do odczytu, uszkodzona baza, brak miejsca)
  nie jest odrzuceniem planu. Walidator przyjmuje zapisany incydent warunku zadania z
  którymkolwiek z dwóch kodów (`EXPRESSION_ERROR` albo `VERIFICATION_FAILED`), więc
  wydanie, które ocenia to samo wyrażenie inaczej, nie odrzuca planów rodzeństwa.
- Silnik BPMN (runda 7): wynik przepływu lub bloku odrzucony na stałe nie jest już
  ponawiany co minutę w nieskończoność: opóźnienie rośnie dwukrotnie po każdej nieudanej
  próbie do godziny, a powtórzone niepowodzenia trafiają do dziennika na poziomie
  debug (błędem są pierwsze i to, które zgłasza incydent). Wiersz, który przegrał wyścig
  rewizji, jest oznaczany jako sprawdzony i nie wraca w każdym takcie. Wyczerpanie ośmiu
  prób na zajętym magazynie jest zgłaszane jako `STORE_BUSY`, nie `REVISION_CONFLICT`.
  Komunikat błędu powtarzanego skryptu (z IO i bez) porównuje walidator z tą samą
  ograniczoną treścią, którą zapisuje silnik, więc komunikat dłuższy niż 32 KiB nie
  powoduje już odrzucenia planu. **Znana, otwarta wada:** powtarzane zadanie skryptu,
  którego odwzorowanie WYJŚCIA IO zawodzi (`ACTIVITY_IO_OUTPUT_FAILED`), nie przechodzi
  walidatora. Na starcie instancja się nie zapisuje. Gdy skrypt jest za stanem
  oczekiwania, odmowa dotyka dopiero późniejszego przejścia: ukończenie zadania
  użytkownika kończy się błędem polecenia bez zapisu i bez incydentu (zadanie zostaje
  otwarte, instancja nie ulega zmianie, ale nie da się jej tą drogą ukończyć), a wynik
  zadania usługi zostaje zapisany i zgłasza incydent `RESULT_RECOVERY_FAILED`, ponawiany
  z narastającym opóźnieniem. Nic nie jest uszkodzone, ale taka instancja czeka na
  anulowanie albo poprawkę silnika.
- Silnik BPMN (runda 8): autoryzacja przepływu zadania usługi blokuje wynik jako
  `SOURCE_ACCESS_REVOKED` tylko przy typowanej odmowie (`DispatchError::Denied`). Błąd
  odczytu (baza, przeterminowana pula odczytu, także w `resolve_org_context`) nie jest
  już odmową: przed granicą wysyłki odkłada zadanie (`FLOW_UNAVAILABLE`), po uzyskaniu
  wyniku zostawia go zapisanym do odzyskania. Ta sama zasada obowiązuje przy claimie:
  stałą odmową jest tylko `ProcessAuthorityDenied`, inny błąd zostawia zadanie w kolejce.
  Nieudany powtarzany skrypt bez IO zapisuje ograniczony komunikat, jak ten z IO.
  Piąte niepowodzenie przebiegu odzyskiwania, które nie zgłasza nowego incydentu, jest
  logowane jako debug.
- Silnik BPMN: plan zamknięcia grupy powtórzeń z zablokowanym wynikiem (TerminateEnd,
  zdarzenie brzegowe na podprocesie nadrzędnym, zdarzenie brzegowe na zadaniu z
  odrzuconą weryfikacją) nie jest już odrzucany przez walidator, a zamknięcie anuluje
  także ordynały `AcceptedBlocked`. Walidator rozpoznaje zapisany incydent po kodzie i
  zadaniu, nie po brzmieniu komunikatu, więc zmiana komunikatu w nowszym wydaniu nie
  odrzuca planów dotykających starych wierszy. Błąd z podprocesu powtarzanego z
  licznością większą niż 1 trafia do zdarzenia brzegowego na jego koordynatorze także
  wtedy, gdy zawiódł inny ordynał niż zamykany jako pierwszy.
- Inspektor zadania usługi BPMN, runda 2: tabela „co się stanie” nazywa zdarzenie, do
  którego wspina się błąd w podprocesie, wymaga reguły wyniku dla obu przycisków,
  jest zwijana (zwinięta na niskim oknie) i łamie kody tylko po podkreślniku.
  Zadanie ma sześć miejsc na zdarzenia brzegowe; przy pełnej granicy przyciski są
  wyłączone z powodem, a timer upuszczony z palety przyczepia się do zadania, na które
  spadł (albo mówi, że nie jest podłączony). Obsługa eskalacji prowadząca od razu do
  zakończenia jest oznaczona w tabeli i odmawiana przy zapisie z jej nazwą; komunikaty
  serwera cytują nazwy elementów zamiast identyfikatorów, widać je pod paskiem
  narzędzi, a automatyczny zapis nie wyświetla toastu co 10 s. Aktywność wywołująca
  sama wczytuje opublikowane definicje i wersje. Dolny pasek w podprocesie to jeden
  wiersz, ciało inspektora ma 400 px i więcej przy 1280x720 i 1024x700, szuflada
  palety zamyka się Escape i kliknięciem płótna.
- Projektant BPMN, runda 3 (GUI): zapis odmawia timera bez strefy czasowej procesu
  z nazwą elementu, rozwija zwinięty wiersz ustawień i ustawia fokus w polu (zamiast
  surowego błędu serwera); żaden komunikat walidacji nie podaje identyfikatora
  elementu bez nazwy. Pasek narzędzi to jeden wiersz o stałej wysokości w pięciu
  językach (Publikuj, Uruchom i Zapisz na pasku, reszta w menu „Więcej”, każda ikona
  inna); toasty wiszą pod paskiem, nie nad Publikuj i Zapisz. Zdarzenie brzegowe
  upuszczone na zadanie bez miejsca ląduje obok, a każdy upuszczony element omija
  istniejące. Cofnij i Ponów działają w widoku, w którym jest autor. Listy pokazują
  nazwy zamiast identyfikatorów, a zapisany cel wywołania (zarchiwizowany lub
  nieistniejący) jest nazwany i oznaczony ostrzeżeniem. Przerysowanie 80 elementów
  nie wymusza układu strony dla każdego z nich, długie kody w etykietach łamią się po
  podkreślniku i mają wielokropek.
- Projektant BPMN, runda 4 (GUI): pasek narzędzi mieści się przy każdej szerokości od
  768 px w pięciu językach (kroki liczy sam pasek: Wstecz i Zapisz same ikony, potem
  bez procentu powiększenia), znacznik zapisu pokazuje cały tekst, a Uruchom ma powód
  na opakowaniu (wyłączony przycisk nie przyjmuje myszy) i sam wybiera jedyne wejście
  startowe. Na telefonie dotknięcie elementu otwiera inspektor (kliknięcie kończące
  naciśnięcie nie zamyka go już jako tła, a pasek kart nadąża za panelem). Okna
  Uruchom i Symulacja, wejścia startowe, listy wyboru, paleta i okruszki nazywają
  elementy (powtórzone nazwy dostają numer), okno instancji podaje kod incydentu
  zamiast identyfikatora zakresu, a identyfikator elementu jest za zwijanymi
  szczegółami technicznymi. `tf-keyvalue-editor` rysuje nagłówek `label`, `tf-menu`
  pomija w strzałkach pozycje ukryte i wyłączone, `tf-button` przekazuje
  `aria-controls` i nie zostawia kopii aria na hoście. Przed publikacją klient nazywa
  nieużywane deklaracje i powtórzenia bez zmiennej wynikowej (zamiast „protocol error
  BadRequest”), strefa czasowa jest przycinana, sprawdzana i znika z ostatnim timerem,
  linie etykiet omijają zadanie, do którego należą, a niepodłączone zdarzenie
  brzegowe ma przerywany kontur i napis.
- Projektant BPMN, runda 5 (GUI): `tf-button` znów trzyma atrybuty aria i `title`
  na hoście (selektory, `closest()`, `toggleAttribute`, `cloneNode`) i odzwierciedla
  je na prawdziwym przycisku. Zadanie Send samo ładuje definicje, identyfikatory
  celu i wywoływanego procesu są za szczegółami technicznymi, a okno instancji,
  wybór subskrypcji i incydenty nazywają kroki. Symulacja z góry mówi, że kroku
  powtarzanego nie uruchomi. Linie do etykiet nie przechodzą przez żaden element,
  a etykieta bez miejsca pokazuje nazwę po najechaniu. Strefa czasowa jest
  zapisywana w pisowni silnika, a tekst, który nią nie jest, blokuje zapis i zostaje
  w polu. Ponowienie mówi „Ponowiono”, zamknięte menu wypada z kolejności Tab,
  upuszczanie z palety działa na całym płótnie, a telefon (pionowo i poziomo) ma
  dolny pasek z Cofnij i Ponów w zasięgu oraz menu „Więcej” w oknie.
- Runda 6 poprawek designera (tylko GUI): jedna odroczona etykieta nie psuje już
  płótna ani zapisu (widok i model nigdy nie dostają NaN), pasek narzędzi w
  bezruchu się nie mierzy, strefa czasowa zachowuje wpisaną pisownię, menu „Więcej”
  przewija się w niskim oknie, minimapa przesuwa płótno i się zwija, a upuszczenie
  schodzi jej z drogi i przewija widok do węzła. Zdarzenia brzegowe na powtarzanych
  zadaniach mają wyjaśnienie w toaście, podpowiedzi i sprawdzeniu przed zapisem,
  historia powtórzeń jest tabelą faktów liczoną od 1, znane odmowy silnika mówią
  zwykłymi słowami, a statystyki mają liczbę mnogą we wszystkich językach.
- Runda 7 poprawek designera (tylko GUI): menu „Więcej” nie zamyka się samo, gdy
  fokus przewija pasek narzędzi, zamykany panel nie łapie kliknięć, a wysokie menu
  mieści się w oknie i się przewija. Host przycisku `tf-button` nie niesie
  globalnych atrybutów ARIA (nazwa i opis są na wewnętrznym przycisku), wystąpienie
  powtarzane czekające na człowieka nie jest opisane jako „zawiodło”, strefa czasowa
  w złej wielkości liter jest odrzucana przed zapisem, a niemieckie notki o
  pominiętych zdarzeniach mają poprawną liczbę pojedynczą.
- Runda 8 poprawek designera (tylko GUI): menu `tf-menu` podąża za wyzwalaczem także
  wtedy, gdy przewija go kontener w shadow DOM (treść okna `tf-window`), i zamyka się,
  gdy wyzwalacz znika pod krawędzią tego kontenera; zamykane menu oddaje fokus
  wyzwalaczowi, jeśli był w panelu; polski status „Nierozpoczęte” jest pisany łącznie.
- Runda 9 poprawek designera (tylko GUI): pasek klasycznego Flow Buildera nie zmienia
  układu przy zmianie chipa autozapisu; menu `tf-menu` zamyka się, gdy wyzwalacz w
  oknie `tf-window` zniknie pod paskiem tytułu, a strzałki na zamkniętym przycisku
  „Więcej” je otwierają; odmowa zdarzenia brzegowego na powtarzanym zadaniu podaje
  rodzaj zadania, nie powtarzanie; strefa czasowa `utc`/`gmt`/`est` jest naprawiana
  do pisowni silnika; zadanie z powtarzaniem może zadeklarować zmienną na zebrane
  wyniki w swojej sekcji.
- Weryfikacja wyniku zadania usługi testem albo krytykiem (B3.5). `ActivityVerification`
  dostało dopisane warianty `Test { flow_id, on_failure }` i `Critic { agent_id,
  criteria, on_failure }` z `on_failure` w `Error` (domyślny: błąd do obsłużenia w
  procesie, `VERIFICATION_FAILED`) albo `NeedsHuman`; `Human` i `Condition` mają te
  same bajty co dotąd, `SCHEMA_VERSION` zostaje 32. Weryfikacja jest drugim etapem
  tego samego joba (`processes/verification.rs`): wynik pracy `Completed` idzie do
  przypiętego przepływu weryfikującego (wejście: wyjścia wyniku i efektywne
  zmienne, werdykt: blok „Wynik aktywności”) albo do drugiego agenta (przebieg pod
  własnym identyfikatorem, wiązanie `role = verification`, tylko narzędzia do
  odczytu plus ujście `core.task_complete`, kryteria i wynik pracy jako ogrodzone
  DANE). Zatwierdzenie zostawia `Completed` i dopisuje
  `tentaflow.verification:approved:test:<przepływ>` /
  `tentaflow.verification:approved:critic:<agent>`; odrzucenie zamienia wynik w `Error` albo `NeedsHuman`
  `VERIFICATION_FAILED` z pochodzeniem `Contract` i oryginalnymi wyjściami, więc
  łapie je zdarzenie brzegowe (eskalację można założyć także na kroku bez reguły
  wyniku). Weryfikator, który nie wydał werdyktu (błąd przepływu, krytyk bez
  zgłoszenia, anulowanie, limit czasu), daje `NeedsHuman("VERIFICATION_UNAVAILABLE")`,
  nigdy zatwierdzenia. Wynik `Completed` bez dowodu weryfikatora (np. zgłoszenie
  agenta przyjęte po restarcie) planner oddaje człowiekowi. Migracja 204 dodaje
  `bpmn_verification_completions` (werdykt krytyka, jeden niezmienny wiersz na
  wywołanie); publikacja przypina weryfikator obok pracy
  (`PinnedServiceRole::Verification`) i odrzuca krytyka będącego agentem pracy,
  wyłączonego agenta albo przepływ bez bloku wyniku. Projektant: zakładka wyniku
  oferuje Test i Krytyka (wybór przepływu lub agenta, kryteria, skutek odrzucenia)
  w pięciu językach.
- Poprawki przeglądu B3.5. Rekord weryfikacji nie powtarza już dowodów i
  podsumowania pracy (`work_summary` tylko przy odrzuceniu lub niedostępnym
  weryfikatorze, `dropped_evidence` tylko dla 64. wpisu), więc odrzucenie dużego
  wyniku mieści się w obserwacji; werdykt, którego obserwacja się nie mieści,
  zapisuje się jako `VERIFICATION_UNAVAILABLE`, a wynik bez miejsca nawet na taki
  zapis idzie do człowieka bez uruchamiania weryfikatora (`OUTPUT_LIMIT` tylko,
  gdy nie mieści się sama praca).
  Po wykonaniu pracy odmowa weryfikatora (wyłączony agent, flow poza ACL, błąd
  odczytu, wycofanie w trakcie oceny) oddaje pracę człowiekowi jako
  `VERIFICATION_UNAVAILABLE` zamiast `SOURCE_ACCESS_REVOKED`; tylko utrata dostępu
  do źródła pracy blokuje krok (przypięty weryfikator nie wstrzymuje już przejść
  całej instancji, tylko krok, który z niego korzysta). Zgłoszenie agenta przyjęte po restarcie przy trybie Test albo
  Krytyk jest zapisywane jako niezweryfikowane (`VERIFICATION_UNAVAILABLE`), nie
  jako zwykłe `Completed`. Krok z krytykiem zajmuje slot klasy agentów
  (migracja 205, kolumna `critic_verified`, zapytanie claim). Krytyk dostaje
  przecięcie narzędzi do odczytu z `allowed_tools` kroku agenta, a jego przepływ
  harnessu nie może zawierać bloków ze skutkami (publikacja i uruchomienie).
  Wynik pracy kroku z Testem albo Krytykiem nie może udawać werdyktu (kody
  `VERIFICATION_*`, dowody w przestrzeni `tentaflow.verification:`), a przepływ
  nie może być własnym testem. Tekst podpowiedzi o skutku odrzucenia opisuje
  restart zgodnie z zachowaniem w pięciu językach.
- Druga runda poprawek przeglądu B3.5. Zatwierdzenie, którego weryfikator został
  wycofany między werdyktem a przyjęciem, nie blokuje już przyjęcia (dawało
  nierozwiązywalny incydent `SOURCE_ACCESS_REVOKED` nad skończoną pracą): planner
  traktuje je jak wynik bez dowodu i oddaje pracę człowiekowi; odrzucenie i
  niedostępna weryfikacja przechodzą tak, jak zapisano. Harness krytyka ma listę
  dozwolonych bloków (zamiast listy zabronionych) i sprawdzanie schodzi w ciała
  `loop`, `map` i `subflow` (do 4 przepływów w głąb), więc `loop` z ciałem
  zawierającym `exec_command` nie omija kontroli; kontrola działa też przed pracą,
  nie tylko po niej. Zastrzeżone kody i dowody werdyktu obowiązują tylko kroki z
  Testem albo Krytykiem (pozostałe kroki mogą ich używać jako zwykłego tekstu), nie
  dotyczą wyniku samego przepływu testowego, a znaczniki mają przestrzeń nazw
  `tentaflow.verification:{approved|rejected|unavailable}:<tryb>:<odwołanie>`.
  Wynik pracy bez miejsca na werdykt trafia do człowieka bez uruchamiania
  weryfikatora zamiast zamieniać się w `OUTPUT_LIMIT`. Migracja 205 uzupełnia
  `critic_verified` dla istniejących wywołań z przypiętych migawek, a adopcja
  zgłoszenia agenta używa werdyktu krytyka, jeśli ten zdążył go zapisać.
- Trzecia runda poprawek przeglądu B3.5. Zatwierdzone zadanie z Testem albo
  Krytykiem, po którym następuje skrypt, wysyłka, sygnał, zadanie ręczne, odbiór,
  link albo koniec Terminate, nie kończy się już odmową walidatora historii i
  incydentem nad skończoną pracą: walidator przyjmuje `verification_passed` z
  `{"mode","reference"}` i ponownie dowodzi zatwierdzenia na zapisanej obserwacji
  (także braku wycofania). Zatwierdzenie, któremu nie wolno ufać, zostawia w
  historii zdarzenie `verification_withdrawn` przed zadaniem dla człowieka (nowa
  etykieta w pięciu językach). Wycofanie krytyka obejmuje jego harness, także przy
  adopcji i odroczonym przyjęciu. Harness krytyka nie może zawierać `loop`, `map`
  ani `subflow` (silnik nie przypina ciał, więc sprawdzenie ciała nie mogłoby
  obowiązywać). Odrzucenie od wycofanego weryfikatora stoi tak samo w trakcie
  wykonania i przy przyjęciu; tylko zatwierdzenie trafia wtedy do człowieka.
- Czwarta runda poprawek przeglądu B3.5. Walidator przyjęcia wyniku sam wylicza
  decyzję planera dla ukończonego wyniku z Testem albo Krytykiem: wynik, któremu
  nikt nie dowiódł zatwierdzenia (albo którego weryfikator wycofano), musi mieć
  dokładnie jedno zadanie weryfikacji na tokenie zadania i nie może mieć
  `verification_passed`, więc plan dostarczony nie obejdzie człowieka, pomijając
  zdarzenie. Próbny plan wykonawcy widzi wycofanie zatwierdzenia tak jak przyjęcie
  (zatwierdzenie wycofane między sprawdzeniem etapu a przyjęciem trafia do
  człowieka zamiast do incydentu `TRANSITION_ERROR`). Odmowa krytyka przed pracą
  ma własny kod `VERIFICATION_AGENT_REFUSED` (nie `SOURCE_ACCESS_REVOKED`), a
  komunikat dla `loop`, `map` i `subflow` mówi, że ich ciało nie jest przypięte;
  podpowiedź krytyka w pięciu językach opisuje ograniczenia harnessu.
- Tłumacz wyrażeń JUEL/FEEL → CEL (B3.6a). Warunki z plików Camundy są
  tłumaczone na CEL z ostrzeżeniami (`EXPR_*`) albo odmową z kodem i pozycją;
  nic nie jest zgadywane po cichu.
- Import plików Camundy (B3.6b). Okno „Importuj” czyta tolerancyjnie pliki
  z przestrzenią `http://camunda.org/schema/1.0/bpmn`: tłumaczy warunki,
  przypisania, timery i podprocesy, a to, czego nie da się przenieść, zgłasza
  jako diagnozy z kodami `CAMUNDA_*` (błąd, ostrzeżenie, informacja). Import
  niczego nie zapisuje; wynik trafia do szkicu dopiero po „Importuj jako
  szkic”. Zadania usługi bez znanej implementacji dostają stan „bez
  implementacji”: szkic się zapisze, ale publikacja jest odrzucana
  (`UNBOUND_IMPLEMENTATION`), dopóki nie wybierzesz bloku, narzędzia albo
  agenta. Plik z jakimkolwiek błędem krytycznym nie daje modelu. Zadanie
  użytkownika z wielokrotnym wykonaniem, którego zmienna elementu jest też
  używana w przypisaniu, jest odrzucane. Przerywające zdarzenie brzegowe na
  podprocesie, który kopiuje zmienne na zewnątrz, daje ostrzeżenie, bo po
  przerwaniu kopiowanie nie następuje. Ostrzeżenia importu żyją tylko w bieżącej
  sesji okna (przycisk „Kopiuj raport”).
- Symulacja zadań usługi i weryfikacji (B3.7a). Krok usługi w symulacji czeka na
  wynik podany przez operatora (Zakończony albo Błąd z kodem; pozostałe wyniki
  tylko przy kroku z wyrażeniem wyniku) albo na wynik przygotowany przy starcie.
  Przejścia po wyniku, werdykty Testu i Krytyka oraz decyzje człowieka w
  weryfikacji idą tym samym kodem co produkcja; wszystko zostaje w prywatnej
  bazie symulacji. Okno pokazuje wejście kroku, formularz wyniku, incydenty
  słowami i ślad ze zlokalizowanymi zdarzeniami. Wykonanie kroku „naprawdę” jest
  w protokole, ale odrzucane (`SIMULATION_REAL_EXECUTION_UNSUPPORTED`); kroki
  agenta i zadania usługi z wejściem/wyjściem działania są w symulacji
  odrzucane z nazwą kroku. Wiadomości, sygnały, podprocesy, wywołania i
  powtórzenia nadal poza symulacją (B3.7b/c).
- Symulacja zdarzeń (B3.7b). Symulacja obsługuje timery brzegowe
  (przerywające i nieprzerywające), wiadomości (catch, throw, zadania wysyłania
  i odbioru, zdarzenia brzegowe), sygnały rzucane przez sam model oraz wyścigi
  bramki zdarzeń, wszystko na prywatnej bazie i wirtualnym zegarze. Operator
  wysyła wiadomość z kluczem korelacji z listy oczekujących kroków (albo
  wpisuje nazwę), a niejednoznaczną wiadomość adresuje dokładnie jednym
  oczekiwaniem. „Przesuń czas wirtualny” skacze do najbliższego terminu
  (timer, wiadomość, wygaśnięcie), nie dalej niż horyzont (kod
  `SIMULATION_HORIZON_REACHED`). Sygnału nie da się wysłać z zewnątrz do
  symulacji, a start wiadomością jest odrzucany. Przy okazji naprawiono błąd
  produkcyjny: nieprzerywający timer brzegowy, na którego ścieżce jest rzucenie
  sygnału, nigdy się nie odpalał.
- Symulacja struktury (B3.7c). Symulacja obsługuje podprocesy osadzone,
  powtórzenia (wielokrotne instancje równoległe i sekwencyjne oraz pętle) i
  wywołania procesów. Opublikowane wersje wywoływanych procesów są czytane
  z produkcji jednorazowo przy starcie i przypinane w prywatnej bazie, więc
  późniejsza archiwizacja nie zmienia trwającego przebiegu. Okno pokazuje
  panel struktury (drzewo wywołań, zakresy, grupy powtórzeń z licznikami),
  numer wystąpienia na kartach zadań, nazwy kroków wywoływanego procesu z jego
  własnego modelu oraz zadania innych wykonawców tylko do odczytu. Limity
  (32 zakresy, 32 grupy powtórzeń, 128 wystąpień, 8 instancji w drzewie
  wywołań) kończą się zrozumiałym komunikatem w pięciu językach. Przebieg
  z kodem wykonawczym `RealExecution` nadal jest odrzucany.
- Polityka ponowień per krok BPMN (B4.6): zadanie usługi może mieć `retry`
  (1–10 ponowień, odstęp od 1 s do 24 h, stały albo rosnący z mnożnikiem i
  limitem). Krok bez polityki serializuje się bajt w bajt jak dotąd, więc
  opublikowane modele zachowują swoje sumy SHA-256. Silnik sam ponawia tylko
  awarię, która dowodnie nic nie zrobiła na zewnątrz; niepewny wynik, przekroczony
  czas po wysłaniu i nieudane mapowanie wyniku nigdy nie są ponawiane
  automatycznie, a ręczne ponowienie liczy się do limitu. Jedna awaria to jeden
  incydent: migracja 210 dodaje do `bpmn_incidents` liczbę prób, limit i termin
  kolejnej próby, więc restart w trakcie oczekiwania nie gubi ani nie podwaja
  ponowienia, a osoba dostaje powiadomienie raz, dopiero gdy silnik rezygnuje.
  Historia ma zdarzenia `job_retry_scheduled`, `job_retries_exhausted` i
  `job_retry_stopped`, a incydent pokazuje `attempts` (wykonane, limit, termin).
  Import `failedJobRetryTimeCycle` (`R<n>/PT…`) mapuje się na politykę, cykle
  nieregularne na najbliższą z ostrzeżeniem. Symulacja uruchamia krok raz i mówi o
  tym w formularzu. Inspektor ma sekcję ponowień w zakładce „Czas”
  (pięć języków); przy kroku agenta ostrzega, że każde ponowienie uruchamia agenta
  od nowa i zużywa jego model, czas i budżet.
  Poprawki po przeglądzie B4.6: ponowienie odłożone, bo źródło lub aktywacja są
  chwilowo niedostępne, nie zużywa limitu (zdarzenie `job_retry_deferred`, termin
  przesuwa się o opóźnienie polityki, po 20 odłożeniach albo 24 h silnik rezygnuje
  z powodem `deferral_limit`); przejściowy błąd bazy (zajęta, zablokowana, I/O)
  nie rozbraja ponowienia, a nieudane rozbrojenie nie blokuje reszty strony;
  `job_retries_exhausted` zapisuje się raz, przy przejściu z uzbrojenia, a zatrzymanie
  przez kod (np. niepewny wynik) zapisuje `job_retry_stopped` z powodem; rozwiązany
  incydent traci termin; opóźnienie wykładnicze zaokrągla w górę, więc 1 s rośnie;
  import Camundy czyta też listę `PT10M,PT17M,PT20M` (jeden interwał to jedno
  ponowienie), oznacza przycięte i nieregularne cykle jako przybliżone i ostrzega
  (`CAMUNDA_RETRY_CYCLE_NO_JOB`), gdy cykl leży na zadaniu bez joba; UI pokazuje
  bezwzględny termin zamiast „za 30 s”; poprawiono podpowiedź o ponowieniach
  w pięciu językach.
- Bramka AI i limity tokenów. Wyczerpany limit tokenów zatrzymuje teraz wywołanie
  modelu w każdym przepływie (zwykłym, systemowym i strumieniowym), a nie tylko w
  kroku procesu; wcześniej zwykły przepływ tylko logował odmowę i działał dalej bez
  wpisu audytu. Odmowa wewnątrz przepływu dociera do klienta jako HTTP 429, nie 500.
  Embeddingi z przepływów przechodzą przez bramkę (limit i audyt), księgują tokeny
  embeddingów dokładnie raz i nie zwiększają puli tokenów czatu; audyt zapisuje
  tylko podsumowanie wejścia, nie jego treść. Bramka używa identyfikatora noda z
  mesh (jak dzierżawy tokenów) zamiast nazwy hosta; wcześniejsze wiersze zostają
  pod nazwą hosta. Znane luki: `/v1/embeddings` bez sprawdzenia limitu na poziomie
  sesji, indeksowanie Projektów i embeddingi aliasu z `service_call` poza bramką,
  możliwe podwójne księgowanie zużycia w przepływie opublikowanym jako model czatu.

### TentaNas

- Lista dozwolonych targetu iSCSI/NVMe-oF jest zapamiętywana w bazie
  (migracja 30: `nas_targets.allowlist_mode` i `nas_targets.open_confirmed`).
  Target z listą dozwolonych, której ostatni wpis usunięto, jest **zamknięty
  dla wszystkich**: nikt nowy się nie zaloguje (także po restarcie węzła czy
  przebudowie grupy portali). Na iSCSI sesje usuniętych klientów są zrywane;
  na NVMe-oF hosty połączone w tej chwili zachowują dostęp, dopóki nie połączą
  się ponownie (aby odciąć je od razu, zatrzymaj target).
  Usunięcie ostatniego wpisu (w edytorze listy albo „Rozłącz … i usuń z listy”)
  nie jest już odrzucane (`refusal:target_last_initiator` wysyłają tylko
  starsze węzły) i wymaga potwierdzenia na ekranie.
- „Otwórz dla wszystkich”: jedyna świadoma droga od listy dozwolonych do
  targetu otwartego, z ostrzeżeniem i przepisaniem nazwy targetu. Usuwa listę
  i nieużywane wpisy w jądrze; odblokowuje też target, który jądro trzymało
  zamknięty wbrew zapisowi. Niedostępne dla NVMe-oF z DH-HMAC-CHAP.
  Protokół: `TargetOpenRequest` (`target_id`, `confirm_name`,
  `expected_updated_at` — okno, które widziało starszą wersję targetu, dostaje
  `refusal:target_open_changed`), `TargetGetResponse.open_blocked`,
  `NasTarget.allowlist_mode`.
- Nowe alerty: `target_left_disabled` (krytyczny: target po zerwaniu sesji
  pozostał wyłączony), `target_open_refused` (jądro trzyma listę dozwolonych,
  której nie ma w zapisie), `target_sessions_reset` (ostrzeżenie: wszystkie
  sesje targetu zresetowano raz, bo klient spoza listy był zalogowany).
- Eksport konfiguracji z zamkniętym targetem o pustej liście ma schemat 2 —
  węzeł starszej wersji odrzuca taki plik w całości, zamiast otworzyć target.
  Pozostałe eksporty mają nadal schemat 1.
- `tentanas-helper` ma wersję `0.17.3`; operacje na targetach wymagają tej
  wersji (bramka wersji odrzuca inną) — po aktualizacji ponów nadanie uprawnień
  systemowych.
- Aktualizacja: kolumna „Czas trwania” sesji targetów zaczyna się jednorazowo
  od nowa dla sesji aktywnych w chwili aktualizacji (klucz sesji zawiera teraz
  wcielenie grupy portali, aby zatrzymanie i wznowienie targetu nie przedłużało
  starej sesji).
- Komunikaty `tentanas-helper` są po angielsku, a te, na które administrator
  może zareagować, helper wysyła jako kody odmowy (`refusal:<kod>?…`, jak rdzeń
  od fali 13): czyszczenie dysku (zamontowany, swap, członek puli ZFS lub
  macierzy md, zajęty, tylko do odczytu, sygnatury po czyszczeniu, dziennik
  macierzy), rezerwacje Elastic przy operacjach ZFS (nazwa, punkt montowania,
  dysk), brak lub zmiana dysku macierzy, dysk zajęty lub z sygnaturą przy
  tworzeniu i dodawaniu, restart węzła przed operacją (najpierw
  „Odtwórz montowania”) oraz wyniki napraw i przerwane przebiegi SnapRAID
  w historii macierzy. Ekran tłumaczy te kody w pięciu językach; zdanie helpera
  trafia do podpowiedzi i do dziennika zadania. Rdzeń przekazuje kod helpera
  bez zmian jako błąd zadania i jako odpowiedź operacji wykonywanych od razu
  (np. tworzenie datasetu lub zvola), bez przedrostka `tentanas-helper:`.
  Wiersze zapisane przez starszego helpera
  (po polsku) wyświetlają się tak jak dotąd.
- `tentanas-helper` ma wersję `0.17.4` (bramka wersji wymaga dokładnie tej
  wersji) — po aktualizacji ponów nadanie uprawnień systemowych. Nagłówek
  pliku reguł audytu NFS zmienia się na angielski przy następnym zapisie.

### TentaBus

- PowerUser może edytować flow: `FlowUpdateRequest`, `FlowVersionRestoreRequest`
  i `FlowFactoryRestoreRequest` wymagają teraz roli PowerUser lub Admin (wcześniej
  tylko Admin). Pozostałe ograniczenia bez zmian: flow systemowe są tylko do
  odczytu, domyślny flow czatu musi być aktywny, kolizje nazwy publikacji są
  odrzucane, a każde sprawdzenie konta usługowego nadal wymaga, by zapisujący
  osobiście miał prawa do tematu. User i Viewer nadal są odrzucani.
- Odbiór z magistrali: gdy kolejka błędów (DLQ) nie przyjmuje jeszcze zapisu,
  odbiór czeka 2 s i wraca ze stanem „oczekuje” (powód `dlq_unavailable`) zamiast
  w pętli uruchamiać flow od nowa; odmowy zapisu flow z kontem usługowym niosą
  nazwę węzła i klucza (nie identyfikator) i są tłumaczone w interfejsie na pięć
  języków; import środowiska zwraca nazwy flow zapisanych jako szkice, a interfejs
  je pokazuje.
- Flow z historią uruchomień nie da się usunąć: `FlowDeleteRequest` odmawia przed
  jakimkolwiek zapisem (zamiast kończyć się błędem klucza obcego w bazie) i
  proponuje dezaktywację; historia wykonań zostaje nietknięta, migracji brak.
  Flow bez historii usuwa się jak dotąd i nadal zapisuje audyt odpięcia konta.
- Naprawa: flow z węzłem `bus_consume` nigdy niczego nie czytał, bo reaktor
  konsumował bez tożsamości, a autoryzator odmawia wołającemu bez aktora. Teraz
  węzeł wskazuje konto usługowe (`service_account`: uid aktywnego klucza ogólnego,
  nigdy klucza użytkownika ani grupy) i flow czyta, publikuje (`bus_publish`) i
  zapisuje do DLQ jako ten klucz, z jego prawami do tematów i regułami ukrywania
  pól; w audycie występuje jako `api_key:<uid>`. Konsumuje w grupie klucza
  `k:<uid>.<grupa>` (autor wpisuje samą grupę). Nowo przypisane konto czyta temat
  od najstarszego zachowanego rekordu. Zapis flow z takim węzłem (tworzenie,
  edycja aktywnego flow, przywracanie wersji i fabryczne) sprawdza przy każdym
  zapisie, że zapisujący (PowerUser lub Admin) sam ma odczyt tematu i zapis na
  każdy temat `bus_publish`, w swojej organizacji, że klucz istnieje, jest aktywny i
  ogólny, ma te same prawa, i odmawia `create_if_missing` (klucz nie tworzy
  tematów); powiązanie trafia do audytu jako `flow.service_account.bind`. Flow bez
  konta usługowego (zapisane przed zmianą) pozostaje bez odbioru z widocznym
  powodem; nic nie jest uzupełniane automatycznie. Broker zapisuje list martwy
  (`on_error = dlq`) dla klucza własnym kontekstem wewnętrznym, tak jak kwarantannę
  schematu, dopiero po sprawdzeniu odczytu klucza na temacie źródłowym; klucz nadal
  nie ma dostępu do tematów `__`. Stan odbioru per węzeł (działa, oczekuje,
  wstrzymany z powodem, zatrzymany) jest w `FlowSummary`/`FlowDetail`
  (`bus_consumer`, dopisane na końcu z `#[serde(default)]`, `SCHEMA_VERSION`
  bez zmian), na liście flow, na węźle i w inspektorze; wstrzymany odbiór wznawia
  się sam w ciągu 5 s po naprawie przyczyny, a zatrzymany (`halt`) po ponownym
  zapisie flow. Nowe żądanie `ServiceAccountListRequest` (PowerUser) zwraca do
  listy wyboru tylko uid i nazwę aktywnych kluczy ogólnych. Flow działający jako
  konto usługowe nie może mieć `bus_publish` w podflow, pętli ani mapie (ani
  węzła `agent`): klucz działa jedną tożsamością w całym przebiegu, więc zapis
  odmawia, a `SubflowRunner` odmawia w czasie wykonania ciała edytowanego po
  powiązaniu. Import środowiska sprawdza flow tak jak zapis; flow, który nie
  przejdzie, trafia jako szkic (audyt `flow.service_account.import_demoted`),
  a zreplikowany zapis niesie ten status, nazwę, opis i typ usługi. Sprawdzenie
  idzie szerzej niż węzły najwyższego poziomu: przeszukiwanie podflow, pętli i map
  jest wszerz (płytsza ścieżka wygrywa), a brakujący lub nieczytelny podflow to
  odmowa (przy imporcie dzieci bierze się najpierw z paczki). Węzły uruchamiające
  agentów (`agent`, `agent_context`, `spawn`, `await_subagents`) są odrzucane, a
  `core.agent_spawn` odmawia w czasie wykonania dla przebiegu konta usługowego;
  model będący opublikowanym flow też nie jest dla niego dostępny. Flow dostarczony
  przez addon nie może wskazać konta usługowego. Zapis sprawdza też regułę
  ukrywania pól po stronie zapisu dla celów `bus_publish`, a błąd odczytu reguły
  jest błędem zapisu. Audyt zna `flow.service_account.unbind` (usunięcie
  powiązania, dezaktywacja, usunięcie flow) i porównuje węzeł, klucz, temat,
  instancję i organizację. Ostrzeżenie o nowej grupie porównuje z ostatnio
  zapisanym flow, nie z edytowanym węzłem. Znane ograniczenia: lista kluczy
  nie jest zawężona do organizacji (klucze nie mają organizacji), a stan
  „wstrzymany” jest tylko w pamięci procesu (po restarcie reaktor ustala go od
  nowa). Bez migracji.
- Korelacja end to end (TB-02, backend): jeden `correlation_id` przechodzi przez
  łańcuch temat → przepływ → temat. Nagłówek rekordu `correlation-id` (ASCII
  `[A-Za-z0-9][A-Za-z0-9._:-]{0,127}`) broker waliduje i zapisuje jako
  `tf.correlation_id`; nagłówki `tf.*` od wołającego nadal są usuwane, a
  `x-correlation-id` i `traceparent` nie są wejściem. Reaktor przenosi id rekordu do
  przebiegu (partia z mieszanymi id dostaje nowe id partii i `bus_correlation_ids` w
  meta), przebiegi z magistrali trafiają teraz zawsze do osi czasu zdarzeń, a ponowienie
  z DLQ zachowuje oryginalne id. REST przyjmuje i zwraca nagłówek `X-Correlation-Id`
  (nieprawidłowy daje 400; bez niego każdy rekord dostaje własne id, a odpowiedź
  publikacji niesie `correlation_id` i `correlation_ids`); wynika z tego, że wyrażenie
  `idempotency_key` czytające nagłówek `correlation-id` nie rozpozna ponowionego
  żądania REST bez `X-Correlation-Id`, bo każde ponowienie dostaje nowe id. Wartości
  nagłówków w NDJSON są kodowane base64. Dashboard nadaje UUID na operację zamiast
  numeru ramki WS. Sub-agenty uruchamiane reaktorem zapisują teraz oś czasu zdarzeń.
  `MessagesBrowseRequest` ma opcjonalny `correlation_id` (ograniczony skan: jeden
  budżet 10 000 rekordów albo 8 MiB na całe żądanie, wspólny dla partycji;
  `next_offset` partycji mówi, dokąd doszedł, a partycja, do której budżet nie
  dotarł, zgłasza swój początek), a `FlowExecutionSummary` — `request_id`,
  `correlation_id` i `origin`. Widoczność zdarzeń: przeglądarka jest zawsze
  ograniczona do organizacji wołającego (także odczyt osi czasu i treści kroku
  pojedynczego przebiegu); poza nią czyta tylko administrator platformy (wcześniej
  `events.read_all` pozwalało czytać cudzą organizację przez `org_id` w żądaniu).
  Zwykłe `events.read` nadal widzi własne przebiegi, także bez organizacji. Przebiegi
  z magistrali nie zapisują treści kroków (`step_io`) ani lustra audytu w outboxie;
  zostają oś czasu i czasy kroków.
- Korelacja w interfejsie (TB-02.4): Zdarzenia mają widoczne pole „Korelacja” (to samo
  filtrowanie co `?correlation=`) i sekcję korelacji w szczegółach przebiegu (kopiowanie,
  współrzędne wiadomości z magistrali z linkiem „Otwórz w TentaBus”, „Pokaż wszystko z tą
  korelacją”). Podgląd wiadomości TentaBus ma kolumnę „Korelacja”, filtr szukający od
  numeru „Od numeru”, link do Zdarzeń w szczegółach wiadomości, a gdy budżet przeszukania
  się wyczerpie — „Przeszukano do numeru N” i „Szukaj dalej od N+1” zamiast „brak wyników”.
  Adres `#/tentabus?topic=…&corr=…&partition=…&offset=…` otwiera podgląd na rekordzie.
  Historia wykonań przepływu ma kolumnę „Korelacja” i link „Oś czasu →”. Zdarzenie
  rozpoczynające przebieg z magistrali zapisuje `bus_source` (instancja, topik, partycja i numer
  rekordu, bez treści).
- Dashboard: ukrywanie danych na tematach z wzorem **Avro, Protobuf albo Thrift**
  (F4 B6 UI). Blokada „treść binarna” zostaje tylko dla tematu bez wzoru, z wycofanym
  wzorem, z nieczytelną listą pól albo na starszym serwerze (`binaryFieldActions`
  w możliwościach); każdy powód ma własny tekst. Okno zasady listuje pola
  najwyższego poziomu z `SchemaFieldsGet` (z typami z tekstu wzoru), odczyt to
  „Pokaż / Ukryj” („Zamaskuj” i „Zahaszuj” są widoczne, ale zablokowane z wyjaśnieniem),
  zapis „Dozwolone / Wymagane / Niedozwolone” z objaśnieniem, co znaczy „ustawione”
  w danym formacie, i ostrzeżeniem, gdy pole Avro bez wartości domyślnej i bez null
  jest „Niedozwolone”. Podgląd „jak widzi…”, przeglądarka wiadomości i okno
  nieprzetworzonej wiadomości pokazują treść binarną jako bajty, numer wzoru
  (`schemaRefId`) i „Pokaż wzór” (`SchemaForRecordGet`); rekord wstrzymany jest
  nazwany. Nowe odmowy (maska/hasz, wzór wycofany lub niepodpięty, pole spoza wzoru,
  zmiana lub odpięcie wzoru pod zasadami, nowa wersja bez pola wymaganego przez
  zasadę zapisu) są opisane po polsku i w czterech pozostałych językach.
- Zasady ukrywania danych działają na tematach z wzorem **Avro, Protobuf albo
  Thrift** (F4 B6). Odczyt (`peek`, `fetch`, REST rekordów, funkcje hosta addonów,
  reaktor/bloki flow, lista „nieprzetworzonych wiadomości”, podgląd „jak widzi X”)
  dekoduje rekord wersją wzoru, którą go zapisano (id wpisane przy publikacji;
  rekord bez id nie wskazuje żadnej wersji i jest wstrzymany), pomija pola, których
  zasada nie pokazuje, i zwraca rekord w tym samym formacie i protokole, taki jak
  napisany pod **pod-schematem** wyprowadzonym dla widocznych pól: Protobuf —
  bez tagów ukrytych pól, Thrift — bez ich nagłówków (w TCompact delty id kolejnych
  pól są liczone od nowa), Avro — bez bajtów ukrytych pól (kolejność pól i gałęzie
  unii bez zmian). Dotyczy pól najwyższego poziomu. Rekord niepoprawny według
  wzoru zapisu, z nieznaną wersją albo z wzorem, który się już nie kompiluje, jest
  **wstrzymany** (pusty ładunek, id 0), nigdy pokazany. Maska i hasz nadal są
  odmawiane na tematach binarnych (`masking and hashing do not apply to a topic
  whose messages are binary`). Temat bez zasad nie płaci niczego; skompilowane
  wersje i teksty pod-schematów są w pamięci podręcznej.
- Rekord niesie id schematu, pod którym jest zapisany: `schema_id` w JSON REST
  rekordów, `schema_ref_id` w podglądach wiadomości i nieprzetworzonych
  wiadomości (`#[serde(default)]`), `schema_id` w rekordzie funkcji hosta
  (`BusRecordOut`, opcjonalne). Dla czytelnika z zasadą to id jego pod-schematu;
  tekst pobiera przez `SchemaForRecordGetRequest` (WS) albo
  `GET /v1/bus/instances/{instance}/topics/{topic}/schemas/{schema_id}?org_id=`
  (to samo prawo co odczyt tematu). Czytelnik z zasadą dostaje wyłącznie swój
  pod-schemat, nigdy pełny wzór; id po zmianie zasady nie jest już obsługiwane
  (`bus.schema_not_found`). `SchemaFieldsGetRequest` zwraca nazwy pól najwyższego
  poziomu wzoru binarnego.
- Zapis: zasada zapisu na temacie binarnym sprawdza pola, które rekord **ustawia**:
  Protobuf — tagi na drucie, Thrift — id pól, Avro (gdzie każde pole jest zawsze
  na drucie) — pola, których wartość jest inna niż domyślna albo `null` w
  nullowalnej unii (pole bez domyślnej i bez `null` jest zawsze ustawione, więc
  musi być dozwolone). Rekord niepoprawny według wzoru to `FieldPolicyPayloadMalformed`.
- Nowe odmowy: zasada na temacie z wzorem wycofanym albo z typem treści
  Avro/Protobuf/Thrift bez powiązanego wzoru (`field policies on a topic whose
  messages are binary need a linked message pattern that is not withdrawn`),
  nazwa pola spoza wzoru, zmiana albo odpięcie wzoru tematu, który ma zasady,
  oraz nowa wersja wzoru bez pola, które zasada zapisu wymaga
  (`registry::VERSION_DROPS_REQUIRED_FIELD`). Zasady już zapisane działają dalej
  po wycofaniu wzoru. Bez migracji i bez podbicia `SCHEMA_VERSION`.
- Poprawki po przeglądzie F4 B6 (R1): zasada nie może pokazać pola, którego typ
  sięga z powrotem do wzoru (rekurencja przez korzeń, np. `next`), gdy ukrywa
  jakiekolwiek inne pole — zagnieżdżona kopia korzenia niesie ukryte pola; taki
  rekord (zasada sprzed nowej wersji wzoru) jest wstrzymany. Zasada odczytu na
  temacie binarnym wymaga, by temat sprawdzał wiadomości (`validation` inne niż
  `off`), a temat z taką zasadą nie przechodzi na `off`: rekord bez id wersji nie
  jest czytany wersją bieżącą. Zasada zapisu, której nie spełni żaden rekord
  (pole Avro bez domyślnej i bez `null`, niedozwolone), jest odmawiana. Zasada
  zapisu nie dotyczy tematu `__dlq.*` (zapis brokera). Czytelnik, któremu
  zasada odczytu ogranicza temat z wzorem binarnym, nie dostaje pełnego wzoru z
  `SchemaGetRequest` ani `SchemaFieldsGetRequest` (tylko administrator tematu);
  zgadywanie id pod-schematu kosztuje jeden skan wersji na zmianę rejestru.
- Poprawki po przeglądzie F4 B6 (R2): zasada zapisu jest pomijana tylko dla
  zapisu brokera do `__dlq.*` (martwy list nieudanej dostawy, kwarantanna), nie
  dla każdego publish na temat o takiej nazwie — publish użytkownika na
  `__dlq.x`, którego wiąże zasada zapisu tematu `x`, jest odmawiany
  (`PermissionDenied`). REST rejestru schematów (`GET .../versions/{n}`) nie
  oddaje pełnego wzoru Avro/Protobuf/Thrift kluczowi, któremu zasada odczytu
  ogranicza temat z tym wzorem (403, klucz nie ma wyjątku administratora).
  Protobuf i Thrift: zasada nie może pokazać pola sięgającego do korzenia nawet
  wtedy, gdy pokazuje wszystkie pola wersji (rekord może nieść tag nowszej
  wersji w zagnieżdżonej kopii). Odmowa wyprowadzenia pod-schematu jest
  pamiętana (jedno ostrzeżenie zamiast jednego na odczyt). Strażnik
  `validation=off` pod zasadą odczytu działa tylko, gdy aktualizacja dotyka
  `validation`.

- Zasada odczytu może pokazać pole **zamaskowane** albo **zahaszowane** (F4 B7;
  tylko JSON, XML i HL7 v2 — w Avro, Protobuf i Thrift zasada nadal jest
  odmawiana, więc maska i hasz też). Maska zostawia ostatnie 3 znaki, resztę
  zastępuje gwiazdkami z zachowaniem długości, a wartość krótszą niż 5 znaków
  maskuje w całości (liczone są znaki Unicode, nie bajty). Hasz to
  `blake3::keyed_hash(klucz organizacji, wartość)`, pełne 256 bitów w hex; ta
  sama wartość ma ten sam hasz we wszystkich instancjach organizacji. Klucz
  (32 losowe bajty) powstaje przy pierwszej zasadzie, która haszuje, leży w
  bazie zaszyfrowany i związany z identyfikatorem organizacji (migracja 206:
  `bus_field_hash_keys` oraz kolumny `masked_fields_json` i `hashed_fields_json`
  zasad), a między węzłami jedzie zapieczętowany dla każdego odbiorcy w
  `SharedSecretsSync` (nigdy w księdze synchronizacji, której treść jest
  jawna); wygrywa wyższa wersja klucza, a przy równej wersji wcześniej
  utworzony klucz (patrz poprawki R1 niżej). Rotacja klucza to osobne, jawne
  żądanie administratora instancji busa (`FieldHashKeyRotateRequest` z wersją
  klucza z okna potwierdzenia, audyt `bus.field_policy.hash_key.rotate`): po niej
  stare hasze przestają pasować w każdej instancji organizacji. Pola `masked_fields`
  i `hashed_fields` (podzbiory `fields`, rozłączne, tylko zasada odczytu) doszły
  do zapisu i listy zasad; `FieldHashKeyInfoRequest` zwraca wersję klucza i liczbę
  zasad, topików i instancji, których zmiana klucza dotyczy (na tym węźle).
  Wartość inna niż tekst w JSON staje się tekstem (`12345` → `**345`),
  `null` i pusty tekst zostają, wartość logiczna i obiekt/tablica dostają stałą
  maskę `***`; w HL7 całe pole (z `^`, `~`) jest jednym tekstem, a znak
  rozdzielający wiadomość nie może trafić do maski; w XML element zostaje, a jego
  tekst (bez atrybutów i znaczników potomnych) zostaje zastąpiony. Węzeł sprzed
  zmiany ukrywa takie pole zamiast je pokazać, bo kolumna `fields_json` zawiera
  tylko pola pokazywane bez zmian. Podgląd „jak widzi X” i `capabilities`
  (`field_actions`) znają akcje `mask` i `hash`. `SCHEMA_VERSION` bez zmian.

- Poprawki recenzji F4 B7 (runda 1), zmiany zachowania:
  - **Klucz haszowania nie powstaje przy odczycie.** Tworzy go zapis pierwszej
    zasady, która haszuje, i tylko na węźle będącym operatorem (inne węzły
    biorą klucz wyłącznie od operatorów, więc klucz zrobiony gdzie indziej
    nigdy by do nich nie dotarł). Odczyt przez zasadę haszującą na węźle, który
    klucza jeszcze nie ma (nowo dołączony, odcięty), **ukrywa pole haszowane**
    do czasu synchronizacji (pozostałe pola i maski działają). Zapis pierwszej
    zasady haszującej i rotacja na węźle, który nie jest operatorem, są
    odmawiane kodem `bus.operator_node_required` zamiast „ok”, po którym węzeł
    haszowałby własnym kluczem. Adopcja klucza od peera: wyższa wersja wygrywa
    zawsze (wersja nie cofa się), przy równej wersji wcześniej utworzony klucz,
    przy równym czasie mniejszy klucz; wersja spoza `u32` jest odrzucana.
    Klucz powstający przy zapisie zasady jest tworzony przed zapisem zasady.
  - **Bus używa klucza głównego węzła.** Szyfr kluczy haszowania pochodzi z
    `AddonManager` (ten sam co w mesh, także na desktopie i mobile, gdzie
    `master.key` leży w katalogu danych), a bus nie czyta już sam pliku, więc
    nie tworzy zbędnego `master.key` w domyślnej lokalizacji. Brak szyfru nie
    zatrzymuje busa: reguły haszujące ukrywają pole (`BusInitConfig.settings_cipher`
    jest teraz `Option`).
  - **DLQ trzyma rekord taki, jaki opublikowano.** `note_delivery_failure`
    czyta surowy rekord z logu po `(temat, partycja, offset)` zamiast ufać
    ładunkowi od wołającego (reaktor przekazywał rzut przez zasadę czytającego:
    zamaskowany i zahaszowany, więc „Ponów” republikował zepsute dane, a odczyt
    DLQ haszował hasz). Rekord podany przez wołającego służy tylko wtedy, gdy
    log go już nie ma (retencja).
  - **Zmiana zachowania odczytu DLQ w `fetch`** (z F4 B7): konsument czytający
    `__dlq.<temat>` widzi teraz zasady i format ładunku tematu źródłowego
    (maska, hasz, XML/HL7 zamiast `{}`) oraz ma ukryty `dlq.error_message`,
    tak samo jak przy `peek`.
  - **Klient sprzed B7 nie zdejmuje masek.** `masked_fields` i `hashed_fields`
    w `FieldPolicySetRequest` to `Option`: brak pola = zachowaj zapisane maski
    i hasze (przecięte z `fields`), `[]` = usuń. Nowy UI wysyła zawsze listy;
    koder JS (`codec.js`) pomija brakujące zamiast wysyłać `[]`.
  - **Podgląd „jak widzi X”** nie haszuje ani nie maskuje drugi raz tego, co
    wywołujący już czyta zamaskowane lub zahaszowane: jego hasz jest prawdziwym
    haszem, a para „maska wywołującego, hasz podmiotu” zostaje maską i jest
    oznaczona `limited_by_caller` (podobnie brak klucza na tym węźle).
  - Drobne: „ponów wszystkie” (`peek_runs` bez ładunku) nie liczy projekcji;
    migracja 206 dostała `CHECK(json_valid ... 'array')` na obu nowych kolumnach
    (istniejące wiersze mają NULL), a materializer odrzuca zasadę z listą, która
    nie jest tablicą nazw; biały znak na brzegach tekstu maskowanego/haszowanego
    elementu XML jest układem dokumentu i jest obcinany; awaria listy kluczy nie
    psuje ramki pozostałych sekretów floty; audyt utworzenia klucza niesie id węzła.
  - **Świadome ograniczenia** (bez zmian kodu): maska liczy znaki Unicode
    (`char`), nie grafemy (ZWJ, NFD i NFC dają różną liczbę odsłoniętych liter;
    wartość nadal pozostaje pod maską); liczby zmiennoprzecinkowe i całkowite
    powyżej u64 są maskowane/haszowane z tekstu zwróconego przez `serde_json`
    (`1.50` → `1.5`), więc ich hasz różni się od haszu tego samego napisu jako
    string; maska i hasz dotyczą tylko pól ładunku, nie klucza rekordu ani
    nagłówków; klucze haszowania jadą do wszystkich zaufanych peerów
    (założenie: jedna flota, jeden operator — mesh nie zna przynależności węzła
    do organizacji); `SCHEMA_VERSION` nie jest podbity (decyzja właściciela:
    węzeł sprzed B7 ukrywa pola maskowane/haszowane, ale nie odrzuca ramek).

- Poprawki recenzji F4 B7 (runda 2 + krytyk GUI), zmiany zachowania:
  - **Edycja zreplikowanej zasady haszującej nie wymaga klucza ani operatora**,
    dopóki nie dodaje nowego pola haszowanego (panel zawsze odsyła pola, które
    zasada już haszuje). Klucz i operator są potrzebne dopiero przy nowym polu
    „Zahaszuj”. Odmowa ma teraz dwa kody: `bus.operator_node_required` (organizacja
    nie ma jeszcze żadnej zasady haszującej, a ten node nie jest operatorem) i
    `bus.hash_key_not_synced` (organizacja już haszuje, a klucz jeszcze nie dotarł
    na ten node; dotyczy też operatora, bo drugi klucz zostałby zastąpiony starszym
    kluczem floty). Tekst odmowy nie zawiera id węzła (trafia do logu).
    `BusFieldHashKeyWire.can_rotate_here` mówi, czy ten node może zmienić klucz.
  - Odczyt przez zasadę haszującą bez klucza loguje ostrzeżenie (raz na minutę na
    organizację), a podgląd „jak widzi…” zwraca `hash_key_missing` osobno od
    `limited_by_caller`; lista `applied` podglądu opisuje to, co faktycznie jest w
    ładunku (para wywołujący-maska/podmiot-hasz, hasz bez klucza = ukryte).
  - Klucz haszowania nie powstaje przed walidacją żądania (`prepare_policy` /
    `commit_policy`). Porządek adopcji klucza ufa znacznikowi czasu tylko w oknie
    wokół zegara lokalnego (zegar z przyszłości albo sprzed 2020 liczy się jak
    „przyszedł przed chwilą”), a klucz z przesuniętym zegarem nie jest już
    odrzucany; klucz, którego węzeł nie umie odszyfrować, nie jest zastępowany
    kluczem starszej wersji.
  - Panel: „Co zrobiły zasady” w podglądzie wymienia tylko pola zmienione (reszta
    jest policzona), w naturalnej kolejności; znaczniki maski i hasza mają etykietę
    tekstową; pytanie o porzucenie szkicu przewija okno do siebie; okno klucza po
    „klucz zmienił się w międzyczasie” bierze aktualną wersję; wzór XSD topiku XML
    podaje pola (dzieci korzenia); francuskie „Caviarder” (maska) odróżnione od
    „Dissimuler” (ukryj).

- Ukrywanie danych w panelu: przy każdym polu zasady odczytu dochodzą „Zamaskuj”
  i „Zahaszuj” obok „Pokaż” i „Ukryj” (F4 B7, UI). Okno Dodaj/Zmień zasadę
  wyjaśnia każdą akcję jednym zdaniem z przykładem maski i mówi, co maska albo
  hasz robi z wartością w formacie tego topiku (JSON, XML, HL7 v2); pola
  wpisywane ręcznie (XML, JSON bez wzoru, dodatkowe adresy HL7) mają osobne listy
  pól zamaskowanych i zahaszowanych. Dostępność akcji bierze się z
  `capabilities.field_actions` serwera — bez niej zostaje samo „Ukryj”; zasada
  zapisu masek nie ma. Tabela zasad pokazuje je jako chipy (zamaskuj, zahaszuj
  obok ukryj), legenda i okno usuwania opisują je tak samo, a „Podgląd, jak
  widzi…” oznacza zamaskowane i zahaszowane wartości w treści (JSON i HL7) i na
  liście pól. Sekcja ma kartę „Klucz haszowania” z datą i wersją klucza organizacji
  oraz — tylko dla administratora instancji — przyciskiem „Zmień klucz
  haszowania”: okno z przepisaniem słów mówi, że stare hasze przestaną pasować
  w każdej instancji organizacji i ile zasad „Zahaszuj” widzi ten node („co
  najmniej”, bo reguły instancji na innych nodach nie są tu liczone); żądanie
  niesie wersję klucza z okna, więc zmiana, którą ktoś zrobił wcześniej, jest
  odmawiana i niczego nie nadpisuje. Przemianowanie pola przy nowej wersji wzoru
  zachowuje maskę i hasz pod nową nazwą. Odmowy serwera (maska na zasadzie zapisu,
  pole zamaskowane poza listą pokazywanych, pole i zamaskowane, i zahaszowane,
  brak klucza, klucz zmieniony w międzyczasie) mają polskie komunikaty w 5 językach.

- Rejestr wzorców waliduje Protobuf (F4 B2; bez nowego crate'a w lockfile:
  `protobuf` i `protobuf-parse` 3.7.2 były w nim już (przez `onnx-ir` z Burn),
  teraz są też bezpośrednimi zależnościami `tentaflow-core`; parser w trybie
  czysto-Rustowym, bez `protoc`, służy tylko do czytania schematu). Wzór to **jeden plik
  `.proto`** (proto3 lub proto2; plik bez `syntax` jest proto2, jak w
  `protoc`), a wiadomością wzorca jest **pierwsza wiadomość najwyższego
  poziomu** albo ta, którą wskaże komentarz `// tentabus:message Nazwa` —
  bez zmian w protokole i bez migracji. Temat o dowolnym typie treści wiąże
  się z wzorcem `protobuf` i może mieć `validation = warn | dlq`: ładunek to
  jedna zserializowana wiadomość (pusty ładunek jest poprawną wiadomością, o
  ile wzór nie ma pola `required`).
- Odrzucane przy rejestracji z jasnym komunikatem: `import` (także typy
  dobrze znane, np. `Any`), `service`, `extend`, `extensions`, grupy, opcje
  własne (`option (x) = ...`), `message_set_wire_format`, `edition`, wzór
  bez wiadomości, komentarz wskazujący wiadomość, której nie ma wśród
  wiadomości najwyższego poziomu, a także to, co parser przepuszcza, a
  `protoc` odrzuca: numer pola poza 1…536870911 lub w 19000–19999, numer w
  zakresie `reserved`, pierwsza wartość wyliczenia proto3 inna niż zero, dwie
  wartości o tym samym numerze bez `allow_alias`, klucz mapy `float`/`double`/
  `bytes`. Dozwolone: zagnieżdżone wiadomości i wyliczenia, `oneof`, `map`,
  `optional`, `repeated`, `reserved`, `package` i opcje, które nie zmieniają
  zapisu na drucie (`deprecated`, `packed`, `json_name`, `java_package`...).
- Parser `protobuf-parse` 3.7.2 jest nieodporny na złośliwy tekst, więc
  poprzedza go kontrola tekstu (przegląd na podstawie pomiarów): parser jest
  rekurencyjny bez limitu głębokości — kilka tysięcy zagnieżdżonych wiadomości,
  agregatów opcji (`{a{a{...`) albo list (`[[[...`) przepełnia stos 2 MiB i
  **przerywa proces**; praca rośnie kwadratowo z liczbą deklaracji (20 000
  wiadomości: 0,86 s; 2 000 wiadomości po 8 odwołań: 1,8 s); dwie wiadomości o
  tej samej nazwie i różnej treści wywołują panikę (`unwrap`) w jego module
  opcji, podobnie jak pole `map<K, V> a` obok typu `AEntry` (wpis mapy), dwie
  mapy o nazwach dających ten sam wpis i grupa obok typu o jej nazwie —
  profil `release` ma `panic = "abort"`, więc taka panika kończy proces, a
  `catch_unwind` pomaga tylko w buildach dev i testowych (dlatego kolizje
  odrzuca kontrola tekstu przed parserem; fuzz 900 000 zmutowanych wzorów nie
  znalazł już paniki); pozwala na `import` wbudowanych plików; nie zna plików
  w pamięci, więc tekst jest zapisywany do prywatnego katalogu tymczasowego, a
  jego komunikaty błędów zawierają ścieżkę tego katalogu. Odrzucane są: nawiasy `{` i `[`
  zagnieżdżone głębiej niż 32, wiadomości zagnieżdżone głębiej niż 16, ponad
  256 wiadomości (mapa też jest wiadomością), 128 wyliczeń, 1024 pola w
  wiadomości i 4096 w całym wzorze, 1024 wartości wyliczenia i 4096 łącznie,
  1024 zakresy `reserved`, 64 pola `required` w wiadomości, 12 288 przypisań
  (`=`), napisy ponad 256 bajtów, nazwy ponad 128 znaków, `(` (opcje własne),
  znaki spoza ASCII poza napisami i komentarzami, grupy oraz powtórzona nazwa
  typu (także wpisu mapy) w jednym zakresie. Wywołanie parsera jest dodatkowo osłonięte
  `catch_unwind`, a jego komunikaty pozbawione ścieżki i skrócone do 200
  znaków (z numerem wiersza).
- Kontrola tekstu Protobuf czyta teraz **ten sam strumień tokenów co parser**
  (leksykalny moduł `protobuf-support` 3.7.2, dodany jako zależność
  korzenia workspace w tej samej wersji, co już była w lockfile). Poprzednia,
  bajtowa, nie widziała komentarza między tokenami: `import /**/ "schema.proto";`
  (rekurencja bez końca), `message /**/ A` powtórzone, `map<…> /**/ a` obok
  `AEntry` i `group /**/ G` obok `message G` omijały ją i kończyły proces.
  Kontrola przechodzi po tokenach z jawnym stosem (plik, wiadomość, oneof,
  wyliczenie), zapisuje nazwy typów z pełnym zakresem, liczy nazwę wpisu mapy
  tak jak parser i odmawia instrukcji, której nie umie sklasyfikować. Mapa w
  mapie jest odrzucana (`a map key or value cannot be a map`). Plik tymczasowy
  ma losową nazwę, a komunikaty parsera tracą ścieżkę także wtedy, gdy jest
  zapisana z podwojonymi ukośnikami (`{:?}` na Windows).
- Znane zaostrzenia walidacji wiadomości Protobuf (świadome): ujemny `int32`
  zapisany na 5 bajtach jest odrzucany (poprawny zapis ma 10), `uint32`/`sint32`
  powyżej 2^32−1 i `bool` powyżej 1 są naruszeniem, numery 19000–19999 są
  odrzucane także nieznane wzorowi, a pole `required` pod-wiadomości, której
  dwa wystąpienia dopiero razem je dostarczają (Protobuf scala je przed
  sprawdzeniem), jest naruszeniem w pojedynczym wystąpieniu. W zgodności
  przeniesienie istniejącego pola do istniejącego `oneof` jest odrzucane
  (oneof zachowuje tylko jedno pole, więc dane drugiego by przepadły), a wzór
  pochodny może mieć do 512 KiB (nazwy typów są w nim pełne).
- Wiadomości są sprawdzane własnym iteracyjnym przejściem, a nie dekoderem
  dynamicznym `protobuf` (ten jest ograniczony długością wejścia, ale buduje
  całą wiadomość w pamięci, nie ma miernika pracy do podziału na paczkę,
  schodzi rekurencyjnie do limitu 100, przyjmuje `bool` równy 2, wartość spoza
  zamkniętego wyliczenia i numery 19000–19999, a o błędzie mówi „Unexpected
  wire type” bez nazwy pola; zostaje wyrocznią w testach: czego odmawia, tego
  musimy odmówić): jawny stos co najwyżej 64 ramek (bez rekurencji, więc
  działa na małym stosie), bez alokacji na wartość; każda
  długość jest sprawdzana względem bajtów pozostałych w wiadomości, zanim cokolwiek
  zostanie przeczytane lub pominięte; zapakowana lista jest obciążana liczbą
  bajtów przed iteracją; każde pole kosztuje jednostkę budżetu (20 mln na
  dokument, wspólne dla paczki), a przekroczenie budżetu albo głębokości to
  `schema_check_too_complex`, nigdy „niepoprawne”. Naruszeniem są: numer pola 0
  lub ponad 2^29-1, numer 19000–19999, typy zapisu grup (3, 4) i niepoprawne
  (6, 7), liczba zapisana na ponad 10 bajtach, typ zapisu niezgodny z typem
  pola (lista zapakowana jest przyjmowana dla powtarzanych pól liczbowych w
  proto2 i proto3), `int32`/`uint32`/`sint32`/wyliczenie poza zakresem, `bool` inny niż 0 i
  1, `string` niebędący UTF-8, wartość spoza **zamkniętego** wyliczenia (proto2),
  brak pola `required`. Przyjmowane są, jak w Protobuf: numery pól
  nieznane wzorowi (są pomijane, z kontrolą ramek), kilka wystąpień pola
  pojedynczego, kilka członków jednego `oneof` i dowolny `int32` w otwartym
  wyliczeniu proto3. Ścieżka w komunikacie to nazwy pól od wiadomości wzorca,
  nigdy wartość.
- Zgodność wersji Protobuf: tożsamością pola jest jego **numer**; nazwy nie mają
  znaczenia na drucie. Pole obecne po obu stronach zachowuje typ (każdy typ prosty
  jest osobnym typem: `int32` → `int64`, `string` → `bytes`, pole pojedyncze →
  `repeated` i wiadomość → mapa są niezgodne), wiadomości są porównywane
  rekurencyjnie po numerach pól, mapa zachowuje typ klucza i wartości. Pole
  tylko po jednej stronie jest pomijane przez czytelnika (dodanie i usunięcie
  pola jest zgodne), z wyjątkiem pola `required` czytelnika, które pisarz musi
  też deklarować jako `required`. Zamknięte wyliczenie czytelnika (proto2)
  musi znać każdą wartość wyliczenia pisarza. Niezależnie od kierunku numer
  zastrzeżony (`reserved`) w starej wersji nie może wrócić w nowej. backward =
  nowy czyta dane starego, forward = stary czyta dane nowego, full = oba;
  porównanie, które nie mieści się w limicie pracy (1 mln jednostek, głębokość
  64), to `bus.schema_compare_too_complex`.
- Wzór pochodny (`derive_subschema`) zostawia dozwolone pola wiadomości wzorca
  (po nazwie) i każdy typ, do którego prowadzą, w tym samym zagnieżdżeniu;
  wiadomość wzorca jest zapisywana pierwsza, więc wynik jest poprawnym wzorcem
  bez komentarza, a jego treść jest deterministyczna. Rzutowanie odczytu dla
  formatów binarnych (polityki pól) to osobny pakiet B6.
- Dashboard: kafel Protobuf w „Dodaj wzór” z podpowiedzią, jak nazwać
  wiadomość wzorca, odmowy po polsku (import i nieobsługiwane elementy,
  numery i nazwy pól, wyliczenia proto3, klucz mapy, limity), powód odmowy
  wersji nazywa pole, oba typy, wartość wyliczenia albo zastrzeżony numer,
  karta „Pola wiadomości” na stronie wzoru (numer, nazwa, typ, obecność) i
  przetłumaczone powody w Nieprzetworzonych (pięć języków).

- Rejestr wzorców waliduje Thrift (F4 B3; bez nowej zależności: crate `thrift`
  to środowisko uruchomieniowe pod kod generowany i nie czyta IDL, więc parser
  podzbioru IDL oraz dekodery obu protokołów są własne). Wzór to **jeden
  plik IDL**, a typem wzorca jest **pierwszy `struct`, `union` albo
  `exception` najwyższego poziomu** albo ten, który wskaże komentarz
  `// tentabus:struct Nazwa` (także `#`). Protokół to TBinaryProtocol albo, po
  komentarzu `// tentabus:protocol compact`, TCompactProtocol (`binary` też
  można napisać wprost; inna wartość jest odrzucana). Temat o dowolnym typie
  treści wiąże się z wzorcem `thrift` i może mieć `validation = warn | dlq`:
  ładunek to **jedna zserializowana struktura** — bez nagłówka `TMessage`, bez
  ramkowania i bez bajtów po niej. Bez migracji i bez zmian w protokole.
- Przyjmowane w IDL: `struct`, `union`, `exception`, `enum` (wartości jawne i
  domyślne, także ujemne i szesnastkowe), `typedef`, `namespace` (czytany i
  pomijany), typy proste `bool byte i8 i16 i32 i64 double string binary uuid`,
  `list<T>` `set<T>` `map<K, V>` zagnieżdżone do 32 poziomów, odwołania do
  typów zapisanych dalej w pliku, jawne numery pól, `required` / `optional` /
  wymagalność domyślna, komentarze `//` `#` `/* */`, adnotacje
  `( klucz = "wartość" )` oraz `const` i wartości domyślne pól — te trzy są
  czytane pod kątem składni w granicach limitów, nie są sprawdzane co do typu
  i nie trafiają do wzoru pochodnego, bo nie zmieniają ani bajtu na drucie.
  Odrzucane z komunikatem nazywającym konstrukcję: `include`, `cpp_include`,
  `service` (z `extends`, `oneway`, `throws`, `void`), `senum`, `slist`,
  `xsd_*`, `cpp_type`, każda inna instrukcja, pole bez jawnego numeru
  (numery domyślne, ujemne, to konwencja kompilatora, której rejestr nie
  odtwarza), numer poza 1…32767, `required` w `union`, nazwa typu z kropką
  (wskazuje plik dołączony), słowo zastrzeżone jako nazwa, powtórzona nazwa
  typu, numer pola, nazwa pola i nazwa wartości wyliczenia, `typedef`
  odwołujący się do samego siebie (także przez kontener), łańcuch `typedef`
  dłuższy niż 32, korzeń, który wymaga samego siebie (wzór bez żadnej
  poprawnej wartości), komentarz wskazujący typ, którego nie ma wśród typów
  najwyższego poziomu. Komunikaty parsera mają numer wiersza (poza błędami
  rozstrzygania nazw typów po przeczytaniu całego pliku) i nigdy nie zawierają
  treści napisu.
- Limity (parser jest całkowity — bez `unwrap` i indeksowania bez sprawdzenia,
  a profil `release` ma `panic = "abort"` — i liczy wszystko, zanim cokolwiek
  zaalokuje): tekst do 256 KiB, 200 000 tokenów, 256 typów, 16 384 wyrażeń
  typów, 1024 pola w strukturze i 4096 łącznie, 1024 wartości wyliczenia i
  4096 łącznie, 64 pola `required` w strukturze, nazwy do 128 znaków i 8
  członów, napisy do 256 bajtów bez znaków sterujących, zerowej szerokości i
  zmieniających kierunek tekstu (komentarze są dowolne), stałe zagnieżdżone do
  16 poziomów i do 16 384 części łącznie, 256 stałych, 64 `namespace`, adnotacje
  do 16 wpisów (1024 łącznie). `typedef` jest nazwą w arenie wyrażeń, nigdy
  kopią, więc łańcuch podwajający mapę w każdym kroku (200 poziomów) kompiluje
  się liniowo (1,2 ms), a jego porównanie kończy się w 2,7 ms.
- Wiadomości są sprawdzane własnym iteracyjnym przejściem (jeden dla obu
  protokołów): jawny stos co najwyżej 64 ramek (struktura, lista lub zbiór,
  mapa — bez rekurencji, więc działa na małym stosie), bez alokacji na
  wartość; każda długość i liczba elementów jest porównywana z bajtami, które
  zostały (liczba także z najmniejszym rozmiarem elementu), zanim cokolwiek
  zostanie pominięte lub przeiterowane; każde pole i element kosztuje
  jednostkę budżetu (20 mln na dokument, wspólne dla paczki), ciągi bajtów
  1/32 jednostki na bajt, ciągi stałych rozmiarów są pomijane w jednym kroku,
  ale obciążone. Przekroczenie budżetu albo głębokości to
  `schema_check_too_complex`, nigdy „niepoprawne”. Pomiary w buildzie
  testowym (bez optymalizacji): bomby długości i liczby elementów odrzucone w
  mniej niż 3 µs, 5000 zagnieżdżonych struktur na stosie 256 KiB w 0,2 ms,
  milion wartości `bool` w 60–120 ms, milion pustych struktur w 220–270 ms,
  20 mln elementów ponad budżet w 1,3 s (`schema_check_too_complex`), napis
  5 MB w 0,2 ms; teksty patologiczne (60 000 zagnieżdżonych `list<`, `[`, `{`,
  200 000 tokenów) odrzucone w mniej niż 4 ms. Fuzz: 1 mln zmutowanych
  wzorów i 4 mln zmutowanych ładunków obu protokołów bez paniki i bez wywołania
  dłuższego niż limit testu; krótkie przebiegi działają w każdym `cargo test`,
  długie (`--ignored`, `TF_THRIFT_FUZZ_ITERATIONS`) na żądanie. Walidator jest
  też sprawdzony dokumentami zapisanymi przez bibliotekę Apache Thrift 0.25
  (Python, oba protokoły), a przyjmowany IDL — ręcznie kompilatorem `thrift`
  0.25.
- Naruszeniem (nie błędem rejestru) są: pole znane wzorowi zapisane innym
  typem zapisu (komunikat nazywa pole), brak pola `required`, `union` z więcej
  niż jednym członkiem (pusty `union` jest poprawny), wartość wyliczenia spoza
  zadeklarowanych (**świadome zaostrzenie**: biblioteki Thrift zachowują
  nieznane numery, rejestr je zgłasza), `string` niebędący UTF-8 (`binary` nie
  jest sprawdzany), `bool` w protokole binarnym inny niż 0 i 1, element `bool`
  w liście zwartego protokołu inny niż 1 i 2, ujemna długość lub rozmiar,
  rozmiar większy niż reszta ładunku, typ zapisu niebędący typem Thrift
  (także w polu nieznanym wzorowi), zmienna długości ponad 3 / 5 / 10 bajtów
  dla `i16` / `i32` / `i64` albo przekraczająca swój typ, numer pola poza
  `i16` w zwartym protokole, bajty po strukturze. Przyjmowane są, jak w
  Thrift: pola o nieznanych numerach (pomijane z kontrolą ramek i głębokości),
  powtórzone pole (ostatnie wygrywa), typy elementów **pustego** kontenera
  nie są porównywane ze wzorem. Ścieżka w komunikacie to nazwy pól od typu
  wzorca (`[]` element, `<key>` / `<value>` wpis mapy, `<unknown>` pole
  nieznane), nigdy wartość ani numer z ładunku.
- Zgodność wersji Thrift: tożsamością pola jest jego **numer**; nazwy nie mają
  znaczenia na drucie. Pole obecne po obu stronach zachowuje typ (typy proste
  muszą być równe — jedyny wyjątek to `string` czytany jako `binary`, nigdy
  odwrotnie, bo `string` jest sprawdzany jako UTF-8; `list` ≠ `set`; kontenery
  są porównywane element po elemencie), `struct`, `union` i `exception` się
  nie mieszają, a czytelnik musi znać każdy numer wyliczenia pisarza. Pole
  tylko po jednej stronie jest pomijane przez czytelnika, z wyjątkiem pola
  `required` czytelnika, które pisarz musi też deklarować jako `required` —
  dodanie pola `required` łamie backward, usunięcie go łamie forward.
  Porównanie, które nie mieści się w limicie pracy (1 mln jednostek,
  głębokość 64), to `bus.schema_compare_too_complex`.
- Wzór pochodny (`derive_subschema`) zostawia dozwolone pola typu wzorca (po
  nazwie) i każdy typ, do którego prowadzą; usunięte pole `required` po prostu
  znika z listy, dozwolone zostaje `required`. Wynik to kanoniczny IDL
  (typ wzorca pierwszy, potem pozostałe w kolejności z pliku, numery pól i
  wartości wyliczeń zapisane wprost, `byte` jako `i8`, komentarz
  `// tentabus:protocol compact` zachowany), bez wartości domyślnych, stałych
  i adnotacji; jest sprawdzany ponownym skompilowaniem (do 512 KiB).
  Rzutowanie odczytu dla formatów binarnych (polityki pól) to osobny pakiet B6.
- Wszystkie sześć rodzajów wzorców ma teraz walidator, więc znika kod dla
  rodzaju „tylko przechowywany”: `stored_only`, `SchemaType::has_validator`,
  `SchemaError::Unsupported`, `BusServiceError::SchemaTypeUnsupported` (kod
  `bus.schema_type_unsupported` i jego tłumaczenie), blokada
  `validation != off` i zgodności dla takiego rodzaju oraz zrzut `Capabilities`
  filtrowany po walidatorze — `schema_types` zawiera teraz wszystkie rodzaje,
  także `thrift`.
- Dashboard: kafel Thrift w „Dodaj wzór” z podpowiedzią (jak nazwać typ
  wzorca i wybrać protokół zwarty), odmowy po polsku (include, service,
  brakujące numery pól, duplikaty, typy bez definicji, cykle `typedef`,
  dyrektywy, limity), powód odmowy wersji nazywa pole, oba typy, numer
  wyliczenia albo zmieniony rodzaj struktury, karta „Pola typu” na stronie
  wzoru (numer, nazwa, typ, obecność) i przetłumaczone powody w
  Nieprzetworzonych (pięć języków).
- Poprawki po przeglądzie Thrift: zmiana protokołu (`binary` ↔ `compact`) między
  wersjami jest **niezgodna** w każdym trybie poza `none` (komunikat „the old
  schema is written in the … protocol, the new one in the …”); komentarz
  zawierający `tentabus:`, który nie jest dokładnie poleceniem w komentarzu
  `//` lub `#` (komentarz blokowy, dokumentujący `///`, polecenie w środku
  komentarza, nieznane polecenie), jest odrzucany zamiast po cichu wracać do
  pierwszej struktury i protokołu binarnego; karta „Pola typu” czyta polecenie
  tak jak serwer; ścieżki w komunikatach zgodności są skracane jak w Avro; dwa
  limity adnotacji mają osobne komunikaty; wzór zapisany, gdy rodzaj był tylko
  przechowywany (lub pod luźniejszym parserem), a dziś się nie kompiluje, jest
  nazwany w odmowie nowej wersji („poprzednia wersja … nie mogła zostać
  sprawdzona”) i przy publikacji na temat, który go sprawdza (`the stored
  schema cannot be compiled under the current rules of its format`), zamiast
  obwiniać nowy tekst; waga skompilowanego wzoru Thrift w pamięci podręcznej
  liczy arenę wyrażeń; `Capabilities.content_types` zawiera teraz
  `application/octet-stream`, więc w kreatorze topiku można założyć topik
  binarny i przypiąć do niego wzór Avro, Protobuf lub Thrift.
- Dashboard (po przeglądzie): limity w odmowach odmieniane przez liczbę („1024
  pola”, „32 poziomy”), skrócona podpowiedź Thrift z resztą pod „Więcej”, okno
  „Nowa wersja” pokazuje podpowiedź formatu (z właściwym numerem wersji),
  ścieżka z jednego członu bez ukośnika, numer wiersza błędu składni w zdaniu
  po polsku, osobne odmowy dla powtórzonego typu, stałej, numeru pola, nazwy
  pola i wartości wyliczenia, „Rejestr nie przyjął” zamiast „Thrift nie
  przyjął”, „Wiadomość nie jest zapisem Thrift” dla ładunku, który nie zaczyna
  się strukturą, i „zwykłe (może nie być)” zamiast „domyślne”.
- Druga runda po przeglądzie Thrift: zasada ukrywania danych na topiku, który
  sprawdza wzór Avro, Protobuf lub Thrift, jest odrzucana przez serwer
  („field policies cannot apply to a topic whose messages are binary”) — zapis
  zawsze kończyłby się błędem, a odczyt dawał pusty wynik; topik z domyślnym
  `application/octet-stream` bez takiego wzoru (stary topik JSON) nadal może mieć
  zasadę. Odmowa zapisanej wersji, której już nie da się skompilować, jest
  pamiętana dla danej treści i generacji rejestru, więc kolejne publikacje nie
  parsują znów do 256 KiB tekstu. Okno „Pokaż” i podgląd wiadomości pokazują
  treść binarną jako „Treść binarna, N bajtów” z widokiem szesnastkowym
  (przesunięcie, bajty, ASCII; pierwsze 512 bajtów); wiadomość odrzucona przy
  zapisie ma jeden wiersz stanu zamiast „Partycja i numer” i „Próby”; ścieżki w
  komunikatach są czytelne („lines › każdy element › sku”) we wszystkich
  formatach; numer wiersza błędu składni stoi w zdaniu przed „Szczegóły są
  niżej”; „Więcej” w oknie dodawania wzoru to nowy komponent `tf-disclosure`
  (używają go też szczegóły techniczne odmów); kreator topiku mówi „treści
  binarnej”; karta „Pola typu Order” nazywa typ; nowe zdania serwera o
  nieczytelnym zapisanym wzorze i o zasadzie na topiku binarnym mają polskie
  odpowiedniki w pięciu językach.
- Trzecia runda po przeglądzie Thrift: podgląd tekstu uciętego w środku znaku
  (serwer tnie podgląd na 4096 bajtach) nie jest już „binarny”, a uciętą
  wiadomość binarną opisuje „co najmniej N bajtów” (prawdziwa długość nie idzie
  po drucie); wzór Avro, Protobuf lub Thrift nie da się przypiąć do topiku,
  który ma już zasady ukrywania danych (i odwrotnie), z polskim wyjaśnieniem;
  naruszenia JSON Schema w Nieprzetworzonych są po polsku, ścieżki czytelnie, a
  korzeń to „(cała wiadomość)”; wiadomość odrzucona przy zapisie ma w listach
  „odrzucona przy zapisie” i „—” zamiast „1 z 1”; widok szesnastkowy ma wersję
  po 8 bajtów na wąskim oknie, a uwaga o ucięciu stoi nad ramką; `tf-textarea`
  jest blokiem (marginesy działają), `tf-disclosure` przyjmuje elementy
  dołożone później, zgłasza `toggle` także po zmianie z kodu i przewija
  otwartą treść w widok, a błąd kompilacji zapisanego wzoru trafia do
  rozwijanych szczegółów technicznych w oknie ponawiania.
- Czwarta runda po przeglądzie Thrift: podgląd ukrywania danych traktuje ucięty
  tekst jak tekst (z „…” na końcu); odmowa przypięcia wzoru binarnego do topiku
  z zasadami (i odwrotnie) zapada w tej samej transakcji co zapis, więc wyścig
  jej nie ominie; ekran Ukrywania danych uznaje topik za binarny według
  przypiętego wzoru (Avro, Protobuf, Thrift), tak jak serwer, a nie według
  rodzaju treści: topik `application/octet-stream` bez takiego wzoru działa jak
  JSON i zasady da się na nim zmieniać; gdy wzór binarny jest, a zasady już są,
  ekran pokazuje je z samym „Usuń” i wyjaśnieniem; bajty 0xC0, 0xC1 i 0xF5–0xFF nie są początkiem znaku;
  nazwy typów JSON są po polsku, wiersz „przy zapisie” nie powtarza się, a
  „Dla programisty” opisuje tylko to, co klucz może; `tf-disclosure` przewija
  tylko po kliknięciu.

- Rejestr wzorców waliduje Avro (F4 B1, biblioteka `apache-avro` 0.22 z ASF,
  bez kodeków kompresji; tylko do czytania schematów i serializacji
  podschematu). Temat `avro/binary`, `application/octet-stream` lub dowolny
  inny wiąże się z wzorcem `avro` i może mieć `validation = warn | dlq`:
  ładunek to **jedna surowa wartość Avro** zapisana schematem wzorca, bez
  bajtów po niej. Nagłówek pliku Avro (`Obj\x01`), kodowanie single-object
  (`C3 01`) i format Confluent (`00` + id) nie są obsługiwane i dają
  naruszenie. Bez migracji i bez zmian w protokole.
- Wartości Avro są sprawdzane własnym przejściem bez alokacji, a nie
  `from_avro_datum` (ta alokuje to, co wartość deklaruje, ma globalny limit
  512 MiB i uznaje urwany boolean lub numer gałęzi unii za null). Zadeklarowana
  długość większa niż reszta ładunku, blok tablicy lub mapy większy niż
  ładunek, ujemna długość, zły UTF-8, numer wyliczenia lub gałęzi poza listą i
  rozmiar bloku niezgodny z jego elementami są naruszeniem od razu; elementy
  zerowej szerokości (`null`, pusty rekord) są liczone jednym kosztem, a praca
  na dokument (20 mln jednostek, wspólna dla paczki), zagnieżdżenie (256) i
  schematy rekurencyjne kończą się `schema_check_too_complex`, nigdy
  werdyktem „niepoprawne”.
- Zgodność wersji Avro według rozwiązywania schematów Avro: backward =
  nowy schemat czyta dane starego, forward = stary czyta dane nowego, full =
  oba. Unia pisarza musi być w całości czytelna, enum bez domyślnego symbolu
  nie może tracić symboli, nowe pole bez `default` łamie backward. Typ
  nazwany jest porównywany po tożsamości, nie po miejscu definicji
  (`SchemaCompatibility::can_read` uznaje przeniesienie definicji między
  polami za niezgodność, a „częściową” zgodność za sukces). Porównanie, które
  nie mieści się w limicie pracy, to `bus.schema_compare_too_complex`.
- Limity wzorca Avro przy rejestracji: 256 KiB tekstu, 4096 typów, 1024 pola
  w rekordzie, 64 gałęzie unii, 1024 symbole enuma, `fixed` do 16 MiB,
  zagnieżdżenie 64; odrzucany jest też rekord, który zawiera sam siebie bez
  wyjścia (nie ma skończonej wartości), pole rekordu niebędące obiektem
  (parser Avro pomijał je po cichu), dwukrotna definicja tej samej nazwy
  (parser brał pierwszą) i wartość domyślna pola unii niepasująca do
  pierwszego wariantu.
- Schemat Avro jest sprawdzany strukturalnie, zanim zobaczy go biblioteka:
  `Schema::parse` kopiuje całe poddrzewo typu nazwanego raz na nazwę i raz na
  alias, a obiekt bez `fields`/`symbols`/`size`, którego `type` nazywa już
  sparsowany typ, zwraca jego pełną kopię (zagnieżdżone: m^k kopii z 20 KB
  tekstu). Odrzucane są: ponad 50 000 wartości JSON, 64 aliasy na typ i 256 w
  całym schemacie, 4096 typów nazwanych, praca kopiowania (aliasy x rozmiar
  definicji) ponad milion wartości, nazwa/namespace/alias/pole/symbol dłuższe
  niż 256 bajtów, `record`/`enum`/`fixed` bez treści, typ nazwany jak słowo
  kluczowe lub typ prosty (`record`, `map`, `int`, ...), `type` zawierający
  kolejny obiekt typu oraz alias równy nazwie lub aliasowi innego typu.
  Wartość domyślna pola musi pasować do typu w całości: `fixed` ma dokładną
  długość, unia jest sprawdzana względem pierwszej gałęzi (zakres `int`,
  symbol enuma, kształt rekordu rekurencyjnie).
- Wartości Avro: `int`, numer wyliczenia i numer gałęzi unii mają najwyżej 5
  bajtów kodowania (jak `readInt` w Javie), `long` nadal 10; urwany `fixed`
  jest naruszeniem, a nie przekroczeniem limitu pracy. Zgodność: unia
  czytelnika jest rozwiązywana względem pierwszej pasującej gałęzi (rekord po
  nazwie bez namespace, enum i `fixed` po pełnej nazwie lub aliasie, typ prosty
  najpierw dokładnie, potem przez promocję), a powód odmowy w pamięci podręcznej
  porównania ma stały rozmiar.
- Łańcuch dostaw: `apache-avro` 0.22 zawsze ciągnie `miniz_oxide` 0.9.1 (kodek
  deflate, którego nie używamy); crate nie ma cechy, która by to wyłączała, więc
  zostaje jako jedyny jego użytkownik w drzewie normalnych zależności.
- Wzór pochodny (`derive_subschema`) zostawia dozwolone pola rekordu
  najwyższego poziomu i przenosi definicję typu nazwanego do pierwszego
  zachowanego miejsca, które go używa (także przez alias); definicja trafia do
  wyniku dokładnie raz, kolejne wzmianki są odwołaniami. Rzutowanie odczytu dla formatów
  binarnych (polityki pól) to osobny pakiet B6.
- Dashboard: kafel Avro w „Dodaj wzór” z podpowiedzią kształtu, odmowy po
  polsku (typy, limity, rekord bez wartości), powód odmowy wersji nazywa pole,
  typ lub symbol enuma, karta „Pola rekordu” na stronie wzoru (nazwa, typ,
  wymagane/opcjonalne) i przetłumaczone powody w Nieprzetworzonych (pięć
  języków).

- Walidacja XSD przy publikacji jest uczciwa: kolejka do czterech miejsc
  walidacji węzła jest FIFO (wątek, który właśnie zwolnił miejsce, nie
  wyprzedza czekającego), jedna organizacja zajmuje najwyżej 2 z 4 miejsc, a
  czekający z organizacji na limicie nie blokuje kolejki innych organizacji.
  Organizacja z 32 czekającymi sprawdzeniami dostaje natychmiast odmowę
  `schema_check_too_complex` (rekord trafia do DLQ albo jest oznaczony
  ostrzeżeniem jak każdy porzucony dokument) zamiast kolejnego zablokowanego
  wątku. Bez nowych kodów w protokole.
- Rejestr wzorców przyjmuje typy `xsd` i `hl7v2_profile` (migracja 179:
  `bus_schema_subjects.schema_type` poszerzony o oba typy, wiersze, wersje
  i znaczniki usunięcia bez zmian; pakiety walidatorów nie mają własnej
  migracji). Oba typy mają walidator: `GET capabilities` wymienia
  `json_schema`, `xsd` i `hl7v2_profile`, temat może mieć `validation =
  warn|dlq` z takim wzorcem, podschemat dla polityki odczytu jest wyliczany,
  a domyślna zgodność `backward` działa. `avro`/`protobuf`/`thrift` nadal są
  tylko przechowywane. Wzorzec `xsd` wiąże się tylko z tematem
  `application/xml` albo `text/xml`, `hl7v2_profile` tylko z
  `application/hl7-v2` albo `x-application/hl7-v2+er7`, `json_schema` nadal
  tylko z JSON-em; formaty binarne wiążą się niezależnie od `content_type`.
  Węzeł bez migracji 179 odrzuca replikowany wzorzec nowego typu (konflikt
  w skrzynce, bez zatrzymania synchronizacji). Wiersz wzorca dociera do
  takiego węzła przy następnym zapisie po uaktualnieniu, ale wersje
  zarejestrowane w międzyczasie nie dotrą bez resetu bazowego
  (`reseed_core_state_from_current_rows`). Rzadki przypadek: stary węzeł
  z podmiotem o tej samej nazwie z generacji 0 może podpiąć wersje nowego
  typu pod swój stary wiersz i zmienić `content_type` tematu powiązanego
  z tym podmiotem. **Uaktualnij wszystkie węzły sieci razem, zanim ktoś
  zarejestruje wzorzec XSD lub profil HL7** (migracja 179 na każdym węźle).
- Profil HL7 v2 (`hl7v2_profile`) to JSON `{"description"?, "required_segments",
  "required_fields"}`: wiadomość ER7 musi się dać odczytać tym samym parserem,
  co polityki pól, zawierać każdy wymagany segment, a wymagane pole
  (`PID-3`) musi być niepuste w każdym wystąpieniu swojego segmentu (pole
  wymagane wymusza też swój segment). `MSH-1`/`MSH-2`, nieznane klucze,
  powtórzenia i listy powyżej 512 wpisów są odrzucane przy rejestracji.
  Zgodność: `backward` — nowa wersja nie wymaga niczego ponad starą,
  `forward` — odwrotnie, `full` — równe zbiory.
- Wzorzec XSD to własny podzbiór (na `quick-xml` i `regex`, bez nowych
  zależności): `element` z `minOccurs`/`maxOccurs`, `complexType` z
  `sequence`, `choice` i `all`, `simpleContent/extension`, `attribute`
  (`required`/`optional`), typy `string`, `int`, `integer`, `decimal`,
  `boolean`, `date`, `dateTime` oraz `restriction` z `minLength`, `maxLength`,
  `pattern` i `enumeration`. `import`/`include`, `group`/`attributeGroup`,
  `any`, `key`/`keyref`/`unique`, `complexContent`, `mixed`, `ref=` i inne
  konstrukcje spoza podzbioru są odrzucane przy rejestracji z nazwą
  konstrukcji. Dokument jest sprawdzany po lokalnych nazwach elementów
  (przestrzenie nazw nie są rozstrzygane, tak jak w politykach pól XML);
  `pattern` jest tłumaczony z dialektu XSD na `regex` z domyślnym
  zakotwiczeniem (`^`/`$` są zwykłymi znakami, konstrukcje o innym
  znaczeniu są odrzucane), a komunikaty naruszeń zawierają ścieżkę i
  ograniczenie, nigdy wartość. Zgodność jest dowodzona zachowawczo
  (inkluzja języków modeli zawartości, typów prostych i atrybutów):
  `backward` — każdy dokument ważny w starym wzorcu jest ważny w nowym,
  `forward` — odwrotnie; zmiana, której nie da się udowodnić, jest
  odrzucana.
- Dashboard: „Dodaj wzór” oferuje XSD i profil HL7 v2 (kafelki z listy
  formatów serwera), strona wzorca opisuje profil HL7 w zwykłych słowach
  (wymagane segmenty i pola z nazwami ze słownika), a opis XSD to jego
  `xs:annotation/xs:documentation`. Odmowa nowej wersji profilu, która
  wymaga czegoś ponad starą wersję, ma przycisk „Usuń OBX-8 i dodaj wersję”.
- Poprawki walidatorów XSD i profilu HL7: numer pola w adresie (`PID-n`) ma
  najwyżej 999 (dłuższy numer w profilu lub polityce pola jest odrzucany
  zamiast zatrzymywać proces), drugi segment `MSH` w wiadomości jest numerowany
  jak pierwszy (to zmienia też politykę pól na topikach z paczkami wiadomości:
  reguły odczytu pokazują nazwane pole drugiego `MSH`, znaki podziału nie są już
  zerowane, a reguły zapisu nie widzą `MSH-1`), a jawne `""` (usuń wartość) nie spełnia pola wymaganego.
  Komunikaty naruszeń nie zawierają już treści wiadomości (nazwy encji
  `&nazwa;`, błędy parsera, identyfikator błędnego segmentu). Zgodność XSD nie
  dowodzi już wyliczenia typu liczbowego lub `boolean` względem wzorca albo typu
  tekstowego (dozwolone `01`, `+1`, `1` jako `true`), ma budżet pracy dla
  szerokich modeli i podaje element, którego nowa wersja wymaga (`the new
  schema requires element 'x'`). `pattern` z nadmiarowym `)` jest odrzucany,
  `maxOccurs="0"` w `xs:all` wyłącza element, a schemat wyprowadzony dla polityki
  odczytu przyjmuje projekcję dokumentów z prefiksami przestrzeni nazw i tekstem
  elementu głównego (wymagalność dzieci, których nazwy projekcja nie może
  dopasować jednoznacznie, jest rozluźniana).
- Sprawdzenie wzorca, które przekroczyło limit pracy (zbyt głęboki lub zbyt duży
  dokument XML), nie jest już zgłaszane jako naruszenie: wiadomość trafia do
  nieprzetworzonych (`validation = dlq`) lub jest przyjęta z ostrzeżeniem
  (`warn`) z powodem `schema_check_too_complex`, odrębnym od
  `schema_violation` (nowa wartość nagłówka `dlq.reason`; starsze programy
  pokażą ją jako „nieznany powód”).
- Dashboard: odmowa dodania profilu HL7 lub XSD mówi po polsku, co jest nie tak
  (adres pola, `MSH-1`/`MSH-2`, nieznany klucz, nieobsługiwana konstrukcja albo
  typ XSD); tekst serwera zostaje w zwiniętych szczegółach. Odmowa nowej wersji
  XSD wskazuje wymagany element, odmowa profilu HL7 rozróżnia segmenty i pola,
  a „Usuń … i dodaj wersję” nie jest oferowane, gdy wynik byłby taki sam jak
  poprzednia wersja. Okno „Dodaj wzór” podaje kształt profilu HL7 i opis
  obsługiwanego XSD, strona XSD ma kartę „Jakie wzory XSD zadziałają”, a okno
  nowej wersji XSD pokazuje różnicę. Segmenty profilu mają nazwy i kolejność
  wiadomości, błąd wiązania wzoru z topikiem o innej treści jest opisany, a
  szczegół nieprzetworzonej wiadomości z profilu HL7 jest po polsku.
- Druga runda poprawek wzorów XSD i profilu HL7: klasa znaków w `pattern` z
  nieucieczonym `]` zaraz po `[` lub `[^` jest odrzucana (kończyłaby się w innym
  miejscu niż rozumie ją `regex`), łańcuch ograniczeń typu prostego i porównanie
  zgodności zagnieżdżonych typów złożonych mają limit głębokości, a budżet pracy
  porównania liczy się globalnie. Porównanie, które przekroczyło budżet, nie jest
  niezgodnością: odpowiedź ma osobny kod `bus.schema_compare_too_complex`, a
  wiadomość, której sprawdzenie przekroczyło limit, nie zwiększa
  `schema_violations_total` i jest audytowana jako `bus.schema.check_too_complex`.
  Profil HL7 musi być obiektem JSON (tablica była czytana pozycyjnie i dawała
  profil bez wymagań), a element `xs:all` z `maxOccurs="0"` nadal jest sprawdzany
  przy rejestracji.
- Koszt sprawdzania wzorów XSD jest liczony jednym licznikiem (`Budget`), który
  przechodzi przez każdą kosztowną operację: sprawdzenie wartości (jedna
  jednostka na bajt wartości dla każdego wzorca), przecięcie wyliczeń (naliczane przed
  obliczeniem), automaty modeli treści, atrybuty i rejestrację wzoru. Walidacja
  dokumentu ma limit 50 mln jednostek (`schema_check_too_complex`), porównanie
  wersji 20 mln (`bus.schema_compare_too_complex`), a kompilacja 5 mln.
  Wyliczenia są zbiorami haszowanymi, atrybuty typu mają indeks, a duplikaty
  atrybutów w dokumencie wykrywa walidator w czasie liniowym. Przy dodawaniu
  wzoru obowiązują twarde limity (zmieniają się razem z kartą „Jakie wzory XSD
  zadziałają”): wartość wyliczenia do 1 KB, wszystkie wyliczenia razem do
  128 KB i 4096 wartości, wzorzec `pattern` do 512 znaków, do 256 wzorców,
  program jednego wzorca do 256 KB (cyfry `\d` są Unicode, więc PESEL `\d{11}`
  potrzebuje 64 KB, a `\p{L}{60}` jest odrzucany) oraz 32 MB pamięci wszystkich
  wzorców razem, najwyżej 1024 atrybuty w typie. Wzór przekraczający limit jest
  odrzucany z opisem po polsku. Pamięć podręczna skompilowanych wzorów magistrali
  ma granicę 1024 wpisów i 256 MB (najdawniej użyty wypada pierwszy), a zmiana
  wzoru innego tematu nie wymusza już ponownej kompilacji niezmienionych.
- Trzecia runda poprawek kosztu sprawdzania XSD. `pattern` jest dopasowywany
  leniwym automatem (`regex-automata`, zależność tylko w głównym
  `Cargo.toml`) i kosztuje jedną jednostkę na bajt; do tego płaci się
  za stany zbudowane przez skanowanie oraz `liczba stanów` za każdą nową
  pamięć podręczną automatu (każda równoległa walidacja ma własną z puli,
  używaną ponownie). Gdy automat zrezygnuje (pamięć się trzęsie), płaci się z
  góry `szerokość × długość` za maszynę Pike VM, gdzie szerokość to stany
  plus wszystkie zakresy przejść (klasy Unicode są szerokie). Realistyczne paczki (kilkanaście tysięcy PESEL,
  NIP, kodów pocztowych i IBAN) mieszczą się więc w budżecie, a wzorzec
  `[01]*1[01]{200}` na megabajcie losowych bitów nadal jest odrzucany jako
  `schema_check_too_complex`. Grupy XSD są niekapturujące (`(?:…)`), pamięć
  wzorca liczy jego program i do 4 pamięci podręcznych automatu (każda
  kolejna równoległa walidacja tego wzorca dokłada jedną), a waga wpisu
  pamięci podręcznej uwzględnia automaty modeli treści (limit pamięci wzorców
  jednego schematu: 32 MB). Tabela znaczników rekordu jest liczona z góry. Praca per element (kopia zbioru startowego,
  flagi `xs:all`) jest liczona w budżecie, a akceptacja modelu to odczyt jednego
  elementu. Pamięć podręczna skompilowanych wzorów ma udział na organizację
  (128 wpisów i 64 MB; przekroczone ogólne granice zwalnia najpierw organizacja
  zajmująca najwięcej), a równoczesne chybienia tego samego tematu czekają na
  jedną kompilację. **Jedna paczka publikacji ma wspólny budżet walidacji
  XSD**: 50 mln jednostek plus 64 na bajt ładunków paczki (limit 50 mln na
  dokument obowiązuje nadal). Opłata, która się nie mieści, jest odrzucana bez
  dopisywania, więc rekord obciąża paczkę tylko pracą, która faktycznie się
  wykonała (najwyżej jego udziałem); rekordy, którym budżetu zabrakło, są
  traktowane jak `schema_check_too_complex`, a kolejne uczciwe rekordy
  przechodzą, dopóki budżet paczki nie zostanie realnie wyczerpany. Błąd
  budowy wzorca podaje powód (zły zakres powtórzeń, zbyt głębokie
  zagnieżdżenie, zakres znaków od końca) i dashboard opisuje go po polsku. Wybór efektywnej wersji tematu z
  wycofaniami czyta tylko numery i skróty wersji, a tekst wybranej jednej.
  **Węzeł waliduje XSD najwyżej czterema dokumentami naraz** (`POOLED_CACHES`,
  wspólna bramka dla publikacji, REST, podglądu i synchronizacji): dalsze
  walidacje czekają na wątku blokującym. Dzięki temu wzorzec ma w pamięci co
  najwyżej cztery cache'e leniwego DFA (zachowane i chwilowe), tyle ile liczy
  `pattern_memory`, niezależnie od rozmiaru puli wątków; pula cache'y ma stałą
  pojemność i nadmiarowe cache'e są zwalniane.

- Katalog podmiotów dla okien „Nadaj dostęp” i „Ukrywanie danych”:
  `SubjectDirectoryRequest { kind, query }` zwraca do 50 osób, grup albo
  addonów (`BusSubjectWire`: rodzaj, identyfikator, nazwa, liczba członków
  grupy) wyłącznie z organizacji wywołującego, posortowanych po nazwie
  i zawężonych frazą (bez rozróżniania wielkości liter). Osoby to aktywni
  członkowie organizacji, podpisani nazwą wyświetlaną albo loginem, nigdy
  adresem e-mail; grupy to te, które mają w organizacji co najmniej jednego
  aktywnego członka, a liczba członków liczy tylko ich; addony to włączone
  addony deklarujące `bus.publish`/`bus.subscribe`, którym macierz uprawnień
  tej instancji daje `bus.read` lub `bus.write`. Wymaga uprawnienia
  `bus.admin` i roli administratora organizacji.
- Podgląd „jak widzi…”: `FieldPolicyPreviewRequest { topic, partition,
  offset, subject_type, subject_id }` pokazuje wiadomość tak, jak odczytałby
  ją wskazany podmiot (osoba: własna zasada, potem zasada jej grupy, potem
  zasada dla wszystkich; grupa i addon: własna zasada, potem zasada dla
  wszystkich), razem z listą pól „pokaż/ukryj”. Podgląd nigdy nie pokazuje
  więcej, niż może odczytać sam administrator; `limited_by_caller` mówi, że
  jego własna zasada zawęziła widok. Wymaga roli administratora i prawa
  administrowania topikiem; wpis audytu `bus.field_policy.preview` nie
  zawiera treści wiadomości.
- Addon jest osobnym rodzajem podmiotu w dostępie do topiku
  (`subject_type = 'addon'` w `AclSetRequest`/`AclListResponse`). Addon
  dostaje dostęp wyłącznie z wiersza addonu, a użytkownik nie dziedziczy
  wiersza addonu. Wiersz „użytkownik” z zakazem i identyfikatorem addonu
  (dotychczasowy sposób ograniczania addonu) nadal blokuje ten addon na każdym
  nodzie — także po synchronizacji, odbudowie noda i instalacji addonu
  później. Nowego wiersza „użytkownik” z identyfikatorem zainstalowanego
  addonu nie da się zapisać (`bus.subject_is_addon`); usunąć stary można.
  Rodzaj wywołującego (użytkownik, addon, klucz API, system) niesie teraz
  jawnie kontekst wywołania busa, ustalany przez punkt wejścia, a nie z pola
  `origin` — flow uruchomiony przez addon w imieniu użytkownika podlega
  zasadom tego użytkownika. Liczba członków grupy w liście dostępu i zasadach
  ukrywania danych liczy teraz tylko członków organizacji, tak jak katalog,
  a wiersz addonu ma jego nazwę.
- Podgląd „jak widzi…” i zapisy dostępu oraz zasad ukrywania danych
  przyjmują identyfikator podmiotu tylko z liter, cyfr i znaków `-_.:@*`
  (do 128 znaków). Każda próba podglądu, także odrzucona, zostawia wpis
  audytu z wynikiem (`outcome=ok` albo `outcome=refused:<kod>`).
- Klucz ogólny może czytać i wysyłać wiadomości wybranych topików przez REST
  rekordów (`/v1/bus/instances/{instancja}/topics/{topik}/records?org_id=…`,
  decyzja P5: jeden klucz ogólny na wszystko). Działa jako on sam, nie jako
  osoba: prawem jest wyłącznie wiersz dostępu topiku `subject_type = 'api_key'`
  z działaniem `read` albo `write` w tej instancji i organizacji; bez takiego
  wiersza klucz nie ma żadnego dostępu (domyślnie zakaz), a czytanie i wysyłanie
  są osobnymi prawami. Klucz nie potrzebuje wpisu w macierzy uprawnień i nigdy
  nie administruje topikiem: nie tworzy topików (także przez
  `create_if_missing`), nie zmienia ustawień ani dostępu, nie czyta
  „Nieprzetworzonych wiadomości” ani topików wewnętrznych. Klucz usunięty albo
  przypisany do osoby nie korzysta z wierszy klucza; klucz przypisany do osoby
  działa jak dotąd — jako ta osoba.
- Grupy odbiorców klucza: klucz odbiera tylko pod `k:<id klucza>` albo
  `k:<id klucza>.<nazwa>`, a pod grupą `k:…` nie odbiera nikt inny — ani osoba,
  ani addon, ani inny klucz. Klucz nie przesunie więc potwierdzeń cudzej grupy,
  a nikt nie przesunie potwierdzeń klucza. Zwykłe nazwy grup nie mogą zawierać
  dwukropka, więc obie przestrzenie się nie spotykają.
- Prawo klucza jest sprawdzane przed wyszukaniem instancji: klucz bez prawa
  dostaje 403 także dla instancji, która nie istnieje, a na starej ścieżce bez
  instancji (`/v1/bus/topics/{topik}/records`) — zanim ścieżka ujawni liczbę
  albo identyfikatory instancji; dopiero klucz z prawem do tego topiku w tej
  organizacji dostaje odpowiedź, jaką dostaje każdy wywołujący.
- Prawa klucza do wiadomości nadaje administrator serwera
  (`ApiKeyCreateRequest.scope_resources`, `ApiKeyScopeSetRequest`,
  `ApiKeyScopeClearRequest`), który musi też być administratorem tego topiku:
  mieć `bus.admin` w instancji i rolę administratora organizacji, której prawo
  dotyczy (tak jak przy `AclSetRequest`); każda zmiana zostawia wpis audytu
  `bus.acl.set`. Administrator topiku nadaje je też przez `AclSetRequest`
  z `subject_type = 'api_key'`. Zakres `topic` wymaga działania `read` albo
  `write` i poprawnego identyfikatora (instancja, organizacja, istniejący
  topik); `admin` i `'*'` są odrzucane, a `AclSetRequest` przyjmuje tylko
  istniejący klucz ogólny. `ApiKeyScopeListResponse` zwraca prawa klucza
  z działaniem, a `ApiKeyScopeClearRequest` z działaniem usuwa tylko to jedno
  prawo. Wcześniej zapisany wiersz klucza z działaniem `'*'` niczego nie
  przyznaje (nadal może tylko zabraniać) i można go usunąć. Odebranie prawa
  działa zawsze: nie wymaga włączonej ani uruchomionej instancji, a gdy
  instancji albo organizacji już nie ma, wystarczy administrator serwera.
- Ukrywanie danych obejmuje klucze: klucz ogólny czyta i wysyła według zasady
  dla wszystkich (zasady nie da się przypisać kluczowi), nigdy według zasady
  osoby o tym samym identyfikatorze. Topik, który ma zasady ukrywania danych
  tylko dla wybranych osób, grup albo addonów, a nie ma zasady dla wszystkich,
  jest dla klucza zamknięty w danym kierunku (odczyt albo zapis):
  `bus.key_needs_topic_wide_rule`, w REST 403 — dopóki administrator nie doda
  zasady dla wszystkich.
- Flow uruchomiony kluczem ogólnym publikuje z prawami zapisu tego klucza.
  Flow uruchomiony kluczem przypisanym do osoby nadal nie publikuje do topiku
  (działa jako klucz, a taki klucz nie ma własnych praw), choć REST rekordów
  traktuje ten sam klucz jak jego osobę.
- Audyt: żądania REST rekordów klucza ogólnego zostawiają wpisy
  `bus.rest.publish` albo `bus.rest.consume` z identyfikatorem i nazwą klucza,
  kodem HTTP i powodem (bez treści wiadomości). Pierwsze żądanie każdego wyniku
  jest zapisywane od razu, kolejne w ciągu minuty są zliczane (liczba żądań
  i wiadomości) i zapisywane zbiorczo co minutę oraz przy zatrzymaniu
  instancji. Sukcesy liczą się osobno dla każdego topiku, odmowy — dla klucza
  i powodu, z liczbą różnych topików i jednym przykładem, więc klucz pytający
  o coraz to nowe topiki nie mnoży wpisów. Liczba liczników na klucz i łącznie
  jest ograniczona; nadmiar trafia do jednego licznika `bus.rest.overflow`.
  Wpisy audytu busa i nagłówek `dlq.producer_actor` kopii
  w „Nieprzetworzonych wiadomościach” wskazują klucz jako `api_key:<id>`,
  a nowy nagłówek wiadomości `tf.actor_kind` mówi, kim jest nadawca
  z `tf.actor` (`user`, `api_key`, `addon`, `system`).
- Zaległość grupy odbiorców odczytuje każdy, kto może czytać topik — także
  grupy klucza `k:…`, bez wpisu odmowy przy każdym odświeżeniu; odbierać
  i potwierdzać pod grupą klucza nadal może tylko ten klucz.
- Aktualizacja: migracja bazy 177 przebudowuje `resource_permissions`
  (dopuszcza `subject_type = 'addon'`) bez zmiany istniejących wierszy.
  Nod w starszej wersji odrzuca zsynchronizowany wiersz addonu (operacja
  trafia do konfliktów synchronizacji) i dalej traktuje addony jak
  użytkowników — nody trzeba zaktualizować razem.
- Strona topiku ma sekcję „Dostęp” (widoczną tylko dla administratora
  topiku; pozostali nie widzą jej w menu). Tabela „Osoby, grupy i addony”
  pokazuje dla każdego wpisu nazwę, rodzaj i liczebność grupy oraz Czytanie /
  Zapis / Administracja jako „pozwolono”, „zabroniono” albo „—” (decyduje rola
  w organizacji). „Nadaj dostęp” wybiera użytkownika, grupę albo addon
  z katalogu (tylko tych bez wpisu), „Zmień” i „Usuń” działają w wierszu;
  każde okno mówi przed zapisem, co się stanie, a usunięcie „zabroniono”
  ostrzega, że rola w organizacji może dać te prawa. Stary wpis na wszystkie
  prawa naraz jest przy zmianie rozpisywany na trzy osobne. Wpis
  użytkownika o identyfikatorze addonu jest pokazany jako nieznany podmiot
  z identyfikatorem i można go usunąć.
- Karta „Systemy zewnętrzne (klucze API)” w tej sekcji pokazuje klucze
  z prawami do wiadomości topiku lub do wzorów wiadomości instancji.
  Administrator serwera widzi też ostatnie użycie i może wydać klucz (nazwa
  i prawa: czyta/wysyła wiadomości, czyta/dodaje wzory — klucz pokazany raz,
  z adresem dla systemu i nazwą grupy odbiorców `k:<id klucza>`), zmienić
  jego prawa i go unieważnić. Pozostali administratorzy topiku widzą prawa
  kluczy do wiadomości tylko do odczytu, z informacją, że klucze wydaje
  administrator serwera. Nowe tłumaczenia błędów: klucz bez zasady ukrywania
  danych dla wszystkich (`bus.key_needs_topic_wide_rule`), wpis użytkownika
  o identyfikatorze addonu, nieznany klucz, prawo spoza własnej organizacji.
  Protokół: `BusCapabilitiesWire.{org_id, org_name}` (organizacja wywołującego;
  pusta u starszego serwera).
- Unieważnienie klucza API usuwa w tej samej transakcji wszystkie jego prawa
  (wiersze `resource_permissions` z `subject_type = 'api_key'` każdego rodzaju
  zasobu), każde jako zsynchronizowane usunięcie; wpis audytu `apikey.delete`
  wymienia usunięte prawa. Wpisy klucza, którego już nie ma (starsze dane),
  karta kluczy pokazuje jako „Klucz usunięty” bez zielonych praw i bez
  liczenia w liczniku sekcji; administrator serwera usuwa je przyciskiem
  „Usuń prawa”.
- Grupa odbiorców klucza (`k:<id klucza>[.<nazwa>]`) nazywa się wszędzie nazwą
  klucza („Klucz Portal”, „Portal · raporty”); grupa klucza, którego już nie
  ma, jest opisana jako „Klucz usunięty”, nie wywołuje alertów i nie wlicza się
  do „Czeka na odbiorców” ani sum zaległości. Protokół:
  `BusGroupSummaryWire`/`BusGroupStatsWire`/`BusGroupDetailWire.{key_name, key_gone}`.
- Zmiana i usunięcie wpisu dostępu wysyłają żądania w kolejności, w której
  przerwanie w dowolnym miejscu nigdy nie daje szerszego dostępu niż przed
  zmianą i po niej: najpierw nowe zakazy, potem zdjęcie pozwoleń, nowe
  pozwolenia, a zdjęcie zakazów na końcu (także starego wpisu na wszystkie
  prawa). Okno „Zmień” mówi, co nowe prawo pozwala albo czego zabrania.
- Okno z jednorazowo pokazanym kluczem pyta raz przed zamknięciem klawiszem
  Escape lub krzyżykiem, jeśli klucza nie skopiowano; wskazówka techniczna jest
  zwinięta w „Dla programisty” z przyciskiem „Kopiuj nazwę odbiorcy”.
- Strona topiku ma sekcję „Ukrywanie danych” (tylko dla administratora topiku).
  Tabela pokazuje zasady: kto (osoba, grupa, addon albo Wszyscy), jakie pola
  ukrywa albo odrzuca przy zapisie, czy dotyczy odczytu czy zapisu i kiedy ją
  zmieniono. Serwer zapisuje pola **dozwolone**, więc ekran pokazuje odwrotność
  (znane pola bez dozwolonych); pole, którego nie ma na liście okna, też jest
  ukryte (a przy zapisie odrzucane), chyba że wpisze się je osobno. Listę pól
  daje wzór wiadomości topiku JSON (właściwości, także z `allOf`, `oneOf`,
  `anyOf`, `if`/`then`/`else` i lokalnych `$ref`; wzór z odwołaniem do innego
  pliku, `patternProperties` albo `additionalProperties` jako schematem
  traktujemy jak brak listy; tak samo wzór zagłębiony ponad 32 poziomy,
  `$ref` do schematu `true`/`false`, `unevaluatedProperties` jako schemat
  i `dependentSchemas`), podręczny słownik około sześćdziesięciu pól HL7 v2
  z nazwami w pięciu językach. Przy **zapisie** pozostałe pozycje tych
  segmentów (MSH-11, PID-1, PID-4, EVN-1, …; do szerokości z HL7 v2.8, także
  PV2, PD1, ROL, GT1, IN2, SFT, TQ1, SPM, MSA, ERR, MRG i NTE-1…9) są domyślnie
  dozwolone, więc zwykła wiadomość nie jest odrzucana, a okno mówi, że
  odrzucony zostanie dopiero inny segment (np. Z). Zasada **odczytu** nigdy
  ich nie dodaje: co nie jest na liście, jest ukryte, tak jak mówi okno
  (wcześniej nowa zasada odczytu zostawiała widoczne ok. 290 pozycji, np.
  PID-4, PID-20, NK1-*, IN1-*). Zapisana wcześniej zasada odczytu, która
  przepuszcza takie pozycje, pokazuje je w „Zmień” jako wpisane pola (można je
  usunąć), a tabela wymienia je w „Zostawia też widoczne”. Adres spoza
  słownika wpisuje się ręcznie i jest sprawdzany przed wysłaniem tak jak na
  serwerze; błędny adres jest zaznaczony na swoim chipie i opisany pod polem,
  także zanim wybrano, dla kogo jest zasada albo, gdy listy nie ma
  (XML, JSON bez wzoru), pola do pozostawienia wpisuje się w całości. Długą
  listę pól można przeszukać. Topik z treścią binarną nie jest tu edytowany,
  a sekcja mówi dlaczego i odsyła do ustawień topiku.
- Zapis zasady jest porównywany z tym, co jest zapisane, **na serwerze, w tej
  samej transakcji co zapis** (`FieldPolicySetRequest.expected_updated_at_ms`
  — zasada ma istnieć z dokładnie tym czasem zmiany — albo `expect_absent` —
  zasady jeszcze nie ma; oba pola dopisane na końcu z `#[serde(default)]`,
  więc starsi klienci zapisują bezwarunkowo, a oba naraz to
  `bus.invalid_argument`). Rozbieżność daje `bus.field_policy_changed`
  (`Conflict`): okno zamyka się z notatką „Nic nie zapisano — zasada zmieniła
  się w międzyczasie”, tabela pokazuje aktualne zasady i nic nie jest
  nadpisane. Zapis, który zastępuje zasadę, zawsze ustawia czas zmiany
  powyżej poprzedniego, żeby dwa zapisy w tej samej milisekundzie nie miały
  wspólnej wartości porównania. Odczyt przed zapisem po stronie przeglądarki
  został usunięty. Synchronizacja tabeli `bus_field_policies` między nodami
  (ledger, operacje stosowane w kolejności HLC) nie jest ruszana i porównania
  nie wykonuje: zmiana zreplikowana na ten node przed zapisem zmienia czas
  zmiany zasady, więc okno otwarte na starej wersji jest odrzucane, a zmiana,
  która dotrze po zapisie, stosuje się jak każda replikowana (ostatnia w
  kolejności HLC wygrywa). Usunięcie zasady porównuje się tak samo
  (`FieldPolicyDeleteRequest.expected_updated_at_ms`, dopisane z `#[serde(default)]`):
  okno „Usuń” na starej wersji zasady nic nie usuwa i pokazuje tę samą notatkę.
  Wpisów dostępu do topiku nie da się już wyczyścić ścieżką uprawnień IAM
  (`IamClearPermissionRequest` odrzuca `topic`, jak ustawianie) — tylko w TentaBus,
  ze sprawdzeniem administratora topiku i wpisem audytu `bus.acl.set`. Okno „Dodaj zasadę” ostrzega przed zapisem, gdy pierwsza zasada
  dla wybranych osób zamknie topik dla systemów z kluczem API; nikogo nie
  wybiera za administratora.
- Lista zasad topiku (`FieldPolicyListRequest`) jest tylko dla administratora
  tego topiku; czytelnik widzi wyłącznie podgląd wiadomości z własnym ukrywaniem.
  Jedno sprawdzenie (`require_topic_admin`: administrator tego topiku według
  jego ACL, w tym zakaz `admin`, i rola Admin w organizacji) stoi teraz za
  listą, zapisem i usunięciem zasady, podglądem, wzorem pochodnym
  (`SchemaDerivedGetRequest`) i nadawaniem albo zabranianiem dostępu
  (`AclSetRequest`). `SchemaDerivedGetRequest` był dostępny dla każdego
  czytelnika instancji i pozwalał odczytać wzór pochodny dowolnej zasady
  oraz sprawdzić, czy zasada istnieje; nie używa go żaden ekran ani addon,
  więc jest tylko dla administratora topiku. Ta sama zasada obejmuje usuwanie
  wpisu dostępu (`clear`): operator, któremu zakazano administracji topiku, nie
  zdejmie ani własnego zakazu, ani cudzego zakazu odczytu lub zapisu. Jedynym
  wyjątkiem jest administrator serwera, który zawsze może usunąć wpis — to droga
  powrotu do topiku, w którym każdy administrator dostał zakaz. Prawo klucza API
  do topiku (tworzenie klucza z zakresem topiku i `ApiKeyScopeSetRequest`) też
  wymaga administracji tym topikiem; odebranie prawa działa jak dotąd.
- „Dodaj zasadę” wybiera osobę, grupę, addon z katalogu (tylko tych, którzy nie
  mają jeszcze zasady w tym kierunku) albo Wszystkich, kierunek i dla każdego
  pola Pokaż / Ukryj (przy zapisie: Dozwolone / Wymagane / Niedozwolone);
  „Zmień” zachowuje podmiot i kierunek, „Usuń” mówi, co zasada robi teraz i kto
  zobaczy całe wiadomości po jej usunięciu (osobno dla osoby, grupy i addonu,
  w kolejności, w jakiej serwer wybiera zasadę). „Zamaskuj” i „Zahaszuj” dojdą
  razem z działaniem na pole w protokole.
- „Podgląd, jak widzi…” pokazuje prawdziwą wiadomość (domyślnie najnowszą
  z najbardziej obciążonej partycji) tak, jak czyta ją wybrany podmiot, z listą
  ukrytych pól. Okno mówi, że każdy podgląd zapisuje się w dzienniku audytu,
  a gdy zasada obowiązująca samego administratora ukrywa pola, które zobaczy
  wybrany odbiorca (`limited_by_caller`), wyjaśnia, że podgląd jest węższy niż
  jego widok, wymienia pola wzoru, których w podglądzie nie ma (ukryła je
  zasada, która obowiązuje także administratora, albo wiadomość ich nie ma),
  i mówi, że pól ukrytych już przez jego zasadę nie wymienia w „Co zrobiły
  zasady”. Wynik znika, gdy zmieni się osoba, grupa albo wiadomość.
  Pod tabelą widać, w którym kierunku topik jest zamknięty dla kluczy API (są
  zasady dla wybranych osób, grup lub addonów, a nie ma zasady dla wszystkich).
- Błędy: `bus.record_not_found` ma tłumaczenie, a `bus.subject_not_found`
  mówi o osobie, kluczu albo addonie (nie tylko o kluczu); nowy
  `bus.field_policy_changed`.
- Notatka po zapisie nie odsyła do zamkniętego okna („chyba że je dopiszesz”),
  a dla wzoru zamkniętego (`additionalProperties: false`) nie mówi o polach
  spoza listy. W zdaniach o skutkach zasada dla wszystkich brzmi „dla
  wszystkich”, a nie „dla „Wszyscy””. Pola nazwane `__proto__` albo
  `constructor` zachowują swoje ustawienie.
- „Usuń topik” mówi, że zasad ukrywania danych nie udało się policzyć, gdy
  lista zasad nie odpowiedziała, zamiast liczyć je jako zero. „Nowa wersja”
  wzoru mówi przed zamknięciem, że tekst zostanie zachowany (bo tak jest).
  „Nadaj dostęp” nie wybiera za administratora pierwszej grupy z listy, a
  kreator topiku pyta przed porzuceniem wpisanych danych tak jak inne okna.
- Rejestr wzorców odmawia tekstu z niewidocznym znakiem albo znakiem zmieniającym
  kierunek tekstu („Trojan Source”), w każdym formacie (JSON Schema, Avro,
  Protobuf, Thrift, XSD, profil HL7 v2) i w całym tekście, także w komentarzach i
  literałach. Jedno sprawdzenie (`schema_registry/text_guard.rs`, lista w
  `hidden_characters.json`) działa przed parserem formatu, przy rejestracji
  (protokół binarny i REST), przy porównaniu z poprzednią wersją (sprawdzany jest
  nowy tekst) i przy wyprowadzaniu podschematu. Odmawiane są: sterujące C0 poza
  TAB, LF i CR, DEL i C1, znaki kierunku (U+061C, U+200E, U+200F,
  U+202A–U+202E, U+2066–U+2069), niewidoczne (U+00AD, U+180E, U+200B–U+200D,
  U+2060–U+2064), cała kategoria Cf (sprawdzona względem Unicode 16, najnowszego
  dostępnego tu), U+2028 i U+2029, oraz niewidoczne znaki spoza Cf: wypełniacze
  Hangul (U+115F, U+1160, U+3164, U+FFA0), U+034F i mongolskie wybieraki
  U+180B–U+180D, U+180F. Celowo przechodzą wybieraki wariantów U+FE00–U+FE0F i
  U+E0100–U+E01EF (emoji i glify CJK). U+FEFF jest odrzucany wszędzie poza
  pierwszym znakiem XSD: czytnik XML go zdejmuje, a XML dopuszcza BOM, więc
  plik z edytora nadal się rejestruje; w pozostałych formatach i w dalszej
  części XSD jest odrzucany. Polskie litery i zwykłe emoji przechodzą; emoji
  sklejone U+200D są odrzucane. Komunikat podaje znak (U+XXXX i nazwę), wiersz i
  kolumnę (liczone w znakach Unicode), a panel wzorów mówi to po polsku („W
  wierszu N, w kolumnie M”). Odrzucenie znaku w literale Protobuf i Thrift ma
  teraz to samo zdanie, co w pozostałych formatach; stare, osobne sprawdzenia
  usunięto. Szczegóły porównania budowane ze starego tekstu zapisują taki znak
  jako `⟦U+XXXX⟧`, tak samo jak panel wzorów w zapisanym tekście (`⟦U+202E⟧`).
- **Aktualizacja: wzorce zapisane wcześniej.** Wersja zapisana przed tą zmianą
  (albo przyjęta z synchronizacji) z odrzucanym znakiem nie jest przepisywana,
  ale przestaje się kompilować. Topik, który sprawdza wiadomości takim wzorcem,
  odmawia publikacji zdaniem „zapisany wzór nie daje się skompilować…” z nazwą
  znaku, wierszem i kolumną. Przy starcie (po migracjach, przed obsługą ruchu)
  węzeł skanuje wszystkie zapisane wersje i dla każdej dotkniętej pisze w logu
  jedną linię `WARN` zaczynającą się od `TentaBus: stored schema version has a
  hidden character` (instancja, organizacja, nazwa wzoru, wersja, znak, wiersz,
  kolumna; nigdy tekst), a na końcu jedną linię z liczbą („N of M stored schema
  versions hold a hidden character”). Jak naprawić: zarejestruj poprawioną
  wersję tego wzoru (porównanie czyta stary tekst bez tego sprawdzenia, więc
  rejestracja działa); jeśli wzór jest wycofany, podepnij topik pod inny wzór.
  Potwierdzeniem, że nic nie wymaga uwagi, jest linia `INFO` „N stored schema
  versions checked for hidden characters, none found” (brak linii `WARN` nie
  wystarcza: skan mógł się nie udać). Błąd skanu jest zapisywany na poziomie
  `ERROR` („the scan of stored schemas for hidden characters failed”). Wersja,
  której wzór został usunięty, nie jest zgłaszana z powodu BOM na początku
  (nie wiadomo, czy to XSD).
- **Poprawianie zapisanego wzoru z niewidocznym znakiem.** Porównanie z poprzednią
  wersją czyta stary tekst z usuniętymi odrzucanymi znakami (ta wersja i tak się
  nie kompiluje, a usunięcie znaków jest właśnie poprawką): pole `pac<U+202E>jent`
  poprawione na `pacjent` to to samo pole, nie zmiana nazwy, więc poprawka
  rejestruje się także w trybie BACKWARD. Szczegóły porównania nadal zapisują
  odrzucany znak jako `⟦U+XXXX⟧`. Panel wzorów: strona wzoru ostrzega, że wersja
  zawiera niewidoczny znak i które topiki przez to nic nie przyjmują, z przyciskiem
  „Dodaj poprawioną wersję”; Stan topiku pokazuje to jako pozycję wymagającą uwagi,
  gdy aktualna wersja wzoru jest dotknięta (wykrywane po stronie panelu tą samą
  listą znaków, jedno odczytanie tekstu na topik i wersję, bez nowego pola w
  protokole). „Nowa wersja” zaczyna od tekstu BEZ tych znaków, z informacją, co
  usunięto i gdzie; różnica jest liczona względem oczyszczonego tekstu („tylko
  usunięte niewidoczne znaki”), a „taki sam jak wersja N” porównuje z bajtami
  zapisanej wersji, więc poprawka liczy się jako zmiana. Okno ostrzega, gdy zasady
  ukrywania danych na topikach wzoru wymieniają pola, których nowa wersja nie ma
  (pole, którego zasada nie wymienia, jest ukryte), i — tylko gdy pole różni się
  wyłącznie niewidocznym znakiem — proponuje zmianę jego nazwy w tych zasadach
  (pole wyboru, domyślnie odznaczone, bo zmienia, jakie dane widzą odbiorcy; z nazwami przed i po, bez propozycji, gdy oczyszczona nazwa była w starym tekście osobnym polem); wykonanie następuje po
  dodaniu wersji, a jego wynik (albo błąd) jest podany osobno w komunikacie. Okna
  „Dodaj wzór” i „Nowa wersja” mają przycisk „Usuń niewidoczne znaki” (zostawia
  początkowy BOM XSD) z raportem, co usunięto, oraz na żywo podają wiersz i kolumnę
  (pole tekstowe nie numeruje wierszy; `tf-code-editor` je numeruje, ale zostawiono
  `tf-textarea`, bo okna są edytowalne). Typowe znaki mają polskie nazwy (miękki
  łącznik, spacja o zerowej szerokości, RLO, BOM...) z kodem U+XXXX, a U+00AD,
  U+200B i U+200D podpowiedź przyczyny (tekst z Worda lub WWW; emoji złożone).
  Nazwy pól w diffach, regułach ukrywania danych, podglądzie i tabelach strony
  wzoru pokazują odrzucany znak jako `⟦U+XXXX⟧`. XSD wklejony z BOM przechodzi
  kontrolę XML w oknie. „Co się stanie po dodaniu” znika, gdy widać odmowę, a toast
  „Kopiuj” znika przy opuszczeniu strony wzoru.
- **Poprawka wzoru z niewidocznym znakiem nie udaje, że zapisane wiadomości się
  zmieniły.** Gdy porównanie przechodzi dopiero po usunięciu znaków ze starego
  tekstu, wersja się rejestruje, a `SchemaRegisterResponse.old_spelling_kept`
  (append-only, `serde(default)`; REST: `old_spelling_kept` tylko gdy niepuste)
  podaje nazwy i wartości, które zapisane rekordy nadal piszą po staremu
  (`pac⟦U+202E⟧jent`); okno mówi to wprost. Usunięcie znaków, które zlałoby dwa
  klucze starego tekstu w jeden (`pesel` i `pesel<U+200B>`), jest odmową z nazwą
  pola — serde_json zachowałby po cichu ostatni. Zdanie „pola i wymagania bez
  zmian” pada tylko wtedy, gdy usunięte znaki nie leżały w żadnej nazwie ani
  wartości; w przeciwnym razie okno wylicza zmienione nazwy. Flaga „topiki nie
  przyjmują wiadomości” jest tylko na wersji, według której topiki sprawdzają
  (inna wersja dostaje samą notkę), Stan topiku ocenia wersję zwróconą przez
  odczyt i nie rzuca alarmu, gdy rodzaj wzoru jest nieznany, a nieudana zmiana
  nazw w zasadach podaje, ile zasad już zmieniono.
- **„Trojan Source” przez sekwencje ucieczki, lista starej pisowni i okna.**
  Sprawdzenie odmawia teraz także znaku zapisanego sekwencją: `\uXXXX` (z parami
  zastępczymi) w JSON Schema, Avro i profilu HL7, odwołania liczbowe
  `&#x202E;` i `&#8238;` poza komentarzami, CDATA i instrukcjami w XSD, oraz
  bajty `\xHH` i ósemkowe oraz `\a \b \f \v` w literale Protobuf, składane w
  UTF-8 (w Protobuf `\u` nie jest sekwencją, parser czyta je jako literę;
  Thrift przyjmuje tylko `\\ \" \' \n \r \t`, więc nic nie dekoduje się tam do
  odrzucanego znaku). Komunikat ma to samo zdanie z miejscem sekwencji i
  dopiskiem „(written as an escape sequence)”; stary zapisany tekst z takim
  znakiem jest porównywany po jego usunięciu, jak ten z samym znakiem.
  Zlanie kluczy po usunięciu znaków jest wykrywane także, gdy stary tekst ma już
  własny duplikat, a biały znak między tokenami nie przerywa odczytu.
  `old_spelling_kept` nie zależy od wyniku zgodności ani trybu (także `none`),
  obejmuje tylko nazwy i wartości (nie tytuły, opisy i komentarze), a
  `old_spelling_kept_total` (append-only, `serde(default)`) podaje prawdziwą
  liczbę ponad ograniczoną do 20 listę. Okno odmowy pokazuje wiersze z
  znakiem jako `⟦U+XXXX⟧`; pole „zmień nazwy w zasadach” opisuje oba wyniki
  (zaznaczone i nie), a to samo zdanie stoi w „Co się stanie po dodaniu” i w
  komunikacie po zapisie. Komunikat po zmianie nie ginie, gdy czytelnik jest poza
  stroną wzoru (toast niesie cały tekst).
- **„Trojan Source”: sekwencje poniżej U+0100, wszystkie miejsca w jednej
  odmowie, odczyt w przeglądarce.** Sekwencja jest odmawiana tylko wtedy, gdy
  dekoduje się do U+0100 lub wyżej albo do U+00AD i U+0085; sterujące i bajty,
  które formaty zapisują tak zwyczajnie (`[^\u0000-\u001F]` we wzorcu JSON,
  `[^&#x7F;-&#x9F;]` w XSD, `"\x00\x01"` w proto2, domyślne `bytes` w Avro oprócz bajtów 0x85 i 0xAD, których `\u0085` i `\u00AD` nadal odmawia), przechodzą,
  więc zapisane wersje z takimi tekstami nadal się kompilują. Surowy znak jest
  odmawiany wszędzie jak dotąd. Odwołania XSD czytane są do ostatniej cyfry
  (zera wiodące nie omijają sprawdzenia), ósemkowe sekwencje Protobuf tak jak jego
  lekser (po pierwszej cyfrze ósemkowej dowolna dziesiętna, bajt się zawija).
  Jedna odmowa wymienia każde miejsce (znak i sekwencję razem, do 20 i „; and N
  more”), by administrator nie był odmawiany dwa razy. Lista starej pisowni
  pomija `pattern`, `$id`, `$schema`, `format` i literały tekstowe Protobuf/Thrift.
  Przeglądarka rozpoznaje te same sekwencje (JSON/Avro `\uXXXX` z parami,
  `&#x…;`/`&#N;`, `\xHH` i ósemkowe), wypisuje je osobno, zaznacza w podglądzie, a
  przycisk „Usuń niewidoczne znaki” obiecuje tylko dla znaków wpisanych wprost.
  Skutek pola „zmień nazwy w zasadach” podaje stare i nowe nazwy oraz topiki, a
  komunikat po zapisie mówi to w czasie przeszłym i prowadzi do ukrywania danych.
  Dalsze poprawki: sąsiednie literały Protobuf (`"\xE2" "\x80\xAE"`) są łączone
  przed dekodowaniem UTF-8 jak w lekserze (także domyślna wartość pola `bytes`:
  `"\x00\x01"` przechodzi, bo dekoduje się poniżej U+0100, a bajty tworzące
  odrzucany znak nie); w XSD odwołania w wartościach atrybutów są czytane także
  po `<!--`, `<?` i `<![CDATA[` wpisanym w atrybut; jedynym wyjątkiem od odmowy
  sekwencji jest `value` elementu `xs:pattern` (wyrażenie regularne XSD nie ma
  `\u`; element jest rozpoznawany po nazwie lokalnej, bez sprawdzania
  przestrzeni nazw). W JSON, Avro i profilu HL7 nie ma wyjątku: znak w `pattern`
  zapisany jako `\u200B` jest odmawiany, a odmowa podpowiada zapis `\\u200B`
  (ukośnik przed sekwencją, który silnik wyrażeń czyta jako znak). Skan przy
  starcie nie czyta sekwencji JSON w wierszu nieznanego rodzaju i dopisuje „(written
  as an escape sequence)” do linii o sekwencji; linki do ukrywania danych są też
  po udanej i częściowej zmianie nazw. Znane ograniczenie: sekwencja znaku
  sterującego C0/C1 poniżej U+0100 w kluczu lub nazwie przechodzi po stronie
  serwera (panel ją oznacza).

## [0.4.0-beta.1] — 2026-09-29

Zmiany od tagu `v0.4.0-beta`. Pełny opis zmian od `v0.3.0-beta` znajduje się
w [sekcji 0.4.0-beta](#040-beta--2026-09-27). Poprzednia próba wydania
nie zakończyła się publikacją artefaktów; ten tag uruchamia ponowną kompilację
wszystkich platform. Wynik kompilacji należy sprawdzić w GitHub Actions.

### TentaNas

- Dodano alerty utraty nasłuchu RDMA i rozpoznawanie połączeń NVMe-oF RDMA.
  Alerty zachowują stan między odczytami, a spóźnione wyniki diagnostyki
  nie nadpisują aktualnego stanu targetu.
- Kreator targetów ostrzega o niedostępnym iSER i blokuje niedostępne opcje.
  Wykrywanie urządzeń RDMA uwzględnia interfejsy VLAN i bond; odczyt GID
  wykonuje się poza asynchronicznym wątkiem wykonawczym.
- Ograniczono ujawnianie informacji o peerach należących do innych organizacji.

### TentaBus

- Topik ma opis i autora. Opis (do 500 znaków) podaje się w pierwszym kroku
  kreatora „Nowy topik” albo w oknie „Zmień” karty „Opis” w Ustawieniach
  topiku; strona topiku pokazuje go pod nazwą, a bez opisu — jak dotąd rodzaj
  treści i wzór. Karta „Opis” pokazuje też, kto utworzył topik (nazwę osoby
  albo klucza API). Autor jest zapisywany z sesji przy tworzeniu i nie zmienia
  go żadna aktualizacja; topiki utworzone przed tą wersją nie mają autora.
  Protokół: `BusTopicOptionsWire.description`,
  `BusTopicConfigWire.{description, created_by, created_by_label}`.
- Wycofanie pojedynczej wersji wzoru wiadomości: `SchemaDeleteRequest`
  z `version` i `deprecate_only` (oraz REST `DELETE …?version=N&deprecate_only=true`)
  oznacza tę wersję jako wycofaną zamiast odrzucać żądanie. Topik sprawdza
  wiadomości najwyższą niewycofaną wersją; gdy wycofany jest cały wzór albo
  wszystkie jego wersje, sprawdza dalej ostatnią wersją. Wycofany wzór nadal
  nie przyjmuje nowych wersji ani nowych powiązań z topikami. Lista wersji
  podaje `deprecated_at_ms` (`BusSchemaVersionWire`, REST), a wpis audytu
  `bus.schema.deprecate` podaje numer wersji.
- Aktualizacja: migracja bazy 176 dodaje `bus_topics.description`,
  `bus_topics.created_by`, `bus_schema_subjects.deprecated_versions_json`
  i `bus_schema_subjects.generation` (wcielenie wzoru: wzór usunięty
  i zarejestrowany ponownie jest nowym wzorem, do którego nie trafia nic,
  co dotyczyło poprzedniego, łącznie z jego wersjami),
  `bus_schema_versions.subject_generation` (wcielenie, pod którym
  zarejestrowano wersję; wersja nowego wcielenia, która dotrze przed nim,
  czeka na nie, a wersja usuniętego jest pomijana) oraz tabelę
  `bus_schema_subject_tombstones` (usunięty wzór nie wraca po spóźnionej
  zmianie z innego noda). Czasy wycofania i usunięcia wersji są znaczone
  zegarem synchronizacji (HLC), nie zegarem ściennym noda.
  Opis i autor synchronizują się w wierszu topiku (ostatni zapis wygrywa,
  autor zostaje ustalony przy tworzeniu). Wycofania wersji i wycofanie całego
  wzoru synchronizują się jako suma: dwa jednoczesne wycofania na różnych
  nodach przetrwają oba, a zapis z noda, który jeszcze nie widział wycofania,
  go nie cofa; wersja usunięta na stałe nie wraca jako wycofana. W klastrze
  z nodami w starszej wersji ich zmiany topiku i wzoru nie czyszczą opisu ani
  wycofań zapisanych na nowszych nodach, a wycofania wysłane przez starszy
  node nie trafiają do wzoru znanego nowszym nodom jako konkretne wcielenie; starsze nody same nie znają opisów
  ani wycofanych wersji (dla nich wzór sprawdza zawsze najnowszą wersję),
  więc do czasu aktualizacji wszystkich nodów instancji walidacja może się
  między nimi różnić. Opis spoza limitów tej wersji (np. dłuższy niż
  500 znaków) jest przy synchronizacji skracany.
- Wzory wiadomości w panelu: wiersz listy otwiera stronę wzoru, a
  administrator instancji dodaje wzór („Dodaj wzór”: nazwa, zgodność, format
  z tych, które serwer sprawdza, tekst wpisany albo wczytany z pliku) i usuwa
  wzór, którego nie używa żaden topik — przy używanym zamiast kosza jest
  kłódka, która po kliknięciu podaje topiki. Strona wzoru pokazuje format, wersję, stan, kto i kiedy
  go dodał, topiki i zgodność, tekst wybranej wersji tylko do odczytu
  z „Kopiuj” i „Pobierz” oraz opis zapisany w samym wzorze (`description`
  JSON Schema, `doc` Avro), wersje z „Wycofaj” przy każdej aktywnej i kartę
  zgodności z „Zmień”. Okna „Nowa wersja”, „Zmień zgodność”, „Wycofaj wzór”,
  „Wycofaj wersję” i „Usuń…” mówią przed potwierdzeniem, według której
  wersji topiki będą sprawdzać wiadomości. Odmowę nowej wersji przez
  zgodność panel opisuje zwykłymi słowami (np. „nowa wersja wymaga pola
  „pilne”, którego stare wiadomości mogą nie mieć”), a nieznany powód
  pokazuje z tekstem serwera w zwijanym bloku; tekst odrzuconej wersji
  wraca przy następnej próbie. Wycofany wzór ma ostrzeżenie „Wzór wycofany”
  i nie przyjmuje nowych wersji. Gdy wzór o tej samej nazwie powstał w
  międzyczasie, „Dodaj wzór” mówi, że dodano do niego wersję (albo że nic
  się nie zmieniło), zamiast ogłaszać nowy wzór. Adres strony wzoru:
  `#/tentabus?instance=…&tab=schemas&subject=…`.
- Edytor kodu w panelu (`tf-code-editor`) po podmianie całego tekstu
  pokazywał wiersze poprzedniego dokumentu, gdy miał on tyle samo linii;
  teraz rysuje nowy tekst.

- Nieprzetworzone wiadomości: usunięto ograniczenia opisane w wydaniu 0.4.0-beta.
  „Wczytaj więcej” nie powtarza wierszy przy wielu partycjach. Ponowienie
  i odrzucenie wykonuje tylko node prowadzący partycję nieprzetworzonych
  wiadomości, a oznaczenie obsługi trafia do każdej jej kopii, także tej, która
  była niedostępna. Ponowienie przerwane po zapisaniu wiadomości w topiku
  nie zapisze jej drugi raz. „Ponów wszystkie” zaczyna od najstarszych
  wiadomości ze wszystkich partycji, a wiadomość znów odrzuconą przez wzór
  liczy jako nieponowioną. Tekst błędu odbiorcy jest ukryty przed osobami,
  które obejmują zasady ukrywania danych topiku. Lista czyta tylko wiadomości,
  które pokazuje, i trzyma z nich jedynie podgląd treści, a „Ponów wszystkie”
  czyta ograniczoną liczbę wiadomości na raz i podaje, ile wiadomości
  odrzuconych przy zapisie zostawiło na liście, by można je było odrzucić.
- Ponowiona wiadomość, którą wzór topiku znów odrzuca, trafia z powrotem do
  tej samej partycji nieprzetworzonych wiadomości. Jeśli jej nowej kopii nie
  uda się zapisać, ponowienie jest odmawiane, a wiadomość zostaje na liście —
  wcześniej znikała. Liczba wiadomości skierowanych do nieprzetworzonych przy
  zapisie (`schema_rejected`) obejmuje teraz tylko te, których kopię zapisano.
  Wiadomości, których kopii nie udało się zapisać — utracone, ani
  opublikowane, ani wśród nieprzetworzonych — podaje nowe pole
  `schema_dropped` w odpowiedzi REST publikacji, w wyniku funkcji hosta
  `bus_publish_v1` (SDK Rust i .NET) oraz w metadanych bloku przepływu
  (`bus_publish_schema_dropped`).
- Ponowienie, które reguła jednorazowego dostarczenia topiku (klucz
  idempotencji) odrzuciłaby jako powtórzenie oryginału, jest odmawiane,
  a wiadomość zostaje na liście — wcześniej znikała z listy, nie wracając do
  topiku. Można ją ponowić po upływie okna tej reguły.
- Nagłówki `dlq.*` dodane przez producenta nie przechodzą do kopii
  nieprzetworzonej wiadomości, więc nie podszyją się pod dane szyny.
- Oznaczenie obsługi przechodzi teraz strumieniem replikacji partycji
  nieprzetworzonych wiadomości: w klastrze, w którym część nodów ma jeszcze
  wersję 0.4.0-beta, oznaczenia nie docierają do kopii do czasu aktualizacji
  wszystkich nodów.
- Znane ograniczenia:
  - Ponowienie wymaga, by ten sam node prowadził partycję nieprzetworzonych
    wiadomości i partycję topiku, do której wiadomość wraca; zapis nie jest
    przekazywany między nodami. W przeciwnym razie ponowienie jest odmawiane
    z nazwą noda prowadzącego partycję topiku.
  - Ochrona przed powtórnym zapisem przerwanego ponowienia działa na nodzie
    prowadzącym partycję topiku; jeśli między przerwaniem a kolejnym
    ponowieniem prowadzenie tej partycji przejmie inny node, wiadomość może
    trafić do topiku drugi raz.
  - Oznaczenie obsługi jest wysyłane do kopii co 0,5 s i nie czeka na ich
    potwierdzenie. Jeśli prowadzenie partycji nieprzetworzonych wiadomości
    przejmie inny node, zanim oznaczenie do niego dotarło (utrata noda
    w tym oknie albo kopia niepołączona od chwili oznaczenia do zmiany
    prowadzenia), ponowiona lub odrzucona wiadomość wraca na listę na nowym
    nodzie i może zostać ponowiona jeszcze raz, także przez „Ponów wszystkie”.
    Nowy prowadzący nie pobiera oznaczeń od pozostałych kopii.

### Poprawki

- Kompilacja zvec dla iOS wybiera SDK hosta macOS dla narzędzi uruchamianych
  podczas budowania. iOS nie należy do macierzy artefaktów tego wydania.
- Numer wersji aplikacji i pakietów dziedziczących wersję workspace ustawiono
  na `0.4.0-beta.1`; Android używa `versionCode = 5`.
- Usunięto błąd kompilacji parsera diagnostyki RDMA na Windows. Rodziny adresów
  IPv4 i IPv6 są rozpoznawane według linuksowego ABI netlink, niezależnie od
  systemu, na którym działa parser.
- Dodano artefakt diagnostyczny kompilacji Androida z logiem kompilacji oraz
  pomiarami pamięci RAM, swapu i miejsca na dysku, ułatwiający analizę przerwań.

## [0.4.0-beta] — 2026-09-27

Zmiany od tagu `v0.3.0-beta` do `v0.4.0-beta`. Wydanie testowe.
Tag `v0.3.0-beta` nie zakończył się publikacją artefaktów wydania na GitHubie.
Dla instalacji z ostatniego opublikowanego wydania `v0.1.0-beta` obowiązują
również opisane poniżej zmiany i uwagi aktualizacyjne wersji 0.2.0 oraz 0.3.0.

### Aktualizacja i zgodność

- **Zaktualizuj wszystkie węzły TentaBus, zaczynając od replik podrzędnych,
  a kończąc na liderach.** Pełna ochrona zatwierdzonych wiadomości oraz zasada
  jednego lidera na epokę wymagają nowej wersji na każdym węźle. Starsze węzły
  nie zapisują epok ani zatwierdzonych offsetów i nie składają trwałych obietnic
  wyborczych; podczas aktualizacji mieszany klaster nie ma pełnych gwarancji.
  Nowy lider może wstrzymać widoczność odtworzonych wiadomości do potwierdzenia
  jego kadencji przez większość nowych replik lub zapisu nowej wiadomości przez
  większość. Stare partycje zaczynają z zatwierdzonym offsetem 0, co zwiększa
  zakres ponownej synchronizacji.
- Migracje bazy obejmują zwalnianie nazw usuniętych workspace'ów (171), generacje
  tematów i porządkowanie pozostałości TentaBus (172–174) oraz trwałe przydziały
  epok replik (175). Partycje zapisują epoki w `partition.epochs` i zatwierdzony
  offset w rozszerzonym `partition.meta`.
- Narzędzia plikowe agentów zwracają teraz `blob_id` i przyjmują
  `expected_blob_id`, zamiast `sha256` i `expected_sha256`. To identyfikator
  obiektu Git kopiowany z wyniku odczytu. Własne prompty i wywołania narzędzi
  wymagają dostosowania; niezmodyfikowany prompt dostarczany z aplikacją jest
  aktualizowany automatycznie.
- `tentanas-helper` ma wersję `0.17.2`; nowe operacje i diagnostyka wymagają
  aktualnego helpera na zarządzanym węźle.

### TentaBus

- Nowe ekrany tematów, odbiorców oraz nieprzetworzonych wiadomości: filtrowanie,
  kreator tematu, podgląd wiadomości, usuwanie z potwierdzeniem nazwy,
  szczegóły ustawień i partycji, opóźnienia odbiorców, wstrzymywanie odczytu
  oraz ponawianie i odrzucanie nieprzetworzonych wiadomości.
- Przesuwanie miejsca czytania według numeru, początku, końca lub czasu pokazuje
  liczbę wiadomości do powtórzenia albo pominięcia. Działa także dla aktywnego
  odbiorcy: stare potwierdzenie nie cofa zmiany, a nowe pobranie używa nowej pozycji.
- Widok „Kopie i nody” pokazuje liderów, repliki, problemy i historię zmian.
  Nowe partycje otrzymują liderów; zmiany ustawień trafiają od razu do innych
  węzłów. Usunięto ręczne przestawianie zestawu replik w interfejsie.
- Uprawnienia tematów obejmują widoczność odbiorców, statystyki, informacje
  o replikach i przenoszenie lidera. Zmiany uprawnień aplikacji działają od razu,
  także po synchronizacji z innego węzła. Usunięcie tematu usuwa jego reguły,
  zasady ukrywania danych i kolejkę nieprzetworzonych wiadomości.
- Ponowne utworzenie tematu o tej samej nazwie tworzy nową generację:
  stare dane i liderzy nie przechodzą do nowego tematu.
- Poprawiono wybory lidera, rozstrzyganie rozbieżnych logów, blokowanie zapisów
  przez odsuniętego lidera oraz natychmiastowe przekazywanie zatwierdzonego
  offsetu replikom. Dla co najmniej trzech replik wybory wymagają większości;
  odtworzone wiadomości wcześniejszej kadencji pozostają niewidoczne, dopóki
  większość nie potwierdzi nowej kadencji.
- Epoka identyfikuje jednego lidera dzięki stałym slotom i trwałym obietnicom
  wyborczym. Dodanie repliki wymaga lidera partycji; limit wynosi 64 przydzielone
  sloty na generację tematu, bez ponownego używania slotów usuniętych węzłów.
  Po wyczerpaniu limitu trzeba odtworzyć temat. Wybory mogą potrwać dłużej
  o losowe opóźnienie do jednej czwartej dzierżawy lidera i dodatkowe oczekiwanie
  na odpowiedź wyborczą (300 ms).
- `acks=all` czeka na żywe, zsynchronizowane repliki, przy co najmniej trzech
  kopiach zachowując wymóg większości. Dla dwóch kopii ocalała replika przyjmuje
  `acks=leader` i `acks=all`; `acks=quorum` nadal wymaga obu.

### TentaNAS

- Rozbudowano diagnostykę RDMA/iSER: wykrywanie urządzeń, rzeczywistego nasłuchu
  oraz transportu aktywnych sesji. Kreator targetów weryfikuje interfejs sieciowy;
  ekran sesji rozróżnia połączenia iSCSI i iSER oraz pozwala rozłączać iSCSI.
- Zaostrzono listy dozwolonych inicjatorów i zamykanie dynamicznych uprawnień
  po przejściu z otwartego dostępu na listę dozwolonych. Dodano wykrywanie
  zdalnych LUN-ów i ochronę operacji niszczenia przed przekroczeniem organizacji.
- Wyłączanie i odinstalowywanie dodatku pokazuje konsekwencje na poszczególnych
  węzłach, status dostępności i potwierdzenia. Zatrzymanie udostępniania oraz
  wyłączenie mogą wymagać zatwierdzenia przez drugą osobę; ponowne włączenie
  przywraca udostępnianie. Odliczanie potwierdzenia pochodzi z węzła.
- Poprawiono raportowanie użytecznej pojemności, liczby targetów, zajętości
  folderów, znanych pul i niedostępnych węzłów. Dodano rozpoznawanie AnyRAID
  oraz odłączanie dysku z puli ZFS.
- Poprawiono zadania rozruchowe, scrub i SMART; dodano zbiorcze zadanie SMART
  oraz przekazywanie alertów organizacji z kontrolą adresów chroniącą przed SSRF.
- Komunikaty odmowy, stanów i harmonogramów mają kody oraz parametry tłumaczeń.
  Dialogi i logi używają czytelnych nazw, a diagnostyka RDMA otrzymała tłumaczenia.

### Code Studio i agenci

- Kolejne tury sesji zachowują rozmowę. Nowa sesja przypina bieżącą, zapisaną
  wersję procesu agentowego, a podagenci otrzymują pierwotne zadanie użytkownika.
- Następne rundy planowania, implementacji i recenzji otrzymują wcześniejsze
  uwagi krytyka oraz wyniki testera. Odmowa zawierająca frazę „BEZ UWAG” nie
  zostaje uznana za akceptację; awaria wszystkich oczekiwanych podagentów
  kończy proces z rzeczywistą przyczyną.
- Usunięto limit 50 uruchomień na sesję oraz domyślny godzinny termin zakończenia
  orkiestratora. Własne limity operatora są zachowane, a pętle recenzji korzystają
  ze swoich budżetów. Odpowiedź ucięta przez limit tokenów nie oznacza sukcesu.
- Strumień pokazuje odpowiedź agenta, trwające myślenie, czas i aktualne narzędzie.
  Akceptacja zmian kończy oczekującą recenzję; pytanie nie zasłania całej rozmowy.
  Usunięcie workspace'u zwalnia jego nazwę.
- Na Linuksie wykrywanie sandboxa sprawdza rzeczywiste uruchomienie `bwrap`.
  Administrator może naprawić profil AppArmor z dashboardu, również na zdalnym
  węźle, podając hasło sudo. Błędy logowania CLI pokazują końcówkę wyjścia
  procesu po usunięciu sekwencji terminala i zamaskowaniu sekretów.

### Modele, RAG i inferencja

- Natywne osadzanie modeli GGUF do embeddingów i rerankingu przez llama.cpp
  rzeczywiście ładuje model, obsługuje ranking par zapytanie–dokument i zwalnia
  model przy zatrzymaniu. Modele pomocnicze mogą działać obok załadowanego LLM.
- Wyszukiwanie wektorowe pomija nieistniejące pola projekcji bez usuwania filtrów;
  błędy wykonania flow zachowują pełny łańcuch przyczyn.
- Rozmowy przekazywane przez mesh zachowują wymianę narzędzi i raportują zużycie
  tokenów. Parser rozpoznaje DSML DeepSeek, także warianty mieszane z JSON
  i argumenty zagnieżdżone w pojedynczym polu `arguments`.
- W `tentaflow-infer` dodano integrację CoreML/Apple Neural Engine do obliczeń
  prefill, eksport odpowiednio przygotowanych modeli oraz pomiary na Apple
  Silicon. Ta ścieżka wymaga wyeksportowanych modeli CoreML; nie oznacza
  automatycznej obsługi dowolnego modelu przez ANE.
- Poprawiono tensor parallelism CUDA i kernele NVFP4 oraz odświeżono receptury
  konfiguracji vLLM.
- Dodano dane, benchmarki i skrypty przygotowania, ewaluacji oraz treningu QLoRA
  dla adaptacji modeli do języka polskiego. Są to narzędzia treningowe,
  a nie nowe wytrenowane wagi dostarczane z aplikacją.

### Platformy i zarządzanie węzłami

- W procesie wydania wydłużono przechowywanie artefaktów bibliotek natywnych
  z jednego do siedmiu dni, aby pozostawały dostępne dla późniejszych etapów
  kompilacji. Numer wersji aplikacji Android ustawiono na `0.4.0-beta`.
- Linux: sherpa-onnx i pozostałe moduły korzystają ze współdzielonego ONNX Runtime,
  co usuwa konflikt ABI powodujący awarię startu `free(): invalid pointer`.
  Pełne archiwa zawierają `libonnxruntime.so.1`; wariant GPU ARM64 korzysta
  z ONNX Runtime 1.30.0 dla CUDA 13.
- macOS: budowanie bibliotek natywnych wybiera SDK aktywnego Xcode.
- Zdalne zatrzymywanie wdrożenia klastrowego trafia do węzła z jego rekordem;
  wydłużono czas na zdalne operacje usług. Widok mesh zachowuje rodzaj węzła
  i oznaczenie operatora.
- Poprawiono tabele na telefonach, okna modalne i ukrywanie `tf-alert`.

### Znane ograniczenia

- Wydanie obejmuje Linux x86_64 i ARM64, macOS Apple Silicon, Windows x86_64
  oraz Android ARM64. Nie zawiera kompilacji dla iOS ani macOS Intel.
  Dostępne warianty akceleracji zależą od platformy i załączonych archiwów.
- APK Android jest podpisany kluczem debug i przeznaczony do instalacji ręcznej.
  Przyszła wersja z innym kluczem podpisu nie zaktualizuje go w miejscu.
- TentaBus z dwiema replikami preferuje dostępność: podział sieci może pozostawić
  dwóch liderów. Po połączeniu wygrywa nowsze przywództwo, a nieprzesłane zapisy
  przegranej strony przepadają. Utrata jedynej pozostałej repliki również może
  oznaczać utratę zatwierdzonych tylko przez nią wiadomości.
- Wstrzymanie odbiorcy i cofnięcie jego miejsca czytania nie są utrwalane na
  wszystkich replikach i nie przetrwają zmiany lidera.
- Lista nieprzetworzonych wiadomości może powtarzać wiersze przy stronicowaniu
  wielu partycji. Ponowienie/odrzucenie nie wymaga jeszcze lidera, a oznaczenie
  obsługi nie dociera do każdej kopii. Awaria między ponownym publikowaniem
  a zapisaniem oznaczenia może pozwolić na powtórne ponowienie wiadomości.
  Ponawianie zbiorcze nie gwarantuje kolejności od najstarszej wiadomości między
  partycjami; ponownie odrzucony zapis może zostać policzony jako ponowiony.
  Tekst błędu odbiorcy nie podlega regułom ukrywania pól. Odczyt listy przegląda
  także obsłużone rekordy w swoim oknie, co może zwiększać koszt odczytu.

[Pełne porównanie zmian 0.3.0 → 0.4.0](https://github.com/Slyb00ts/TentaFlow/compare/v0.3.0-beta...v0.4.0-beta).

## [0.3.0-beta] — 2026-09-24

Zmiany od tagu `0.2.0-beta`. Tagi `0.2.0-beta` i `0.3.0-beta` zostały
przygotowane, ale ich procesy wydania nie zakończyły się publikacją artefaktów.
Ostatnim opublikowanym wydaniem pozostało `0.1.0-beta`; przy aktualizacji
obowiązują również poniższe uwagi wersji `0.2.0-beta`.

### Upgrade notes

- **Update every mesh node together.** The binary protocol moved from schema 29
  to 32; an older and a newer node reject each other's handshake.
- **Sign in to agent CLI accounts again.** Agent CLI accounts now live in the
  provider-account registry. Every migrated account starts as "sign-in
  required": credentials stored in the old places (the Code Studio content
  database and the bridge's on-disk logins) are **not** carried over, because
  they cannot be decrypted with another node's key. At startup the node logs how
  many credential rows it removed, per node and engine and in total, so the
  loss of old entries never looks like silent data loss.
- **TentaBus partitions damaged by the old segment-roll bug no longer open.** A
  partition whose segment files were misnamed by that bug now fails with
  `SegmentOffsetMismatch` instead of being silently truncated; recover it by hand
  from a healthy replica.

### Windows

- Release archives for Windows x86_64 — `slim`, `full-vulkan` and `full-cuda13`
  — built in CI by the same `setup.ps1` → `build-all.ps1` → `build.ps1` scripts
  a developer runs. Each archive carries its Visual C++ runtime (and cuBLAS for
  `full-cuda13`); CI starts every archive from a bare `PATH` before publishing.
- `install.ps1` installs TentaFlow as the `TentaFlow` Windows service: explicit
  edition choice, verified download, the GStreamer runtime the `full` editions
  need (checksum-verified), a virtual service account, automatic start with
  restart on failure, firewall rules and a data directory only the service and
  administrators can read. `uninstall.ps1` removes it (`-Purge` also removes
  the data).
- `tentaflow start|stop|restart|status` drive the Windows service, and
  `tentaflow update` updates a Windows installation, including a newer
  GStreamer runtime when the release needs one.
- CI installs, upgrades, drives and uninstalls every Windows archive on a clean
  runner before a release is published.
- Host telemetry on Windows: GPU utilisation and VRAM (DXGI + PDH, independent
  of the UI language), disks, and network interface details now report real
  values instead of zeros.

### Android

- The Android debug APK (arm64-v8a) is attached to the release. It is
  debug-signed: it installs by sideloading, and a future properly signed build
  will not upgrade it in place.

### TentaNAS and storage

- Elastic Array: isolated mounts for new arrays, a durable service mode, a
  verified single-disk cache tier and a mover that relocates files from the
  cache to the data disks automatically, on a schedule or on demand — without
  freezing the share.
- Scheduled sync and scrub (every array is scrubbed monthly); a failed parity
  run is recorded as a cause instead of wedging the array, a Sync over a fault
  needs an explicit acknowledgement, and an unfinished disk add can be resumed
  or undone.
- Repair, grow and dissolve an array; share and adopt arrays between nodes;
  export an array over NFS; clear a disk only behind a namespace-proof guard
  and a retyped name.
- Disk health: correct SMART and NVMe self-test decoding, one self-test per
  disk at a time, failures that reach the health verdict, and a dashboard that
  tells a failing disk from a warning.
- Multi-tenancy: jobs, alerts, disks, approvals, share sources and targets are
  scoped to the asking organisation; internal configuration rows stay hidden
  from tenants.
- The UI names nodes, users and disks instead of showing identifiers, and
  patches the dashboard, pool and array views in place instead of rebuilding
  them.

### TentaBus

- Package 1: field policies reach the dead-letter queue, per-action ACLs,
  instance isolation, quota enforcement and validation.
- Schema-registry REST API with per-action API-key scopes; the compliance
  retention floor applies to bus topics.
- Replication: followers acknowledge on their own cadence and the leader waits
  without a thread per publish, raising quorum throughput from about 1k to about
  220k messages/s. Fixed a segment-roll bug that could make consumers skip
  records, and leader-term races (term rollback, duplicate leaders after
  failover, unauthenticated leader handshakes). The replication transport is
  hardened and the failover audit names the instance.
- Lag without side effects, truthful deprecation, lag history, and the first
  TentaBus screen (shell and overview).

### Agents, Code Studio and provider accounts

- Provider accounts: an on-demand runtime, CLI sign-in with a login GUI,
  sessions, shared accounts and credentials that sync between nodes.
- The account card no longer claims an account has no sessions when it only
  sees this node's. It tells apart an account homed on this node, one homed on
  another reachable node (named, with its own subset) and one homed on a node
  that does not answer ("no data" instead of an empty list).
- Coding agents run on Linux and Windows; only the Code Harness that Code
  Studio actually runs is kept; agents without a model get a chat model.

### Robotics (Go2)

- Cloud onboarding, the `data2=3` handshake with a per-device AES key and GPU
  camera anonymisation with tighter privacy regions.
- Shared map: a persistent occupancy model, frame carving, chunk storage, a
  control plane and relocalisation against the shared map by branch-and-bound
  scan matching.

### Inference, vision and voice

- ONNX Runtime never falls back to the CPU with device-bound inputs, and cuDNN
  is preloaded for the CUDA provider.
- Addon model requirements can be installed from the addon's settings.
- GPU detection runs in a child process, so a crashing driver cannot take the
  node down.
- A bundled Jarvis voice clone for Supertonic TTS.

### Mesh and sync

- Baseline adoption and node-log catch-up no longer block between nodes; a
  donor that refuses a fuller requester adopts from it instead; a replicated
  flow version whose number is already taken locally is kept.
- Security and mesh audit findings closed.

### Build

- The `slim` edition compiles again; Arch Linux setup and macOS build errors
  are fixed; generated browser assets are no longer tracked in Git.
- Native library and toolchain versions live in one file,
  `scripts/versions.env`, read by every build script on every platform.

## [0.2.0-beta] — 2026-09-08

The main changes since the last published release, `0.1.0-beta`.

### TentaNAS and storage

- A new application for managing disks, ZFS pools, datasets, snapshots, network
  shares and block storage across the fleet, with schedules, an access audit, a
  restricted privileged helper and second-administrator approval for selected
  operations.
- Elastic Array joins data and cache disks through mergerfs with periodic
  SnapRAID parity. Operations run with a durable intent record; data moves off
  the cache, sync and scrub run on demand, mounts can be restored, and data still
  waiting for parity protection is visible.

### Applications, TentaBus and agent accounts

- Multiple application instances with separate data, permissions and lifecycle.
  TentaBus gained a durable log, replication, a schema registry, field policies
  and integration with flows, REST and the SDK; instance isolation and recovery
  after a leader change were fixed.
- Code Studio manages agent CLI accounts, isolated working directories and moving
  accounts between nodes. TentaVM foundations: a host registry, capability
  probing, and granting and requesting access.

### TentaQuant

- A new quantum-circuit studio and notebook with an OpenQASM 3 subset parser,
  CPU/WGPU simulation, in-browser execution through WASM and Qiskit export. Jobs
  can be cancelled, stream their results, visualise the state, and compare and
  export results.

### Mesh, clusters and processing

- The Mesh list shows the local node, devices discovered over mDNS and trusted
  nodes, keeping transitive trust; the relay does not create a global device
  directory.
- iroh update and reconnection fixes after a restart or an address change.
  Cluster sync uses a change log, and model recovery takes peer availability and
  startup time into account.
- One way of handling the default conversation flow. Node configuration refreshes
  correctly in the editor; selected voice and image-processing paths were fixed.

### Build, dependencies and cache

- One Cargo workspace, a shared lockfile and central dependency versions and
  profiles, checked by CI. Source exports keep self-contained container and SDK
  contexts; unused dependencies were removed and code adapted to new APIs.
- ThinLTO for releases, an incremental `release-fast` profile and automatic
  artefact retention in the shared scripts; less duplicated output and fewer
  needless rebuilds. [Measurements and cache rules](docs/build-performance.md)
  are documented separately.

### Addons and integrations

- Outlook, SharePoint RAG and Teams moved into the shared addons directory. The
  separate `teams-bot` WASM addon was removed; the native Meeting Bot remains.

### Installation and distribution

- An explicit Full/Slim choice also when the installer is piped. A new
  configuration has mesh enabled and HTTPS reachable from the LAN; the Linux
  system installer opens the ports in an active UFW/firewalld, keeps an existing
  configuration and warns about a loopback bind.
- The macOS Metal archive requires the Meeting Bot: before publishing, the
  workflow checks it is present, executable, built for the right architecture,
  has its dependencies and starts with `--help`.

## [0.1.0-beta] - 2026-09-02

### Added
- Installer for Linux and macOS (`curl … | sh`) with an explicit edition choice: hardware
  detection proposes, the user decides. Installs into `/opt/tentaflow/versions/<ver>` behind a
  `current` symlink, keeps configuration and data across updates, registers a systemd unit
  (Linux) or a LaunchDaemon (macOS) and starts it.
- `tentaflow start|stop|restart|status` — status reports service state, autostart, PID, config
  and probes `/health`.
- `tentaflow update` — own updater over GitHub Releases: mandatory checksum verification,
  whole-version-directory swap, previous version kept for rollback, service restarted only if it
  was running.
- `tentaflow init-config` — writes the default configuration from `NodeConfig`, pinning all three
  listeners to the chosen bind address.
- `slim` distribution edition (`--no-default-features`): gateway, mesh, flows, dashboard, addons
  and containers with no local inference engine. Its catalog keeps every cloud provider and the
  utility infrastructure — 21 entries instead of 94.
- CI `verify` job installs the built archive on a runner with real systemd and asserts autostart,
  liveness and `/health` before publishing; `scripts/ci-local/` reproduces the whole release build
  and the install test locally, across Ubuntu, Debian, Fedora and Arch.

### Changed
- ROCm/HIP removed from the application: AMD and Intel run on the portable Vulkan path, the same
  one Burn uses for vision. NVIDIA keeps CUDA.
- Release builds on Ubuntu 22.04, making glibc 2.35 / GLIBCXX 3.4.30 the supported floor; the
  installer refuses to install below it instead of leaving an unrunnable service enabled.
- Whisper is target-gated, not only feature-gated, so `full` on Apple no longer links whisper.cpp
  next to MLX.
- Vision model runners moved to `vision/runners.rs`; they are shared by the flow vision node,
  local CV and the inference batcher, and no longer live behind the camera feature.

### Fixed
- `mesh.frame_rejected` from unpaired peers no longer floods the audit log (it was 98.5% of
  1.15M rows).
- Reasoning deltas are forwarded over the mesh reverse stream.
- Mesh peer trust survives pairing, boot prune and `persisted_version` domains.

## [0.0.2-alpha] - 2026-04-23

### Added
- Added the new `www/` dashboard SPA and migrated the app to the binary WebSocket protocol with generated browser codecs.
- Added the service manifest registry, universal service catalog, and deploy wizard for Docker, native, and external engines.
- Added embedded and bundled deployment flows for AI engines, including live deployment progress streaming and shared model storage.
- Added a full meeting-bot stack with protocol support, database persistence, per-session container lifecycle, and dedicated frontend screens.
- Added multi-hop mesh topology propagation, route awareness, peer liveness tracking, and richer model and service visibility across nodes.
- Added QR-based pairing flows for mobile and tablet devices, including camera scanning and invite/PIN confirmation improvements.
- Added mobile-focused improvements across iOS and Android, including native discovery integration, QR scanning fallback, and mobile web packaging groundwork.
- Added installer and release automation, including GitHub Releases, packaged artifacts, and install scripts for Unix and Windows.
- Added IAM foundations with users, groups, role metadata, and resource permission protocol and handler support.

### Changed
- Switched the main dashboard static asset pipeline from `wwwroot/` to `www/`.
- Reworked deployment execution to use manifest-driven jobs and streamed deployment status instead of the older direct service deploy path.
- Upgraded the deploy wizard UI from simple radio inputs to richer option cards and per-GPU selection controls.
- Expanded mesh model and topology views so the UI can show backend, size, route, and peer-derived fallback data more consistently.

### Fixed
- Fixed native embedded deploys so `llama.cpp`, `MLX`, and `Whisper` create persistent service records, reappear in `Services`, and restore correctly after app restart.
- Fixed iOS and Xcode build issues around toolchain setup, Metal platform support, and mobile startup behavior.
- Fixed container bundle deployment path resolution and Docker build context handling for manifest-based deploys.
- Fixed multiple mesh pairing and discovery regressions, including duplicate connect/disconnect events, pairing completion handling, peer identity propagation, and reconnect behavior.
- Fixed mobile window sizing, fullscreen handling, and QR scanner error behavior.

## [0.0.1-alpha] - 2026-04-14

First public alpha. Everything listed below has been implemented,
compiled on Linux x86_64 + RTX 4090, and test-bootstrapped.

### Added — deploy and containers
- Generic `tentaflow-sidecar` crate (role-based QUIC bridge) with
  built-in keep-alive, idle detection, graceful shutdown. 7
  integration tests cover request/response, server shutdown notifying
  clients, client disconnect, handler errors, parallel streams, and
  long-idle keepalive.
- `ReverseProxy` sidecar role translating `ModelRequest` ↔ OpenAI /
  llama.cpp / sherpa / raw HTTP, with SSE → CBOR stream passthrough.
- Dockerfile + config + entrypoint for every model container:
  `llm-llamacpp`, `llm-vllm`, `llm-sglang`, `llm-ollama`, `stt-whisper`,
  `stt-parakeet`, `stt-qwen-asr`, `tts-sherpa`, `tts-xtts`, `tts-voxcpm`,
  `embeddings`, `reranker`, `comfyui`.
- `tentaflow-core/build.rs` embeds the container contexts as a single
  `tar.gz` (~26 MB) so a vanilla tentaflow binary can build and run any
  of them without git clone.
- `tentaflow-core/src/deploy/` module: `bundle::extract_to`,
  `docker::deploy` (bollard build + run), REST endpoints
  `GET /api/deploy/containers` and `POST /api/deploy/<name>`.

### Added — Docker-free deploy (Python bundles)
- `tentaflow-containers/python-bundles/` with one `bundle.toml` per
  engine (vLLM, SGLang, XTTS, VoxCPM, Parakeet, Qwen-ASR, ComfyUI) that
  pins python version, source (git head or pypi), launch command with
  `${MODEL}` / `${VENV_DIR}` substitution, required platforms, and per-
  backend install variants (CUDA / ROCm 7 / Metal / XPU).
- `deploy::python_venv::bootstrap` and `deploy::python_venv::deploy`:
  downloads `python-build-standalone` and `uv` into
  `~/.cache/tentaflow/`, creates a venv, installs the engine with the
  correct `--extra-index-url` and extras, then spawns it. All 7 bundles
  bootstrap end-to-end on a host with only system Python 3.14 present.
- Upstream compatibility fixes: `install_subdir` (SGLang's `python/`),
  `install_mode = "requirements_txt"` (ComfyUI), `extras_no_build_isolation`
  (flash-attn needs torch to be installed first), and a defensive
  `patch_pyproject_if_needed` that strips the `license` field so both
  old and new setuptools can build the cloned repos.

### Added — Docker-free deploy (native C/C++ binaries)
- `tentaflow-containers/native-binaries/` build scripts for
  llama.cpp, whisper.cpp, sherpa-onnx, text-embeddings-inference, and
  stable-diffusion.cpp. Each script auto-detects CUDA / Metal / Vulkan /
  CPU and produces a tarball of binary + required shared libs.
- Successful builds on the reference host: `llama-server` (CUDA, 27 MB),
  `whisper-server` (CUDA, 2 MB), `sd-server` (CUDA, 58 MB), sherpa CLI
  bundle (CPU, 36 MB).

### Added — system detection
- `system_check::collect()` reports CPU features (AVX2/AVX512/NEON), RAM,
  NVIDIA GPUs (via `nvidia-smi`), AMD GPUs (via `rocminfo` and
  `/opt/rocm/.info/version`), Intel XPU (via `sycl-ls`), Metal, Vulkan,
  plus runtime versions (`docker`, `podman`, `python`, `nvcc`).
- `GpuBackend` enum with `preferred_backend` resolution
  (CUDA → ROCm → Metal → XPU → CPU) used by `pick_install_variant`.
- Per-engine capability matrix returned to the GUI wizard so users see
  what will and will not run on their hardware.
- REST endpoint `GET /api/system/capabilities`.
- `cargo run --example system_check` CLI helper.

### Added — GUI integration
- `ws_deploy.rs` recognises both backends: for engines mapped to an
  embedded container it builds and runs via `deploy::docker::deploy`; if
  `deploy_mode == "native"` it hands off to `deploy::python_venv::deploy`.
  Falls back to legacy `docker compose` path when the engine is not
  recognised.
- Respects every wizard field by parsing the wizard's generated
  `compose_yaml` — container name, ports (TCP/UDP mix), volumes, env
  (`HF_TOKEN`, `MODEL_ID`, `GPU_MEMORY_UTILIZATION`, `GGUF_PATH`,
  `shm_size`) and GPU selection.
- LLM deploy wizard GPU picker replaced with a multi-checkbox dropdown
  — users can target any subset of their cards; the compose emits
  `device_ids: ['0','4']` and the sidecar passes `NVIDIA_VISIBLE_DEVICES`
  through.
- Three unit tests covering GPU multi-select + compose parsing.

### Added — meeting bot persistence
- Transcripts are now stored in SQLite (tables `meeting_sessions` and
  `meeting_transcripts`) instead of process memory or a JSONL file.
  Survives restart, indexed by `(session_id, timestamp_ms)`.
- Endpoints `GET /api/meeting-bot/sessions`,
  `GET /api/meeting-bot/sessions/{id}/transcripts`,
  `GET /api/meeting-bot/sessions/{id}/download`.
- Meeting bot GUI panel: download button fetches the full session;
  transcript list re-renders incrementally without resetting scroll.
- Speaker match thresholds retuned to cut false positives
  (`MATCH_CONFIDENT 0.55`, `MATCH_VERY_CONFIDENT 0.70`, strict
  `is_match()`, `INCREMENTAL_LEARN_THRESHOLD 0.65`, tracker
  similarity 0.50).

### Added — release pipeline
- `.github/workflows/release.yml`: tag `v*` triggers a matrix build
  (`x86_64-linux`, `aarch64-linux`, `aarch64-macos`, `x86_64-windows`)
  and publishes a GitHub Release with tarballs, SHA-256 sidecars,
  `install.sh`, and `install.ps1`. Tags with `-alpha`/`-beta`/`-rc`
  are marked as pre-release automatically.
- `scripts/install/install.sh` + `install.ps1` one-liner installers
  that detect platform, download the archive, verify SHA-256, install
  to `/opt/tentaflow` (or user path), and register auto-start via
  systemd / launchd / Scheduled Task.
- `scripts/release.sh` helper that bumps `tentaflow/Cargo.toml`, adds
  a CHANGELOG section, commits, tags, and pushes.
- `tentaflow update [--check|--force]` subcommand using `axoupdater` to
  swap the running binary from the latest GitHub Release.
- `RELEASING.md` documents the whole flow.

### Added — shutdown hardening
- SIGTERM + SIGINT both handled in `tentaflow/src/main.rs`.
- Unified HTTPS server now selects on the service-manager shutdown
  channel, so port 8090 is released immediately instead of sitting in
  `TIME_WAIT`.
- `MetricsCollector` background tasks join on the shutdown channel
  instead of looping forever.
- `db::checkpoint_wal` invoked on exit so SQLite WAL is flushed before
  the process dies.

### Changed
- Container images use `FROM rust:slim-bookworm` (no pinned Rust
  version) so sidecar builds always use the current stable toolchain.

### Fixed
- `tentaflow-voice` build no longer requires a system `protoc`; the
  build script falls back to `protobuf-src` when `PROTOC` is not set.
