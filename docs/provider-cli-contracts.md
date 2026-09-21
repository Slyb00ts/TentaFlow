# Kontrakty nowych aplikacji agentowych

Stan pomiarów: 2026-09-06, macOS arm64; Antigravity CLI ponownie zmierzono 2026-09-20 na wersji 1.2.7 i na tej podstawie zamknięto integrację jako świadomie pominiętą (sekcja Antigravity). Kontrakty wykorzystano do implementacji adapterów Grok Build (ACP) i Muse Code (MSP). Pomiary bez konta nie potwierdzają działania abonamentu ani zakończonego logowania. Nie użyto kont użytkownika ani kluczy API. Procesy testowe miały prywatne HOME, zablokowaną sieć oraz odczyt katalogu domowego operatora i usługę Keychain. Nie uruchamiano instalatorów modyfikujących profil powłoki.

## Zweryfikowane artefakty

| Aplikacja | Wersja wykonana lokalnie | Dystrybucja | Wynik bez konta |
|---|---|---|---|
| Antigravity CLI `agy` | 1.1.27 (2026-09-06); 1.2.7 (2026-09-20) | Oficjalny manifest platform `darwin_arm64`, `linux_amd64`, `linux_arm64`; archiwa SHA512 zgodne | `--version`, `--help` działają; niezalogowana tura headless kończy się kodem 1 z czytelnym błędem |
| Grok Build `grok` | 1.0.13, commit 5e9a58528b76 | Oficjalny kanał stable, binarka macos-aarch64 133 486 016 B | Help, login help, ACP initialize działają |
| Muse Code `muse` | 1.0.3-R2198.1 | Oficjalny kanał muse-stable, manifest publiczny, binarka 241 984 144 B, SHA256 zgodne | Help, eksport schematu MSP, initialize, session/start oraz obsługa błędu turn/completed działają |

Źródła dystrybucji: [Antigravity installer](https://antigravity.google/cli/install.sh), [Grok installer](https://x.ai/cli/install.sh), [Muse installer](https://dev.meta.ai/install.sh), [Muse launcher](https://api.meta.ai/muse-launcher.sh).

Instalator agy akceptuje `--dir`, a od 1.2.7 dokumentuje także `--skip-aliases` i `--skip-path` (oba dotyczą profilu powłoki, nie uwierzytelniania); nie publikuje natomiast builda musl, mimo że binarka 1.2.7 obsługuje `linux_amd64` i `linux_arm64`. Jego `case` rozpoznaje wyłącznie Darwin i Linux, choć komunikat błędu wymienia Windows. Nie należy odtwarzać instalacji przez wykonywanie treści dokumentacyjnego przykładu. Installer Grok potrafi odczytać istniejące `~/.grok/auth.json`; zarządzany instalator TentaFlow powinien pobierać wybrany artefakt bez czytania kont operatora. Launcher Muse sam aktualizuje binarkę: zarządzany runtime powinien uruchamiać konkretną zweryfikowaną wersję.

## Antigravity

[Headless](https://antigravity.google/docs/cli/headless/) opisuje trwały proces:

```text
agy --input-format stream-json --output-format stream-json
```

Runner ustawia cwd na przyznany projekt. Wejście NDJSON ma postać `{"event":"user","message":{"content":"..."}}`. Parser obsługuje `init` z conversation_id, `step_update.step_update.text_delta`, a zakończenie tury `result.result.status` i response. Liczniki usage i num_turns w result są kumulatywne, więc metryki tury wymagają różnicy między kolejnymi wynikami. `--conversation <id>` wznawia konkretną rozmowę; nie używać globalnego `--continue`. Zamknięcie stdin kończy proces po bieżącej turze. Od 1.1.27 przybyły m.in. `--mode accept-edits|plan`, `--sandbox`, `--print-timeout`, `--project`/`--new-project`, demon `remote-control` oraz `mcp`/`plugin`/`update`.

**Bloker uprawnień (potwierdzony na 1.2.7, ostrzejszy niż w 1.1.27):** protokół odrzuca `control_request` i `control_response` z kodem 2 (zero wystąpień obu literałów w binarce), więc nie istnieje zdarzenie zatwierdzenia, na które klient mógłby odpowiedzieć. Narzędzie wymagające zgody jest w trybie headless automatycznie odrzucane, a tura **kończy się kodem 0** — binarka wypisuje własny komunikat o auto-odmowie. Zielony przebieg z pominiętą pracą jest gorszy niż brak silnika: zaciemnia stan sesji, wynik przebiegu i analitykę, więc nie wolno takiego sygnału wprowadzać do platformy. Jedyne dźwignie to statyczne reguły `permissions.allow` (`command(git)`, `command(regex:…)`, `write_file(src/)`) albo `--dangerously-skip-permissions`, którego używać nie wolno.

**Bloker wielu loginów (mechanizm rozpoznany):** binarka linkuje `zalando/go-keyring`, czyli Apple Keychain / Secret Service / Windows Credential Manager, kluczowany parą (service, user), bez pojęcia profilu. Ani `--help`, ani [Installation & Auth](https://antigravity.google/docs/cli/install/) nie opisują wyboru konta, profilu ani katalogu konfiguracji; takiego przełącznika nie ma też wśród ~30 zmiennych `AGY_*`/`ANTIGRAVITY_*`/`GEMINI_*`. Poświadczenia nie ma więc w pliku, który moglibyśmy utrzymywać per konto, rozsyłać na nody i odświeżać. Logowanie jest wyłącznie interaktywne (TUI: „Select login method:", „Enter the authorization code:"), bez podkomendy i bez flagi. Prywatne HOME poprawnie izoluje pliki (`$HOME/.gemini/antigravity-cli/`, `$HOME/Library/Caches/` — nic nie wycieka do katalogów operatora), ale nie izoluje keyringu. Niezalogowana tura headless kończy się kodem 1 z `{"status":"ERROR","error":"authentication failed or timed out"}`, czyli porażka jest wykrywalna. [Settings](https://antigravity.google/docs/cli/settings/) umieszcza ustawienia pod `~/.gemini/antigravity-cli/settings.json`; klucze `permissions` i `modelProvider` są rzeczywiste. Wariant API wymaga równocześnie `GEMINI_API_KEY` i `modelProvider=gemini`, tworzy sesję bez konta i nie jest równoważny subskrypcji.

**Katalog nie oferuje `agy` z powodu tych dwóch blokerów.** Model kont wymaga, żeby poświadczenie dało się trzymać per konto (globalne albo użytkownika), rozesłać na nody i odświeżyć, a każde wywołanie narzędzia przechodziło przez politykę uprawnień. `agy` nie spełnia żadnego z tych warunków: konto jest jedno na maszynę i przypisane do jej użytkownika (dwóch użytkowników Code Studio na jednym node dzieliłoby jedną subskrypcję), nie ma czego przenieść ani odświeżyć, a poza stałą allowlistą praca jest pomijana przy zielonym wyniku. Izolacja keyringu na Linuksie (prywatna sesja dbus z własnym keyringiem) mogłaby dać wiele kont na jednej maszynie, ale nie usuwa blokera zatwierdzeń, który jest rozstrzygający.

**SDK Antigravity — `google-antigravity` 0.1.17 (Alpha, Apache-2.0, Python ≥3.10, `pip install google-antigravity`)** ma obie brakujące rzeczy: polityki `deny("*")` / `allow(...)` / `ask_user(..., handler=…)` z handlerem po naszej stronie oraz poświadczenie w pliku lub zmiennej (`GEMINI_API_KEY`, `api_key=`, ADC). Wheele pokrywają `macosx_11_0_arm64`, `manylinux_2_17_x86_64`/`aarch64`, `musllinux` (od 0.1.14) i Windows. **Został jednak wykluczony decyzją właściciela z 2026-09-20: rozlicza przez Gemini API / Vertex AI, a nie subskrypcję Antigravity.** Dodatkowo jest to Python ze skompilowanym runtime w paczce (adapter wymagałby sidecara), a wersja 0.1.x jest Alpha. Nie traktować tego SDK jako zamiennika `agy` — to inny produkt i inny model rozliczenia.

Integracja wróci do rozważenia dopiero wtedy, gdy Google doda w `agy` kanał zatwierdzeń w trybie headless albo wybór profilu uwierzytelnienia (pliku poświadczenia).

## Grok Build

Preferowany transport: ACP JSON-RPC przez stdio, osobny proces i GROK_HOME dla instancji:

```text
grok --no-auto-update agent --no-leader stdio
```

Rzeczywisty `initialize` z protocolVersion=1 zwrócił loadSession, sessionCapabilities.list/resume/close, modelState z modelami, currentWorkingDirectory oraz agentVersion=1.0.13. Bez konta jedyną authMethods pozycją było `grok.com`; nie należy wpisywać na stałe `cached_token` na podstawie przykładu dokumentacji.

Sekwencja adaptera: initialize → wybór oferowanego authMethod → authenticate → session/new(cwd,mcpServers) → session/prompt(sessionId,prompt tekstowy). Odpowiedź session/prompt i wiadomości session/update są odrębnymi kanałami. Adapter musi obsłużyć również żądania uprawnień od serwera i odmówić nieobsługiwanych reverse RPC. Nie reklamować fs/terminal capability, jeśli klient nie wykonuje tych operacji. Resume/load/close dobierać z negocjowanych możliwości.

[Enterprise](https://docs.x.ai/build/enterprise) oraz instrukcja dołączona do binarki potwierdzają GROK_HOME (domyślnie ~/.grok), auth.json, automatyczne odświeżanie i `grok login --device-auth`. Istnieje `auth_provider_command`: stdout zwraca token albo JSON access_token/refresh_token/expires_in, a GROK_AUTH_EXPIRED=1 oznacza ciche odświeżanie. To możliwy punkt integracji brokera kont. Login i refresh nadal wymagają prawdziwego konta do testu.

[Headless/ACP](https://docs.x.ai/build/cli/headless-scripting) nie zastępuje pomiaru wersji. Na 1.0.13 `--session-id` tworzy NOWĄ sesję i odrzuca istniejące UUID; wznowienie wymaga `--resume`, a fork `--fork-session`. Jest to istotna różnica względem skróconej tabeli WWW.

## Muse Code

Najlepszy punkt integracji ujawnia sama binarka: `muse serve`, stały host MSP przez JSON-RPC stdio. Umożliwia interaktywne zatwierdzenia, podczas gdy `muse exec --json` jest powierzchnią jednorazową.

```text
muse schema generate-json-schema --out <katalog-kontraktu>
muse serve
```

Schemat jest generowany offline z tej konkretnej binarki. Zweryfikowany fingerprint stabilnej powierzchni: `sha256:03312c213efd14277a0e0a102f70adeae497a469ca4edf7242f479953ed758b7`.

Kontrakt minimalnego adaptera:

- initialize: clientInfo.name i version; następnie notyfikacja initialized.
- session/start: commandId **UUIDv7**, workspaceRoot; wynik session.sessionId i viewCursor. Ten sam commandId jest kluczem idempotencji.
- session/resume: commandId, sessionId; session/fork ma własny kontrakt z punktami przecięcia historii.
- turn/start: commandId UUIDv7, sessionId, input=[{type:"text",text:...}]. Ack oznacza przyjęcie, nie ukończenie.
- item/delta oraz item/completed dostarczają wyjście; turn/completed zawiera stan terminalny i błąd.
- turn/interrupt: commandId, sessionId i preferowane jawne turnId.
- approval/decide: commandId, sessionId, approvalId, **bieżące requirementId** i choiceId z availableChoices. To zatwierdzenie wieloetapowe; nie mapować do samego bool.
- model/list: zapytanie o modele, bez commandId.

Handshake i utworzenie sesji wykonano bez konta. Próba tury zwróciła poprawną notyfikację `turn/completed` z terminal=failed i informacją o braku loginu. Choć session/start akceptuje providerId=echo w metadanych, MSP w pomiarze nadal żądał loginu. Osobny `muse exec --provider echo --json` zwrócił deterministyczny wynik offline; to test formatu, nie test modelu ani subskrypcji.

Prywatne katalogi zgodnie z plikami pomocy dostarczonymi przez binarkę: XDG_CONFIG_HOME/muse (settings.json, auth.json, trust.json) i XDG_DATA_HOME/muse (sesje, indeks, bundled skills). Initialize potwierdził prywatny museHome. [Auth](https://dev.meta.ai/docs/muse-code/auth?locale=en_US) oraz `muse login --help` potwierdzają login kodem w przeglądarce i nadrzędność META_API_KEY nad browser credential. [Subscriptions](https://dev.meta.ai/docs/muse-code/subscriptions) odróżnia specjalne poświadczenie subskrypcyjne od API PAYG.

Sandbox hosta jest ustalany przy `serve`; nie można go negocjować przez wire. Zewnętrzny sandbox TentaFlow musi obejmować także hooks/MCP. Nigdy nie dodawać --yolo jako sposobu dopasowania adaptera. [Extending](https://dev.meta.ai/docs/muse-code/extending?locale=en_US) opisuje te powierzchnie i różnicę między wynikiem procesu a wynikiem testów projektu.

## Artefakty pomiarowe

Lokalne pliki w `/tmp/tf-provider-probe/`: help każdej binarki, grok-initialize.txt, muse-schema/{manifest.json,msp.schema.json}, muse-msp-events.json oraz skrypty probe.py/acp.py/msp.py. Zawierają wyłącznie syntetyczne sesje bez poświadczeń. Nie są częścią runtime produkcyjnego. Pomiar Antigravity 1.2.7 z 2026-09-20 wykonano w katalogu tymczasowym z prywatnym HOME, bez logowania i bez użycia kont operatora; katalog usunięto po odczytaniu wyniku, więc po tym pomiarze nie zachowano artefaktów ani żadnego poświadczenia.

Grok Build i Muse Code mają adaptery, instalatory przypiętych wersji z SHA256 oraz manifesty macOS. Testy adapterów obejmują prawdziwe procesy bez konta i odmowę niezalogowanej tury Muse. Rozpoczęcie logowania kodem pod sandboxem potwierdziło dokładne domeny auth.x.ai i auth.meta.com; nie ukończono logowania. Stan samego pliku poświadczenia jest oznaczany jako niezweryfikowany. Zmienione przez proces poświadczenie nie zastępuje automatycznie centralnego konta: wymaga ponownego zweryfikowania tożsamości/logowania. Antigravity nie jest oferowane i zostało świadomie pominięte (sekcja Antigravity): `agy` nie trzyma poświadczenia w pliku per konto — keyring `zalando/go-keyring` kluczuje parą (service, user) i nie ma wyboru profilu — a w trybie headless nie istnieje kanał zatwierdzeń, więc narzędzie wymagające zgody jest automatycznie odrzucane przy zielonym kodzie wyjścia. Wariant SDK wykluczył właściciel, bo rozlicza przez Gemini API / Vertex AI, a nie subskrypcję.

## Wynik wdrożenia adaptera MSP

Adapter `muse.rs` korzysta ze wspólnego transportu JSON-RPC oraz produkcyjnego sandboxa procesowego i supervisora launchd. Wykonano bez konta rzeczywisty `model/list`, initialize, session/start, turn/start i notyfikację błędu turn/completed, następnie potwierdzono zakończenie procesu. Próby używają pustych prywatnych XDG_CONFIG_HOME/XDG_DATA_HOME i portu proxy bez listenera. Test nie potwierdza płatnej tury ani logowania subskrypcyjnego.

Testy protokołu potwierdzają unieważnienie poprzedniego identyfikatora zatwierdzenia przy zmianie requirementId i odmowę zamiany jednorazowej zgody na regułę persistent/session. Adapter wybiera wyłącznie jednoznaczną, zaoferowaną przez serwer decyzję `approved`, `denied` lub `abort` z `scope=once`. Nieobsługiwane pytanie strukturalne jest anulowane przez `userInput/cancel` z jawnym powodem; nie dostaje wymyślonej odpowiedzi użytkownika. Wznowienie sesji sprawdza zgodność workspaceRoot z bieżącym przyznanym katalogiem.

## Domena inicjowania logowania

Dodatkowa ograniczona próba `muse login` i `grok --no-auto-update login --device-auth` użyła produkcyjnego Seatbelt/supervisora oraz lokalnego proxy wymagającego losowego hasła. Proxy przekazywało TLS wyłącznie do jawnie wymienionych domen. Zaobserwowane CONNECT: Muse `auth.meta.com:443`; Grok `auth.x.ai:443`. Obie aplikacje doszły do instrukcji kodu urządzenia. Nie otwierano przepływu przeglądarkowego ani nie zatwierdzano żadnego konta; procesy zakończono z potwierdzonym cleanup. Grok wypisał również informację o niepowodzeniu, więc pomiar nie dowodzi pełnego logowania. Nie zapisywano ani nie publikowano kodów urządzenia. Domeny dalszego odświeżania tokenów i płatnych wywołań nadal wymagają testu z kontem.


Grok macOS x86_64 pobrano z oficjalnego adresu wersji: 149 694 528 B, SHA256 `8eacec87f5ecdb9259c6d812d12ce9e2d405b1526e36ae9d7fc81ec31dbd74d6`. Nie uruchomiono binarki Intel na fizycznym Macu Intel. Sumy Grok zostały przypięte po pobraniu przez HTTPS; nie są podpisem wydawcy. Muse sprawdzono dodatkowo względem SHA256 w oficjalnym manifeście.

Końcowy test adaptera Grok 1.0.13 wykonał ACP initialize i odczyt oferowanych modeli przez rzeczywisty sandbox oraz supervisor, po czym potwierdził cleanup. Wynik: 1 test zakończony powodzeniem w 3,33 s. Profil nie zawierał `auth.json`, a port proxy nie miał listenera. Pięć testów wspólnego transportu RPC sprawdza m.in. timeout obejmujący zapis i odpowiedź oraz usunięcie oczekującego żądania po anulowaniu.
