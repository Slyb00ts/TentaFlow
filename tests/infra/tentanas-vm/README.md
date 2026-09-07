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

2 vCPU, 4 GiB RAM; system 12 GiB thin, data1/data2 po 1 GiB, parity 2 GiB, cache 1 GiB jako NVMe, spare 1 GiB. System oraz data/parity/spare używają virtio. Są to małe nośniki na korpus funkcjonalny i metadane FS, nie pomiar szybkości sprzętu.

Profil jest jawny: `q35,accel=kvm,smm=off`, CPU `host`, bez automatycznego fallback do TCG. Na stanowisku przygotowania domyślne SMM powodowało reset gościa przed załadowaniem kernela; wyłączenie SMM wyłącznie w tej VM pozwoliło uruchomić kernel. Nie oznacza to diagnozy błędu hostowego KVM ani naprawy firmware; nie jest też testem SMM/Secure Boot. Profil trafia do manifestu i jest kontrolowany przy użyciu runtime. Zwykły reboot gościa jest dozwolony.

Obraz [Debian 13 generic amd64 20260831-2587](https://cloud.debian.org/images/cloud/trixie/20260831-2587/debian-13-generic-amd64-20260831-2587.qcow2) ma stały URL i SHA512 w kontrolerze. Suma pochodzi z oficjalnego datowanego `SHA512SUMS` przez TLS. Zaufanie nie jest oparte na podpisie PGP: Debian opisuje brak podpisanych sum bieżących cloud images. Pobieranie wymaga HTTPS również po przekierowaniu; niezgodny SHA512 blokuje nawet qemu-img. System po konwersji nie ma backing, podobnie jak pozostałe QCOW2; external data file jest zabroniony. Przed każdym startem sprawdzane są inody, rozmiary i format obrazów oraz hash seed.

Runtime mode 700 zawiera wszystkie obrazy, manifest, klucze klienta/serwera SSH, seed, znany klucz hosta, QMP i log serial. Klucze/seed/obrazy nigdy nie trafiają do Git ani raportów. Guest host key jest generowany przed bootem i podawany przez cloud-init, więc pierwsze połączenie również ma `StrictHostKeyChecking=yes`, bez ssh-keyscan/TOFU. Hasła i root SSH zablokowane. NOPASSWD dotyczy wyłącznie nowego konta `tentanas` **wewnątrz gościa**.

Sieć `restrict=on,ipv6=off` nie daje wyjścia do hosta/LAN/Internetu; wyjątek to jawny forwarding `127.0.0.1:port → guest:22`. Bez bridge/tap, hostfs, 9p/virtiofs, USB/PCI/physical disk passthrough i agent/X11 forwarding. Późniejsza instalacja pakietów wymaga osobnego, jawnego etapu, nie działa z domyślnie zamkniętym egress.

## Testy guardów

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/infra/tentanas-vm -v
```

Testy rzeczywiście tworzą małe QCOW2 w prywatnym katalogu tymczasowym i sprawdzają odmowę raw/backing/external data, podmiany inoda/seed/kluczy, nieprawidłowej tożsamości dysków i procesu. Nie bootują VM ani nie formatują systemów plików. Oddzielny odbiór operacyjny wymaga SSH, inventory oraz stop/start z zachowanym znacznikiem na OS. Historie nieudanych prób i finalne wyniki należą do raportu etapu.

Pierwszy odbiór V01 zachował trzy prywatne runtime, bez kasowania: `tentanas-vm.p1qFGt` — zatrzymana diagnostyka SMM; `tentanas-vm.CFz367` — zatrzymany bootstrap z ostrzeżeniem schemy cloud-init (`ssh_genkeytypes: []`); `tentanas-vm.ZQe92T` — finalny bootstrap z `[ed25519]`, cloud-init kod 0, ścisły SSH, sześć dysków i poprawny stop/start ze stałym SHA znacznika OS oraz zmienionym boot_id. Finalna VM pozostała uruchomiona do dalszych etapów; dwa wcześniejsze runtime nie są stanowiskami zaakceptowanymi. Nie nadpisywano ich seed/manifestu, aby udawać udany bootstrap.
