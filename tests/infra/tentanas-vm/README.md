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

`start` potwierdza proces i QMP, nie gotowość SSH ani cloud-init. Pierwsze połączenie może odmówić przed uruchomieniem sshd; najpierw sprawdzić `status`, następnie ponowić odczyt. `inventory` wymaga dokładnego UUID VM i sześciu dysków o oczekiwanych serialach, rozmiarach i magistralach, z root wyłącznie na OS oraz bez systemów plików/partycji/mountów pozostałych ról. To kontrola pustego stanowiska V01, nie preflight późniejszej sformatowanej macierzy.

`ssh` zachowuje granice argumentów. Jeśli potrzebna jest składnia powłoki gościa, wywołać ją jawnie, np. `ssh RUNTIME sh -c 'id && uname -r'`. Nie używać tego interfejsu do danych produkcyjnych. Polecenia wykonywane są tylko w gościu po sprawdzeniu tożsamości procesu i klucza SSH.

`stop` wysyła ACPI powerdown przez własny QMP, czeka maksymalnie 90 s i nie wysyła kill. Kontroluje PID, starttime, argv, właściciela i UUID QMP. Zakończony zapisany proces można uzgodnić jako stopped; ponownie użyty PID powoduje odmowę. Legalny restart własnej zatrzymanej VM jest dozwolony, ponowny start działającej/obcej/niekompletnej nie. Przy timeout VM i stan pozostają do diagnostyki. Nie ma automatycznego destroy.

## Dyski, obraz i dostęp

2 vCPU, 4 GiB RAM; dwa zamknięte profile dysków QCOW2 thin:

| Profil | OS | data1 | data2 | parity | cache NVMe | spare |
|---|---:|---:|---:|---:|---:|---:|
| `storage` (domyślny) | 12 GiB | 1 GiB | 1 GiB | 2 GiB | 1 GiB | 1 GiB |
| `e2` | 12 GiB | 32 GiB | 32 GiB | 40 GiB | 1 GiB | 40 GiB |

`create --profile e2` tworzy wyłącznie nowe, puste stanowisko dla produkcyjnego
Elastic z niezmienionym `minfreespace=20G`; spare pozostaje fizyczną nazwą roli
i może zostać jawnie wybrany jako drugi parity w osobnym teście API. System oraz
data/parity/spare używają virtio. Są to nośniki funkcjonalne, nie benchmark sprzętu;
rozmiar logiczny QCOW2 nie gwarantuje dostępnego miejsca na hoście.

Manifest schema 1 pozostaje niezmieniony: profil wynika wyłącznie z dokładnej mapy
sześciu ról, seriali i rozmiarów, zgodnej z jedną z dwóch powyższych konfiguracji.
Nie ma dowolnych rozmiarów ani migracji manifestów istniejących VM. Lifecycle,
kontrola pustych dysków, ścisły SSH i pakiety działają dla obu profili. `storage`
oraz `detach-data2` odmawiają dla `e2` na hoście, przed SSH i zapisem intentu:
formatowanie oraz odbiór E2 należą do produkcyjnego API, nie `guest_storage.py`.

Wyłącznie manifest E2 ma losowy `api_port`, różny od portu SSH. Kanoniczne argv
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
storage, pobiera podpisane indeksy i archiwa pięciu pakietów z zależnościami.
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
na nowej VM r2 `PM0Ymz`, UUID `b1fa1b6e-bd25-41c9-aa6f-a8f2140544aa`, boot
`e26f6d28-942d-41e4-8c92-285c4d540d24`. To kandydat bramki wyłączności,
nie produkcyjna bramka ani mover. R2 ma przygotowane stanowisko, lecz pomiary A0
jeszcze niewykonane; prywatne dowody mają katalog `GgNLCE`.
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

## Uruchomienie testów guardów

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/infra/tentanas-vm -v
```

Testy rzeczywiście tworzą małe QCOW2 w prywatnym katalogu tymczasowym i sprawdzają odmowę raw/backing/external data, podmiany inoda/seed/kluczy, nieprawidłowej tożsamości dysków i procesu. Nie bootują VM ani nie formatują systemów plików. Oddzielny odbiór operacyjny wymaga SSH, inventory oraz stop/start z zachowanym znacznikiem na OS. Historie nieudanych prób i finalne wyniki należą do raportu etapu.

Pierwszy odbiór V01 zachował trzy prywatne runtime, bez kasowania: `tentanas-vm.p1qFGt` — zatrzymana diagnostyka SMM; `tentanas-vm.CFz367` — zatrzymany bootstrap z ostrzeżeniem schemy cloud-init (`ssh_genkeytypes: []`); `tentanas-vm.ZQe92T` — finalny bootstrap z `[ed25519]`, cloud-init kod 0, ścisły SSH, sześć dysków i poprawny stop/start ze stałym SHA znacznika OS oraz zmienionym boot_id. Finalna VM pozostała uruchomiona do dalszych etapów; dwa wcześniejsze runtime nie są stanowiskami zaakceptowanymi. Nie nadpisywano ich seed/manifestu, aby udawać udany bootstrap.
