# Konta aplikacji agentowych — obsługa i zakres

Stan implementacji: 2026-09-06. Zmiany są w lokalnym checkoutcie. Testy kodu, rzeczywistego GUI i przerwanego transferu między dwoma nodami przeszły na macOS. Próby nie obejmowały płatnych zapytań ani zakończonego logowania subskrypcyjnego; te wymagają rzeczywistych kont dostawców.

## Dodawanie i dostęp

W Services wybierz kategorię **Aplikacje agentowe**, dodaj serwis z katalogu i wskaż node. Każde dodanie tworzy odrębny UUID konta i prywatny profil, także gdy kilka serwisów korzysta z tego samego Codexa. Instalacja programu jest współdzielona według wersji.

Po wdrożeniu logowanie dotyczy dokładnie utworzonego serwisu. Administrator nadaje mu czytelną nazwę i w panelu **Konto agenta → Dostęp** wskazuje użytkowników. Grant pozwala używać konta; historia i zdarzenia poszczególnych sesji nadal należą do ich użytkowników. Konto bez jawnych grantów jest dostępne administratorom. Zarządzanie grantami oraz logowaniem wymaga administratora.

**Dostęp do konta należy nadawać wyłącznie zaufanym osobom.** CLI otrzymuje poświadczenie wybranego konta w prywatnym profilu lub środowisku. Kod wykonywany w tej sesji może je odczytać; ukrycie tokenu w GUI nie zapobiega jego skopiowaniu przez użytkownika uprawnionego do wykonywania kodu. Sandbox oddziela inne konta i projekty. Cofnięcie grantu zatrzymuje sesje i połączenia TentaFlow, ale nie unieważnia skopiowanego tokenu u dostawcy. Pełne unieważnienie wymaga operacji u dostawcy.

Jedno konto obsługuje jedną aktywną sesję naraz. Oddzielne konta mogą działać równolegle. Samo dodanie drugiego profilu w TentaFlow nie tworzy drugiej subskrypcji ani nie zwiększa limitu u dostawcy. Zamknięcie sesji zwalnia konto, a jawne wznowienie własnej historii ponownie sprawdza uprawnienia.

Obecne aplikacje: Codex CLI, Claude Code, Grok Build i Muse Code. Claude używa `setup-token`; ten tryb nie udostępnia Remote Control ani konektorów claude.ai. Stan „poświadczenie zapisane, niezweryfikowane” oznacza obecność pliku, a nie potwierdzenie ważnego abonamentu. Antigravity CLI (`agy`) nie jest oferowane jako zarządzane konto do czasu potwierdzenia izolacji systemowego keyringu.

## Praca w istniejącym katalogu

Administrator tworzy workspace Code Studio, wybiera katalog lokalny i podaje jego absolutną ścieżkę **na wybranym nodzie**. Przykładowo `/Users/critix/repos/rust/TentaFlow` wskazuje oryginalne pliki, bez kopiowania do innego projektu. Inny użytkownik uzyskuje dostęp przez członkostwo tego workspace’u; nie może podmienić ścieżki sesji na dowolny katalog hosta.

Ten wariant wymaga zwykłego repozytorium Git z własnym katalogiem `.git`. Zewnętrzne metadane linked worktree, dowiązania w metadanych Git, konfiguracja Git dołączająca pliki z zewnątrz i hardlinki prowadzące poza przydział są odrzucane. Dopuszczone wewnętrzne hardlinki muszą należeć w całości do tej samej klasy dostępu. Zastąpienie katalogu innym obiektem wymaga ponownej rejestracji przez administratora.

Uruchamiana binarka TentaFlow i jej prywatne dane muszą znajdować się poza przyznawanym projektem. Dotyczy to również pracy nad samym TentaFlow: uruchom instalację spoza repozytorium, zanim nadasz agentowi zapis do repozytorium. Agent nie może dostać prawa nadpisania działającego brokera ani jego kluczy.

Kliknięcie workspace’u otwiera własny czat. Wejście i odświeżenie strony nie rozpoczyna zapytania do modelu. W nowej sesji wybierz konkretne konto z noda projektu; ten wybór nie przełączy się automatycznie na płatny klucz API organizacji. Zmiana konta wymaga nowej sesji. Katalog i node są widoczne w pustym czacie.

Na katalog lokalny przypada jedna otwarta sesja pisząca TentaFlow. Aby zacząć kolejną, zamknij poprzednią. Zewnętrzny edytor nie respektuje tej blokady. Usunięcie workspace’u usuwa zarządzane dane TentaFlow, zachowując oryginalne repozytorium, pliki śledzone i nieśledzone. Agent ma dostęp do plików projektu; zapis metadanych `.git` odbywa się przez kontrolowane operacje Git w Code Studio.

## Przeniesienie konta

Dla Codex i Claude wybierz **Konto agenta → Przenieś konto**, zamknij aktywne sesje i wskaż sparowany, obsługiwany node. Administrator oraz użytkownicy mający granty muszą być znani nodowi docelowemu. Sprawdzenie celu odbywa się przed zamrożeniem źródła.

Przeniesienie jest operacją wyłączną: źródło blokuje nowe sesje, cel przygotowuje konto, źródło trwale rezygnuje z uruchamiania, a dopiero potem cel aktywuje konto. Zakończone źródło usuwa kanoniczne poświadczenie; zachowane prywatne profile historii pozostają zablokowane przed uruchomieniem. Transfer nie unieważnia tokenów u dostawcy. Zapisany przebieg pozwala ponowić operację po zerwaniu połączenia lub restarcie; nie przywraca automatycznie drugiej aktywnej kopii na źródle. W panelu można odświeżyć stan i ponowić transfer do tego samego celu.

Transfer obejmuje kanoniczne poświadczenie, UUID, nazwę i granty. Nie przenosi aktywnego procesu, rozmów, projektów ani niezapisanych plików. Grok i Muse nie udostępniają jeszcze przenoszenia poświadczeń. Tożsamość profilu TentaFlow nie jest dowodem, że użytkownik nie zalogował tego samego konta dostawcy w dwóch różnych profilach.

Po zakończeniu aktywacji można przenieść odebrane konto dalej albo z powrotem. Panel wymaga jawnego wybrania kolejnego celu. Powrót na poprzedni node wykorzystuje ten sam UUID konta; nie tworzy kolejnej niezależnej subskrypcji.

## Granice obecnej walidacji

Sandbox nie wymaga Dockera ani osobnego systemu gościa. macOS używa Seatbelt i supervisora opartego na launchd; potrzebuje dostępnej domeny `gui/<uid>`. Wykorzystuje również niepubliczne API odczytu przynależności procesów do coalition, więc dostępność jest sprawdzana przy uruchomieniu. Linux ma ścieżkę bubblewrap dla pracy bez sieci, ale profil sieciowy kont agentowych nie jest jeszcze udostępniony; Windows nie deklaruje gotowej izolacji. Manifesty zarządzanych aplikacji są obecnie ograniczone do macOS.

Prywatny HOME nie dziedziczy kluczy, ustawień i historii operatora. Narzędzia projektu muszą być dostępne w dopuszczonym zakresie systemowym; sandbox nie udostępnia automatycznie całego `~/.cargo`, `~/.rustup` ani innych osobistych katalogów narzędzi. Dostępność konkretnego kompilatora lub SDK wymaga osobnej kontroli przed uruchomieniem buildu projektu.

Kod projektu może modyfikować własny profil sesji. Z tego powodu zmieniony token jest odkładany do weryfikacji i nie zastępuje automatycznie kanonicznego loginu. W takim przypadku kolejne użycie może wymagać ponownego logowania. Przezroczysty refresh i ponowne użycie ważnej subskrypcji po przeniesieniu wymagają testów z rzeczywistymi kontami.

Granica sandboxa dotyczy procesów uruchamianych przez TentaFlow. Nie chroni przed wrogim procesem działającym poza nim na tym samym użytkowniku systemowym; taki proces może zmieniać pliki i dowiązania na hoście. Nie jest to VM ani limit zasobów CPU/RAM.

Szczegółowe źródła i wyniki prób: [kontrakty dostawców](provider-cli-contracts.md), [walidacja sandboxa](process-sandbox-validation.md), [analiza i plan](agent-accounts-code-studio-plan.md).
