# Demo TB-04: rozkład prowadzenia topików na 2–3 nodach

Scenariusz do przećwiczenia przez administratora: rozkład przy tworzeniu, zapis i odczyt przez node nieprowadzący, bezpieczne przeniesienie, awaria z automatycznym powrotem, odczyt z repliki. Opisy zachowania: `docs/BUS_ROZKLAD_LIDEROW_ADMIN.md`.

**Status weryfikacji.** Scenariusz przeklikano na żywo na dwóch nodach (dwa procesy na jednym serwerze, build z ekranami K8, commit 111999003). Czasy i liczby w tym dokumencie pochodzą z tego przeklikania. Przeklik wykrył kilka błędów; tekst opisuje zachowanie **po ich poprawkach (K10)**: nody są nazwane (nazwa noda, a bez niej pierwsze 8 znaków id), a po awarii drugiego noda odczyt przy RF=2 nie zatrzymuje się. Drugi i trzeci przeklik potwierdziły te poprawki. Trzeci znalazł krytyczny błąd: po restarcie noda drugi node mógł przejąć partycje zdrowemu liderowi i **zgubić zapisy potwierdzone kodem 200**. Opisano go w pkt 4 razem z poprawką, która nie była jeszcze przeklikana na żywo. Przy ponownym przekliku sprawdź szczególnie pkt 3 i 4. Czego nie sprawdzono na żywo (blokady z pkt 3b, trzeci node, rozkład po aktualizacji), jest oznaczone.

Placeholdery: `<HOST_A>`, `<HOST_B>` adresy nodów; `<PORT_A>`, `<PORT_B>` porty dashboardu (domyślnie 8090, HTTPS z certyfikatem własnym, stąd `curl -k`; przy dwóch procesach na jednym serwerze różne porty); `<API_KEY_A>` token klucza wydanego **na nodzie A** (patrz pkt 0.4); `<ORG_ID>` identyfikator organizacji; `<INSTANCE>` identyfikator instancji TentaBus (`tentabus-<8 znaków hex>`); `<TOPIK_A>` i `<TOPIK_B>` nazwy topików prowadzonych przez A i B (pkt 1); `<ID_A>` pełne id noda A (w ekranie najechanie na skróconą nazwę pokazuje je w podpowiedzi).

W ekranach node jest nazwany nazwą hosta maszyny, na której działa (u pozostałych nodów: nazwą zgłoszoną przez mesh, a w razie braku nazwą z rejestru synchronizacji). W panelu nie ma pola do jej ustawienia: zmienia się ją w systemie operacyjnym (nazwa hosta). Gdy dwa nody mają tę samą nazwę, np. dwa procesy na jednym serwerze, serwer nie podaje żadnej z nich: także nazwa z rejestru synchronizacji, równa nazwie hosta któregokolwiek noda, jest czyszczona, a ekran nazywa oba pierwszymi 8 znakami id (pełne id jest w podpowiedzi po najechaniu). Tak ma być na obu nodach, także w komunikacie „Czeka: ten topik prowadzi node …” na stronie odbiorcy. Trzeci przeklik pokazał jeszcze nazwę hosta na nodzie B (rejestr podawał tę samą wspólną nazwę); to poprawiono i nie było przeklikane ponownie. W tym dokumencie piszemy „A” i „B”.

## 0. Przygotowanie

1. Dwa nody A i B (HA: dwa procesy na jednym serwerze), **to samo środowisko**. Opcjonalnie trzeci node C (pkt 3b, 5).
2. **Parowanie.** Na nodzie A otwórz Mesh i kliknij „Paruj”; A pokaże 6-cyfrowy PIN. Na nodzie B w Mesh, w sekcji „Prośby o parowanie”, kliknij „Potwierdź parowanie” i wpisz PIN. Oba nody mają się pokazać jako „Sparowane nody”.
3. **Instalacja.** Addon TentaBus wystarczy zainstalować na jednym nodzie: instalacja replikuje się na drugi. Włącz tę samą instancję TentaBus na obu nodach.
4. **Klucze API.**
   - Klucz działa **tylko na nodzie, który go wydał**: na drugim daje 401. Do wywołań przez A użyj klucza wydanego na A, do wywołań przez B klucza wydanego na B.
   - Flow zapisujący do topiku potrzebuje **klucza topiku z prawem** w „Dostęp” › „Macierz dostępu (per klucz)”. Zapis z flow na kluczu użytkownika jest odmawiany (audyt `bus.produce.denied`) i rekord nie trafia do topiku. Flow bez strumieniowania kończy się wtedy błędem 500 `flow.failed`; flow ze strumieniowaniem (`"stream":true`) odpowiada 200 ze zdarzeniem błędu SSE `flow.failed` przed `[DONE]`.
   - Klucz użytkownika (REST w pkt 2) musi należeć do organizacji i mieć prawo administratora na tworzonych topikach.
5. **Wszystkie nody na buildzie z linii TB-04.** Dopóki choć jeden node środowiska nie ogłasza przekazywania, nowy topik prowadzi node tworzący (jak przed TB-04) i krok 1 nie da rozkładu; w „Kopie i nody” taki node ma chip „starsza wersja”.
   - [ ] Mesh pokazuje oba nody jako sparowane, instancja działa na obu, po utworzeniu pierwszego topiku „Kopie i nody” pokazuje dwa nody. Przed pierwszym topikiem lista nodów jest budowana z kopii topików, więc jest pusta albo pokazuje tylko jeden node; ekran pokazuje wtedy puste miejsce z wyjaśnieniem zamiast liczby, a licznik nodów w nagłówku jest pominięty.

## 1. Rozkład przy tworzeniu

1. Na nodzie A utwórz cztery topiki `demo.t1` … `demo.t4` (nazwy topików są małymi literami), każdy z 4 partycjami (ekran TentaBus albo `TopicCreateRequest`). Liczby kopii nie wybierasz: kreator ustawia ją sam (przy dwóch nodach 2) i mówi to w kroku 3.
2. W kroku 3 kreatora wiersz „Node prowadzący” mówi, jak instancja wybierze node: preferowany dla najmniejszej liczby partycji, z nazwą noda i liczbą jego partycji. Wiersz jest także przy pierwszym topiku instancji; gdy ekran nie zna jeszcze nodów, mówi tylko, jak node jest wybierany, bez nazwy i liczby. Przy remisie liczby partycji wiersz wymienia oba nody („jeden z nodów: X albo Y”).
3. Sprawdź dziennik audytu: wpisy `bus.topic.create` mają w szczegółach `leader=<node> leader_rf=<RF>`.
4. Sprawdź, kto prowadzi który topik: zakładka „Kopie i nody”. Tabela „Rozkład prowadzenia” podaje dla każdego noda liczbę partycji i jego topiki; „Partycje według topiku” pokazuje „prowadzą: …” per topik.

**Oczekiwane:** topiki trafiają na node preferowany dla najmniejszej liczby partycji; przy remisie decyduje `hash(nazwa) mod 2` (kreator nie liczy tego skrótu, więc przy remisie hint w kroku 3 mówi „jeden z nodów: X albo Y”, a nie wskazuje jednego; w teście hint wskazał przy remisie zły node). Każdy topik jest w całości (partycje i jego DLQ) na jednym nodzie. W teście z czterema topikami t1 i t3 trafiły na B, a t2 i t4 na A; u Ciebie podział może wyjść inaczej, więc **odczytaj go z tabeli** i podstaw nazwy: `<TOPIK_B>` to topik prowadzony przez B, `<TOPIK_A>` przez A.

**Liczby są wszędzie takie same.** Karta noda, tabela „Rozkład prowadzenia” i podsumowanie w oknie „Rozłóż prowadzenie” liczą partycje topików użytkownika razem z partycjami ich DLQ (`__dlq.<topik>`, o ile topik ma DLQ) i nigdy topików wewnętrznych (`__bus.metrics` i inne `__bus.*`). Jeśli widzisz różnicę, to błąd.

Na nodzie B lista topików i „Kopie i nody” odświeżają się same przy kolejnym odpytaniu (do ok. 10 s); nie trzeba przeładowywać strony.
   - [ ] Dwa topiki prowadzi A, dwa B; `leader=` w audycie zgadza się z tym, co pokazuje zakładka; karta noda i tabela mają te same liczby.

## 2. Zapis i odczyt przez node, który nie prowadzi

Zapis do `<TOPIK_B>` wysłany do **A**:

```
curl -k -X POST \
  -H 'Authorization: Bearer <API_KEY_A>' \
  -H 'Content-Type: application/x-ndjson' \
  --data-binary $'{"payload_b64":"aGVsbG8="}\n' \
  'https://<HOST_A>:<PORT_A>/v1/bus/instances/<INSTANCE>/topics/<TOPIK_B>/records?org_id=<ORG_ID>'
```

**Oczekiwane:** 200 z `{"published":1,...}` (przed TB-04: 500). Rekord leży w logu u lidera B. Odczyt wysłany do A:

```
curl -k -H 'Authorization: Bearer <API_KEY_A>' \
  'https://<HOST_A>:<PORT_A>/v1/bus/instances/<INSTANCE>/topics/<TOPIK_B>/records?org_id=<ORG_ID>&group=demo-grp&max_records=10&wait_ms=1000'
```

**Oczekiwane:** 200 z `{"records":[...]}` zawierającymi `aGVsbG8=` (`payload` w base64). Odczyt został przekazany do B, który zatwierdził offset grupy `demo-grp`; ponowny odczyt tej samej grupy zwróci 0 rekordów. W przeglądarce wiadomości na B widać notkę, że widok pochodzi z repliki.
   - [ ] Zapis i odczyt przez A działają dla topiku prowadzonego przez B (w teście: zapisy REST i flow, odczyt grupy 8, potem 0).
   - [ ] W audycie (u lidera) nie ma odmów. Pole `forwarded_from` nie było sprawdzane na żywo, a odmowy `bus.produce.denied` z użycia klucza użytkownika w flow (pkt 0.4) go nie mają.

## 3. Bezpieczne przeniesienie prowadzenia

1. W osobnym terminalu uruchom zapis w pętli do `<TOPIK_B>` na nodzie A (np. 1 zapis na 100 ms przez 60 s, licząc wynik 200 i inne kody).
2. W trakcie przenieś prowadzenie `<TOPIK_B>` z B na A: w „Kopie i nody” albo w szczegółach topiku (sekcja „Partycje i kopie”) kliknij „Przenieś prowadzenie” przy dowolnej partycji. Okno mówi o całym topiku: **przenosi go razem z DLQ**. Można też wysłać `LeaderTransferRequest { topic: "<TOPIK_B>", partition: 0, target_node_id: "<ID_A>" }`.
   - Przycisk jest aktywny według odpowiedzi **node'a prowadzącego**. Na nodzie, który topiku nie prowadzi, przycisk bywa przez chwilę nieaktywny z podpowiedzią „Sprawdzam…”; odpowiedź lidera przychodzi po ok. 0,1 s, więc tego stanu praktycznie nie widać. Po kliknięciu pojawia się od razu toast „Sprawdzam u nodu prowadzącego stan kopii…”, a okno otwiera się po odpowiedzi lidera, najpóźniej po 2 s. Jeśli lider nie odpowie w tym czasie, okno otwiera się z widokiem tego noda i notką, że lider nie odpowiedział (lider i tak odrzuci przeniesienie na kopię, która jest w tyle). Okno z nodu nieprowadzącego otwiera się w ok. 0,1 s (6 prób), a przyciski są aktywne po 110–240 ms.
3. Po pętli policz rekordy w `<TOPIK_B>` (przeglądarka wiadomości albo odczyt REST z nową grupą).

**Oczekiwane:** wszystkie zapisy zakończone 200, a liczba rekordów w topiku zgadza się z liczbą 200 (w teście 115 z 115). Zapis, który w trakcie przenoszenia musiał poczekać, jest po przeniesieniu wysyłany ponownie do nowego lidera, jeśli przeniesienie trwało nie dłużej niż 2 s; producent nie widzi wtedy błędu, tylko dłuższy czas odpowiedzi. Przy przeniesieniu dłuższym niż 2 s zapis nadal dostaje 503 `bus_not_leader` (z `Retry-After: 1`, bez nazwanego lidera); nic wtedy nie zapisano i ponowienie jest bezpieczne. Pojedyncze przeniesienia w przekliku nie dały błędów, ale **rozkład ręczny** (pkt 4b) kilka razy dał 1–2 zapisom 503 po ok. 3 s (6 zapisów w 4 z 5 rozkładów), zanim doszło ponowienie na nowego lidera; po poprawce nie było to przeklikane. 504 `bus_forward_outcome_unknown` dotyczy awarii lidera (pkt 4), nie przeniesienia.
   - [ ] Liczba rekordów w topiku równa liczbie zapisów, które dostały 200.
   - [ ] Audyt na nodzie, na którym kliknięto: `bus.leader.transfer` dla każdej partycji topiku (i jego `__dlq.*`, jeśli istnieje), `reason=manual`, `from_node` = B, `target_node_id` = A, rosnące `leader_epoch`. Wpisy audytu są **per node**, więc na drugim nodzie ich w dzienniku audytu nie ma. Lista „Przejęcia prowadzenia” w „Kopie i nody” pyta wszystkie nody i scala ich wpisy, więc pokazuje przeniesienie na obu tak samo (w przekliku obie listy były takie same). Wyjątek: tuż po restarcie noda lista może przez chwilę zawierać tylko wpisy z własnego audytu; ekran zachowuje wtedy ostatnią scaloną listę i dopisuje, że może być niepełna.
   - [ ] Filtr „Akcja” w dzienniku audytu wymaga pełnej nazwy: `bus.leader.transfer` (samo `bus.leader` daje „Brak wpisów”).
   - [ ] **Pinezki preferowanego noda.** Po przeniesieniu wszystkie partycje topiku (i DLQ) mają wskazywać ten sam node preferowany, czyli A. Obserwowano raz, że po przeniesieniu pojawiło się kilka fałszywych wpisów `lease_expired` („B przestał odpowiadać”, choć B działał), a część partycji zostawała z pinezką na B; wtedy automatyczny powrót pomija taki topik, bo jego partycje nie zgadzają się co do noda preferowanego. Sprawdź pinezki w sekcji „Partycje i kopie”.
   - [ ] Automatyczny powrót nie cofa przeniesienia po 5 minutach (A jest teraz preferowany dla `<TOPIK_B>`).
   - Pojedyncze przeniesienia (11 prób w obie strony, także z noda nieprowadzącego) były czyste: bez wpisów `bus.leader.failover` i z identycznymi pinezkami na obu nodach. W trzecim przekliku 12 przeniesień ręcznych nie dało żadnego przejęcia.

### 3b. Błędy przeniesienia (opcjonalnie, z trzecim nodem C lub wstrzymaną grupą; **nie sprawdzono na żywo**)

- Wstrzymaj grupę odbiorców topiku i spróbuj go przenieść → `bus.transfer_blocked` (powód `consumer_group_paused`). Nic się nie zmienia. W oknie „Rozłóż prowadzenie” taki topik jest w sekcji „Pominięte” z nazwą wstrzymanego odbiorcy. Wznów grupę.
- Node C bez repliki topiku jako cel → `bus.not_a_replica`.
- Jeśli C jest starszym buildem bez przekazywania: przeniesienie na niego → `bus.node_without_forwarding`; przycisk „Rozłóż prowadzenie” jest wtedy nieaktywny z podpowiedzią „Zaktualizuj wszystkie nody…”.

## 4. Awaria i automatyczny powrót

Potrzebny jest topik prowadzony **i preferowany** przez B: topik z pkt 1 prowadzony przez B, którego nie ruszano w pkt 3 (przeniesienie z pkt 3 zrobiło z A node preferowany dla `<TOPIK_B>`).

1. Zabij proces B (`kill -9`).
2. A przejmuje wszystkie partycje B. **Przejęcie trwa ok. 7,5–8,4 s** (8 zabić w trzecim przekliku; ostatnie przebiegi: 7,66, 7,95 i 7,78 s) i jest rozłożone w czasie: przez krótki czas topiki są podzielone między nody, bo partycje wybierają liderów po kolei. Interfejs nadąża po 1–2 odpytaniach (10–20 s). Audyt: `bus.leader.failover` dla partycji dotychczas prowadzonych przez B.
3. Zapisy przez A trwają, z wyjątkiem kilku w oknie przejęcia, które dostają **504 `bus_forward_outcome_unknown`** (w trzecim przekliku 15 z ok. 27 900 zapisów, czyli rzadko). To zwykła cecha awarii lidera, nie wyjątek: **producent musi ponawiać**, najlepiej z kluczem idempotencji lub tożsamością producenta, bo zapis mógł trafić do logu.
4. **Odczyty też trwają.** Po awarii jedynej repliki lider RF=2 dalej posuwa granicę odczytu (high watermark), więc nowa grupa czyta wszystko, co dostało 200. (W buildzie z K8 odczyt zatrzymywał się na wartości z chwili awarii, dopóki B nie wrócił; to poprawiono w K10. Sprawdź: odczyt nowej grupy po awarii daje tyle rekordów, ile zapisów dostało 200.)
5. **Topik rozdzielony i restart noda.** Topik jest rozdzielony, gdy jego partycje albo jego DLQ prowadzą różne nody. Karta „Rozkład prowadzenia” liczy go wtedy w „poza preferowanym” przy jego preferowanym nodzie (z dopiskiem „rozdzielony: …”) i pokazuje ostrzeżenie „topik jest rozdzielony między nody”. Rozdzielenie jest krótko normalne w trakcie przejęcia; przejęcie samo je zbiera. Odczyt REST takiego topiku działa: każdy lider odpowiada za swoje partycje, a odpowiedź zawiera rekordy wszystkich (partycja bez znanego lidera nadal daje 503 `bus_not_leader`).
   - **Poprawiony błąd krytyczny.** W trzecim przekliku, ok. 2 minuty po restarcie B, B przejmował partycje zdrowemu liderowi A, a A obcinał log do krótszej kopii B: **zapisy potwierdzone kodem 200 znikały** (ok. 1186 rekordów w dwóch zdarzeniach), choć skróty logów obu nodów były równe. Teraz node po restarcie **nie przejmie partycji, dopóki nie dogoni żywego lidera**. Wyjątek: gdy odpowiadają wszystkie repliki (restart całego zbioru replik), node staje normalnie. Jeśli lider padnie, zanim druga replika nadrobi, partycja czeka bez lidera na replikę z pełnym logiem zamiast zgubić zapisy (w logu ostrzeżenie „not standing — this node has not caught up…”). Poprawka nie była jeszcze przeklikana na żywo.
   - **Sprawdź po krokach z restartem:** w dzienniku audytu na obu nodach filtr „Akcja” = `bus.leader.tail_discarded`. **Powinno nie być żadnego wpisu.** Każde obcięcie potwierdzonego ogona jest logowane jako błąd i zapisywane w audycie z liczbą odrzuconych rekordów. Porównaj też liczbę rekordów z liczbą zapisów, które dostały 200: równość skrótów logów tego nie wykrywa.
   - po rozkładzie ręcznym (pkt 4b) topik mógł mieć partycje na A, a DLQ na B; po włączeniu wyłącznika B zebrał go w ok. 3 s z `reason=preferred_restore` (bez wpisu `split_gather`).
6. Uruchom B ponownie i poczekaj, aż dogoni (jest w ISR u A).
7. Poczekaj **co najmniej 5 minut stabilnego ISR**, a potem najwyżej **jeden topik na 30 s**: A oddaje B jego topiki po kolei.

**Oczekiwane:** po 5 min od dołączenia B do ISR B znów prowadzi swoje topiki. W teście B wrócił do ISR ok. 15:47:54, pierwszy topik wrócił po 5 min 27 s, drugi po kolejnych 30 s. Audyt: `bus.leader.transfer` z `reason=preferred_restore` dla każdej partycji (z DLQ) oraz **jeden `bus.leader.rebalance` na topik** (`actor=system`, `reason=preferred_restore`, `moves=1`), co 30 s, a nie jeden na cały powrót. `replicas[0]` się nie zmienia. Liczba rekordów w topikach przed awarią i po powrocie się zgadza, a hash logów obu nodów jest identyczny (poza `__bus.metrics`, które jest lokalne dla każdego noda).
   - [ ] Po awarii A prowadzi wszystkie partycje, bez utraty rekordów (poza 504, które producent ponawia).
   - [ ] W trakcie awarii karta „Rozkład prowadzenia” zaczyna od tego, że node B nie odpowiada („Node B nie odpowiada — N topików prowadzi inny node”), a przycisk „Rozłóż prowadzenie” jest nieaktywny (bez zachęty „możesz rozłożyć teraz”). Informację o braku odpowiedzi nosi też karta noda („brak połączenia”) i podpowiedź przycisku. **Przez pierwszą chwilę po zabiciu (do ok. 20 s, zwykle krócej) karta może jeszcze mówić, że node „odpowiada”, z aktywnym przyciskiem i zachętą „możesz rozłożyć teraz”**: node wypada z listy dostępnych dopiero, gdy zamilknie jego strumień replikacji (5 s ciszy) albo mesh uzna go za niedostępnego, a ekran uznaje za niemy node, którego ostatni sygnał jest starszy niż 10 s lub którego partycja nie ma użytecznego lidera. Po restarcie B karta pisze, że B odpowiada „przed chwilą”, potem „od 1 min” i tak dalej, **ale tylko wtedy, gdy strona pozostała otwarta przez cały restart**; po przeładowaniu widać zwykły tytuł. Zdanie „kopie … jeszcze doganiają” pojawia się tylko, gdy node odpowiada.
   - [ ] Po powrocie B prowadzi z powrotem swoje topiki; `bus.leader.rebalance` ma `reason=preferred_restore`. Lista „Przejęcia prowadzenia” pokazuje „automatyczny powrót do preferowanego nodu” i czas ułamków sekundy jako „<1 s”; te same wpisy na obu nodach tylko wtedy, gdy nody łączą się bezpośrednio (pkt 3).

### 4b. Wyłącznik powrotu

1. Wyłącz powrót w „Kopie i nody”, w karcie „Rozkład prowadzenia”: przełącznik „Automatyczny powrót do preferowanego nodu” (ustawienie replikowane do wszystkich nodów instancji; zmienia je administrator platformy, dla innych przełącznik jest nieaktywny z dopiskiem). Zmiana dociera na drugi node w ok. 1 s w obie strony (sprawdzone); przełącznik na B pokazuje ten sam stan.
2. Powtórz awarię i powrót B (pkt 4, kroki 1–6) i odczekaj dłużej niż 6 minut.

**Oczekiwane:** topiki zostają na A; brak wpisów `preferred_restore` (w teście 7,5 min bez żadnego). Karta mówi, że automatyczny powrót jest wyłączony i topiki zostaną, dopóki nie rozłożysz prowadzenia.
   - [ ] Przy wyłączonym przełączniku żaden topik nie wraca sam.

3. **Ręczny rozkład.** Przycisk „Rozłóż prowadzenie” w tej samej karcie (dla administratora instancji; nieaktywny, gdy drugi node nie odpowiada albo ma starszą wersję) otwiera okno z podglądem: tabela „Przeniesienia” (topik, partycje, z → na, dlaczego), sekcja „Pominięte” z blokadami i podsumowanie „Co się stanie po rozłożeniu” (ile partycji i topików będzie prowadził każdy node, liczone jak w tabeli karty). Po „Rozłóż prowadzenie” okno pokazuje postęp i wynik per topik (np. „przeniesiono 4 z 8”: liczone są partycje topiku razem z jego DLQ, a dla topiku rozdzielonego liczba przeniesionych jest mniejsza niż wszystkich; „czeka na kopie”, „w kolejce”, błąd z powodem). Stanów „czeka na kopie” i „w kolejce” oraz sekcji „Pominięte” **nie zaobserwowano**: nic nie było zablokowane, a cztery przeniesienia trwały 411 ms (wcześniej dwa 241 ms), więc postęp jest ledwo widoczny. Audyt: `bus.leader.transfer` z `reason=rebalance` dla każdej partycji i `bus.leader.rebalance` z `actor` = użytkownik, `reason=rebalance`, `moved=2`, `result=ok`.
   - [ ] Podgląd pokazuje przeniesienia A→B dla topików B; po rozłożeniu B prowadzi je znów, a liczby w oknie, w karcie noda i w tabeli się zgadzają.
   - [ ] Po rozkładzie sprawdź w karcie, czy żaden topik nie jest „rozdzielony”. W drugim przekliku rozkład zakończony `result=ok` został raz cofnięty fałszywym przejęciem, gdy DLQ powstał w trakcie restartu B; w trzecim przekliku nie powtórzyło się to w 6 próbach.
4. Włącz przełącznik z powrotem.

## 5. Odczyt z repliki

Z node B (trzyma replikę topików prowadzonych przez A):

1. Otwórz przeglądarkę wiadomości lub listę odbiorców dla topiku prowadzonego przez A.
2. Opublikuj kilka rekordów na A i odśwież widok na B.
3. Na B spróbuj operacji zmieniającej stan: wstrzymaj grupę, zresetuj offset, ponów rekord DLQ.

**Oczekiwane:** widok na B pokazuje rekordy do `min(hw, committed_offset)` repliki; odpowiedzi mają `from_replica = true` (partycje przeglądarki, grupa, lag), a ekran dopisuje notkę, że widok pochodzi z kopii i może być o ok. 1 s opóźniony. Na stronie odbiorcy stan pokazuje „Czeka: ten topik prowadzi node <nazwa> (tam działa odbiór)”, gdzie `<nazwa>` to nazwa z listy nodów albo skrócone id (nigdy nazwa hosta wspólna dla obu nodów). „Wstrzymaj”, „Wznów” i „Przesuń” (miejsce czytania) są **nieaktywne** na nodzie, który nie prowadzi topiku, a podpowiedź mówi, który node prowadzi. Operacja wymuszona poza ekranem (np. API) kończy się `bus.not_leader: leader_node_id=<A> leader_epoch=<n>` (kod Conflict), a okno pokazuje „Ten node nie prowadzi tego topiku. Prowadzi go teraz: <nazwa>.” i niczego nie zmienia. Lag na B może się różnić od lagu na A o ostatnie okno łączenia offsetów (500 ms) i o rekordy między granicą odczytu repliki a hw lidera; świeżo opublikowane rekordy mogą pojawić się z opóźnieniem.
   - [ ] Odczyt na B działa, `from_replica = true`, notka widoczna; sterowanie odbiorcą jest nieaktywne z nazwą lidera.

W teście sprawdzono wstrzymanie grupy na nodzie nieprowadzącym (przed poprawką przycisk był aktywny, a toast mówił o partycji). Reset offsetu i ponowienie rekordu DLQ na nodzie nieprowadzącym wynikają z kodu i nie były klikane.

**Granice świeżości (nie do pokazania w krótkim demo, ale do powiedzenia komisji):** ACL i polityki ukrywania pól replika stosuje według własnej kopii ledgera; pauza grupy, która nie zdążyła dojść do repliki w oknie 500 ms, ginie z poprzednim liderem.

## 6. Po demo

- Usuń topiki `demo.*`, grupę `demo-grp`, **klucze API wydane na potrzeby demo** i **flow testowe**.
- Sprawdź pinezki noda preferowanego (pkt 3): po ręcznych przeniesieniach wszystkie partycje topiku mają wskazywać ten sam node.
- Jeśli wyłączono powrót (4b), włącz go ponownie: domyślnie jest włączony.
- Jednorazowy rozkład po aktualizacji (`bus.leader.rebalance`, `actor=system`, `reason=post_upgrade_spread`) przeznaczony jest dla instancji z topikami sprzed aktualizacji i uruchamia się sam po ok. 60 s od startu noda o najniższym id. W trzecim przekliku zdarzył się także w świeżym demo, przy pierwszym restarcie noda o najniższym id, zamiast czekać 5 min automatycznego powrotu (przeniósł dwa topiki po ok. 62 s). Przyczyną było to, że instalacja bez topików nie zapisywała znacznika wykonania; teraz taka świeża instalacja oznacza rozkład jako wykonany od razu. Poprawka nie była jeszcze przeklikana na żywo. Rozkład po aktualizacji nie jest częścią tego scenariusza.
