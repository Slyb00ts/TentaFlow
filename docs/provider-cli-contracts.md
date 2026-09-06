# Kontrakty nowych aplikacji agentowych

Stan pomiarów: 2026-09-06, macOS arm64. Kontrakty wykorzystano do implementacji adapterów Grok Build (ACP) i Muse Code (MSP). Pomiary bez konta nie potwierdzają działania abonamentu ani zakończonego logowania. Nie użyto kont użytkownika ani kluczy API. Procesy testowe miały prywatne HOME, zablokowaną sieć oraz odczyt katalogu domowego operatora i usługę Keychain. Nie uruchamiano instalatorów modyfikujących profil powłoki.

## Zweryfikowane artefakty

| Aplikacja | Wersja wykonana lokalnie | Dystrybucja | Wynik bez konta |
|---|---|---|---|
| Antigravity CLI `agy` | 1.1.27 | Oficjalny manifest platformy `darwin_arm64`; archiwum 49 187 973 B, SHA512 zgodne | `--version`, `--help` działają |
| Grok Build `grok` | 1.0.13, commit 5e9a58528b76 | Oficjalny kanał stable, binarka macos-aarch64 133 486 016 B | Help, login help, ACP initialize działają |
| Muse Code `muse` | 1.0.3-R2198.1 | Oficjalny kanał muse-stable, manifest publiczny, binarka 241 984 144 B, SHA256 zgodne | Help, eksport schematu MSP, initialize, session/start oraz obsługa błędu turn/completed działają |

Źródła dystrybucji: [Antigravity installer](https://antigravity.google/cli/install.sh), [Grok installer](https://x.ai/cli/install.sh), [Muse installer](https://dev.meta.ai/install.sh), [Muse launcher](https://api.meta.ai/muse-launcher.sh).

Instalator agy aktualnie akceptuje `--dir`, ale nie opisane w dokumentacji `--skip-aliases`. Nie należy odtwarzać instalacji przez wykonywanie treści dokumentacyjnego przykładu. Installer Grok potrafi odczytać istniejące `~/.grok/auth.json`; zarządzany instalator TentaFlow powinien pobierać wybrany artefakt bez czytania kont operatora. Launcher Muse sam aktualizuje binarkę: zarządzany runtime powinien uruchamiać konkretną zweryfikowaną wersję.

## Antigravity

[Headless](https://antigravity.google/docs/cli/headless/) opisuje trwały proces:

```text
agy --input-format stream-json --output-format stream-json
```

Runner ustawia cwd na przyznany projekt. Wejście NDJSON ma postać `{"event":"user","message":{"content":"..."}}`. Parser obsługuje `init` z conversation_id, `step_update.step_update.text_delta`, a zakończenie tury `result.result.status` i response. Liczniki usage i num_turns w result są kumulatywne, więc metryki tury wymagają różnicy między kolejnymi wynikami. `--conversation <id>` wznawia konkretną rozmowę; nie używać globalnego `--continue`. Zamknięcie stdin kończy proces po bieżącej turze.

**Bloker uprawnień:** protokół odrzuca `control_request` i `control_response` z kodem 2. Nie jest zgodny z kanałem zatwierdzeń Claude. Polecenie wymagające zatwierdzenia jest w trybie headless odmawiane, mimo że proces może zakończyć się kodem 0. Nie wolno zastępować brakującej obsługi zatwierdzeń flagą `--dangerously-skip-permissions`.

**Bloker wielu loginów:** [Installation & Auth](https://antigravity.google/docs/cli/install/) opisuje natywny keyring systemu i login SSH z kodem. [Settings](https://antigravity.google/docs/cli/settings/) umieszcza ustawienia pod `~/.gemini/antigravity-cli/settings.json`, lecz prywatne HOME nie dowodzi osobnej tożsamości w systemowym Keychain. Nie potwierdzono eksportu/przypisania browser credential do oddzielnego konta TentaFlow. Wariant API wymaga równocześnie GEMINI_API_KEY i modelProvider=gemini, nie jest równoważny subskrypcji.

Minimalny adapter tekstowy jest technicznie możliwy. Pełne udostępnienie subskrypcji wielu użytkownikom pozostaje zablokowane do potwierdzenia izolacji loginu i wybranego kontraktu narzędzi/zatwierdzeń. Binarne help nie udostępnia flagi wyboru profilu uwierzytelnienia.

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

Lokalne pliki w `/tmp/tf-provider-probe/`: help każdej binarki, grok-initialize.txt, muse-schema/{manifest.json,msp.schema.json}, muse-msp-events.json oraz skrypty probe.py/acp.py/msp.py. Zawierają wyłącznie syntetyczne sesje bez poświadczeń. Nie są częścią runtime produkcyjnego.

Grok Build i Muse Code mają adaptery, instalatory przypiętych wersji z SHA256 oraz manifesty macOS. Testy adapterów obejmują prawdziwe procesy bez konta i odmowę niezalogowanej tury Muse. Rozpoczęcie logowania kodem pod sandboxem potwierdziło dokładne domeny auth.x.ai i auth.meta.com; nie ukończono logowania. Stan samego pliku poświadczenia jest oznaczany jako niezweryfikowany. Zmienione przez proces poświadczenie nie zastępuje automatycznie centralnego konta: wymaga ponownego zweryfikowania tożsamości/logowania. Antigravity nie jest jeszcze oferowane jako zarządzana subskrypcja z powodu niepotwierdzonej izolacji keyring.

## Wynik wdrożenia adaptera MSP

Adapter `muse.rs` korzysta ze wspólnego transportu JSON-RPC oraz produkcyjnego sandboxa procesowego i supervisora launchd. Wykonano bez konta rzeczywisty `model/list`, initialize, session/start, turn/start i notyfikację błędu turn/completed, następnie potwierdzono zakończenie procesu. Próby używają pustych prywatnych XDG_CONFIG_HOME/XDG_DATA_HOME i portu proxy bez listenera. Test nie potwierdza płatnej tury ani logowania subskrypcyjnego.

Testy protokołu potwierdzają unieważnienie poprzedniego identyfikatora zatwierdzenia przy zmianie requirementId i odmowę zamiany jednorazowej zgody na regułę persistent/session. Adapter wybiera wyłącznie jednoznaczną, zaoferowaną przez serwer decyzję `approved`, `denied` lub `abort` z `scope=once`. Nieobsługiwane pytanie strukturalne jest anulowane przez `userInput/cancel` z jawnym powodem; nie dostaje wymyślonej odpowiedzi użytkownika. Wznowienie sesji sprawdza zgodność workspaceRoot z bieżącym przyznanym katalogiem.

## Domena inicjowania logowania

Dodatkowa ograniczona próba `muse login` i `grok --no-auto-update login --device-auth` użyła produkcyjnego Seatbelt/supervisora oraz lokalnego proxy wymagającego losowego hasła. Proxy przekazywało TLS wyłącznie do jawnie wymienionych domen. Zaobserwowane CONNECT: Muse `auth.meta.com:443`; Grok `auth.x.ai:443`. Obie aplikacje doszły do instrukcji kodu urządzenia. Nie otwierano przepływu przeglądarkowego ani nie zatwierdzano żadnego konta; procesy zakończono z potwierdzonym cleanup. Grok wypisał również informację o niepowodzeniu, więc pomiar nie dowodzi pełnego logowania. Nie zapisywano ani nie publikowano kodów urządzenia. Domeny dalszego odświeżania tokenów i płatnych wywołań nadal wymagają testu z kontem.


Grok macOS x86_64 pobrano z oficjalnego adresu wersji: 149 694 528 B, SHA256 `8eacec87f5ecdb9259c6d812d12ce9e2d405b1526e36ae9d7fc81ec31dbd74d6`. Nie uruchomiono binarki Intel na fizycznym Macu Intel. Sumy Grok zostały przypięte po pobraniu przez HTTPS; nie są podpisem wydawcy. Muse sprawdzono dodatkowo względem SHA256 w oficjalnym manifeście.

Końcowy test adaptera Grok 1.0.13 wykonał ACP initialize i odczyt oferowanych modeli przez rzeczywisty sandbox oraz supervisor, po czym potwierdził cleanup. Wynik: 1 test zakończony powodzeniem w 3,33 s. Profil nie zawierał `auth.json`, a port proxy nie miał listenera. Pięć testów wspólnego transportu RPC sprawdza m.in. timeout obejmujący zapis i odpowiedź oraz usunięcie oczekującego żądania po anulowaniu.
