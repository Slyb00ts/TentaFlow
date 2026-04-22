# Review TentaFlow

## Aktualizacja 2026-04-22: Teams Bot / Meeting Bot

Zakres tej aktualizacji obejmuje tylko obszar `teams-bot` / `meeting-bot` po ponownej weryfikacji zmian w:

- `tentaflow-core/addons-pro/teams-bot`
- `tentaflow-core/addons-pro/teams`
- `tentaflow-core/src/addon/host_functions/service.rs`
- `tentaflow-core/src/api/dashboard/api_addon_system.rs`
- `tentaflow-containers/agents/docker/teams-bot`
- `tentaflow-containers/sidecar`

Sprawdzone zostały też buildy `cargo check` dla:

- `tentaflow-containers/agents/docker/teams-bot`
- `tentaflow-containers/sidecar`
- `tentaflow-core/addons-pro/teams`
- `tentaflow-core/addons-pro/teams-bot`

Wszystkie powyższe crate'y obecnie się kompilują, ale `teams-bot` dalej generuje ostrzeżenia o martwym kodzie i nieużywanych polach/metodach.

## Korekta względem wcześniejszego review

Poniższe stare zarzuty są już nieaktualne:

- Runtime `meeting-bot` nie jest już samą atrapą. Ma realne `join/leave/status` po QUIC oraz faktyczną automatykę Chromium dla wejścia do spotkania w `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:382` i `tentaflow-containers/agents/docker/teams-bot/src/browser.rs:88`.
- Kontener robi realny pipeline audio `Chromium -> WebSocket bridge -> VAD -> STT przez router` w `tentaflow-containers/agents/docker/teams-bot/src/main.rs:95` i `tentaflow-containers/agents/docker/teams-bot/src/main.rs:300`.
- W kontenerze doszły `meeting_id_override` i `secret_key_hex`, więc wcześniejszy zarzut o pełnym braku kontroli nad stabilnym `meeting_id` / kluczem endpointu jest już nieaktualny.

To nie znaczy, że obszar jest uporządkowany. Po zmianach pojawił się inny, ważniejszy problem: dziś istnieją równolegle co najmniej trzy niespójne kontrakty sterowania botem.

## Najważniejsze problemy

### 1. Krytyczne: addon `teams-bot`, dashboard i runtime używają różnych kontraktów sterowania

W repo jednocześnie występują trzy warianty:

- Addon `teams-bot` wysyła przez `service_request_call("teams-bot", ...)` surowy JSON w formacie `{"type":"join|leave|speak", ...}`:
  - `tentaflow-core/addons-pro/teams-bot/src/lib.rs:24`
  - `tentaflow-core/addons-pro/teams-bot/src/lib.rs:62`
  - `tentaflow-core/addons-pro/teams-bot/src/lib.rs:486`
  - `tentaflow-core/addons-pro/teams-bot/src/lib.rs:519`
  - `tentaflow-core/addons-pro/teams-bot/src/lib.rs:572`
  - `tentaflow-core/addons-pro/teams-bot/src/lib.rs:397`
- Host `service_request` nie mapuje tego na żaden nowy format, tylko pakuje payload 1:1 do `CompletionPayload.prompt`:
  - `tentaflow-core/src/addon/host_functions/service.rs:220`
- Runtime `meeting-bot` rozpoznaje wyłącznie format `{"tool":"teams-bot.join_meeting|leave_meeting|get_status","params":...}`:
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:382`
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:398`
- Dashboard ma jeszcze trzeci wariant ścieżki: buduje poprawny envelope `tool/params`, ale wysyła go do serwisu o innej nazwie niż manifest agenta:
  - `tentaflow-core/src/api/dashboard/api_addon_system.rs:2055`
  - `tentaflow-core/src/api/dashboard/api_addon_system.rs:2061`
  - `tentaflow-containers/agents/_services/teams-bot.toml:4`

Skutek:

- `join/leave/speak` wywołane z addonu `teams-bot` nie trafiają w aktualny handler runtime.
- Dashboard i addon nie sterują tym samym protokołem.
- W repo istnieją jednocześnie dwa identyfikatory serwisu: `teams-bot` i `tentaflow-meeting-bot`.

To jest dziś główny dług architektoniczny tego obszaru.

### 2. Krytyczne: addon może oznaczać sukces mimo że runtime nie wykonał komendy

Jeśli runtime nie rozpozna komendy, wpada do ogólnego `process_request()` i zwraca syntetyczny tekst `"Meeting bot kontener — odebrano completion request: ..."`, zamiast twardego błędu:

- `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:472`

Po stronie addonu wynik `Ok(...)` jest traktowany jako sukces i np. ustawia stan meetingu na `active`:

- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:527`
- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:529`
- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:580`
- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:582`

To oznacza fałszywie pozytywne zachowanie UI i storage.

### 3. Wysokie: ścieżka TTS/odpowiedzi bota nadal nie jest domknięta end-to-end

`respond_to_meeting()` w addonie generuje odpowiedź LLM, a potem wysyła komendę `{"type":"speak"}`:

- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:352`
- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:397`

Problem:

- Runtime nie ma handlera `teams-bot.speak`; obsługuje tylko `join_meeting`, `leave_meeting`, `get_status`:
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:398`
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:421`
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:425`
- Ogólny handler TTS w runtime zwraca pusty bufor audio:
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:505`
- Kanał playback istnieje, ale nie jest używany przez główny pipeline:
  - `tentaflow-containers/agents/docker/teams-bot/src/audio.rs:34`
  - `tentaflow-containers/agents/docker/teams-bot/src/audio.rs:56`
  - `tentaflow-containers/agents/docker/teams-bot/src/main.rs:98`
- `tts_voice` jest obecnie tylko wczytywane i nie uczestniczy w wykonaniu:
  - `tentaflow-containers/agents/docker/teams-bot/src/main.rs:117`

W praktyce: bot już słucha i transkrybuje lepiej niż wcześniej, ale nadal nie ma jednej, spójnej i działającej ścieżki "usłysz pytanie -> wygeneruj odpowiedź -> powiedz ją w meetingu".

### 4. Wysokie: speaker attribution nadal jest niedomknięty

Po stronie runtime lokalny transcript nadal wysyła fallback `"Nieznany"`:

- `tentaflow-containers/agents/docker/teams-bot/src/main.rs:318`
- `tentaflow-containers/agents/docker/teams-bot/src/main.rs:324`

Jednocześnie:

- `browser::get_active_speaker()` nadal kończy się `Ok(None)`:
  - `tentaflow-containers/agents/docker/teams-bot/src/browser.rs:268`
- streaming transcript z kontenera korzysta z lokalnego kanału, więc niesie ten fallback dalej:
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:530`

Router ma w repo dużo bogatszą infrastrukturę diarization / voice profiles, ale lokalna ścieżka `teams-bot` nadal nie przekazuje do własnego bufora wyników speakerów w sposób spójny z tym, co ma core.

### 5. Wysokie: duplikacja `teams` vs `teams-bot` nadal istnieje

W repo dalej są dwie osobne ścieżki "meeting bot":

- dedykowany addon `teams-bot`
- osobny flow meetingowy w addonie `teams`

Przy czym `teams.join_meeting()` w addonie `teams` nadal jest głównie warstwą metadata/storage/eventów, a nie realnym dołączeniem bota do audio call:

- `tentaflow-core/addons-pro/teams/src/lib.rs:1057`

To oznacza równoległe, częściowo zdublowane odpowiedzialności:

- `teams` trzyma swój flow OAuth/Graph/meeting notes
- `teams-bot` trzyma osobny flow transcript/LLM/respond
- runtime kontenera jest jeszcze trzecią implementacją faktycznego meeting execution

Bez scalenia odpowiedzialności ten obszar będzie dalej produkował rozjazdy kontraktów.

### 6. Wysokie: generyczny `tentaflow-sidecar` nadal nie przejął roli TeamsBot

`tentaflow-containers/sidecar` nadal deklaruje `Role::TeamsBot`, ale runtime kończy się `bail!()`:

- `tentaflow-containers/sidecar/src/main.rs:50`

To oznacza, że migracja do wspólnego sidecara nadal jest niedokończona, a osobny crate `tentaflow-teams-bot` pozostaje realną, równoległą implementacją.

## Problemy średnie i śmieci

### 7. Martwe lub pozorne pola/metody w kontenerze

Obecnie nadal widać kilka ewidentnych kandydatów do usunięcia albo dokończenia:

- `audio_device` w configu jest trzymane, ale aktualny pipeline nie używa już PulseAudio:
  - `tentaflow-containers/agents/docker/teams-bot/src/config.rs:43`
- `auth_cookies_path` nadal jest wymagane, ale loader cookies pozostaje `TODO`:
  - `tentaflow-containers/agents/docker/teams-bot/src/config.rs:16`
  - `tentaflow-containers/agents/docker/teams-bot/src/browser.rs:77`
- `AudioPlayback::send()` istnieje, ale główny kod z niego nie korzysta:
  - `tentaflow-containers/agents/docker/teams-bot/src/audio.rs:39`
- `RouterClient::synthesize()` istnieje, ale obecnie jest martwe:
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:136`
- `MeetingQuicServer::router_client()` wygląda na nieużywane API:
  - `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:224`

To pokrywa się z ostrzeżeniami `cargo check`.

### 8. Niespójna powierzchnia narzędzi

Obecnie:

- addon wystawia `teams-bot.get_transcript`
- runtime wystawia `teams-bot.get_status`
- dashboard specjalny path używa tylko części komend meetingowych

Referencje:

- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:137`
- `tentaflow-core/addons-pro/teams-bot/src/lib.rs:550`
- `tentaflow-containers/agents/docker/teams-bot/src/quic_server.rs:425`

To nie jest jeszcze krytyczny błąd wykonania, ale jest jasnym sygnałem, że publiczne API tego obszaru nie ma jednego właściciela.

### 9. Potencjalny rozjazd permissionów dla `service_request`

Manifest addonu `teams-bot` deklaruje permission `service.call`:

- `tentaflow-core/addons-pro/teams-bot/manifest.toml:47`

Host function `service_request` sprawdza literalnie permission `service`:

- `tentaflow-core/src/addon/host_functions/service.rs:66`

Jeżeli nie ma dodatkowej translacji permission IDs gdzie indziej, addon może być odrzucany jeszcze przed wysłaniem requestu do runtime. Ten punkt wymaga szybkiego potwierdzenia w pełnym flow uprawnień, ale już teraz wygląda podejrzanie.

### 10. Addon `teams` nadal ma dwa stare długi

Poza samym `teams-bot`, w powiązanym addonie `teams` nadal są dwa stare problemy:

- `refresh_oauth_token()` buduje body `application/x-www-form-urlencoded` przez surowy `format!`, bez jawnego URL-encoding:
  - `tentaflow-core/addons-pro/teams/src/lib.rs:442`
- `teams.join_meeting()` nadal nie realizuje prawdziwego call-runtime, tylko inicjalizuje transcript/event/status:
  - `tentaflow-core/addons-pro/teams/src/lib.rs:1057`

## Co uprościć / scalić

Najbardziej sensowny kierunek uproszczenia:

1. Ustalić jeden identyfikator serwisu i jeden kontrakt komendy.
   Najprościej: wszędzie `teams-bot`, wszędzie format `{"tool": "...", "params": {...}}`.

2. Wybrać jedną ścieżkę sterowania.
   Albo dashboard special-case zostaje i addon używa tego samego adaptera, albo cały flow idzie przez `service_request`. Nie oba naraz.

3. Wybrać jednego właściciela funkcji meetingowych.
   `teams` powinien odpowiadać za OAuth/Graph, a `teams-bot` za call runtime, albo odwrotnie. Dziś oba robią fragmenty tego samego.

4. Dokończyć albo usunąć TTS playback.
   Obecny stan jest półproduktem: config i bridge są, ale runtime nie ma działającego kontraktu `speak`.

5. Usunąć martwe pola/metody po domknięciu kierunku.
   Najpierw kontrakt i architektura, dopiero potem czyszczenie `audio_device`, `tts_voice`, `synthesize`, `get_active_speaker`, itp.

## Priorytet napraw

Najpierw naprawić:

1. Jeden kontrakt addon/dashboard/runtime.
2. Jeden identyfikator serwisu.
3. Twarde błędy zamiast fałszywych sukcesów przy nierozpoznanej komendzie.
4. Działające `speak` albo usunięcie martwego TTS flow.
5. Decyzję architektoniczną `teams` vs `teams-bot` vs `tentaflow-sidecar`.

Dopiero po tym ma sens robić dalsze czyszczenie śmieci i scalanie helperów.
