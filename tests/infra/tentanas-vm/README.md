# Prywatna VM TentaNas

Mała maszyna do rzeczywistych, funkcjonalnych testów operacji dyskowych. Nie uruchamia core/UI i nie stanowi benchmarku fizycznego NAS. V01 przygotowuje **puste** dyski testowe; nie formatuje ich i nie instaluje automatycznie pakietów.

## Wymagania

Linux, Python 3.11+, dostęp użytkownika do `/dev/kvm`, `/usr/bin/qemu-system-x86_64`, `/usr/bin/qemu-img`, `curl`, `xorriso`, `ssh`, `ssh-keygen` oraz `mktemp`. Nie uruchamiać jako root. Harness używa własnych plików w `/mnt/d/repos`, nie hostowego sudo ani libvirt. Bezwzględna ścieżka qemu-img zapobiega przypadkowemu użyciu wersji z Android SDK w PATH.

## Użycie

Z głównego katalogu repo:

```bash
python3 tests/infra/tentanas-vm/vm.py create
```

Ostatnia linia podaje nowy runtime, np. `/mnt/d/repos/tentanas-vm.ABC123`. Każde `create` tworzy nowy katalog przez mktemp; nie przyjmuje miejsca istniejącej VM i nie nadpisuje go. Przy nieudanym przygotowaniu zachowuje prywatny runtime do diagnostyki, bez możliwości bootu niekompletnego manifestu.

```bash
python3 tests/infra/tentanas-vm/vm.py start /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py status /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py ssh /mnt/d/repos/tentanas-vm.ABC123 cloud-init status --wait
python3 tests/infra/tentanas-vm/vm.py inventory /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py stop /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py start /mnt/d/repos/tentanas-vm.ABC123
```

`start` potwierdza proces i QMP, nie gotowość SSH ani cloud-init. Pierwsze połączenie może odmówić przed uruchomieniem sshd; najpierw sprawdzić `status`, następnie ponowić odczyt. `inventory` wymaga dokładnego UUID VM i dysków profilu (sześciu; w `block` wyłącznie OS) o oczekiwanych serialach, rozmiarach i magistralach, z root wyłącznie na OS oraz bez systemów plików/partycji/mountów pozostałych ról. To kontrola pustego stanowiska V01, nie preflight późniejszej sformatowanej macierzy.

`ssh` zachowuje granice argumentów. Jeśli potrzebna jest składnia powłoki gościa, wywołać ją jawnie, np. `ssh RUNTIME sh -c 'id && uname -r'`. Nie używać tego interfejsu do danych produkcyjnych. Polecenia wykonywane są tylko w gościu po sprawdzeniu tożsamości procesu i klucza SSH.

`stop` wysyła ACPI powerdown przez własny QMP, czeka maksymalnie 90 s i nie wysyła kill. Kontroluje PID, starttime, argv, właściciela i UUID QMP. Zakończony zapisany proces można uzgodnić jako stopped; ponownie użyty PID powoduje odmowę. Legalny restart własnej zatrzymanej VM jest dozwolony, ponowny start działającej/obcej/niekompletnej nie. Przy timeout VM i stan pozostają do diagnostyki. Nie ma automatycznego destroy.

## Twardy reset, liczniki blockstats i profil błędów odczytu

```bash
python3 tests/infra/tentanas-vm/vm.py reset /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py blockstats /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py fault /mnt/d/repos/tentanas-vm.ABC123 cache --read-error-sector 123456
python3 tests/infra/tentanas-vm/vm.py fault /mnt/d/repos/tentanas-vm.ABC123 none
```

Runtime ma jeden plik `lock` i dwie klasy poleceń. Dzielony `flock LOCK_SH` biorą polecenia, które nie zapisują katalogu runtime: `ssh`, `status`, `blockstats` i `reset`. Wyłączny `flock LOCK_EX` biorą wszystkie pozostałe, czyli `start`, `stop`, `inventory`, `fault`, `powercut`, `bootstrap-packages`, `install-packages`, `storage` i `detach-data2`; domyślną klasą nowej podkomendy jest wyłączna, a lista dzielonych jest jawna w jednym miejscu (`SHARED_COMMANDS` w `vm.py`). Oba warianty są nieblokujące: zajęty runtime kończy się odmową „Runtime zajęty przez inne polecenie”, nigdy czekaniem. Twardy reset gościa w trakcie długiej operacji uruchomionej przez `vm.py ssh` jest możliwy wyłącznie dlatego, że `reset` należy do klasy dzielonej; gdyby był wyłączny, jedyny scenariusz, dla którego powstał, kończyłby się odmową. Jednocześnie `stop`, `start` i `fault` nadal odmawiają, dopóki żyje którykolwiek posiadacz locka dzielonego.

Jest jeden jawny wyjątek od zasady „polecenia dzielone nie zapisują katalogu runtime”: `blockstats --record-cut-mark` dopisuje znacznik odcięcia do append-only `powercut-marks.jsonl` i do uzbrojonego wpisu. Decyzja właściciela 2026-09-12, z podanym powodem: w chwili odcięcia sesja `ssh` movera trzyma lock dzielony, więc polecenie wyłączne nie mogłoby się wykonać, a przebudowa procedury tak, by ten lock zwolnić, oznaczałaby, że helper nie jest już wstrzymany w środku fazy — przypadki dowodziłyby mniej. Wyjątek jest wąski: pisze wyłącznie ta flaga, wyłącznie te dwa miejsca, po tych samych kontrolach tożsamości co każde inne polecenie; bez flagi `blockstats` nie dotyka runtime. Nie „naprawiać” tego z powrotem na polecenie wyłączne bez ponownej decyzji właściciela.

Sam monitor QMP ma własny lock, bo chardev QEMU przyjmuje tylko jednego klienta, a `reset`, `status` i `blockstats` mogą teraz działać równocześnie. Każde wejście do monitora bierze `flock LOCK_EX|LOCK_NB` na `qemu.pid` — pliku, który istnieje przez cały czas działania VM i którego żadne z tych poleceń nie zapisuje — więc drugi klient dostaje natychmiastową odmowę „Monitor QMP zajęty przez inne polecenie”, zamiast czekać na powitanie do timeoutu i zostawić pusty plik dowodowy. Lock jest wewnątrz `qmp()`, więc obejmuje wszystkie ścieżki monitora, w tym `start`, `stop`, `recover_start` i awaryjny `quit`. Kolejność locków jest stała i nieblokująca: najpierw lock runtime, potem monitor, nigdy odwrotnie. QEMU pilnuje swojego `-pidfile` blokadą `fcntl`, która nie koliduje z `flock`, więc oba mechanizmy współistnieją.

`reset` wysyła QMP `system_reset`. To nie jest czyste zamknięcie gościa: nie ma ACPI powerdown, gość nie synchronizuje systemów plików i traci zawartość RAM. `stop` pozostaje jedyną czystą drogą wyłączenia. Gość może stracić zapisy, których nigdy nie zflushował; zapisy, które dotarły do wirtualnego dysku, przetrwają, bo proces QEMU żyje dalej i trzyma je w page cache hosta. Reset nie modeluje więc zaniku zasilania hosta i sam nie wykryłby brakującego `fsync`. Wymaga `status == running`, odmawia po rozpoczęciu odłączenia data2 i sprawdza dokładnie tę samą tożsamość co `stop`/`status`: PID, starttime, argv, właściciela procesu oraz UUID QMP. Resetuje wyłącznie gościa i nie zapisuje niczego w runtime — ani `state.json`, ani obrazów — nie używa też kill ani `quit`. Wypisuje `{"runtime","uuid","pid","reset":true}`.

`blockstats` jest wyłącznie odczytem: po tej samej kontroli tożsamości wysyła QMP `query-blockstats` i wypisuje JSON z runtime, UUID, aktualnym profilem błędów, jego historią oraz odpowiedzią QMP. Zawiera `flush_operations` per dysk. Nie zmienia stanu, nie dotyka gościa i nie wymaga SSH.

`fault` renderuje profil błędów odczytu QEMU `blkdebug` dla dysku cache. Domyślnie żaden runtime nie ma błędów: profil powstaje wyłącznie przez `fault RUNTIME cache --read-error-sector N` (opcję można powtórzyć) i znika wyłącznie przez `fault RUNTIME none`. Wymaga profilu `e2-cache`, roli `cache` i zatrzymanej VM (`prepared`/`stopped`); odmawia dla pozostałych profilów, przy działającej VM, przy pustym, powtórzonym, ujemnym lub nie-całkowitym sektorze oraz przy sektorach podanych do wyłączenia.

Sektor jest adresem w hostowym pliku `cache.qcow2`, nie w przestrzeni widzianej przez gościa: `blkdebug` leży pod sterownikiem qcow2 i dopasowuje offsety żądań do tego pliku. Procedura jest więc taka: zatrzymać VM, zmierzyć zakres pliku gościa (`filefrag`/`xfs_db` w gościu), zmapować go na offset hosta przez `qemu-img map --output=json <RT>/cache.qcow2`, podzielić przez 512 i dopiero ten sektor podać do `fault`. Mapowanie zmienia się przy zapisach, więc liczy się je po zatrzymaniu VM, tuż przed `fault`. `fault` wymusza dokładnie tę procedurę, zamiast tylko ją opisywać: przy zatrzymanej VM sam uruchamia `qemu-img map --output=json` na przypiętym przez inode `cache.qcow2` i odmawia, jeżeli sektor nie leży wewnątrz zakresu `data: true`. Odpada więc zarówno sektor powyżej długości pliku (nie mógłby nic wstrzyknąć), jak i sektor trafiający w metadane qcow2 lub dziurę — pierwszy psułby otwarcie obrazu zamiast jednego pliku gościa, drugi dałby przebieg, w którym nic się nie dzieje, a dowód wyglądałby na udany. To, że błąd trafia w zamierzony plik i że sąsiedni pozostaje czytelny, sprawdza dopiero prototyp przypadku 8 w gościu.

Wynika z tego wprost, że uzbrojenie profilu wymaga zapisanych danych gościa. Świeżo utworzony `cache.qcow2` ma około 197 KB i mapuje się do jednego zakresu `data: false`, więc każde `fault RUNTIME cache --read-error-sector N` na runtime w stanie `prepared` odmawia „Sektor poza zapisanymi danymi”. Na `prepared` działa wyłącznie `fault none`. Odrzucany jest też zakres `data: true` z flagą `zero: true`, którym mapuje się preallokacja metadanych: QEMU odpowiada na takie odczyty zerami bez sięgania do pliku hosta, więc profil nic by nie wstrzyknął, a przebieg wyglądałby na udany.

Reguła w prywatnym `blkdebug-cache.cfg` (0600) to `[inject-error] event="read_aio" iotype="read" errno="5" sector="N" once="off" immediately="off"`. SHA-256 tego pliku jest przypięte w `state.json` i w prywatnym `fault.json`, tak samo jak inody obrazów i hash seed; podmiana pliku, dowodu albo zapisu w stanie blokuje `start` i `status` przed uruchomieniem QEMU. Dysk cache wchodzi wtedy do argv jako `-drive if=none,id=cache,format=qcow2,file.driver=blkdebug,file.config=<RT>/blkdebug-cache.cfg,file.image.driver=file,file.image.filename=<RT>/cache.qcow2`, linia `-device nvme,…,serial=…` pozostaje bez zmian, a całe argv jest częścią tożsamości procesu sprawdzanej przy każdej operacji. Bootstrap pakietów (otwarty egress) jest przy włączonym profilu zabroniony.

Runtime, który kiedykolwiek miał włączone błędy, jest rozpoznawalny: `fault_history` w `state.json` i w `fault.json` rośnie o wpis przy każdym włączeniu i wyłączeniu, nigdy nie jest kasowana, a `save_state` odmawia jej cichej zmiany poza podkomendą `fault`. `status`, `blockstats` i wynik samego `fault` pokazują tę historię, więc runtime z wstrzykiwanymi błędami nie może zostać pomylony z czystym. Wyłączenie kasuje `blkdebug-cache.cfg` jako ostatni krok; pozostawiony plik po przerwaniu jest martwy, bo argv przestaje go wskazywać.

Zapisy `fault` idą przez prywatne pliki tymczasowe `*.next` i rename, a na czas całej sekwencji (konfiguracja, dowód, stan) blokowane są SIGINT i SIGTERM — tak samo jak przy `start`. Przerwanie sygnałem nie zostawia więc runtime w połowie zapisu. Gdyby sekwencję przerwała awaria twardsza niż sygnał (SIGKILL, zanik zasilania hosta), runtime pozostaje w stanie fail-closed: `start` i `status` odmawiają, a naprawą jest powtórzenie dokładnie tego samego polecenia `fault`, które dokańcza swój własny zapis. Dowód niezgodny z tym poleceniem — obcy UUID, obca historia, inne sektory — nadal jest odmawiany i wymaga diagnostyki, nie edycji plików dowodowych.

## Utrata niezflushowanych zapisów — `powercut`

```bash
python3 tests/infra/tentanas-vm/vm.py powercut /mnt/d/repos/tentanas-vm.ABC123 cache on
python3 tests/infra/tentanas-vm/vm.py powercut /mnt/d/repos/tentanas-vm.ABC123 cache off
```

`reset` nie modeluje zaniku zasilania: proces QEMU żyje dalej, więc zapisy, których gość nigdy nie zflushował, i tak przetrwają, a brakujący `fsync` pozostaje niewidoczny. `powercut` uzbraja pod emulowanym NVMe filtr QEMU `blklogwrites`, który zapisuje każdy zapis i każdy flush dysku cache do osobnego logu; po odcięciu host odtwarza obraz do ostatniego flush, więc zostaje dokładnie to, co było trwałe. Filtr leży **pod** urządzeniem NVMe i **nad** qcow2, więc gość nadal widzi cały dysk i reguła całych dysków w helperze zostaje nietknięta. Wymaga profilu `e2-cache`, roli `cache` i zatrzymanej VM (`prepared`/`stopped`), jest podkomendą wyłączną i odmawia po rozpoczęciu odłączenia data2.

Uzbrojenie tworzy prywatny, pusty `cache-writelog.img` (0600) i przypina go **inode**, a nie SHA: QEMU dopisuje do tego pliku przez cały czas działania VM, więc jego treść z definicji się zmienia. Dysk cache wchodzi wtedy do argv jako `-drive if=none,id=cache,driver=blklogwrites,file.driver=qcow2,file.file.filename=<RT>/cache.qcow2,log.driver=file,log.filename=<RT>/cache-writelog.img,log-sector-size=512`, linia `-device nvme,…,serial=…` pozostaje bez zmian, a całe argv jest częścią tożsamości procesu. Uzbrojenie przypina też **baseline**: SHA-256 obrazu `cache.qcow2` z tej chwili. VM stoi, więc to jedyny moment, w którym obraz jest stabilny, a replayer odmawia, jeśli podany mu baseline nie ma dokładnie tego SHA — inaczej prefiks zostałby nałożony na obraz, którego nikt nie zmierzył. Koszt na tym stanowisku to 2,1 GiB/s, czyli ~2,4 s dla 5 GiB i ~15 s dla w pełni zaalokowanych 32 GiB; liczenie jest jednorazowe, przy zatrzymanej VM.

Runtime, który kiedykolwiek był uzbrojony, jest rozpoznawalny tak samo jak przy `fault`: `powercut_history` w `state.json` i w prywatnym `powercut.json` rośnie przy każdym uzbrojeniu i rozbrojeniu, nigdy nie jest kasowana, a `save_state` odmawia jej cichej zmiany poza podkomendą `powercut`. Wpis rozbrojenia zapisuje na stałe rozmiar i SHA-256 logu, nazwę jego archiwum oraz — gdy log cokolwiek zawierał — komplet liczb odtworzenia: granicę, indeks cięcia, liczbę wpisów zastosowanych i odrzuconych, SHA baseline oraz SHA obrazu przed i po zapisie.

`powercut` i `fault` wykluczają się wzajemnie: uzbrojenie jednego jest odmawiane, dopóki drugi jest włączony (`fault cache …` przy uzbrojonym odcięciu i `powercut cache on` przy włączonym profilu błędów). Rozbrajanie jest dozwolone zawsze, więc żadna para nie potrafi zablokować runtime. Zakaz jest też wpisany w `qemu_command`, więc nawet podmienione dowody nie wyrenderują dysku z dwoma filtrami naraz. Powód jest dowodowy, nie techniczny: przebieg z wstrzykiwanymi błędami odczytu nie może być przedstawiony jako czyste odcięcie zasilania, a żaden przypadek planu nie potrzebuje obu naraz.

`log-append` jest wyłączony, więc QEMU przepisuje log od pierwszego bajtu przy **każdym** boot. Log poprzedniego bootu jest jedynym zapisem tamtych zapisów, dlatego `start` odmawia, dopóki log nie jest pusty: po odcięciu trzeba najpierw odtworzyć obraz, potem `powercut cache off` i dopiero wtedy `start`. Tak samo `powercut cache on` odmawia, gdy zastanie niepusty log; pusty log przerwanego uzbrojenia jest ponownie używany, więc powtórzenie tego samego polecenia dokańcza własny zapis — jak przy `fault`. Bootstrap pakietów i obie fazy pakietów są przy uzbrojonym odcięciu zabronione, bo każda z nich restartuje VM i skasowałaby log.

Drugie `--apply` jest odmawiane. Powtórzenie identycznego dokańcza przerwane odtworzenie (i na gotowym jest bezczynne), ale odtworzenie z inną granicą, innym logiem albo innym baseline nie może podmienić zapisanego — bez tej odmowy wystarczyłby przebieg z całym logiem zamiast znacznika, żeby odtworzyć obraz sprzed odcięcia i zapisać w historii, że nic nie zginęło. Archiwa są równie nienaruszalne: `powercut_record` żąda, by każdy plik nazwany w historii istniał i był prywatny, więc runtime, z którego skasowano archiwum, przestaje przechodzić jakiekolwiek polecenie.

Rozbrojenie jest kontraktem, nie sprzątaniem. `powercut cache off` przy niepustym logu **wymaga dowodu odtworzenia**: prywatnego `powercut-replay.json`, który `c2_replay.py --apply` zapisuje przed konwersją i przepisuje po niej. Harness sprawdza w nim fazę `done` (przerwany `qemu-img convert` zostawia `pending`, więc półodtworzonego obrazu nie da się zwolnić jako gotowego), SHA logu, SHA obrazu bazowego z uzbrojenia oraz SHA obrazu po zapisie — ten ostatni musi się zgadzać z obrazem leżącym w runtime, więc obraz sprzed odtworzenia albo cudzy nie przejdzie. Bez tego dowodu runtime zostaje uzbrojony i `start` nadal odmawia, czyli przebieg, w którym replay pominięto albo się nie udał, nie może wystartować obrazu z niezflushowanymi zapisami i zostać przedstawiony jako przebieg po odcięciu. Log i dowód nie są kasowane, tylko **archiwizowane przez rename** na `cache-writelog-<n>.img` i `powercut-replay-<n>.json` (rename zamiast kopii: atomowo, bez podwajania miejsca i bez okna, w którym istnieją dwie kopie); archiwizacja poprzedza zapis stanu, więc przerwanie w środku naprawia powtórzenie tego samego polecenia, które mierzy już archiwum. `powercut cache off` na nieuzbrojonym runtime jest odmawiane, żeby nie dopisywać historii bez treści.

Wyjściem dla logu, którego nie da się rozliczyć, jest `powercut cache discard`. Dotyczy dwóch sytuacji: QEMU zginęło bez domknięcia dysku i log nie ma superbloku (replayer odmawia „Obcy magic logu”), albo odtworzenia nie da się dokończyć. `discard` archiwizuje log i ewentualny dowód tak samo jak `off`, ale zapisuje w historii `discarded` — powód, rozmiar i SHA porzuconego logu oraz fazę nierozliczonego odtworzenia — i **nie** zapisuje żadnego `replay`. Nad dowodem w fazie `pending` odmawia, dopóki obraz nie ma tożsamości — przerwany `convert` zostawia bajty, które nie są ani obrazem po awarii, ani odtworzeniem; trzeba przywrócić baseline albo dokończyć identyczne odtworzenie. **Dziennik odtworzeń kopiuje się poza runtime po każdym `--apply`** (decyzja właściciela 2026-09-12). Katalog runtime należy do operatora, więc żaden plik w środku nie obroni się przed skasowaniem; jedyne, co harness może zagwarantować, to że `powercut-replays.jsonl` jest kompletny i zfsyncowany, zanim `--apply` wróci, i że raport podaje jego ścieżkę, rozmiar i SHA-256 (`replay_journal` w JSON-ie raportu). Procedura kopiuje ten plik do pakietu dowodowego natychmiast po każdym odtworzeniu; późniejsze usunięcie wiersza wewnątrz runtime wykrywa się wtedy z łańcucha dowodowego, a nie z harnessu — harness tego nie wykryje i nie udaje, że wykryje.

Unieważnienie nie wisi na obecności dowodu i nie da się go zmyć przywróceniem obrazu. Świadków jest dwóch: append-only `powercut-replays.jsonl`, do którego `--apply` dopisuje wiersz **zanim** dotknie obrazu, oraz pin obrazu z chwili uzbrojenia. Jeżeli dziennik ma wpis dla tego odcięcia, `discard` unieważnia sprawę niezależnie od tego, do czego obraz hashuje się teraz — przywrócony bajt w bajt z baseline wygląda bowiem identycznie jak nigdy nieodtworzony. Pin obrazu zostaje jako drugi sygnał, na wypadek odtworzenia, którego nie cofnięto (oba powody: `obraz przepisany bez dowodu odtworzenia`). Sam `powercut cache off` też wymaga wpisu w dzienniku odtworzeń zgodnego z dowodem, więc skasowanie dziennika nie jest darmowe: zamyka uczciwą drogę wyjścia, zamiast otwierać nieuczciwą. Nad dowodem `done` `discard` jest dozwolony jako **jedyna sankcjonowana droga wyjścia z odtworzenia zrobionego nie tak**, ale **unieważnia sprawę**: wpis historii dostaje `forfeited: true` wraz z powodem, użytą granicą, znacznikiem i SHA zwalnianego obrazu, a taki runtime nie uzbroi już żadnego odcięcia — przypadek powtarza się na odbudowanym runtime. Drugiego odtworzenia „obok” nie ma: jedno odtworzenie na jedno odcięcie. O unieważnieniu decyduje **wyłącznie dziennik odtworzeń** (decyzja właściciela 2026-09-12). Jeżeli dla tego odcięcia jest w nim wpis, `discard` unieważnia sprawę; jeżeli nie ma, `discard` jest zwykłym, sankcjonowanym wyjściem i runtime wraca do użycia — także wtedy, gdy obraz różni się od pinu z chwili uzbrojenia, bo po rzeczywistym odcięciu gość zapisywał przez filtr i **zawsze** się różni. To jest sens tej decyzji: zepsuty znacznik nie może kosztować całego runtime. Pin obrazu z niczego tu nie wnioskuje, ale SHA zwalnianego obrazu zostaje w historii jako dowód do ręcznego porównania. Powód jest przez to prawdziwy w obu wypadkach: `odtworzenie zapisane w dzienniku…` mówi, że odtworzenie naprawdę było, a `porzucone bez odtworzenia obrazu` nie twierdzi nic o tym, kto zapisał obraz.

Odtworzenia zapisanego w dzienniku, którego nie dokończono, **nie porzuca się** — dokańcza się je powtórzeniem identycznego `--apply`. `discard` nad takim stanem jest odmawiany, dopóki obraz nie odzyska tożsamości (przywrócony baseline), a jeżeli go przywrócono, unieważnia sprawę, bo dziennik pamięta, że odtworzenie było.

Jeden przypadek, który wcześniej kończył się runtime bez wyjścia, jest dziś odmawiany zawczasu: wpis adresujący sektor spoza dysku. Replayer sprawdza zakres **przed** utworzeniem czegokolwiek — przed plikiem raw i przed dowodem — więc nie powstaje ani rozdmuchane odtworzenie, ani dowód w fazie `pending`, którego `off` nie przyjmie, a `on` nie pozwoli pominąć. Dla runtime, który w taki stan wszedł wcześniej, drogą wyjścia jest `discard`.

Granica odcięcia należy do harnessu, nie do wiersza poleceń replayera. `blockstats --record-cut-mark` dopisuje jeden rekord do `powercut-marks.jsonl` (uuid, indeks odcięcia, długość logu, czas) i zapisuje tę samą liczbę w uzbrojonym wpisie jako `cut_mark_bytes`. Dziennik tylko rośnie: powtórzony pomiar jest **dopisywany**, nigdy nie podmienia poprzedniego, a obowiązuje **pierwszy** rekord dla danego odcięcia — więc granicy nie da się przesunąć po fakcie, a próba przesunięcia zostaje widoczna. Każdy rekord jest walidowany przy odczycie; obcy albo uszkodzony wiersz, jak i zniknięcie dziennika przy zapisanym `cut_mark_bytes`, blokują wszystkie późniejsze polecenia. `powercut cache off` przyjmuje wyłącznie dowód odtworzenia, którego `limit_bytes` **i** `cut_mark_bytes` równają się zapisanemu znacznikowi; odtworzenie bez granicy (`null`) albo z inną granicą jest odrzucane, a nie składane do historii. Dzięki temu skasowanie `powercut-replay.json` niczego nie otwiera: podstawione odtworzenie z całego logu ma `limit_bytes` równe rozmiarowi logu, czyli różne od znacznika.

Granicę odcięcia podaje `blockstats`: przy uzbrojonym runtime jego koperta ma dodatkowo `powercut`, `powercut_history`, `powercut_log_bytes` — długość logu zmierzoną w tej właśnie chwili, zanim poleci `query-blockstats` — oraz `powercut_cut_mark_bytes`, czyli znacznik zapisany trwale. Do replayera podaje się **ten drugi**: pierwszy jest pomiarem, drugi jest zobowiązaniem, a `powercut cache off` sprawdza zgodność właśnie z nim. To jest znacznik, bez którego dowód by się rozpadł: `stop` to ACPI powerdown, więc gość przy wyłączaniu synchronizuje systemy plików i dopisuje do logu własne zapisy zamknięcia wraz z końcowym flush. Odtworzenie „do ostatniego flush w całym logu” zachowałoby wtedy wszystko i nie zgubiłoby niczego. Procedura bierze więc `blockstats --record-cut-mark` przy wstrzymanym helperze — klasa nadal dzielona, bo to jest dokładnie ten jeden dopuszczony zapis z wyjątku opisanego przy klasach blokad — a replay dostaje `--limit-bytes` z pola `powercut_cut_mark_bytes` tego pliku.

Sam replayer nie jest częścią harnessu: to `reviews/e2-09-c2-vm/c2_replay.py` z własnymi testami. Jedyną drogą zapisu do runtime jest `--apply`, które bierze wyłączny lock runtime i wykonuje wszystkie kontrole tożsamości:

```bash
# W chwili odcięcia, przy wstrzymanym helperze. Jedyne polecenie, które zapisuje znacznik:
python3 $VM blockstats "$RT" --record-cut-mark > $R/E2-09-C2-VM-P-03c-blockstats-atcut.json

# Granica bierze się z zapisanego znacznika, nie z bieżącej długości logu:
python3 $S/c2_replay.py replay --log "$RT/cache-writelog.img" \
  --baseline "$RT/cache-baseline.qcow2" --output "$R/E2-09-C2-VM-P-03c-replayed.raw" \
  --limit-bytes "$(jq -r .powercut_cut_mark_bytes $R/E2-09-C2-VM-P-03c-blockstats-atcut.json)" \
  --apply "$RT" --report "$R/E2-09-C2-VM-P-03c-replay.json"
```

Odtworzenie idzie przez plik raw (`--output`), bo prefiks nakłada się zwykłym zapisem pod offsety; do runtime wraca przez `qemu-img convert -n`, co zachowuje inode przypięty przez `check_images`. Log czytany jest strumieniowo, dwa przejścia, bez trzymania wpisów w pamięci — koszt pamięci nie zależy od okna; kosztem jest miejsce: log rośnie o każdy zapisany bajt plus 512 B nagłówka na żądanie, a `replayed.raw` jest rzadkim plikiem wielkości logicznej dysku (32 GiB, zaalokowane tyle, ile danych). Własności formatu potwierdzone na realnych logach i w źródle `block/blklogwrites.c`: `sector` i `nr_sectors` są w jednostkach `log-sector-size` (`>> s->sectorbits`), dlatego pinujemy 512; `data_len` jest zawsze zerowe; `LOG_FUA_FLAG` i `LOG_MARK_FLAG` nie są zapisywane nigdy, więc trwałość deklaruje wyłącznie wpis flush, a znaczniki nazwane nie istnieją — licznik `fua_entries` w raporcie jest z tego powodu **strukturalnie zawsze zerowy** i nie wolno go czytać jako dowodu czegokolwiek o gościu; wartość niezerowa oznaczałaby wyłącznie, że sterownik QEMU się zmienił; `nr_entries` w superblocku QEMU przepisuje przy każdym flush i co 4096 wpisów, a sam licznik podnosi pod mutexem **przed** zapisem wpisu, więc superblock bywa zarówno w tyle za plikiem, jak i chwilowo przed nim — może deklarować wpis, którego slotu jeszcze nie ma na dysku. Mniej wpisów niż deklaruje superblock jest więc albo tym wyścigiem, albo uszkodzeniem; w obu wypadkach replayer odmawia, bo brakujący slot może być właśnie tym zapisem, od którego zależy cięcie. Pin `log-sector-size` jest kontrolą tożsamości, nie arytmetyki: parser bierze rozmiar sektora z superbloku i log 4096 sam w sobie przeliczyłby się spójnie — po prostu nie jest tym logiem, który ten runtime uzbroił. Odmawia też pustego nagłówka wpisu: offset w logu jest rezerwowany pod mutexem i zwalniany przed I/O, więc przy więcej niż jednym żądaniu w locie slot może być jeszcze dziurą, a odczytanie jej jako „zapisu zerowej długości” zgubiłoby wpis, który do tego slotu należy.

Dwie granice samego mechanizmu, wprost. Po pierwsze: znacznik mówi, **ile** logu obowiązuje, ale nic w harnessie nie potwierdza, **kiedy** został wzięty. Znacznik wzięty za wcześnie zawyża `dropped_before_limit` dokładnie tak samo łatwo, jak wzięty za późno kasuje utratę; że padł przy wstrzymanym helperze i przed `stop`, dowodzi wyłącznie procedura i jej pliki dowodowe, nie ten kod. Po drugie: unieważnienie żyje w `state.json` i `powercut.json`, więc **dwa** pliki wystarczą, by je wymazać — kto przepisze w obu `reason` na „porzucone bez odtworzenia obrazu”, `forfeited` na `false` i `discarded.image_sha256` na pin uzbrojenia, przejdzie walidację, bo ona porównuje zapisane pola, a nie liczy hasha obrazu przy każdym poleceniu. Dziennik odtworzeń `powercut-replays.jsonl` podnosi cenę takiego fałszerstwa (trzeba usunąć także jego wiersze), ale go nie zamyka. To jest świadome fałszerstwo na kilku plikach naraz, wykonane ręką operatora; harness go nie wykrywa i nie udaje, że wykrywa.

Po trzecie: tolerancja urwanego ostatniego wiersza dziennika jest zarazem gestem prania dowodu. Obcięcie kilku bajtów z jedynego wiersza sprawia, że harness widzi zero rekordów, więc odtworzenie, które naprawdę było, porzuca się jako nieunieważniające. Zaostrzenie nie jest darmowe: awaria w trakcie samego dopisywania zostawia wiersz urwany naprawdę — bez dowodu i z nietkniętym obrazem — a odmowa zabrałaby wtedy `discard` jedyne wyjście. Kod zostaje więc taki, jaki jest, a gest nazywamy wprost. Ta sama sztuczka zamyka też drogę uczciwą (`off` wymaga wpisu w dzienniku), a łapie ją kopia dziennika do pakietu dowodowego wykonana zaraz po `--apply`.

Po czwarte, reszta po decyzji o jedynym świadku: odtworzenie wykonane **poza** `--apply` — ręcznym `qemu-img convert`, czego plan zabrania — nie zostawia wiersza w dzienniku, więc jego `discard` nie unieważnia sprawy, choć obraz różni się od pinu uzbrojenia. Usunięta klauzula pinu obrazu łapała dokładnie ten przypadek, ale odmawiała także przypadku sankcjonowanego (po prawdziwym odcięciu obraz różni się zawsze) i dlatego odpadła. W jej miejsce stoją dwie rzeczy spoza kodu: zakaz ręcznego odtwarzania w planie i ręczne porównanie SHA obrazu z zapisem w historii.

Po piąte: naprawa przerwanego zapisu znacznika działa tylko dopóki QEMU żyje, a runtime jest `running`, bo robi ją `blockstats`, który tego wymaga. Pokrywa więc SIGKILL na `vm.py`; awaria hosta w tym samym oknie nie jest naprawialna i kończy się odbudową runtime.

Po szóste: archiwa są pinowane nazwą i **rozmiarem**, nigdy treścią. `log_sha256` w historii jest jedynym świadkiem zawartości zwolnionego logu i nic go ponownie nie liczy — kto chce mieć pewność, że `cache-writelog-<n>.img` to ten log, liczy jego SHA-256 ręcznie i porównuje z wpisem historii. Tak samo `cache-baseline-<n>.qcow2` odpowiada polu `image_sha256` tego samego wpisu.

Granice dowodu, wprost. Odtworzony obraz nie dowodzi trwałości po stronie hosta (QEMU, btrfs, nośnik), nie modeluje rozdartych sektorów (log zapisuje całe żądania), nie modeluje dowolnych podzbiorów utraty wewnątrz jednego okna flush — ginie całe okno — i nie obejmuje kolejkowania samej emulacji NVMe. Utrata zasilania hosta pozostaje poza każdym wariantem. Wpis rozbrojenia dowodzi, że log istniał, ile miał bajtów i że został zwolniony razem z zapisanym odtworzeniem; **nie** dowodzi, że odtworzony obraz jest poprawny — to wynika z testów replayera i z porównania SHA, nie z samej historii. Przebieg z uzbrojonym odcięciem **nie jest identyczny wejściowo/wyjściowo** z przebiegiem bez niego: dochodzą zapisy do logu, `request_alignment` podnosi się do rozmiaru sektora logu, a czasy są inne, więc wyniki wydajnościowe i wyścigi czasowe z takiego przebiegu nie przenoszą się na przebieg zwykły. Licznik `flush_operations` z `blockstats` i wpisy flush w logu to **dwie różne populacje** — licznik pochodzi z urządzenia blokowego QEMU i obejmuje całe życie procesu, a wpisy logu tylko to, co przeszło przez filtr od ostatniego bootu; rosnący licznik przy braku wpisów flush (albo odwrotnie) jest sygnałem do analizy, nie automatycznym findingiem produktu.

## Dyski, obraz i dostęp

2 vCPU, 4 GiB RAM; cztery zamknięte profile dysków QCOW2 thin:

| Profil | OS | data1 | data2 | parity | cache NVMe | spare |
|---|---:|---:|---:|---:|---:|---:|
| `storage` (domyślny) | 12 GiB | 1 GiB | 1 GiB | 2 GiB | 1 GiB | 1 GiB |
| `e2` | 12 GiB | 32 GiB | 32 GiB | 40 GiB | 1 GiB | 40 GiB |
| `e2-cache` | 12 GiB | 32 GiB | 32 GiB | 40 GiB | 32 GiB | 40 GiB |
| `block` | 12 GiB | — | — | — | — | — |

`create --profile e2` oraz `create --profile e2-cache` tworzą wyłącznie nowe, puste stanowisko dla produkcyjnego
Elastic z niezmienionym `minfreespace=20G`; spare pozostaje fizyczną nazwą roli
i może zostać jawnie wybrany jako drugi parity w osobnym teście API. System oraz
data/parity/spare używają virtio. Są to nośniki funkcjonalne, nie benchmark sprzętu;
rozmiar logiczny QCOW2 nie gwarantuje dostępnego miejsca na hoście.

Manifest schema 1 pozostaje niezmieniony: profil wynika wyłącznie z dokładnej mapy
ról, seriali i rozmiarów, zgodnej z jedną z czterech powyższych konfiguracji.
Nie ma dowolnych rozmiarów ani migracji manifestów istniejących VM. Lifecycle,
kontrola pustych dysków, ścisły SSH i pakiety działają dla wszystkich czterech profili. `storage`
oraz `detach-data2` odmawiają dla `e2`, `e2-cache` i `block` na hoście, przed SSH i zapisem intentu:
formatowanie oraz odbiór E2 należą do produkcyjnego API, nie `guest_storage.py`.

Wyłącznie manifesty E2 (`e2` i `e2-cache`) mają losowy `api_port`, różny od portu SSH. Kanoniczne argv
QEMU przekazuje `127.0.0.1:<api_port>` do stałego portu gościa `8090`, również
przy `restrict=on`, aby umożliwić testy HTTP/WebSocket/Playwright. Port jest
sprawdzany przy odczycie manifestu oraz jako część tożsamości procesu. Profil
storage odrzuca to pole i zachowuje wyłącznie forwarding SSH. Nie uruchamia to
serwera API; pozostaje on osobnym etapem. Nie włącza się SSH forwarding ani dostępu LAN.

Profil jest jawny: `q35,accel=kvm,smm=off`, CPU `host`, bez automatycznego fallback do TCG. Na stanowisku przygotowania domyślne SMM powodowało reset gościa przed załadowaniem kernela; wyłączenie SMM wyłącznie w tej VM pozwoliło uruchomić kernel. Nie oznacza to diagnozy błędu hostowego KVM ani naprawy firmware; nie jest też testem SMM/Secure Boot. Profil trafia do manifestu i jest kontrolowany przy użyciu runtime. Zwykły reboot gościa jest dozwolony.

Obraz [Debian 13 generic amd64 20260831-2587](https://cloud.debian.org/images/cloud/trixie/20260831-2587/debian-13-generic-amd64-20260831-2587.qcow2) ma stały URL i SHA512 w kontrolerze. Suma pochodzi z oficjalnego datowanego `SHA512SUMS` przez TLS. Zaufanie nie jest oparte na podpisie PGP: Debian opisuje brak podpisanych sum bieżących cloud images. Pobieranie wymaga HTTPS również po przekierowaniu; niezgodny SHA512 blokuje nawet qemu-img. System po konwersji nie ma backing, podobnie jak pozostałe QCOW2; external data file jest zabroniony. Przed każdym startem sprawdzane są inody, rozmiary i format obrazów oraz hash seed.

Runtime mode 700 zawiera wszystkie obrazy, manifest, klucze klienta/serwera SSH, seed, znany klucz hosta, QMP i log serial. Klucze/seed/obrazy nigdy nie trafiają do Git ani raportów. Guest host key jest generowany przed bootem i podawany przez cloud-init, więc pierwsze połączenie również ma `StrictHostKeyChecking=yes`, bez ssh-keyscan/TOFU. Hasła i root SSH zablokowane. NOPASSWD dotyczy wyłącznie nowego konta `tentanas` **wewnątrz gościa**.

Sieć `restrict=on,ipv6=off` nie daje wyjścia do hosta/LAN/Internetu; wyjątki to jawny forwarding `127.0.0.1:port → guest:22` oraz wyłącznie dla E2 `127.0.0.1:api_port → guest:8090`. Bez bridge/tap, hostfs, 9p/virtiofs, USB/PCI/physical disk passthrough i agent/X11 forwarding. Późniejsza instalacja pakietów wymaga osobnego, jawnego etapu, nie działa z domyślnie zamkniętym egress.

## Live inspect API E2 przez Playwright

Osobny `tests/e2e/tentanas-elastic-live.playwright.config.js` nie uruchamia VM ani core. Operator musi wcześniej przygotować działające HTTPS API, aktualny WASM z sześcioma eksportami Elastic, włączoną instancję NAS oraz pakiety mergerfs/SnapRAID/ext4/XFS. Odbiór operacyjny nadal oczekuje wykonania; ten test nie jest widokiem ani kreatorem Elastic.

Przekazać przez środowisko: `TENTANAS_LIVE_BASE_URL` (wyłącznie `https://127.0.0.1:<api_port>`), `TENTANAS_LIVE_PHASE=inspect`, `TENTANAS_LIVE_NODE_ID` (dokładny identyfikator core) i `TENTANAS_LIVE_MANIFEST` (zewnętrzny manifest VM operatora, nie dane pobrane z API). Test porównuje wszystkie sześć seriali/rozmiarów z inventory; data1/data2/parity muszą przekraczać 20 GiB. UUID manifestu jest etykietą kontraktu, nie niezależnym pomiarem QMP w przeglądarce.

Logowanie używa prawdziwego formularza: `TENTANAS_LIVE_USERNAME` (domyślnie admin), `TENTANAS_LIVE_PASSWORD`, `TENTANAS_LIVE_ROTATION=required|none`, a przy wymaganej rotacji także `TENTANAS_LIVE_NEW_PASSWORD`. Hasła dostarczyć bez zapisywania ich w poleceniu, historii lub raporcie. Nie ustawiać `DEBUG=pw:*` ani szerszego debugowania obejmującego Playwright; nie włączać trace/wideo. Osłona błędu fill chroni reporter, nie zewnętrzny logger debug.

Po ustawieniu środowiska, z katalogu lokalnej zależności Playwright:

```bash
cd tests/e2e
npx --no-install playwright test --config=tentanas-elastic-live.playwright.config.js
```

Opcjonalnie `TENTANAS_E2E_BROWSER` wskazuje lokalną przeglądarkę, a `TENTANAS_LIVE_ARTIFACTS` katalog raportu. Test ma zero retry i nie podmienia transportu. Po logowaniu/ewentualnej rotacji wykonuje tylko odczyt katalogu, gotowości NAS, capabilities i dysków; nie instaluje, nie włącza instancji i nie tworzy macierzy. Błąd kontraktu lub brak gotowości oznacza odmowę, nie automatyczny dobór dysków.

## Pakiety — dwa jawne kroki

```bash
python3 tests/infra/tentanas-vm/vm.py bootstrap-packages /mnt/d/repos/tentanas-vm.ABC123
```

Ten krok tylko przygotowuje oficjalne źródła HTTPS Debian trixie/updates/security
main, zachowuje oryginał źródeł i stan timerów apt, blokuje konkretną automatykę
storage, pobiera podpisane indeksy i archiwa pakietów profilu z zależnościami
(pięciu dla `storage`/`e2`/`e2-cache`, dwóch dla `block`).
Nie uruchamia maintainer scripts pobranych archiwów. Log zawiera pełne metadane,
listy plików, skrypty kontrolne oraz SHA256 wszystkich archiwów do osobnego review.
Tylko na czas pobrania przełącza własną VM na jawny `restrict=off` (stan
`bootstrap` z rzeczywistym argv); **to nie jest sieć ograniczona tylko do apt**,
gość ma wtedy ogólny egress, również potencjalnie do hosta/LAN. Nie dodaje
forwardingów, bridge, proxy ani usług hosta. Powrót do restrict=on jest w finally,
także po błędzie i obsłużonym SIGINT/SIGTERM; SIGKILL i awaria hosta mogą
przerwać cleanup, więc taki stan wymaga jawnej diagnostyki przed kontynuacją.

Po pobraniu VM znów jest izolowana, a archiwa czekają na przegląd. Timery apt
pozostają zamaskowane pomiędzy download i install, aby nie zmieniały transakcji.
Dopiero po zaakceptowaniu rzeczywistych skryptów i zależności:

```bash
python3 tests/infra/tentanas-vm/vm.py install-packages /mnt/d/repos/tentanas-vm.ABC123
```

Instalacja sprawdza SHA256 całego cache i ponawia guard masek, cron oraz udev.
Działa stale offline przez apt `--no-download`, bez dist-upgrade/usuwania pakietów.
Następnie sprawdza dpkg audit, wersje/hash narzędzi i SnapRAID status w prywatnym
katalogu na OS. DMI sprawdza root gościa, ale proces sondy najpierw trwale zrzuca
grupy/GID/UID do konta tentanas; SnapRAID nie działa jako root. Po przywróceniu
izolacji oraz zastanego stanu apt powstaje root-owned mode600
`/var/lib/tentanas-vm-packages/<uuid>/ready.json` dla kolejnego kroku storage.
`downloaded.json`, `installed.json` i `ready.json` oznaczają różne stany.

Awaryjny QMP quit należy wyłącznie do cleanup bootstrapu po nieudanym powerdown;
ponownie sprawdza pełną tożsamość procesu i UUID, a potem faktyczne zakończenie.
Taki przebieg zawsze kończy etap błędem i nie tworzy gotowości. Zwykłe `stop`
z V01 nadal nie stosuje quit/kill. Jeśli prepare odmawia przy działającej
izolowanej VM (np. trwa legalne apt), cleanup potwierdza profil bez restartu
i nie przerywa tej pracy. Przerwanej instalacji nie wolno nazywać poprawną.

Test izolacji porównuje TCP443 tego samego zapisanego publicznego IP oficjalnego
endpointu apt: połączenie w czasie download ma działać, po zamknięciu egress ma
odmówić z krótkim timeoutem, bez mylenia awarii DNS z blokadą sieci. Całe V02.1
pozostawia pięć nośników testowych pustych; realny cykl macierzy jest odrębny.

## Profil block — iSCSI i NVMe-oF w gościu

Osobne stanowisko do pomiaru zachowania jądra celów blokowych: LIO z inicjatorem
`open-iscsi` (`iscsiadm`) oraz nvmet z `nvme-cli`, target i inicjator w tym samym
jądrze gościa przez 127.0.0.1. Profil ma wyłącznie dysk OS 12 GiB i nie ma portu API;
LUN-y pomiaru to pliki loop w `/var/tmp`, nie dyski VM.

```bash
python3 tests/infra/tentanas-vm/vm.py create --profile block
python3 tests/infra/tentanas-vm/vm.py start /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py ssh /mnt/d/repos/tentanas-vm.ABC123 cloud-init status --wait
python3 tests/infra/tentanas-vm/vm.py inventory /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py bootstrap-packages /mnt/d/repos/tentanas-vm.ABC123
python3 tests/infra/tentanas-vm/vm.py install-packages /mnt/d/repos/tentanas-vm.ABC123
```

Kontrakt pakietów niesie profil wyliczony z mapy dysków; gość odmawia nieznanego
profilu, instaluje dokładnie `open-iscsi` i `nvme-cli` (bez `targetcli`/`targetcli-fb`:
jego saveconfig byłby drugim źródłem prawdy LIO), sprawdza archiwa względem tego
zestawu, a `downloaded.json` i `ready.json` zapisują rzeczywisty zestaw. Oprócz masek
storage maskowane są `iscsid.socket`, `iscsid.service` i `open-iscsi.service`, a reguły
udev `open-iscsi` i `nvme-cli` zastępuje `/dev/null`: gość sam nie uruchamia iscsid
ani nie loguje się do zapisanych node'ów. Alias `iscsi.service` musi mieć LoadState
`masked` lub `not-found` (na żywym gościu: `not-found`, bo maski powstają przed
instalacją). Sonda nieuprzywilejowana potwierdza po instalacji brak procesu iscsid,
sesji iSCSI, kontrolerów NVMe, modułów `target_core_mod`/`nvmet` oraz drzew LIO/nvmet
w faktycznie zamontowanym configfs; nieczytelna ścieżka kończy sondę błędem.

Pomiar uruchamia demona jawnie (`sudo /usr/sbin/iscsid`) przed `iscsiadm` i kończy go
`sudo iscsiadm -k 0`. `systemctl start iscsid` pozostaje zablokowane maską, a fallback
iscsiadm (`systemctl start iscsid.socket`) kończy się odmową. `inventory` oczekuje
wyłącznie dysku OS, więc urządzenia loop, iSCSI i NVMe-oF z pomiaru trzeba odłączyć
przed kolejnym inventory lub etapem pakietów.

## Cykl storage w przygotowanym gościu

```bash
python3 tests/infra/tentanas-vm/vm.py storage /mnt/d/repos/tentanas-vm.ABC123 preflight
```

Zamknięte fazy to `preflight`, `prepare`, `exercise`, `verify`, `corruption`. Pierwsza jest
odczytowym guardem; `prepare` formatuje jednorazowo wyłącznie data1/data2/parity,
`exercise` wykonuje rzeczywisty cykl danych i odzyskania, `verify` po restarcie
kontroluje istniejące FS/dane bez formatowania. Każda faza wymaga osobnej zgody
na właściwe operacje; samo poprawne preflight nie uruchamia prepare.
Kontroler dopuszcza tylko działającą VM restricted i sprawdzoną tożsamość procesu.
Przesyła kod `guest_storage.py` na stdin przez ścisły SSH oraz jeden cytowany JSON
z fazą, UUID i pełnymi sześcioma rolami z lokalnego zweryfikowanego manifestu.
Użytkownik nie podaje ścieżek urządzeń ani kontraktu dysków. Lokalne oczekiwanie
procesu SSH ma limit 900 s; nie gwarantuje to zatrzymania zdalnej operacji ani
rollbacku. Po timeout rozpoznać bieżący stan gościa i journal, nie zakładać,
że operacja zakończyła się, i nie ponawiać przez kasowanie journala.
Nie wywołuje sondy pustych dysków V01: po prepare jej odmowa jest oczekiwana,
a osobne guardy gościa sprawdzają receipt pakietów, automatykę i bieżącą tożsamość FS.

`corruption` jest osobnym, jednorazowym testem kontrolowanej zmiany danych korpusu,
wykrycia przez scrub i odzyskania do pierwotnego SHA. Wymaga osobnego odbioru kodu
i zgody operatora; nie jest częścią wykonanego checkpointu V02.2 z tabeli poniżej.
Journal zachowuje poprzedni baseline/boot_id w corruption.before i przechodzi
do stage=corrupting. Dopiero pełny sukces ustala nowy baseline/boot_id oraz exercised.
Istniejące verify nadal sprawdza ostatni ukończony checkpoint i wymaga późniejszego
restartu; stan corrupting odmawia. Obecność wpisu corruption blokuje ponowny test,
a original.json pozostaje niezmienny. Nie ma aliasu verify ani automatycznego resume.

Pomiar FS/UUID całych dysków używa bezpośredniego `blkid -p -o export`,
a lsblk nadal dostarcza seriale i topologię; niezależne wipefs i kontrole mountów
pozostają wymagane. Samo lsblk może chwilowo pokazywać stary stan udev po mkfs.
Nie zastępować odczytu UUID wartością oczekiwaną ani nie usuwać guardu po rozbieżności.
Journal `format_pending` oznacza możliwy rzeczywisty format nawet wtedy, gdy licznik
potwierdzonych formatów nadal wynosi zero. Powtórzenie prepare odmawia; częściowego
journala nie kasuje się dla retry. Osobny świeży runtime zachowuje dowody starej próby.
Pierwszy rzeczywisty przebieg zatrzymał się na tej rozbieżności po jednym formacie;
bieżący wynik nowego cyklu i odbiór poprawki znajdują się w raportach, nie są
domyślnie uznawane za sukces na podstawie samych testów jednostkowych.

## Rzeczywisty checkpoint funkcjonalny

Testowane pakiety gościa: SnapRAID `12.4-1` i mergerfs `2.40.2-5`,
identyfikowane metadanymi dpkg oraz SHA256 archiwów/binarek. Ich faktyczne
CLI drukują odpowiednio `vnone` i `vunknown`; nie są to testy hostowego SnapRAID 14.7.
Runtime, klucze i obrazy pozostają poza Git. Surowe logi oraz artefakty odbioru
są w zewnętrznych raportach `new_apps/reviews`, nie w źródłach harnessu.

| Etap na nowej VM | Rzeczywisty wynik |
| --- | --- |
| Prepare | Trzy ext4, kod 0; cache/spare i OS poza celami formatowania |
| Korpus | 131 MiB zapisane przez unię, pliki na obu niezależnych FS |
| Sync i scrub | diff 2 → sync 0 → diff 0; pełny scrub 268 bloków, 100%, bez błędów |
| Odzyskanie | Usunięte 64 MiB z data2; ograniczony fix, oryginalny SHA zgodny, check 0 / final diff 0 |
| Restart i verify | Nowy boot_id, zgodne trzy UUID FS i SHA siedmiu plików; check 0, 100%, 138 MB; zero mkfs/sync, journal identyczny |

Statvfs unii dwóch odrębnych FS zmierzył sumę `2041405440 B`, nie pojemność
jednej gałęzi. Dawny pomiar kilku katalogów na wspólnym FS nie opisuje tego układu.
To nie benchmark fizycznego NAS. Cache/mover i ENOSPC oraz pełne E2 core/UI
pozostają poza zakresem. Osobny późniejszy test utraty całego data2 opisano niżej.

### Kontrolowana cicha korupcja V02.7

Na tych samych buildach pakietów rzeczywista faza corruption zakończyła się kodem 0.
Zmiana zawartości restore.bin na data2 dała inny SHA przy diff 0; pełny scrub
zakończył się kodem 1 i wskazał jeden błędny blok pliku d2/restore.bin, pozycja 0.
Ograniczony fix zakończył się kodem 0 (1 błąd naprawiony, 0 nieodzyskanych), przywracając
pierwotny SHA. Check 0 i kolejny pełny scrub 268 bloków / 100% / zero błędów
potwierdziły naprawę. Nie wykonano sync ani mkfs. Oryginalny manifest, config
i parity zachowane; nowy baseline content powstał po czystym scrub.
Ponowne corruption i verify bez restartu odmówiły kodem 1, nie zmieniając journala.
Normalny stop/start i verify po nowym checkpointcie zakończyły się kodem 0:
nowy boot_id, check 100% / 138 MB, siedem SHA przed/po restarcie identycznych,
journal niezmieniony i zero mkfs/sync. Sam V02.7 nie dowodzi utraty całego
dysku; osobny późniejszy wynik V03 znajduje się poniżej.

## Zimne odłączenie data2 i odzysk na spare

`detach-data2 RUNTIME` wymaga zdrowego checkpointu po ukończonej fazie
`corruption`, pełnych sześciu dysków i działającej izolowanej VM. Samo
`exercise` oraz restart/verify nie zastępują obowiązkowej korupcji i odzysku.
Przed wywołaniem detach operator odczytuje guest `state.json` i potwierdza
`stage=exercised`, `format_count=3`, `corruption.clean_blocks>0`, brak wpisu
`replacement` oraz zgodność oryginalnych SHA i baseline; zachowuje ten odczyt
w artefaktach wraz z udanym verify po restarcie checkpointu korupcji.
Nie należy używać detach jako sondy tych warunków: jeszcze przed SSH utrwala hostowy operation_id
w `detach-intent.json` i state.retirement. Wewnętrzny replacement-arm sprawdza
gościa i zapisuje journal; następnie zwykły stop potwierdza zakończenie procesu.
Dopiero wtedy kontroler mierzy SHA256 starego QCOW2 i utrwala końcowy rekord
oraz `retired-data2.json`. Polecenie kończy stopped, bez automatycznego startu.

Kolejny zwykły start i każdy restart mają dokładnie pięć dysków: nie przekazują
QEMU ani drive, ani device starego data2. Jego plik, inode i SHA pozostają
kontrolowanym dowodem poza gościem; manifest sześciu oryginalnych ról nie jest
zmieniany. Spare zachowuje własny serial. Nie ma reattach, kasowania intent
ani resume przerwanego detach. Pending intent blokuje start/pakiety/storage/SSH
i ponowienie; ważny zapis pozwala odczytać status i normalnie zatrzymać
pierwotny proces. Brak lub sprzeczność któregokolwiek dowodu oznacza odmowę.

Po osobnym odbiorze odłączenia służą zamknięte fazy
`storage RUNTIME replacement-preflight`, `replacement-prepare` i
`replacement-recover`. Kontroler przekazuje operation_id z końcowego rekordu
jako replacement_id, nigdy z argumentu użytkownika. Gość sprawdza dokładne pięć
ról i własny journal; jedyny nowy mkfs dotyczy spare. Zwykłe verify zachowuje
znaczenie kontroli ostatniego ukończonego checkpointu po restarcie, z jawnym
mapowaniem logicznego data2 na fizyczny spare. Inventory V01 oraz pakiety po
odłączeniu odmawiają; nie omija się ich guardów ani nie otwiera ponownie egress.

Rzeczywisty V03 na tych samych buildach pakietów potwierdził poniższy cykl.

| Etap | Wynik operacyjny |
| --- | --- |
| Zimne odłączenie | Normalny stop i nowy boot bez drive/device starego data2; pięć dysków potwierdzonych przez argv, QMP i gościa |
| Przygotowanie spare | Jeden mkfs ext4 na pustym spare; nowy UUID logicznego data2, licznik formatów 3 → 4 |
| Odzysk całego d2 | Fix 0: 256 błędów naprawionych, 0 nieodzyskanych; pierwotny SHA odzyskanych 64 MiB zgodny, cały korpus 131 MiB zachowany |
| Kontrola i checkpoint | Oryginalne SHA i check przed sync; następnie sync/diff/full scrub/check kod 0, 268 czystych bloków / 100% |
| Negatywy | Powtórne prepare/recover/detach i verify bez nowego boot odmawiają; journal bez zmian |
| Restart | Zwykły stop/start/verify kod 0, nadal pięć dysków, nowe UUID i siedem SHA zgodne, count 4; zero mkfs/sync i zmian journala |

Obie kopie content mają nowy wspólny baseline; parity, config i original.json
pozostały niezmienione. Niezależne porównanie po restarcie potwierdziło również
stały SHA wycofanego QCOW2 i oryginalnego manifestu hosta. Nie użyto starego obrazu
jako źródła odzysku. Jest to funkcjonalna utrata dostępu do całego nośnika gościa,
nie awaria elektroniki ani test wielu brakujących dysków. Cache/mover, ENOSPC
i produkcyjne E2 nadal wymagają osobnych etapów.

## ENOSPC pojedynczej gałęzi V04a

Po ukończonej wymianie data2 dostępne są zamknięte fazy
`storage RUNTIME enospc-preflight` i `storage RUNTIME enospc`.
Kontroler wymaga istniejącego profilu detached/restricted i przekazuje zgodny
replacement_id; ukończenie replacement oraz aktualne mounty/SHA sprawdza gość.
Bezpośrednio przed mutującym SSH enospc host mierzy filesystem runtime przez
statvfs i wymaga co najmniej 3 GiB dostępnych (`f_bavail * f_frsize`). Brak pomiaru
lub mniejsza wartość powoduje odmowę bez uruchomienia zdalnej fazy.

Przed fill PM wykonuje osobny odczytowy eksport bezpieczeństwa 131 MiB korpusu
i metadanych, sprawdza SHA oraz wolne miejsce już po eksporcie. Nie jest to
automatyczny backup manager ani pole potwierdzenia kontrolera. Preflight nie
potwierdza wykonania tego eksportu. Test ma dotyczyć dopisywania przez unię do
jednego nowego pliku przypiętego do data2/spare, przy wolnym data1 i rzeczywistym
moveonenospc=false; nie testuje wyboru gałęzi tworzenia ani pełnej unii.
Przyjęty rzeczywisty profil cache.files to libfuse, nie off:
writeback/direct_io/kernel_cache/auto_cache=false, cache.statfs=0.
To odczyt runtime zaakceptowany po przeglądzie dokumentacji źródłowej;
nie przełączano opcji VM, aby dopasować je do pierwotnej propozycji testu.
Wynik wymaga rzeczywistego errno ENOSPC, cleanup wyłącznie własnego pliku,
oryginalnych SHA i check bez sync/fix/scrub oraz późniejszego restart/verify.
Pierwszy rzeczywisty przebieg V04a zakończył się kodem 1: write przez unię
zwrócił errno 28 przy dostępnych 0 B na data2 i wolnym data1, lecz nasz guard
błędnie wymagał najwyżej 2 MiB f_bfree. Ext4 zachowało 16 MiB wewnętrznej rezerwy
(4096 reserved_clusters × 4096 B), bez zmiany ustawień FS. Potwierdzone write
to 936378368 B, widoczny plik 936509440 B, alokacja 936513536 B — nie są
to wymienne liczniki. Cleanup usunął tylko własny plik, siedem pierwotnych SHA
pozostało zgodnych, miejsce wróciło, a osobny check operatora zakończył się kodem 0.
Journal pozostał enospc_filling z cleaned=true, bez completed; ponowienie zabronione.
To diagnoza nieudanego kryterium, nie zaliczone V04a ani nowy restart/verify.
Przyjęta korekta wymaga available==0 oraz zgodnego bilansu fizycznego:
`abs((before.free - after.free) - allocated_bytes) <= 2 * chunk`.
Nie dodaje parsera sysfs ani stałej rezerwy 16 MiB; pozostają wymagane errno 28,
właściwy inode i payload oraz dotychczasowe guardy. Regresja używa małego
rzeczywistego pliku 8 KiB i pomiarów z rezerwą 16 MiB; odrębna retrospekcja
sprawdza zmierzone 936513536 B alokacji z nieudanej próby. Nie jest nowym testem VM.
Poprawkę odebrano kodowo, a późniejszy świeży przebieg V04-r3 opisano poniżej;
nie zmienia on historycznego kodu 1 ani istniejącego pending journala.
Cache, mover i ENOSPC parity pozostają osobnymi zakresami.

V04-r3 na nowej VM 58cpLt ukończył pełną kolejność: prepare/exercise,
obowiązkowa corruption, restart/verify, zimne odłączenie data2, odzysk na spare
i restart/verify. Poprzednia próba 6CpFrJ pominęła corruption: detach odmówił,
hostowy intent pozostał pending, a VM normalnie zatrzymano bez usuwania dowodów.
Nie użyto tam retry ani edycji journala do obejścia warunków.

Po odrębnym eksporcie i sprawdzeniu SHA korpusu/metadanych V04-r3 enospc
zakończył się kodem 0: rzeczywisty errno 28/write, available data2=0,
free=16 MiB i bilans ubytku free równy fizycznej alokacji 936513536 B.
Data1 i unia nadal miały dostępne 879681536 B. Cleanup tylko własnego pliku
przywrócił available data2=882827264, oryginalne SHA i check 0 zachowane,
completed=true/count4, bez sync ani nowych mkfs. Verify na tym samym boot
odmówił bez zmiany journala. Następny normalny stop/start/verify kod 0
potwierdził nowy boot, UUID, SHA i niezmieniony journal, profil pięciu dysków
oraz zachowany obraz starego data2 poza gościem. To odebrany test append
przy wyczerpaniu miejsca dostępnego dla zapisu na jednej gałęzi, nie całej unii
ani produkcyjnego E2. Historia pVtkPK i 6CpFrJ pozostaje zachowana.

## Niepusty korpus na produkcyjnie utworzonej macierzy E2

`guest_e2_snapraid.py` obsługuje wyłącznie `preflight`, `corpus`, `protect`
i `verify` dla przypiętego stanowiska kjpyi0, UUID
`561fce56-b0f7-43da-9efe-1a690798337e`, oraz macierzy `e2-xfs-two`.
Aktualny odbiór używa preflight/corpus i odczytowego verify; **nie uruchamia
protect**, ponieważ Sync/Scrub wykonuje rzeczywiste UI aplikacji.
Nie uruchamia Create, mkfs, mount ani zapisów na macierzy ext4 lub cache.
Piny DMI, sześciu seriali/rozmiarów, spec/trzech UUID XFS i configu pochodzą
z jawnego prywatnego kontraktu postcreate, sprawdzanego po SHA bajtów.
Osobny checkpoint wiąże stationSha256, bootId, journalSha256 oraz trzy
odebrane emptyContent (bytes=133/sha256). Binarka SnapRAID pozostaje
przypięta. Mutacje wymagają tego samego bootu i root journala.

Źródłem fixture jest wersjonowany `readonly-elastic-audit.py`
(SHA256 `41640b6fe396e8eb5539b19fd2e69e8a9541b46ba1b939396c51143e7d180cdd`)
oraz istniejący `guest_storage.py`
(`9c2298169682b3f7e8170ce1aa736f18eeb49063d368c81faa1546dbf4cd164e`).
Operator dostarcza dokładne pliki do `/root/tentanas-e2-audit` (root, 700;
pliki root, 600). Harness weryfikuje bajty przed importem, nie wykonuje ich
main i nie szuka alternatywnych źródeł w cwd, new_apps lub sieci.
Brak albo inny SHA oznacza odmowę; testy lokalne korzystają z plików repo.

Wyłącznie operator VM, po niezależnym odbiorze źródeł i świeżym sprawdzeniu
jobs/klientów/udziałów oraz miejsca hosta, przekazuje skrypt przez istniejący
ścisły SSH do `sudo -n python3 - FAZA KONTRAKT SHA CHECKPOINT SHA`.
Oba pliki wejściowe muszą mieć root600 w prywatnych root700 katalogach.
Każda faza jest osobnym wywołaniem;
nie wolno wykonywać poniższego przykładu na hoście:

```bash
sudo -n python3 - preflight /prywatny/kontrakt.json SHA_KONTRAKTU /prywatny/checkpoint.json SHA_CHECKPOINTU < guest_e2_snapraid.py
```

`preflight` nie zapisuje. `corpus` tworzy tylko nowy własny katalog
`/mnt/e2-xfs-two/tentanas-e2-corpus` i dwa pliki 8 + 9 MiB oraz trwały
manifest oryginalnych SHA, rozmiarów, inode, device i mtime_ns.
Obie macierze muszą być przypięte do wspólnego finalnego kontraktu przed
korpusem używanym przez operacyjny skrypt UI. Nie kopiujemy starych SHA
losowego korpusu ani failed/pending stanowiska TNuDcB.
`protect` wykonuje dokładnie diff → jeden sync → diff → check → pełny scrub;
wymaga dwóch własnych nowych plików, 68 bloków po 256 KiB, 100% odczytu,
zerowych błędów, obu niepustych parity oraz trzech zgodnych content.
`verify` przyjmuje corpus_done lub protected i wyłącznie porównuje rzeczywisty
korpus z trwałym original/state, bez narzędzi SnapRAID i zapisu. Pełny root
guard nadal wymaga zgodnych dysków/spec/UUID/mountów/konfiguracji i Ready.
Legalnie zmieniony boot/root receipt/content po UI nie jest porównywany do
pustego checkpointu. Wynik corpus_only nie dowodzi Sync/Scrub, niezmienności
content ani ochrony danych; potrzebny jest osobny odczyt receipt/logów.
Baseline do porównania restartu należy odebrać po ostatniej udanej operacji.
Nie jest to test korupcji/odzysku.

Cała faza trzyma istniejący root lock EX/NB; mutujące dzieci dziedziczą FD.
Osobny journal w `/var/lib/tentanas-e2-payload/e2-xfs-two` utrwala pending
przed korpusem i każdym narzędziem. Logi fsync także przy wyjątku/timeout.
Pending lub powtórzenie ukończonej fazy mutującej odmawia, bez resume, kasowania
dowodów lub automatycznego ponowienia. Root receipt Create/Restore i DB
aplikacji pozostają nietknięte. Korpus i logi zostają po zakończeniu.

Odbiór rzeczywistej aplikacji c5 na kjpyi0 (2026-09-08), po dwóch
odebranych Create i korpusie; prywatne dowody w `tentanas-e208-runtime.CsGwzZ`:

| Operacja | Odebrany wynik |
|---|---|
| UI Sync nowych danych | Jedna próba, terminalny job succeeded; dwa własne pliki, [diff,ok], errors0/0/0, 100%; trzy content1383B i dwie parity17MiB. |
| UI pełny Scrub | Czysty diff, rzeczywiste `-p full`, 68/68 bloków, errors0/0/0 i 100%; dane/parity/config zachowane, content legalnie zaktualizowany; data ostatniego Sync niezmieniona. |
| Osobny UI Sync bez zmian | [equal,ok], `Nothing to do` bez `Everything OK`, errors0/0/0; liczniki bloków/postępu null, nie zera; lastScrub zachowany. Content ma ten sam SHA, lecz nowe inode po zapisie. |
| Normalny restart — root | Nowy boot `2a6a7cb4-014b-40fd-9301-4a783671a892`; obie macierze measured/Ready/null, stable równe baseline **after-nochange**, korpus dokładnie zgodny z original, receipt i bajty logów bez zmian. |
| Historia UI po restarcie | Odczyt kod0/zero mutacji: XFS Active, trzy Succeeded, history/lastSync/lastScrub równe before-reboot; kontrolna ext4 Active/No parity, pusta historia i niedostępne CTA SnapRAID. |

E2-08 odebrano w zakresie ręcznych Sync/Scrub, wyników/historii i normalnego
restartu, nie całego Elastic. Pierwszy screenshot historii Sync miał stary
DOM running i pozostaje nieprzyjętym dowodem terminalnym; wynik potwierdzają
osobne JSON i przyjęty późniejszy odczyt UI. Cała bramka harnessu ma siedem
testów, w tym nową regresję terminalnego modala/DOM w Chromium, bez zmiany
produktu lub ponowienia Sync. Każda mutacja była
osobno dopuszczona, bez retry/protect. Nie zmieniano failed/pending starej
TNuDcB. Odbiór nie obejmuje korupcji/fix, awarii dysku ani cache/mover.

## Sonda cache E2-03 — osobne stanowisko

`guest_cache_probe.py` dopuszcza wyłącznie VM `49efc20d-4b22-44e5-ac25-2ad5063b19eb`
profilu `e2`. Logiczny cache to data1 32 GiB, logiczne data to parity 40 GiB;
nie jest to pomiar wydajności NVMe. OS pozostaje poza formatowaniem/fillerem,
ale przechowuje pakiety, skrypt, mały journal i logi. Trzy inne puste dyski
(data2, NVMe cache 1 GiB, spare) pozostają nietknięte.

Operator dopuszcza osobno kolejność `preflight → nc → race → enospc`.
Przed nią wymagane są: restricted/odebrane pakiety, dokładna szóstka seriali,
root-owned fixture `/root/tentanas-e2-audit/guest_storage.py` (plik 600,
katalog 700, SHA zgodny ze stałą sondy), piny mergerfs/mkfs oraz przynajmniej
64 GiB wolnego miejsca hosta na wzrost QCOW2. Rezerwę hosta sprawdza operator,
nie sonda gościa; sonda wymaga 2 GiB dostępnego OS, ogranicza filler do 32 GiB
i 20 minut. Nie podawać hostowych urządzeń ani alternatywnego UUID.

Przykład w przypiętym gościu, dopiero po odbiorze źródeł i zgodzie operatora:

```bash
sudo -n python3 - preflight < guest_cache_probe.py
```

`preflight` tylko odczytuje. `nc` formatuje dokładnie dwa cele XFS i sprawdza
nowy plik na RW oraz append istniejącego na NC. `race` używa rzeczywistego
FD i barier pipe: po kopii dopisuje marker, zachowując obie wersje bez unlink.
Surowy raport pisarza i metryki obu kopii zapisuje przed oceną predykatu.
W FUSE `fstat` może przejść przez ścieżkowy `getattr`; `newest` porównuje
sekundy mtime, a remis wybiera późniejszą gałąź. Union fstat size/inode są
osobną obserwacją, nie tożsamością backing FD. Dowód wymaga stabilnego
urządzenia FUSE, backing device/inode, dokładnych rozmiarów i SHA obu kopii,
child rc 0 oraz markera po barierze; bezpośredni FD zachowuje ścisłe asercje.
Podstawa: [mergerfs 2.40.2 libfuse](https://github.com/trapexit/mergerfs/blob/2.40.2/libfuse/lib/fuse.c#L1586-L1640),
[newest](https://github.com/trapexit/mergerfs/blob/2.40.2/src/policy_newest.cpp#L110-L141)
i [Linux v6.12 vfs_fstat](https://github.com/torvalds/linux/blob/v6.12/fs/stat.c).
`enospc` oddziela odmowę create poniżej 20G, zapis wcześniejszym FD, fizyczny
brak miejsca fillera i końcowy append przez unię. MFS wyklucza NC także jako
cel moveonenospc; brak spill jest pomiarem negatywnym, nie gotowym moverem.
Odmowa polityki przy create dopuszcza ENOSPC/28 albo EROFS/30 i osobno
wymaga braku obu backing obiektów; raw errno/obecność są utrwalane przed
predykatem. To nie fizyczne ENOSPC: błąd zapisu fillera lub held FD musi
nadal mieć errno 28, a poprawny wcześniejszy append musi zachować dane.
Podstawa odmowy polityki: [mergerfs 2.40.2 policy_error.hpp](https://github.com/trapexit/mergerfs/blob/2.40.2/src/policy_error.hpp)
oraz [policy_mfs.cpp](https://github.com/trapexit/mergerfs/blob/2.40.2/src/policy_mfs.cpp).
Append zakończony na cache bez errno pozostaje utrwalonym wynikiem
nierozstrzygniętym i odmową fazy, nie dowodem relokacji.
XFS może odzyskać spekulacyjną prealokację EOF podczas ENOSPC; dlatego
ochrona poprzednich plików wyłącza z równości wyłącznie `allocated`,
zachowując device/inode/bytes/mtime/SHA. Surowa alokacja przed/po pozostaje
obserwacją, a bilans fillera uwzględnia jej zmierzoną signed deltę na cache
oraz held-append, nadal z tolerancją 16 MiB i niezmienionymi limitami.
Podstawa: [XFS reclaim przed odmową zapisu](https://github.com/torvalds/linux/blob/v6.12/fs/xfs/xfs_file.c).

Journal zapisuje pending przed mutacją; ten sam boot, inode blokady i EX/NB
chronią kolejność. Brak automatycznego retry, resetu, cleanup i ponownego
formatowania. Po odmowie zachować stan, mounty i pliki do odczytu operatora.
Testy `test_guest_cache_probe.py` wykonują guardy, prywatny journal, rzeczywisty
flock między procesami oraz pipe/FD, ale nie montują FUSE ani nie zapełniają
urządzeń. Stara VM `44fc2cf1-06f3-4135-a26b-220a2d5beba5` zaliczyła NC,
ale race-r1 odmówił, pozostawiając `nc/pending=race`, bez retry i ENOSPC.
Utraconego writer fstat r1 nie odtworzono. R2 UUID `5185b7bd…` zaliczyło
NC/race, ale odmówiło na złożonym warunku progu, zachowując
`race/pending=enospc`; rzeczywiste errno utracone, późniejszy odczyt nie
znalazł obu backing plików. Nie zaliczono fizycznego ENOSPC i nie ponawiano
fazy. R3 na exact `325ca3467…` wykonało NC/race/ENOSPC kod 0, journal
`enospc/pending=null` na tym samym boocie; krytyk przyjął trzy bazowe pomiary.
Próg create faktycznie odmówił EROFS 30 przy available 20665876480 B, bez
obiektów; wcześniejszy FD dopisał 19 B. Filler przyjął 33583988736 B przed
write ENOSPC 28, końcowy union append 4060 B przed write 28. Plik 4096 B
(prefix 17 + marker 19 + append 4060) pozostał na cache, `no_spill_nc`,
SHA obu odczytów backing/union zgodne `003eb859…`. Wolne cache 266240 B,
data 42009751552 B; `available_zero_allocation=false`, nie zero free.
Bilans 33550446592 B jest dokładnie zgodny po zmierzonej delcie prealokacji
-33546240 B. Sześć wcześniejszych plików zachowało SHA/device/inode/bytes/mtime;
trzy pozostałe puste dyski nietknięte. To trzy bazowe pomiary, nie implementacja
produkcyjnego cache/movera ani odbiór innych wariantów polityk. Projekt
wyłączności pisarzy dla E2-09 pozostaje szkicem, nie przyjętą implementacją.

## Sonda A0 — zakres readonly FUSE przed moverem service-mode

`guest_writer_gate_probe.py` przygotowuje osobny pomiar `prepare → local → global`
na nowej VM r3 `mOudMN`, UUID `5a6b69ca-df2a-4083-b90d-7bbd3f486a3f`, boot
`95a783e6-9609-4d4b-a0cf-4725d87987f4`. To kandydat bramki wyłączności,
nie produkcyjna bramka ani mover. Opublikowany `fdd5ad4f…` ma exact 217/217 PASS
(1.402 s), push i osobny ls-remote potwierdzone. R3 preflight/prepare/local/global
zakończone kodem 0, journal `global/pending=null`; dowody `crrYg8`.
Local: `perMountBypass=true`, `umountOutcome=busy`, errno 16 — umount nieudany.
Global: przy held FD remount odmówił 16, trzy held-write dopisały po 5 B;
po close/unmap remount 0, trzy aliasy mają superblock/statvfs RO i wszystkie
cztery tryby otwarcia odmawiają EROFS 30. Direct backing nadal dopisał 15 B:
to dostęp poza bramką FUSE. Mmap ENODEV 19 / `mmap_tested=false` nie jest testem
blokowania. PM porównał osiem identity/SHA, dane NC puste, dodatkowe dyski puste;
Dispatch przyjął niezależnie wynik tylko w zakresie FUSE. Następny krok: izolacja direct branch przed
copy/unlink; pełne A0 i produkt E2-09 pozostają otwarte. Wynik `perMountBypass` jest odrębny od
`umountOutcome=succeeded|busy|unexpected`: busy nie oznacza odmontowania.
Przed syscall sonda utrwala pełne mountinfo rodzica i aktorów wraz z PID/start/
namespace; brak zgodnego, trwale zapisanego kontekstu blokuje komendę. Globalne
kryteria pozostają ścisłe. Historii r1/r2 poniżej nie przeklasyfikowujemy.
Sonda używa małych plików i mergerfs na katalogach OS, bez formatowania pięciu
dodatkowych dysków, kopiowania danych ani unlink. Testowe branche są dostępne
do przejścia od początku; prywatny journal i lock pozostają pod root 0700/0600.

Rozróżniamy readonly konkretnego montowania od readonly całego superbloku,
z aliasem bind i aliasem w user/mount namespace zwykłego użytkownika UID 1000.
Held FD, ponowne otwarcie i mapowanie pamięci wymagają oddzielnych dowodów.
Python VM 3.13.5 obsługuje `mmap(..., trackfd=False)`; po zamknięciu FD odczyt
`/proc` ma potwierdzić brak deskryptorów przy nadal żywej mapie. Przy rzeczywistym
`cache.files=off` odmowa mmap `ENODEV` oznacza niedostępny wariant, nie zaliczony
test blokowania. Dodatni zapis bezpośrednio do backing pliku jest kontrolą
zakresu FUSE, nie dowodem awarii readonly unii.

R1 `kaD6aK` na opublikowanym `fdf01ca5…` wykonało prepare, lecz local odmówiło
przed remount: raw `PermissionError 13 /proc/self/setgroups`, wtórny
`KeyError('before')`, journal `prepare/pending=actor-start`. Bez retry/global,
stan i dowody `SkOasH` zachowane. Dumpability r1 nie została wtedy zmierzona.
Nowy aktor po potwierdzeniu UID 1000/caps 0 ogranicza core dump do zera i tylko
przed userns ustawia/sprawdza dumpability 1. Handshake zapisuje etapy setup,
własność plików proc i pierwotną odmowę. Lokalny test wykonuje prawdziwy
fork/unshare i zapis mapowań: dumpability 0 daje EACCES, ustawienie 1 pozwala
przejść setup. Na hoście działa już UID 1000; podmienione jest tylko niedozwolone
bez roota `setgroups([])`, więc test nie dowodzi rzeczywistego root→1000 w VM.
Wymaga dostępnego nieuprzywilejowanego userns, bez fallbacku lub ukrytego skipa.

R2 potwierdziło już rzeczywisty setup root→1000/caps 0, dumpability 0→1,
mapowania UID/GID i RLIMIT_CORE 0. Local remount zakończył się kodem 0, lecz
normalny umount zwrócił EBUSY 16; przyczyna pozostaje w diagnozie. Główne
montowanie jest `ro`, superblock `rw`, oba aliasy `rw/rw`; held-write obu
aktorów dopisał po 5 B, reopen WRONLY/RDWR/TRUNC po 7 B, create odmówił 30.
Mmap ENODEV 19 jest nieprzetestowanym wariantem. Journal `prepare/pending=reopen`,
wszystkie dzieci `alive=false`; SHA/stat ośmiu plików zgodne z expected,
dodatkowe dyski puste. Nie ponawiać local ani nie resetować journala dla global.
W następnej rundzie zdiagnozować busy i na nowej VM wykonać odrębny pomiar
globalnego RO w ramach już zatwierdzonego eksperymentu service-mode. Ani A0, ani
E2-09 nie są zaliczone; izolacja bezpośrednich branchy nadal otwarta.

Krytyk niezależnie przyjął tylko częściowy dowód: **per-mount RO nie blokuje
zapisów globalnie**. Mount 71 ma ro/rw, bind 215 i userns 315 rw/rw; umount
errno 16 jest odmową syscalła mimo wrappera rc 0. Wszystkie osiem identity/SHA
i markery zgodne: held 4096 + 3×9 + 2×5 + 4×7 = 4161 B, truncate 7 B,
pozostałe 4096 B. Nie jest to odbiór całej local/umount. Kolejny krok to
diagnostyka referencji montowania i odrębny pomiar globalnego RO, bez resetu.

Późniejszy odczyt potwierdził `/ shared`. Propagacja przez rodzica może legalnie
uczestniczyć w EBUSY mimo prywatnej unii; nie utrwalono jednak parent mountinfo
dziecka przed umount, więc to hipoteza zgodna ze źródłami, nie identyfikacja
konkretnej referencji r2. Kolejna sonda ma oddzielić `perMountBypass` od
`umountOutcome` i zebrać parent mountinfo przed operacją; rc 1 r2 nie zmieniamy.
Podstawa: [Linux v6.12 pnode.c](https://github.com/torvalds/linux/blob/v6.12/fs/pnode.c)
i [mount_namespaces(7)](https://man7.org/linux/man-pages/man7/mount_namespaces.7.html).

Przed wykonaniem PM musi odebrać źródła, testy i piny. Błąd pozostawia dowody
i pending; bez automatycznego ponowienia, resetu lub sprzątania. Przykład testu
lokalnego, który nie montuje FUSE i nie dowodzi zachowania VM:

Nowy test A0 oraz pełny `unittest discover` infrastruktury wymagają także na
hoście Python **3.13 lub nowszego** ze wsparciem `trackfd=False` (lokalnie
sprawdzono 3.14.7). Wcześniejsze minimum 3.11 dla kontrolera VM nie wystarcza
do tego zestawu; brak API nie jest ukrywany przez fallback ani skip.

```bash
python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_writer_gate_probe.py -v
```

## Sonda A1 — prywatne branche, opublikowany FUSE

`guest_branch_isolation_probe.py` przygotowuje jednorazowe `preflight/run` na
VM `DpCJ1y`, UUID `77b5de74-031a-4d95-9e36-9d52b9e8ee96`, boot
`c32a49c0-a9b1-46a6-94a5-bc96242b9af5`; dowody `ByvrXm`. Opublikowany checkpoint
`9f7b9c7b…`: exact 244/244 PASS (1.616 s), push i osobny ls-remote zgodne.
Preflight/run kod 0, `completed=true/pending=null`; dispatch FINAL ACCEPT dotyczy tylko private-tmpfs/FUSE.
14/14 prób UID 1000 i własnego userns odmówiło errno 13 na ścieżkach direct/proc;
trasa open→setns nie dowodzi wywołania samego setns. Przy held FD global remount
odmówił EBUSY 16, po zamknięciu pisarzy RO kod 0 i 12/12 otwarć EROFS 30.
Root worker dopisał 13 B; PM potwierdził cztery identity/SHA (held 4138 B,
direct 4109 B, pozostałe 4096 B). Mmap ENODEV 19 pozostaje nieprzetestowane.
Pięć dzieci zakończonych, mergerfs PID 4345/start 129069/ns 4026532393 celowo
zachowany z przypiętym SHA: utrzymuje tmpfs device 53, publiczny FUSE device 54.
Pięć dodatkowych dysków pustych. To mały dowód tmpfs ≤8 MiB w prywatnym mount namespace,
bez mkfs dodatkowych dysków, transferu danych i unlink, nie architektura produktu.
Host otrzymuje wyłącznie FUSE przez jawny protokół deskryptora; aktorzy UID 1000
i userns mają odmawiać dostępu do branchy przez host path i żywe cele `/proc`
(root/fd/setns). Każdy cel i alias musi mieć pełny dowód tożsamości; błąd setup
nie jest dodatnim wynikiem izolacji. Po zamknięciu pisarzy globalne RO wymaga
odmów wszystkich trybów otwarcia trzech aliasów, a root worker dodatniego zapisu.

Raw i pending są trwałe przed potwierdzeniem/operacją. Bounded worker i aktorzy
kończą pracę; istniejący mergerfs pozostaje do odczytu PM, bez automatycznego
unmount/cleanup/retry. Tmpfs jest ulotny: crash/recovery oraz produkcyjny mover
nie są przetestowane. Przypięty SHA modułu A0 pozwala użyć istniejących helperów
bez zmiany jego pinów ani zamrożonych źródeł. Lokalne testy używają prawdziwych
plików, procesów, SCM_RIGHTS i journala; jawny adapter typu filesystemu i granicy
mount nie zastępuje osobnego odbioru VM. Python 3.13+ jak dla pełnego zestawu A0.

```bash
python3 -m unittest discover -s tests/infra/tentanas-vm -p test_guest_branch_isolation_probe.py -v
```

## A2.1 — nowe prywatne macierze

Helper 0.9 tworzy nowe macierze z journalem schema 2 i obowiązkową prywatną
topologią. Istniejące publiczne journale schema 1 pozostają bez migracji i bez
adopcji. `elastic_namespace.rs` izoluje branche; host otrzymuje wyłącznie
zweryfikowany FUSE. Kotwica wiąże boot/PID/start/namespace/binarkę/SHA256 i FUSE;
`sha2` służy pomiarowi binarki. Child zapisuje zamiar przed publikacją i kończy
go po ACK; parent tylko odczytuje journal. Inspect, Restore i Sync/Scrub używają
właściwej namespace oraz niezależnego pomiaru publikacji hosta, bez fallbacku.
Po zmianie boot Restore weryfikuje trwałe FSUUID i odtwarza prywatną topologię,
bez mkfs. Przy żywej kotwicy dopuszczony jest kontrolowany ponowny eksport tego
samego FUSE na rzeczywiście pusty cel, nie uruchomienie drugiego daemona.

Opublikowany checkpoint helpera `00c674c67004e8f1bb5329f0246b0d87af9de0ff`:
exact 163/163 testy helpera (0.06 s), release 0.9 (6.10 s), 262/262 core (11.17 s).
Clippy nadal 101, te same 11/13 diagnostyk co baza A2.0. PM sprawdził także
306/306 testów infrastruktury (2.348 s); osobny immutable harness ma 62/62 PASS
(0.614 s). To odrębne dowody od pomiarów VM.

`guest_private_lifecycle.py` wywołuje operacje macierzy publicznym helperem,
audyty wykonuje pod istniejącym SH/NB lockiem; osobno tworzy własne katalogi
testowe i payload. Prywatne manifesty SHA przypinają L
`c4a36ec3-7a92-4c61-96e3-77208b628721` (dwa XFS, parity 0) i P
`ca4b0b07-e238-4c5e-85a7-f049cfe9bfa2` (XFS + parity). Każda faza jest osobnym
wywołaniem `python3 guest_private_lifecycle.py PHASE STATION_JSON SHA256`.
Zamknięta kolejność to `preflight/create/inspect-created/payload`, następnie
L: `isolation/restore-live/inspect-live/reboot-checkpoint/restore-reboot/inspect-reboot`,
P: `sync/scrub/nochange`. Dwie fazy po restarcie wymagają dodatkowo
`BOOT_JSON BOOT_SHA256`, związanych ze starym receipt i rzeczywistym nowym boot; manifest
nie jest przepisywany. Limit 400 trwałych zdarzeń, 2 MiB na artefakt; brak retry,
kasowania historii i ręcznej zmiany journala produktu. Seed-ROM jest osobno
przypięty obok sześciu dysków, odczyt eksportuje wyłącznie SHA/metadane.

L: Create/Inspect, zapis UID 1000 przez FUSE, osiem odmów direct/proc oraz dwa
dodatnie otwarcia FUSE, Restore/Inspect w tym samym boot i pojedynczy normalny
reboot zakończone kodem 0. Restore po nowym boot także kod 0; PM potwierdził
wszystkie trzy SHA/FSUUID/inode/uid/mode/nlink/mtime. Końcowy osobny Inspect kod 0
odebrany przez krytyka: 293 zdarzenia, pending null, sześć wywołań helpera.
P: 17 MiB + 4 KiB, Sync, pełny Scrub 69/69 i no-change Sync
odebrane niezależnie; błędy 0/0/0, payload i parity SHA zachowane. Zmiany content
po Scrub/no-change są legalne. Odmowy tras open→setns nie dowodzą wykonania
samego setns. To nie UI/history, crash-recovery, mmap, service-inhibit ani mover;
copy/unlink niewykonane. Stare sondy/piny 0.8 i historyczna odmowa preflight r1
pozostają zachowane. Minimalny odbiór A2.1 L/P przyjęty przez PM i niezależnego
krytyka; A2/E2-09 pozostają otwarte. Nie testowano restartu core/unita cgroup.

## A2.0 — istniejące bramki Restore

Cztery regresje w `tentanas-helper/src/elastic.rs` sprawdzają rzeczywiście używane
`restore_checkpoint_guard` i `restore_mount_guard`: trwały Root save/drop/reopen,
Union w tym samym/nowym boot, Sync/Maintenance, brak pierwszego sync przy parity,
niepełne formatowanie, leniwe wywołanie sondy i propagację jej błędu. Odczyt
odmawia journala z pustym lub nieprawidłowym identyfikatorem boot; bajty pozostają niezmienione. Testy nie
uruchamiają pełnego Restore, inventory, mount ani ścieżki save→execute_steps.
Odtworzenie Union po zmianie boot pozostaje zamierzonym odzyskiwaniem montowania;
ten przyrost nie dodaje service inhibit ani prywatnego lifecycle i nie zamyka A2.
PM zweryfikował dokładny checkpoint źródeł/testów
`b9c8fcd1ed2733d4c463560ede7e567ac273988c`, opublikowany z osobnym potwierdzeniem remote.
`cargo test -p tentanas-helper --lib --locked` przez wspólny wrapper: 150/150 PASS,
kod 0, 0.03 s (`E2-09-A20-exact-tests.log`); build release kod 0, 7.87 s
(`E2-09-A20-exact-release.log`), binarka `--version` zwraca `0.8.0`.
Clippy nadal kod 101: 11 diagnostyk lib / 13 libtest, lista identyczna z bazą
`63b709e2…` (diff 0; logi `E2-09-A20-exact-clippy.log` i
`E2-09-A20-exact-baseline-clippy.log`). Brak nowych diagnostyk nie oznacza zielonego Clippy.
Nie wykonywano nowego pomiaru VM.

## Uruchomienie testów guardów

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/infra/tentanas-vm -v
```

Testy rzeczywiście tworzą małe QCOW2 w prywatnym katalogu tymczasowym i sprawdzają odmowę raw/backing/external data, podmiany inoda/seed/kluczy, nieprawidłowej tożsamości dysków i procesu. Nie bootują VM ani nie formatują systemów plików. Oddzielny odbiór operacyjny wymaga SSH, inventory oraz stop/start z zachowanym znacznikiem na OS. Historie nieudanych prób i finalne wyniki należą do raportu etapu.

Pierwszy odbiór V01 zachował trzy prywatne runtime, bez kasowania: `tentanas-vm.p1qFGt` — zatrzymana diagnostyka SMM; `tentanas-vm.CFz367` — zatrzymany bootstrap z ostrzeżeniem schemy cloud-init (`ssh_genkeytypes: []`); `tentanas-vm.ZQe92T` — finalny bootstrap z `[ed25519]`, cloud-init kod 0, ścisły SSH, sześć dysków i poprawny stop/start ze stałym SHA znacznika OS oraz zmienionym boot_id. Finalna VM pozostała uruchomiona do dalszych etapów; dwa wcześniejsze runtime nie są stanowiskami zaakceptowanymi. Nie nadpisywano ich seed/manifestu, aby udawać udany bootstrap.

## A2.2 — ręczny test trybu service Elastic

Odbiór wykonuje PM ręcznie na świeżej VM E2 przez istniejący `vm.py` oraz
produkcyjnego helpera 0.10. Nie dostarczono nowego harnessu do tego przebiegu;
odrzucone szkice `guest_service_mode.py` i jego testu pozostają poza repozytorium
i nie wolno ich uruchamiać ani publikować. Requesty intentu i surowe logi są
przechowywane w `/mnt/d/repos/tentanas-a22-proof.4qPnCD/`, a szczegółowy wynik
w `/mnt/d/repos/new_apps/reviews/E2-09-A22-wynik.md`.

Zakres obejmuje jedną macierz XFS z data1 32 GiB i bez parity. Operator sprawdza
Create, następnie trzymany FD pisarza powodujący `EBUSY`/16 przy próbie
EnterService Busy, jawne
wejście w Hold z globalnym RO, osiem odmów `EROFS`/30 dla publicznej ścieżki
i zwykłego aliasu, Restore odrzucony bez zmiany journala oraz Resume z
potwierdzonym RW. Po normalnym reboot Hold musi zostać zachowany; Inspect,
Restore odrzucony w Hold i jawny Resume są wykonywane jako osobne operacje.

Każda mutacja service ma niepowtarzalny request i osobny raw log; Inspect i
Restore mają stały request, który może być użyty w zaplanowanych kontrolach.
`pending` jest stanem journala helpera, nie dodatkowym stanem sondy.
Po nieoczekiwanej odmowie operator nie ponawia, nie resetuje VM i nie czyści
dowodów. Zaplanowana odmowa Busy pozostawia `Hold.pending=true`, a nowe jawne
Hold po zamknięciu FD jest częścią scenariusza. Test nie obejmuje nowego UI/API,
crash, cgroup, movera, copy/unlink ani SnapRAID.

Źródła odbioru wskazują commit `6417560b3aff5999715c801e99e4213bbcb79bf1`.
Wynik obejmuje 180 testów helpera, 267 testów core i 8 testów zewnętrznych,
wszystkie zakończone sukcesem. Clippy zakończył się kodem 101 z bazowymi
11 diagnostykami biblioteki i 13 testów biblioteki; nie jest zielony.
