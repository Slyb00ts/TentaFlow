# Dokończenie sandboxa procesowego na Linuxie

## Zadanie

Dokończ i przetestuj na rzeczywistym Linuxie sandbox procesowy dla Code Studio oraz zarządzanych kont CLI. Zachowaj wspólny kontrakt z macOS. Przejdź do implementacji i testów; sam plan lub odblokowanie flag platformy nie wystarcza. Docker i mikro-VM nie są wymaganym runtime tej funkcji. Windows i integracja Antigravity nie należą do tego zadania.

Przed pracą przeczytaj `AGENTS.md` oraz:

- `docs/agent-accounts-operations.md` — aktualne działanie, uprawnienia i ograniczenia.
- `docs/process-sandbox-validation.md` — wykonane testy; pierwsza próba SRT jest historyczna, bieżący runner nie używa SRT.
- `docs/provider-cli-contracts.md` — rzeczywiste kontrakty CLI i ograniczenia logowania.
- `docs/agent-accounts-code-studio-plan.md` — plan docelowy; część analityczna opisuje stan sprzed implementacji.

## Co już istnieje

- Osobne UUID kont, prywatne profile i historie, uprawnienia użytkowników, jedna aktywna sesja na konto, blokady OS i wspólne instalacje według wersji.
- Administracyjne przypisanie istniejącego katalogu Git do workspace'u, kontrola tożsamości katalogu i członkostwa, osobne ścieżki exec, PTY i CLI. Czat jest domyślnym widokiem.
- Gateway z uwierzytelnieniem i polityką dostawcy, odebranie dostępu zatrzymujące sesje, odzyskiwanie gatewaya po awarii Core oraz trwałe przenoszenie bezczynnych kont Codex/Claude.
- Na macOS działa Seatbelt i supervisor launchd. Testy obejmowały odłączone procesy potomne, SIGKILL, dwa konta, dwa nody, przerwanie transferu, powrót konta i ponowny start obu kont naraz.
- Linux ma ścieżkę bubblewrap bez sieci. Samo istnienie `/usr/bin/bwrap` nie jest wystarczającą kontrolą możliwości. Transport gatewaya i pełny nadzór cyklu życia procesów wymagają dokończenia oraz testów.

## Miejsca w kodzie

| Obszar | Pliki |
|---|---|
| Wspólna polityka i wrapper OS | `tentaflow-containers/agents/native/process_sandbox.rs` |
| Wzorzec gwarancji supervisora macOS | `tentaflow-containers/agents/native/macos_supervisor.rs` |
| Core: exec, PTY, katalogi, cleanup | `tentaflow-core/src/code_studio/{sandbox.rs,location.rs,exec/,terminal.rs,git_broker.rs,cli_bridge.rs}` |
| Gateway, dostęp i transfer kont | `tentaflow-core/src/services/{coding_agent_proxy.rs,coding_agent.rs,account_move.rs,supervisor.rs}` |
| Instalacja i start kont | `tentaflow-core/src/services/deploy/{managed_cli.rs,binary.rs,mod.rs}` |
| Procesy i adaptery CLI | `tentaflow-containers/agents/native/coding-agent-bridge/src/` |
| Dostępność silników w GUI | `tentaflow-containers/agents/_services/{codex,claude-code,grok-build,muse-code}.toml` |

W `process_sandbox.rs` sprawdź szczególnie `Policy::check_available`, `Policy::with_proxy`, `Policy::wrap`, `ensure_quiescent`, `supervisor_root` i linuksowe `platform_command`. Obecne `--unshare-all` odcina sieć, więc hostowy adres `127.0.0.1` gatewaya nie może być traktowany jako gotowy transport do odizolowanej sesji. `coding_agent_proxy::start` celowo odrzuca obecnie inne systemy niż macOS. Instalator Grok/Muse ma zweryfikowane artefakty tylko dla macOS; same zmiany manifestów tego nie naprawią.

## Wymagany wynik implementacji

1. Zbadaj faktyczne możliwości hosta: dystrybucję, kernel, architekturę, user namespaces, bubblewrap, dostępny sposób nadzoru potomków i delegację cgroup, jeśli wybierzesz ten mechanizm. Wykonaj próbę uruchomienia polityki. Brak wymaganej izolacji ma dawać czytelny błąd przed startem procesu.
2. Doprowadź wszystkie ścieżki do tej samej izolacji: exec, PTY, logowanie CLI, odczyt modeli i sesje agentów. Prywatny HOME/środowisko, dokładny katalog projektu, jawne zakresy odczytu/zapisu, ochrona `.git` i stanu TentaFlow. Nie udostępniaj całego HOME operatora ani osobistych katalogów narzędzi jako sposobu naprawienia buildu.
3. Zapewnij kontrolowany transport do istniejącego gatewaya. Dobierz i sprawdź na tym hoście sposób przekazania połączenia przez granicę namespace'u. Nie otwieraj całej sieci hosta przez `--share-net`; nie wystawiaj gatewaya bez uwierzytelnienia. Zachowaj politykę egress, ochronę innych usług lokalnych i odcięcie aktywnych połączeń po odebraniu dostępu.
4. Zapewnij zakończenie całego drzewa procesów, także po podwójnym fork, `setsid`, zamknięciu PTY, Stop, anulowaniu i SIGKILL Core/bridge. Samo zabicie grupy procesów nie jest dowodem cleanupu. Wykorzystaj sprawdzony na tym Linuxie mechanizm namespace/supervisora lub delegowanej cgroup. Zajętość konta i katalogu wolno zwolnić dopiero po potwierdzonym sprzątaniu. Zachowaj trwały zamiar startu i ochronę przed ponownym użyciem PID.
5. Uruchom rzeczywiste CLI w prywatnej instalacji. Dla Grok/Muse zweryfikuj oficjalne linuksowe artefakty, rozmiary i SHA-256. Włącz Linux w manifestach oraz kontrolach backendu dopiero dla faktycznie działających silników i architektur. GUI ma pokazywać rzeczywistą dostępność; używaj istniejących komponentów `tf-*`.
6. Sprawdź dwa konta tego samego dostawcy i dwa nody na jednym hoście. Zachowaj oczekiwanie na współdzieloną instalację, rozpoznawanie nodów po identyfikatorze oraz trwałe adresy po restarcie. Nie twórz równoległej implementacji zarządzania kontami ani osobnego protokołu transferu.

## Testy akceptacyjne

- Dozwolony projekt: odczyt/zapis w dokładnym katalogu. Obcy projekt/profil: odmowa również przez `..`, symlink i hardlink. Odrzuć hardlink poza przydziałem i alias `.git` w zapisywalnej części projektu; sprawdź również próbę utworzenia aliasu po starcie sandboxa.
- Brak odziedziczonych tokenów, agentów SSH, nieprzydzielonych socketów i konfiguracji operatora. Wybrane poświadczenie CLI pozostaje świadomie dostępne kodowi tej sesji; nie deklaruj ukrywania go przed uprawnionym użytkownikiem.
- Rzeczywista sieć: dozwolone połączenie przez gateway; odmowa bezpośredniego TCP/UDP, obejścia proxy, dostępu do innych portów hosta i prywatnych usług. Sprawdź DNS oraz zarówno IPv4, jak i IPv6, jeśli są dostępne.
- Rzeczywisty terminal: odczyt, zapis, resize, zamknięcie. Potomkowie po fork/setsid mają zniknąć po Stop i awarii; procesy drugiego konta mają pozostać żywe.
- Anulowanie podczas tworzenia sesji/instalacji nie może uruchomić jej z opóźnieniem. Po awarii Core nowy bridge musi mieć działający gateway nowego Core.
- Dwa przypięte konta startują razem, zachowując UUID, granty i prywatne profile. Użytkownik bez grantu nie widzi cudzej historii ani nie używa konta.
- Dwa prawdziwe nody: transfer Codex/Claude, przerwanie i wznowienie, powrót konta, brak dwóch aktywnych kopii. Sprawdź automatyczne ponowne połączenie po restarcie bez ponownego parowania.
- Zamknięcie/usunięcie workspace'u zachowuje oryginalne repozytorium, zmienione pliki śledzone i pliki nieśledzone.
- Zmierz koszt startu i pamięć na testowanym hoście. Oddziel koszt instalacji CLI od kosztu uruchomienia gotowego sandboxa.

Uruchom odpowiednie testy Core (`cargo test --lib ...`), pełny build głównej aplikacji oraz build produkcyjnego bridge'a (`cargo build --release` w jego crate). Nie ma nadrzędnego workspace Cargo. Testy istniejące pod `cfg(target_os = "macos")` nie potwierdzają Linuxa; dodaj rzeczywiste linuksowe próby i nie raportuj pominiętych testów jako sukcesu. Zaktualizuj dokumentację oraz wynik kontroli GUI.

Domyślnie używaj sztucznych poświadczeń i oddzielnych profili testowych. Prawdziwe logowanie, rotacja tokenów i wywołanie modelu są osobnym etapem z kontami wskazanymi przez użytkownika. Nie kopiuj istniejących profili operatora. Po testach usuń uruchomione procesy, mounty/namespaces i listenery testowe, zachowując raporty.

Nie obniżaj ochrony macOS, nie dodawaj nieizolowanego trybu awaryjnego, atrap ani deklaracji wsparcia bez testów. Zakończ raportem: co działa na tej dystrybucji/architekturze, dowody testów, ograniczenia i polecenia odtworzenia.
