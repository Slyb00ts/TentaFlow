# Walidacja sandboxa procesowego i kont agentów

Data: 2026-09-06. Pierwsza część opisuje historyczną próbę SRT, wykonaną przed zmianami produkcyjnego runnera. Bieżąca implementacja jest natywna. Jej wyniki są w sekcjach [Końcowe testy kodu](#końcowe-testy-kodu) i [Integracja dwóch nodów i rzeczywisty interfejs](#integracja-dwóch-nodów-i-rzeczywisty-interfejs); ograniczenia opisano również w [instrukcji obsługi](agent-accounts-operations.md).

## Środowisko i odtworzenie

- macOS 26.6.2 ARM64, Node 26.8.1.
- `@anthropic-ai/sandbox-runtime` 0.0.75, zainstalowany wyłącznie w katalogu tymczasowym, bez skryptów instalacyjnych.
- Rzeczywisty Codex CLI 0.153.4, uruchomiony tylko z `--version`. Bez logowania, odczytu istniejącego profilu ani zapytania do modelu.
- Syntetyczne katalogi dwóch użytkowników, osobny HOME i profile aplikacji. Żaden projekt użytkownika nie był celem prób zapisu.

```sh
npm install --prefix /private/tmp/tentaflow-process-sandbox-probe --ignore-scripts --no-audit --no-fund @anthropic-ai/sandbox-runtime@0.0.75
python3 scripts/test-process-sandbox.py \
  --runtime-dir /private/tmp/tentaflow-process-sandbox-probe/node_modules/@anthropic-ai/sandbox-runtime \
  --report /private/tmp/tentaflow-process-sandbox-report.json \
  --codex /Users/critix/.local/bin/codex
```

Test trzeba uruchomić poza ograniczeniami zewnętrznego sandboxa narzędzia, żeby sprawdzać docelowy mechanizm OS. Parametr `--codex` jest opcjonalny. Skrypt dopuszcza Linux, ale nie wykonano na nim testów. Windows jest jawnie niedopuszczony przez ten skrypt.

## Wynik

16 z 18 kontroli zakończyło się powodzeniem. Liczba obejmuje kontrole pozytywne i pomiary startu, więc nie jest procentową miarą bezpieczeństwa. Cały test kończy się kodem 1.

| Próba | Wynik |
|---|---|
| Odczyt i zapis we wskazanym projekcie; zmiana widoczna bezpośrednio na hoście | Przechodzi |
| Zapis do prywatnego HOME, uruchomienie Node z odczytem projektu | Przechodzi |
| Odczyt obcego projektu przez ścieżkę absolutną, `..` i symlink | Zablokowany |
| Zapis poza projektem, również przez symlink | Zablokowany |
| Proces potomny Node odczytuje obcy projekt | Zablokowany |
| Bezpośrednie TCP do działającego listenera localhost | Zablokowane |
| `kill(pid, 0)` wobec własnego testowego procesu poza sandboxem | Zablokowane |
| Codex `--version` w prywatnym profilu | Przechodzi |
| Odczyt istniejącego hardlinku w projekcie do pliku drugiego użytkownika | **Dozwolony — wymaganie niespełnione** |
| Zapis przez ten hardlink | **Dozwolony — wymaganie niespełnione** |

Hardlink udostępnia ten sam obiekt plikowy pod dozwoloną nazwą. Ograniczenie ścieżek nie rozpoznaje pochodzenia zawartości. Potrzebny jest kontrakt dopuszczania katalogów i przeciwdziałania zmianom aliasów podczas sesji. Ostrożny skan `st_nlink > 1` może odrzucić również prawidłowe hardlinki wewnętrzne; to świadomy koszt konserwatywnej polityki, a nie pełne rozwiązanie wyścigów. Zaufanie do administratora przy rejestracji katalogu nie usuwa ryzyka jego późniejszej modyfikacji przez inne procesy hosta.

W pojedynczej próbce `/usr/bin/true` startował około 2,5 ms bez wrappera i 189 ms przez CLI SRT. To pomiar uruchomienia całego wrappera Node/proxy/sandbox, nie koszt pojedynczego procesu potomnego w istniejącej sesji. Nie zmierzono RAM, warm start, pracy agenta ani porównania z Dockerem. Rozmiar rozpakowanej paczki npm to 8 530 589 bajtów; nie obejmuje Node i całej dystrybucji zależności.

## Ograniczenia i warunki integracji

Polityka prób odmawia odczytu `/`, następnie dopuszcza katalog projektu, prywatny profil i systemowy toolchain. Domyślna szeroka możliwość odczytu SRT nie jest odpowiednia dla izolacji użytkowników. Obecna testowa lista obejmuje m.in. `/usr`, `/private/etc` oraz instalacje Homebrew i wymaga zawężenia/przeglądu przed produkcją; nie jest polityką do bezpośredniego skopiowania do runnera. Shell zgłaszał brak dostępu do `/private/var/select/sh`, choć polecenia działały; ten przypadek wymaga osobnej kontroli zgodności.

Test TCP nie potwierdza wszystkich kanałów sieci/IPC, a sygnał 0 nie potwierdza całej izolacji procesów. Do wykonania pozostają m.in. Unix sockets, Keychain, odziedziczone deskryptory, dostęp do innych profili, rzeczywisty terminal PTY, odłączanie potomków, limity zasobów, cleanup, sieć dostawcy i niezmienność polityki wobec hooks/MCP. Uruchomienie `--version` nie dowodzi działania sesji Codex, logowania ani jego wewnętrznego sandboxa.

Przyjęty kierunek: sandbox procesowy, najpierw walidacja macOS, następnie Linux i Windows po uzyskaniu środowiska. SRT pozostaje kandydatem; test nie uzasadnia jeszcze udostępnienia go jako produkcyjnej granicy między użytkownikami. Integracja musi objąć exec, PTY oraz osobną ścieżkę CLI bridge, z prywatnym stanem konta i odmową startu przy braku izolacji.

## Natywna implementacja po próbie SRT

Dalsza implementacja nie używa SRT. Wspólna polityka `tentaflow-containers/agents/native/process_sandbox.rs` obsługuje Core i CLI bridge: macOS Seatbelt oraz Linux bubblewrap. Windows odmawia wykonania. Proces otrzymuje jawne środowisko, prywatny HOME i wskazany katalog projektu. Sieć macOS dopuszcza tylko port uwierzytelnionego gatewaya; Linux nie uruchamia profilu z gatewayem bez wymaganego mechanizmu przekazania ruchu. Usunięcie uprawnienia wyłącza gateway wraz z aktywnymi połączeniami.

Zaufana instalacja CLI i startowy odczyt `--version` działają poza sandboxem sesji. Startowy odczyt dziedziczy prywatne środowisko konta i katalog instalacji bridge; nie wykonuje poleceń projektu. Operacje logowania, modeli i sesji używają właściwego wrappera. Nie należy utożsamiać izolacji pracy projektu z izolacją instalatora ani twierdzić, że każde uruchomienie binarki dostawcy jest sandboxowane.

macOS uruchamia każdy proces przez osobne zadanie launchd i identyfikuje jego resource coalition. Członkostwo przetrwało w rzeczywistych próbach podwójny fork i setsid; zwykłe zabicie grupy procesów i samo bootout zadania tego nie gwarantują. Worker przekazuje standardowe deskryptory, przejmuje PTY i usuwa potomków także po zabiciu frontendowego procesu. Przed sygnałem ponownie sprawdza coalition i czas narodzin PID. Powtarza enumerację do potwierdzenia braku żywych potomków. Stan dopuszczenia pozostaje zajęty, jeśli cleanup nie został dowiedziony. Ta kontrola nie stanowi atomowego sygnalizowania tożsamości procesu; pomiędzy ostatnim sprawdzeniem PID a `kill` pozostaje wyścig ponownego użycia PID.

Mechanizm odczytu coalition opiera się na macOS SPI (`PROC_PIDCOALITIONINFO`), a nie stabilnym publicznym kontrakcie Apple. Sprawdza rozmiar odpowiedzi i dostępność domeny `gui/<uid>`; nie uruchamia procesu przy braku wymaganej obsługi. Nie jest obecnie rozwiązaniem dla bezgłowego macOS bez tej domeny. Pliki handoff, token i marker znajdują się poza wszystkimi katalogami przyznanymi sandboxowi. Zamiar startu jest zapisany przed spawn; tylko potwierdzony błąd spawn pozwala anulować pusty zamiar. Runtime zapisuje ścieżkę tego stanu w bazie, aby restart nie usuwał plików pod żywym wykonaniem.

Wykonane na tym macOS testy natywnego modułu: odmowa hardlinków poza przydziałem i socketów; ograniczenie odczytu/zapisu oraz potomków; izolacja read-only; dostęp tylko do konkretnego portu proxy; dziedziczony PTY; PTY przekazany przez launchd; usunięcie odłączonego potomka po normalnym zakończeniu i SIGKILL frontendu. Testy supervisora używają rzeczywistego launchd i automatycznie budowanego, testowego hosta; nie wymagają konta dostawcy ani zewnętrznego pliku wykonywalnego. Osobno sprawdzono rzeczywistą implementację Core `open_pty`, odczyt `stty size` i zmianę rozmiaru terminala.

Granica zaufania nadal wyklucza wrogie procesy działające poza sandboxem na tym samym koncie systemowym. Skan hardlinków przed dopuszczeniem katalogu nie blokuje wyścigów zmian wykonywanych przez takie procesy. Nie jest to izolacja zasobów CPU/RAM ani VM. Nie wykonano testów runtime Linux ani Windows na docelowych maszynach. Pierwotne dwa niepowodzenia SRT pozostają odnotowane powyżej i nie należy raportować tamtej próby jako zielonej.

## Konta agentów i prywatne profile

Rekord serwisu wskazuje osobny UUID konta. Core przechowuje lokalny token IPC w prywatnym katalogu konta, a nie w publicznym `config_json`. Zwykłe operacje sesji sprawdzają aktywność użytkownika, grant do konta i właściciela konkretnej sesji. Odebranie grantu zamyka sesje tego użytkownika. Dodatkowy monitor sprawdza aktywność i uprawnienie co 500 ms; Code Studio oddzielnie sprawdza członkostwo projektu i stan sesji. To maksymalny typowy interwał wykrycia, nie synchroniczne zatrzymanie w chwili zapisu zmiany w bazie. Potwierdzenie zamknięcia wymaga zakończonego cleanupu procesów.

Jedno konto ma wyłączną blokadę OS na proces bridge oraz jedną aktywną sesję. Każda sesja otrzymuje osobny HOME, cache i historię. Kopiowane jest wyłącznie wybrane poświadczenie, bez profilu CLI, historii, hooks ani konfiguracji poprzedniego użytkownika. Konsola w Services ma prywatny katalog roboczy; istniejący projekt uruchamia się przez autoryzowany katalog Code Studio. Zamkniętej sesji nie można uruchomić ponownie przez `session.turn`; wznowienie przechodzi przez pełne `session.create`, kontrolę właściciela i blokadę konta.

Grant użycia konta jest delegacją jego poświadczenia zaufanemu użytkownikowi. Kod wykonywany w profilu sesji może odczytać plik uwierzytelnienia albo zmienną środowiska CLI. Obecna izolacja nie ukrywa tego wybranego poświadczenia przed użytkownikiem wykonującym kod; chroni inne profile i zasoby. Odebranie grantu wyłącza wykonanie i gateway TentaFlow, lecz nie odwołuje tokenów ani sesji u dostawcy. Ochrona przed wyniesieniem wybranego tokenu wymaga osobnego, wspieranego przez dostawcę kontraktu uwierzytelniania i walidacji rozdzielenia CLI od jego narzędzi.

Zmodyfikowane poświadczenie z katalogu sesji **nie zastępuje automatycznie centralnego poświadczenia konta**. Plik jest dostępny dla procesu projektu, więc nawet zgodny `account_id` w JSON nie jest kryptograficznym dowodem tożsamości. Zmieniony materiał trafia do prywatnego `pending-credential.json`; konto wymaga ponownego logowania przed następną sesją. Przezroczyste odświeżanie wymaga zweryfikowanego u dostawcy kontraktu tożsamości i testów z prawdziwym kontem. Z tego powodu nie można jeszcze deklarować trwałego „jednego logowania” przy rotacji tokenów ani automatycznej mobilności kont pomiędzy nodami.

Claude używa udokumentowanego `claude setup-token` i `CLAUDE_CODE_OAUTH_TOKEN`, z prywatnym plikiem `setup-token.json`. Token drukowany przez CLI jest przechwytywany do pliku 0600; surowe wyjście tej procedury nie trafia do zdarzeń GUI. Nie kopiujemy systemowego Keychain. Ten tryb subskrypcyjny nie obsługuje Remote Control ani konektorów claude.ai; nie deklaruje również API odczytu limitów. Prawdziwe logowanie, zakres konta i wywołanie modelu czekają na konta przekazane przez użytkownika.

Bez kont i bez płatnych wywołań wykonano prawdziwe Codex 0.153.4 (`--version`, app-server initialize oraz cleanup), Claude Code 2.1.258 (`--version`) i Muse 1.0.3-R2198.1 (MSP initialize, sesja, jawny błąd braku logowania i cleanup). Skrypt `scripts/test-agent-bridge.py` sprawdza rzeczywisty serwer HTTP bridge: health, brak/błędny token IPC, pusty profil i blokadę drugiego bridge na tym samym koncie. Testy syntetyczne sprawdzają oddzielenie historii, niedopuszczenie niezweryfikowanego refresh do centralnego konta, blokadę hardlinków, prywatne przechwycenie tokenu Claude oraz rollback nieudanego startu. To nie zastępuje testów abonamentu, OAuth ani wielonodowego transferu.

Skan dopuszczenia wykonuje pełny spis `(dev, ino)` i liczy nazwy każdego pliku. Hardlinki wewnątrz projektu są dopuszczone tylko wtedy, gdy liczba znalezionych nazw równa się `st_nlink` i wszystkie należą do tej samej klasy dostępu. Nazwy pod `.git` są liczone oddzielnie, więc alias metadanych w zapisywalnym projekcie jest odrzucany. Symlinki nie zwiększają licznika. Przed dopuszczeniem ponownie sprawdzane są tożsamość, liczba linków, rozmiar, tryb i znaczniki czasu plików/katalogów. Przejście kontroli nie dopuszcza dalszych zmian przez zewnętrzny proces hosta. Testy obejmują dwa prawidłowe aliasy wewnętrzne, trzeci link poza projektem, alias `.git` i symlink do linku zewnętrznego.

Osobna próba wykonała `/bin/ln` już po dopuszczeniu i uruchomieniu sandboxa: z obcego pliku do projektu oraz z `.git/config` do zapisywalnego aliasu w projekcie. Obie operacje zostały odrzucone przez OS (`EPERM`), aliasy nie powstały, zawartość źródeł pozostała niezmieniona, a cleanup obu supervisorów został potwierdzony. Nie zmienia to ograniczenia dotyczącego wrogiego procesu poza sandboxem.

Przenoszenie bezczynnego konta Codex/Claude jest realizowane jako trwała operacja administracyjna między zaufanymi nodami. API `account.move` przyjmuje wyłącznie docelowy `node_id`; `account.move.status` zwraca etap i błąd ostatniej próby. Profil projektu, historia, środowisko i pliki robocze nie wchodzą do transferu. Kopiowane są wyłącznie kanoniczne poświadczenie oraz identyfikator konta, nazwa i przydziały użytkowników; użytkownicy muszą już istnieć na nodzie docelowym. Materiał jest przesyłany prywatną komendą mesh, której `Debug` nie ujawnia treści, i nie trafia do bazy sagi ani odpowiedzi GUI.

Źródło najpierw zapisuje barierę blokującą sesje, cel przygotowuje zamrożony runtime, następnie źródło trwale rezygnuje z wykonania. Dopiero wtedy cel aktywuje konto. Po potwierdzeniu źródło usuwa poświadczenie i zatrzymuje runtime. Zerwane połączenie nie przywraca automatycznie źródła: zapisany etap jest ponawiany co pięć sekund oraz po restarcie. Powtórzona aktywacja starszego transferu jest odrzucana, a zakończona aktywacja nie cofa późniejszej ręcznej pauzy administratora. Zmiana nazwy, uprawnień i cyklu życia podczas trwającej operacji jest blokowana. Odwołanie zaufania albo uprawnień administratora zatrzymuje dalszą aktywację.

Testy bez kont dostawców obejmują bariery bridge oraz opóźnione rozpoczęcie anulowanej sesji. Testy sagi wstrzykują awarię stagingu, utratę potwierdzenia wyłączenia źródła, utratę potwierdzenia aktywacji celu i przerwane sprzątanie. Test integracyjny dwóch sparowanych procesów mesh opisano niżej; ponowne użycie ważnej subskrypcji na celu nadal wymaga rzeczywistego konta dostawcy. Nie jest to przezroczyste przenoszenie aktywnej sesji; odświeżone przez nieufny proces poświadczenie nadal wymaga weryfikacji opisanej wyżej. Grok/Muse nie mają włączonego transferu, dopóki przenośność ich poświadczeń nie zostanie potwierdzona.

## Końcowe testy kodu

Wyniki na macOS ARM64 z 2026-09-06:

| Zakres | Wynik |
|---|---|
| Core `code_studio` — katalogi, granty, sesje, sandbox, PTY, cleanup | 565 testów przeszło, 0 pominiętych |
| Core `delegate_cli` — również Stop podczas tworzenia sesji i odrzucenie spóźnionego startu | 19 testów przeszło, 0 pominiętych |
| Core `coding_agent` — autoryzacja i profile kont | 9 testów przeszło |
| Core `account_move` — wznowienie aktywacji, odrzucenie starego transferu i blokada podczas instalacji | 4 testy przeszły |
| Wdrożenie konta — trwały UUID, blokada OS i ponowienie po nieudanym przygotowaniu | 1 test przeszedł |
| Repozytorium usług — również zwolnienie zakończonej blokady wdrożenia | 9 testów przeszło |
| Migracje Core — świeża baza, klucze obce, mapowanie identyfikatorów | 35 testów przeszło |
| Zmiana hasła początkowego — blokada operacji i sprawdzenie bieżącego hasła | 1 test przeszedł |
| Tożsamość TLS / instalator CLI / proxy kont | Odpowiednio 6 / 3 / 3 testy przeszły |
| Supervisor — odtworzenie gatewaya po restarcie oraz blokada spóźnionej kontroli zdrowia | 2 testy przeszły |
| Rejestr nodów — trwałe adresy, scalanie, unieważnianie i granice partii | 32 testy przeszły |
| Parowanie i porządkowanie kontaktów | 10 testów przeszło |
| Odkrycie odrębnego noda na tym samym IP | 1 test przeszedł |
| Bridge — profile, transfer, protokoły, procesy | 40 testów przeszło; 4 próby wymagające jawnych ścieżek do CLI uruchamiano oddzielnie |

Końcowy `cargo build` głównej aplikacji przeszedł po poprawkach migracji i interfejsu. Nie uruchamiano całego zestawu testów wszystkich niezależnych crate’ów. W buildzie pozostają wcześniejsze ostrzeżenia repozytorium oraz ostrzeżenie linkera o rozmiarze sekcji unwind. Wyniki nie zastępują testu zalogowanej subskrypcji ani testów Linux/Windows.

Rzeczywista instalacja z GUI wykryła osobny błąd kompilacji produkcyjnego bridge’a: funkcja sprawdzająca proces była nadal ograniczona przez `cfg(test)`, mimo że używała jej kontrola tożsamości PID. Po usunięciu tego ograniczenia `cargo build --release` bridge’a przeszedł w 27,71 s, a trzy testy ochrony tożsamości PID ponownie przeszły. Przebudowano również aplikację z poprawionym źródłem instalatora; sam pozytywny wynik testów jednostkowych nie był dowodem poprawnej instalacji.

Kolejna próba instalacji ujawniła konflikt npm: prywatna konfiguracja użytkownika i globalna wskazywały ten sam plik. Po rozdzieleniu plików rzeczywiste `npm install @openai/codex@0.153.4` z pustym prywatnym HOME i jawnym środowiskiem zakończyło się powodzeniem w 5 s; plik wykonywalny został zainstalowany. Nie uruchamiano logowania ani zapytania do modelu.

## Integracja dwóch nodów i rzeczywisty interfejs

Na dwóch oddzielnych testowych profilach TentaFlow uruchomiono z GUI/API dwa serwisy Codexa z odrębnymi UUID i profilami. Instalator zbudował produkcyjny bridge oraz zainstalował przypięty Codex. Nie inicjowano logowania; okno postępu zamknięto istniejącą akcją pracy w tle przed automatycznym otwarciem loginu.

Nody sparowano przez rzeczywiste uwierzytelnione API. HTTPS pozostał na loopback, a połączenie mesh korzystało z prywatnego adresu LAN tego komputera. Początkowo ustawienie relay z bazy nadpisywało konfigurację testu; poprawiono je przez API. Nie należy opisywać całej próby jako wykonanej bez skonfigurowanego zewnętrznego relay.

Do prywatnego profilu A zapisano wyłącznie sztuczne poświadczenie. Przeniesienie uruchomiono w panelu konta, a proces źródłowego Core zakończono po utrwaleniu `source_frozen`. Cel zachował `target_staged` bez aktywacji. Po ponownym uruchomieniu źródła operacja osiągnęła `source_complete`, a cel `target_active` z `activation_complete=1`. Sprawdzono zgodność UUID, dokładną zawartość syntetycznego poświadczenia na celu i brak kanonicznego pliku poświadczenia na źródle. B zachował swój niezależny UUID.

Zwykły użytkownik zalogowany przez rzeczywiste GUI uzyskał przez podpisane przekazanie mesh `can_use=true` dla przeniesionego A, `can_manage=false`, odmowę użycia B i wyłączonego źródłowego A. Próba odczytu przydziałów obu kont została odrzucona jako wymagająca administratora. Poświadczenia nie były odczytywane przez ten test uprawnień.

Powrotne przeniesienie uruchomiono również przez rzeczywisty panel konta. Odebrane konto początkowo nie miało wybranego celu; przycisk stał się dostępny po jawnym wyborze pierwotnego noda. Operacja zakończyła się `source_complete`, przywróciła poprzedni rekord usługi z tym samym UUID i grantem, przeniosła dokładne sztuczne poświadczenie oraz usunęła je z opuszczanego noda. Konto B pozostało niezmienione. Końcowe zrzuty mobilne sprawdzono niezależnie: brak obciętych treści, czytelne etapy i poprawny stan przycisków. Dowody: `return-after.json`, `final-onward-selected-mobile.png`, `final-return-complete-mobile.png` w katalogu raportu.

Oddzielny test `SIGKILL` ujawnił żywy bridge B z nieczynnym proxy poprzedniego Core. Poprawiona aplikacja zatrzymała osierocony bridge i uruchomiła nowy z działającym gatewayem. Ponowiono rzeczywisty `SIGKILL` już na poprawionym buildzie: stare proxy przestało działać, a restart ponownie utworzył nowy proces i dostępne proxy. Sprawdzano wyłącznie port prywatnego gatewaya; nie wykonywano zapytań do dostawcy. Testy supervisora sprawdzają także zachowanie ręcznie zatrzymanego konta i odrzucanie spóźnionego wyniku kontroli zdrowia podczas odtwarzania. Test proxy potwierdza, że monitor starego procesu nie usuwa wpisu nowego gatewaya. Wszystkie 19 testów końcowego zestawu cyklu życia, transferu, repozytorium usług i proxy przeszło. Raporty i zrzuty z tej lokalnej próby zapisano w `/tmp/tf-mobility-e2e/`.

Po wyłączeniu rzeczywistego relay test ujawnił gubienie bezpośrednich adresów sparowanego noda. Aktualizacja samej nazwy mogła zastąpić cały zestaw adresów zarówno w rejestrze pamięciowym, jak i w SQLite. Poprawka scala poznane adresy, wymaga jawnego unieważnienia ich rodzaju i odrzuca usuwanie przez przestarzały zapis. Stan noda i jego adresy trafiają teraz do kolejki w jednej operacji, dzięki czemu granica partii nie może ich rozdzielić.

Ponowienie na rzeczywistej binarce wykryło drugą przyczynę: pipeline uznawał inny node korzystający z tego samego adresu IP za własny node. Usunięcie go z rejestru kasowało także trwałe adresy. Dwie instancje testu mają odrębne tożsamości i porty na jednym hoście, więc ten warunek był błędny. Usunięto rozpoznawanie własnego noda po IP, zachowując kontrolę identyfikatora.

Build obu poprawek przeszedł. Po jednorazowym ponownym parowaniu oba nody zachowały adresy drugiej instancji. Następnie zrestartowano oba procesy bez ponownego parowania ani ręcznego podawania kontaktów. Adresy przetrwały restart, a rzeczywiste `MeshNodeList` po obu stronach pokazało `connected`, `p2p`, `lan` i prawidłowe porty drugiego noda. Testowy nieczynny relay pozostał skonfigurowany. Dowody: `contacts-before-restart-fixed.json`, `contacts-after-restart-fixed.json`, `automatic-reconnect-fixed.json`. Końcowa kompilacja testów przeszła; wszystkie 46 testów rejestru, parowania, wspólnego IP i instalatora zakończyło się powodzeniem, bez pominięć.

Kontrola obu przypiętych kont po restarcie wykryła konflikt wspólnej instalacji CLI: natychmiastowe `try_lock` odrzucało drugie konto podczas równoczesnego sprawdzania gotowego cache. Instalator czeka teraz asynchronicznie na blokadę OS, maksymalnie 300 sekund. Rzeczywiste błędy I/O są nadal zgłaszane od razu; anulowane oczekiwanie nie pozostawia zablokowanego wątku, który mógłby później przejąć instalację. Dwa testy dokładnego kodu blokady przeszły, obejmując kolejność dostępu, timeout i anulowanie. Pełny build przeszedł. Po ponownym uruchomieniu oba przypięte konta osiągnęły stan `running`, miały różne PID, niezmienione identyfikatory i profile oraz dwa działające prywatne gatewaye. Dowody: `concurrent-warm-start-fixed.json`, `final-concurrent-A-gateway.json`, `final-concurrent-B-gateway.json`.

Po ostatnim restarcie zwykły użytkownik nadal mógł używać wyłącznie przydzielonego konta A; B i wycofana kopia na drugim nodzie pozostały niedostępne. Oba testowe Core zakończono przez `SIGTERM`. Kontrola sprzątania nie znalazła pozostałych prywatnych procesów Core, bridge ani supervisorów; oba porty HTTPS i gatewaye były zamknięte. Raport `cleanup.json` potwierdza wynik. Zachowano dowody i sztuczne katalogi testowe, bez uruchomionych usług.
