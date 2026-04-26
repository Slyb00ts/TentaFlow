// =============================================================================
// Plik: browser.rs
// Opis: Automatyzacja przegladarki Chromium — dolaczanie do spotkan Teams,
//       wykrywanie aktywnego mowcy, injekcja mostu audio (browser_inject.js).
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt;
use tentaflow_protocol::{
    MeetingEventPayload, LIFECYCLE_BROWSER_LAUNCHED, LIFECYCLE_JOINING,
    LIFECYCLE_NAVIGATING, LIFECYCLE_PREJOIN_READY,
};
use tokio::sync::Mutex;

use crate::config::MeetingConfig;
use crate::dom_observer::{self, DomObserver, RosterState, SpeakerState};
use crate::quic_server::RouterClient;

/// Uchwyt do opcjonalnego RouterClient współdzielonego z main.rs — gdy None,
/// `join_meeting` po prostu pomija wysyłanie lifecycle events (np. w trybie
/// stand-alone bez hosta). Meeting_key identyfikuje sesję po stronie routera.
pub type RouterHandle = Arc<Mutex<Option<Arc<RouterClient>>>>;

/// Wysyła pojedynczy `LifecycleUpdate` bez twardej zależności od aktualnego
/// stanu routera — błąd/brak klienta jest logowany i połykany, żeby diagnostyka
/// nie zabiła flow dołączania.
async fn emit_lifecycle(router: &RouterHandle, meeting_key: &str, stage: &str, details: Option<String>) {
    let client = {
        let guard = router.lock().await;
        guard.as_ref().cloned()
    };
    let Some(client) = client else {
        tracing::debug!(stage, "emit_lifecycle: router client niedostepny — pomijam");
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if let Err(e) = client
        .send_meeting_event(
            meeting_key,
            ts,
            MeetingEventPayload::LifecycleUpdate {
                stage: stage.to_string(),
                details,
            },
        )
        .await
    {
        tracing::warn!("emit_lifecycle({}) failed: {}", stage, e);
    }
}

/// Max czas na wykrycie JAKIEGOKOLWIEK stanu join (Lobby albo Joined). Jesli
/// ani jedno ani drugie nie pojawi sie w tym oknie — Teams jest zepsuty,
/// timeout jest slusznie blad. 5 minut to znaczny zapas na pelny redirect
/// chain Teams + prejoin click.
const PRESENCE_TIMEOUT: Duration = Duration::from_secs(300);

/// Max czas na CZEKANIE w lobby na admit hosta. Bot raz wykryty w lobby
/// czeka cierpliwie — host moze byc poza biurkiem. 60 minut chroni przed
/// wiszacym kontenerem gdy meeting nigdy sie nie zaczyna.
const LOBBY_GRACE: Duration = Duration::from_secs(3600);

/// Skrypt wstrzykiwany do strony Teams — przechwytuje audio przez captureStream()
/// i wysyla PCM do Rust przez WebSocket ws://127.0.0.1:9999/bridge. Obsluguje tez
/// mic injection przez monkey-patch getUserMedia + MediaStreamTrackGenerator.
const AUDIO_BRIDGE_JS: &str = include_str!("browser_inject.js");

/// Uruchamia headless Chromium z wczytanymi cookies sesji.
/// Kazde uruchomienie tworzy UNIKALNY user_data_dir — bez tego Chromium
/// przy drugim launch'u blokuje sie na "profile locked".
pub async fn launch_chromium(config: &MeetingConfig) -> Result<Browser> {
    let instance_id = uuid::Uuid::new_v4().to_string();
    // std::env::temp_dir() — przenosne miedzy linux (/tmp), macOS (/var/folders/...)
    // i Windows (C:\Users\<user>\AppData\Local\Temp). Docker dziala dalej tak samo
    // bo tam temp_dir() == /tmp.
    let user_data_dir_path = std::env::temp_dir()
        .join(format!("chromium-meeting-bot-{}", &instance_id[..8]));
    let user_data_dir = user_data_dir_path.to_string_lossy().to_string();
    let prefs_dir = user_data_dir_path.join("Default");
    std::fs::create_dir_all(&prefs_dir).ok();
    let user_data_dir = user_data_dir.as_str();
    let prefs_dir = prefs_dir.to_string_lossy().to_string();
    tracing::info!(user_data_dir, "Uruchamianie Chromium z unikalnym profilem");
    // content_settings: 1 = allow, 2 = block
    // - media_stream_mic: allow (bot musi mowic przez Teams)
    // - media_stream_camera: block (bot nie wysyla video)
    // - notifications: block (nie chcemy w ogole)
    // - loopback_network: allow TYLKO dla Teams — od Chrome 147 pojawil sie
    //   popup "Access other apps and services on this device" przy starcie
    //   Teams meeting (LAN access dla STUN/connectivity probes). Pre-allow
    //   DLA KONKRETNEJ DOMENY omija popup bez nadawania permissji globalnie.
    std::fs::write(
        format!("{}/Preferences", prefs_dir.as_str()),
        r#"{"profile":{"content_settings":{"exceptions":{"media_stream_mic":{"*,*":{"setting":1}},"media_stream_camera":{"*,*":{"setting":2}},"notifications":{"*,*":{"setting":2}},"loopback_network":{"https://teams.microsoft.com:443,*":{"setting":1},"https://teams.live.com:443,*":{"setting":1}}}}}}"#,
    ).ok();

    let chrome_exe = find_chromium_executable()
        .ok_or_else(|| anyhow::anyhow!(
            "Nie znaleziono Chromium/Chrome — zainstaluj przegladarke (Chromium, Google Chrome, Brave lub Edge)"
        ))?;
    tracing::info!(path = %chrome_exe.display(), "Wybrana binarka Chromium");

    // Tryb headless sterowany env-em `TENTAFLOW_HEADLESS` (default: headless=new
    // dla natywnego deploya, zeby nie wyskakiwalo okno Chrome). Stary `--headless`
    // (HeadlessMode::True) nie zenkoduje canvas captureStream — uzywamy New.
    // `TENTAFLOW_HEADLESS=0` wymusza tryb z oknem (przydatne do debugowania
    // lokalnie albo w Dockerze gdzie Xvfb i tak daje wirtualny display).
    let headless_disabled = std::env::var("TENTAFLOW_HEADLESS")
        .ok()
        .map(|v| matches!(v.as_str(), "0" | "false" | "no"))
        .unwrap_or(false);
    let mut builder = BrowserConfig::builder()
        .chrome_executable(&chrome_exe);
    builder = if headless_disabled {
        builder.with_head()
    } else {
        builder.new_headless_mode()
    };
    let browser_config = builder
        .no_sandbox()
        .window_size(1920, 1080)
        .user_data_dir(user_data_dir)
        .arg("use-fake-ui-for-media-stream")
        .arg("autoplay-policy=no-user-gesture-required")
        .arg("enable-features=MediaStreamTrackGenerator")
        // --disable-gpu tears down the WebRTC video pipeline: canvas
        // captureStream() reported the track as live but the encoder never
        // got composited frames. swiftshader provides a software GL backend
        // that's enough for canvas readbacks and WebRTC encoding inside the
        // headless container.
        .arg("use-gl=swiftshader")
        .arg("enable-unsafe-swiftshader")
        .arg("disable-blink-features=AutomationControlled")
        .arg("ignore-certificate-errors")
        // Wymuszamy Linux Chromium UA zeby Teams nie serwowal wariantu
        // launcher.html z `msLaunch=true&directDl=true` ktory na macOS/Windows
        // probuje odpalic handler protokolu `msteams://` i zawiesza navigation
        // na 30s+. Docker dziala bo Linux Chromium dostaje wariant launchera
        // bez deeplinkow do desktop app.
        .arg("user-agent=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36")
        // Blokuje dialog "Open in app" dla zarejestrowanych protokolow
        // (msteams://, zoommtg:// itp.) na realnym Chrome poza Dockerem.
        .arg("disable-features=ExternalProtocolDialog")
        .build()
        .map_err(|e| anyhow::anyhow!("Blad konfiguracji Chromium: {}", e))?;

    let (browser, mut handler) = Browser::launch(browser_config).await?;

    // Handler zdarzen przegladarki — uruchomiony w tle
    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if event.is_err() {
                tracing::warn!("Zdarzenie przegladarki zakonczylo sie bledem");
                break;
            }
        }
    });

    // Force-grant camera, microphone and the related media-capture permissions
    // for Teams' origins. Without this navigator.permissions.query returned
    // 'prompt' (or 'denied') for anonymous joiners — Teams then rendered the
    // 'Camera/Mic is not available — Go to your device settings' UFD and the
    // toggle stayed aria-disabled. The Chromium CDP setPermission call works
    // at the browser level and is honoured by every later getUserMedia call,
    // unlike --use-fake-ui-for-media-stream which only suppresses the UI
    // prompt without changing the stored permission state.
    {
        use chromiumoxide::cdp::browser_protocol::browser::{
            PermissionDescriptor, PermissionSetting, SetPermissionParams,
        };
        let names = ["camera", "microphone", "midi", "midi-sysex"];
        let origins = [
            "https://teams.microsoft.com",
            "https://teams.live.com",
        ];
        for origin in origins.iter() {
            for name in names.iter() {
                let params = SetPermissionParams {
                    permission: PermissionDescriptor::new(*name),
                    setting: PermissionSetting::Granted,
                    origin: Some((*origin).to_string()),
                    embedded_origin: None,
                    browser_context_id: None,
                };
                if let Err(e) = browser.execute(params).await {
                    tracing::warn!(origin = origin, name = name, "setPermission failed: {}", e);
                }
            }
        }
        tracing::info!("Camera/microphone permissions granted for Teams origins");
    }

    // TODO: Wczytanie cookies z config.auth_cookies_path
    // Cookies Teams sa wymagane do automatycznej autoryzacji.
    // Format: JSON array z polami name, value, domain, path, httpOnly, secure
    tracing::info!(
        cookies_path = %config.auth_cookies_path,
        "Cookies do wczytania (TODO: implementacja)"
    );

    Ok(browser)
}

/// Nawiguje do URL spotkania Teams i dolacza jako gosc lub czeka na logowanie VNC.
/// Emituje `LifecycleUpdate` events do routera na każdym kluczowym przejściu:
/// `browser_launched` (caller już po `launch_chromium`) → `navigating` → `prejoin_ready`
/// → `joining` → `joined`.
pub async fn join_meeting(
    browser: &Browser,
    url: &str,
    config: &MeetingConfig,
    router: &RouterHandle,
    meeting_key: &str,
    roster_state: RosterState,
    speaker_state: SpeakerState,
) -> Result<(Page, DomObserver)> {
    use chromiumoxide::cdp::browser_protocol::page::SetBypassCspParams;

    emit_lifecycle(router, meeting_key, LIFECYCLE_BROWSER_LAUNCHED, None).await;

    // Otworz pusta strone, zainstaluj init script PRZED nawigacja do Teams.
    // Dzieki temu monkey-patch getUserMedia jest aktywny zanim Teams go wywola.
    let page = browser.new_page("about:blank").await?;

    // Wylacz Content Security Policy dla tej strony — Teams ma strict CSP
    // ktore blokuje WebSocket do ws://127.0.0.1:9999. setBypassCSP to CDP
    // method przeznaczony dla debuggerow, calkowicie pomija CSP dla target.
    page.execute(SetBypassCspParams { enabled: true }).await
        .map_err(|e| anyhow::anyhow!("Blad setBypassCSP: {}", e))?;
    tracing::info!("CSP wylaczone dla strony (setBypassCSP=true)");

    // Port mostu WS dla tej sesji — JS w `browser_inject.js` czyta
    // `window.__tfBridgePort` zeby wiedziec do ktorego portu sie podlaczyc.
    // Native bot dostaje port przez env `TENTAFLOW_BRIDGE_PORT` (per-sesja
    // alokacja w MeetingManager); Docker uzywa default 9999.
    let bridge_port = crate::audio::bridge_port();
    let bridge_port_js = format!("window.__tfBridgePort = {};", bridge_port);
    page.evaluate_on_new_document(bridge_port_js).await
        .map_err(|e| anyhow::anyhow!("Blad evaluate_on_new_document(bridgePort): {}", e))?;

    page.evaluate_on_new_document(AUDIO_BRIDGE_JS).await
        .map_err(|e| anyhow::anyhow!("Blad evaluate_on_new_document: {}", e))?;
    tracing::info!(bridge_port, "Zarejestrowano init script (audio bridge) dla wszystkich dokumentow");

    // Bot display name dostepny dla JS (uzywany przez detector active speakera
    // do filtrowania wlasnego tile'a). Cudzyslowy w nazwie escapowane na wszelki
    // wypadek — ofertujemy raw string a nie JSON.stringify zeby uniknac
    // serde_json zaleznosci tutaj.
    let bot_name_js = format!(
        "window.__tentaflowBotName = \"{}\";",
        config.bot_name.replace('\\', "\\\\").replace('"', "\\\"")
    );
    page.evaluate_on_new_document(bot_name_js).await
        .map_err(|e| anyhow::anyhow!("Blad evaluate_on_new_document(botName): {}", e))?;

    // Push-based DOM event bridge — registers `__tentaflowEvent` binding and
    // spawns a tokio task that forwards JS-side MutationObserver events to
    // the router. Done before `goto` so the binding exists when the injected
    // observer first fires. Observer also drives lobby/joined lifecycle and
    // `wait_for_joined` further down.
    let observer = dom_observer::start(
        page.clone(),
        router.clone(),
        meeting_key.to_string(),
        config.bot_name.clone(),
        roster_state,
        speaker_state,
    )
    .await?;
    tracing::info!("DOM observer (push-based) uruchomiony");

    // Forward JS console messages z Chromium do naszych logow Rust
    // Dzieki temu bledy ze wstrzyknietego browser_inject.js widac w docker logs
    spawn_console_forwarder(&page).await;

    // NIE wywolujemy wait_for_navigation — Teams robi chain redirectow
    // (launcher.html → v2 → light-meetings) i event 'navigation done' nigdy
    // nie przychodzi. Nasz polling click_when_present sam poczeka na DOM.
    emit_lifecycle(router, meeting_key, LIFECYCLE_NAVIGATING, None).await;
    // Teams launcher.html z `msLaunch=true` potrafi nigdy nie odpalic eventu
    // `load` (czeka na handler `msteams://`). CDP `Page.navigate` zwroci wtedy
    // dopiero po 30s default timeoucie i propaguje blad. Polling DOM (ponizej)
    // sam dosiegnie do prejoin/lobby/joined niezaleznie od tego czy load
    // sie dokonczyl, wiec timeoutujemy nav agresywnie i jedziemy dalej.
    match tokio::time::timeout(Duration::from_secs(8), page.goto(url)).await {
        Ok(Ok(_)) => tracing::info!(url = url, "Nawigacja do spotkania Teams rozpoczeta"),
        Ok(Err(e)) => tracing::warn!(url = url, "page.goto zwrocil blad (kontynuujemy z pollingiem DOM): {}", e),
        Err(_) => tracing::warn!(url = url, "page.goto przekroczyl 8s (kontynuujemy z pollingiem DOM)"),
    }

    // Czekamy na zaladowanie strony light-meetings
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Prejoin DOM powinien być dostępny po 3s — emitujemy stage zanim
    // klikniemy "Continue without audio or video" / kamerę.
    emit_lifecycle(router, meeting_key, LIFECYCLE_PREJOIN_READY, None).await;

    // KROK 1: Dialog "Are you sure you don't want audio or video?"
    // Gdy bot_video_enabled=true NIE klikamy "Continue without" — zamiast tego
    // wlaczamy kamere w prejoin zeby canvas captureStream video-track byl
    // aktywny. Init script jest doklejany na document_start ale samo
    // setupVideoInjection wykonuje sie asynchronicznie — daj mu chwile
    // przed sprawdzeniem flagi.
    let video_available = if config.bot_video_enabled {
        let mut available = false;
        for _ in 0..20 {
            let v = page
                .evaluate("!!window.__tentaflowVideoAvailable")
                .await
                .map(|v| v.into_value::<bool>().unwrap_or(false))
                .unwrap_or(false);
            if v {
                available = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        available
    } else {
        false
    };
    if config.bot_video_enabled && !video_available {
        tracing::warn!(
            "bot_video_enabled=true ale window.__tentaflowVideoAvailable nie ustawione \
             po 2s — canvas/captureStream niedostepne, fallback na 'Continue without'"
        );
    }
    if config.bot_video_enabled && video_available {
        tracing::info!("bot_video_enabled=true — wlaczam kamere w prejoin");
        let _ = click_when_present(
            &page,
            r#"
            (function() {
                const toggle = document.querySelector('[data-tid="toggle-video"]')
                    || document.querySelector('button[aria-label*="camera" i]');
                if (!toggle) return false;
                const pressed = toggle.getAttribute('aria-pressed') === 'true'
                    || toggle.getAttribute('aria-checked') === 'true';
                if (!pressed) { toggle.click(); }
                return true;
            })()
            "#,
            Duration::from_secs(10),
            "prejoin toggle camera ON",
        ).await;
    } else {
        tracing::info!("bot_video_enabled=false — klikam 'Continue without audio or video'");
        let _ = click_when_present(
            &page,
            r#"
            (function() {
                const btn = Array.from(document.querySelectorAll('button'))
                    .find(el => el.textContent && el.textContent.trim() === 'Continue without audio or video');
                if (btn) { btn.click(); return true; }
                return false;
            })()
            "#,
            Duration::from_secs(10),
            "dialog 'Continue without audio or video'",
        ).await;
    }

    tokio::time::sleep(Duration::from_secs(1)).await;

    // KROK 2: Wpisanie nazwy bota w polu "Type your name"
    tracing::info!(bot_name = %config.bot_name, "Wpisywanie nazwy bota w polu prejoin");
    let name_js = format!(
        r#"
        (function() {{
            const input = document.querySelector('input[placeholder="Type your name"]')
                || document.querySelector('[data-tid="prejoin-display-name-input"]')
                || document.querySelector('input[name="displayName"]');
            if (!input) return false;
            input.focus();
            const setter = Object.getOwnPropertyDescriptor(
                HTMLInputElement.prototype, 'value').set;
            setter.call(input, "{}");
            input.dispatchEvent(new Event('input', {{ bubbles: true }}));
            input.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return true;
        }})()
        "#,
        config.bot_name.replace('"', r#"\""#)
    );
    let _ = click_when_present(
        &page,
        &name_js,
        Duration::from_secs(15),
        "pole 'Type your name'",
    ).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // KROK 3: Klikniecie "Join now"
    tracing::info!("Klikanie przycisku 'Join now'");
    emit_lifecycle(router, meeting_key, LIFECYCLE_JOINING, None).await;
    let joined = click_when_present(
        &page,
        r#"
        (function() {
            // Najpierw szukamy po tekscie bo Teams zmienia data-tid
            const btn = Array.from(document.querySelectorAll('button'))
                .find(el => {
                    if (el.disabled) return false;
                    const t = (el.textContent || '').trim();
                    return t === 'Join now' || t === 'Dolacz teraz';
                });
            if (btn) { btn.click(); return true; }
            // Fallback data-tid
            const byTid = document.querySelector('[data-tid="prejoin-join-button"]');
            if (byTid && !byTid.disabled) { byTid.click(); return true; }
            return false;
        })()
        "#,
        Duration::from_secs(15),
        "przycisk 'Join now'",
    ).await.is_ok();

    if joined {
        tracing::info!("Dialog prejoin zaakceptowany — oczekiwanie na wejscie do meetingu");
    } else {
        tracing::warn!(
            "Auto-join nie udal sie — wymagana interwencja VNC na porcie 5900"
        );
    }

    // Czekamy na realne dolaczenie. Lifecycle (lobby_waiting, joined) emituje
    // sam DomObserver w momencie gdy JS-side MutationObserver zauwazy stage —
    // tutaj tylko czekamy az state przejdzie w Joined albo wybuchnie timeout.
    observer.wait_for_joined(PRESENCE_TIMEOUT, LOBBY_GRACE).await?;
    tracing::info!("Pomyslnie dolaczono do spotkania");

    if config.bot_video_enabled {
        // Camera controls are disabled while the prejoin/lobby screen is up,
        // so the toggle click we did earlier was a no-op. After the in-call
        // surface renders, re-enable the camera so getUserMedia fires and
        // our canvas track reaches Teams. We log a JSON status object back so
        // the bot logs reveal whether we actually clicked, which selector
        // matched, or why we gave up.
        let res = page.evaluate(
            r#"
            (async function() {
                const sels = [
                    '#video-button',
                    '[data-tid="toggle-video"]',
                    'button[aria-label*="camera" i]',
                    'button[aria-label*="kamera" i]',
                    'button[title*="camera" i]',
                ];
                const findBtn = () => {
                    for (const sel of sels) {
                        const b = document.querySelector(sel);
                        if (b) return { btn: b, sel };
                    }
                    return null;
                };
                const click = (btn) => {
                    try { btn.removeAttribute('disabled'); } catch (_) {}
                    try { btn.removeAttribute('aria-disabled'); } catch (_) {}
                    btn.click();
                };
                for (let i = 0; i < 20; i++) {
                    const found = findBtn();
                    if (found) {
                        const disabled = found.btn.disabled
                            || found.btn.getAttribute('aria-disabled') === 'true';
                        if (!disabled) {
                            const pressed = found.btn.getAttribute('aria-pressed') === 'true'
                                || found.btn.getAttribute('aria-checked') === 'true';
                            if (!pressed) click(found.btn);
                            return JSON.stringify({ ok: true, selector: found.sel, mode: 'enabled-click', pressed });
                        }
                    }
                    await new Promise((r) => setTimeout(r, 500));
                }
                const found = findBtn();
                if (found) {
                    try { click(found.btn); } catch (_) {}
                }
                let gumOk = false;
                try {
                    const stream = await navigator.mediaDevices.getUserMedia({
                        video: true,
                        audio: true,
                    });
                    gumOk = !!stream && stream.getVideoTracks().length > 0;
                    window.__tentaflowForcedStream = stream;
                } catch (e) {
                    return JSON.stringify({
                        ok: false,
                        reason: 'gum forced call rejected: ' + (e && e.message ? e.message : String(e)),
                    });
                }
                return JSON.stringify({
                    ok: gumOk,
                    mode: 'forced-gum',
                    fallbackClick: !!found,
                    reason: 'no enabled camera button — used forced getUserMedia',
                });
            })()
            "#,
        ).await;
        let report = res
            .map(|v| v.into_value::<String>().unwrap_or_default())
            .unwrap_or_default();
        tracing::info!(report = %report, "Post-join camera toggle result");
    }

    // Post-join unmute + replaceTrack na wszystkich audio senders zeby
    // KAZDY pc.transceiver audio uzywal naszego micGenerator. Teams w
    // anonim joinach tworzy 2 audio sendery: jeden z naszego getUserMedia
    // override (micGenerator, ale Teams go disabuje) i drugi z Chromium
    // fake-input (enabled, ale wysyla pusty PCM). Bez replaceTrack tone
    // idzie do disabled sendera = void. Dumpujemy stan przed i po.
    let unmute_report = page
        .evaluate(
            r#"
            (async function() {
                const result = {
                    unmuteAttempted: false,
                    unmuteClicked: false,
                    replacedAudio: 0,
                    replacedVideo: 0,
                    enabledOurTrack: 0,
                    dismissedAreYouSure: false,
                    sendersBefore: [],
                    sendersAfter: [],
                    renegotiationAttempts: 0,
                    renegotiationOk: 0,
                    renegotiationErrors: [],
                    sdpDump: [],
                };

                // Teams light-meetings dla anonim joinerow zaraz po joined
                // wystrzeliwuje modal "Are you sure you don't want audio or
                // video?" ktory zaslania caly call UI (w tym kafelek kamery).
                // ESC nie zamyka go (Teams modal nie sluucha global keydown),
                // klikniecie "Continue without" zamknie ale Teams oznaczy
                // sesje jako bez-mediow. Najpewniejsze: ukryj kontener modala
                // i jego backdrop bez klikania zadnego z buttons. Robimy to
                // pare razy bo Teams renderuje modal asynchronicznie.
                function dismissAreYouSure() {
                    const dialogs = document.querySelectorAll('[role="dialog"], [data-tid="modal"]');
                    let hidden = 0;
                    for (const d of dialogs) {
                        const txt = (d.textContent || '').toLowerCase();
                        if (txt.includes("are you sure")
                            || txt.includes("don't want audio")
                            || txt.includes("audio or video")
                            || txt.includes("nie chcesz")) {
                            try {
                                d.style.display = 'none';
                                d.setAttribute('aria-hidden', 'true');
                                // Backdrop tez znika.
                                let p = d.parentElement;
                                while (p && p !== document.body) {
                                    if (p.classList && (p.classList.contains('overlay')
                                        || p.classList.contains('backdrop')
                                        || p.classList.contains('ms-Modal'))) {
                                        p.style.display = 'none';
                                    }
                                    p = p.parentElement;
                                }
                                hidden += 1;
                            } catch (_) {}
                        }
                    }
                    return hidden;
                }
                result.dismissedAreYouSure = dismissAreYouSure() > 0;
                const sels = [
                    '#mic-button',
                    '[data-tid="toggle-mute"]',
                    'button[aria-label*="microphone" i]',
                    'button[aria-label*="mikrofon" i]',
                    'button[aria-label*="unmute" i]',
                ];
                for (const sel of sels) {
                    const btn = document.querySelector(sel);
                    if (btn) {
                        result.unmuteAttempted = true;
                        const pressed = btn.getAttribute('aria-pressed') === 'true'
                            || btn.getAttribute('aria-checked') === 'true';
                        const muted = (btn.getAttribute('aria-label') || '').toLowerCase().includes('unmute');
                        if (pressed || muted) {
                            try { btn.click(); result.unmuteClicked = true; } catch (_) {}
                        }
                        break;
                    }
                }
                const dumpSenders = (pcs) => {
                    const out = [];
                    for (const pc of pcs) {
                        try {
                            for (const s of pc.getSenders()) {
                                const t = s.track;
                                out.push(t ? {
                                    kind: t.kind, id: t.id,
                                    enabled: t.enabled, muted: t.muted,
                                    readyState: t.readyState,
                                } : null);
                            }
                        } catch (_) {}
                    }
                    return out;
                };
                const pcs = (window.__tentaflowPeerConnections instanceof Set)
                    ? Array.from(window.__tentaflowPeerConnections) : [];
                result.sendersBefore = dumpSenders(pcs);
                const mic = window.__tentaflowMicGenerator;
                const vid = window.__tentaflowVideoTrack;
                // Audio sender pojawia sie razem z prejoin getUserMedia, video
                // sender Teams dodaje DOPIERO po SDP renegotiation post-join —
                // potrafi zajac 1-3s. Stad kilka prob z opoznieniem.
                const TRIES = 20;
                const TRY_DELAY_MS = 500;
                const replacedAudioSenders = new WeakSet();
                const replacedVideoSenders = new WeakSet();
                // PC-i, w ktorych wymusilismy direction=sendrecv na video
                // transceiverze. Po petli odpalamy renegocjacje tylko na nich.
                const directionForcedPCs = new Set();
                for (let attempt = 0; attempt < TRIES; attempt++) {
                    // Re-dismiss modal — Teams potrafi go ponownie wyrenderowac.
                    try { dismissAreYouSure(); } catch (_) {}
                    for (const pc of pcs) {
                        for (const s of pc.getSenders()) {
                            if (!s.track) continue;
                            try {
                                if (s.track.kind === 'audio' && mic && !replacedAudioSenders.has(s)) {
                                    await s.replaceTrack(mic);
                                    result.replacedAudio += 1;
                                    replacedAudioSenders.add(s);
                                } else if (s.track.kind === 'video' && vid && !replacedVideoSenders.has(s)) {
                                    // Teams po replaceTrack na video sender
                                    // robi track.stop() jako anti-spoofing.
                                    // Mamy stop() guard w injected JS ktory
                                    // blokuje to dla naszych singletonow,
                                    // wiec video track przezyje replace.
                                    await s.replaceTrack(vid);
                                    result.replacedVideo += 1;
                                    replacedVideoSenders.add(s);
                                    // Teams dla anonim guesta negocjuje video
                                    // transceiver jako recvonly/inactive, wiec
                                    // nawet po replaceTrack encoder nie pali.
                                    // Wymuszamy sendrecv; renegocjacja ponizej.
                                    try {
                                        const tr = pc.getTransceivers().find((t) => t.sender === s);
                                        if (tr && (tr.direction !== 'sendrecv' || tr.currentDirection !== 'sendrecv')) {
                                            tr.direction = 'sendrecv';
                                            directionForcedPCs.add(pc);
                                        }
                                    } catch (e) {
                                        console.warn('[tentaflow] force sendrecv failed:', e);
                                    }
                                }
                            } catch (e) {
                                console.warn('[tentaflow] replaceTrack failed:', e);
                            }
                        }
                    }
                    if (result.replacedAudio > 0 && result.replacedVideo > 0) break;
                    await new Promise((r) => setTimeout(r, TRY_DELAY_MS));
                }
                // Client-initiated renegocjacja na PC-ach gdzie wymusilismy
                // sendrecv. Teams moze odrzucic answer (anonim guest) — to OK,
                // raportujemy stan zamiast cichego fail.
                for (const pc of directionForcedPCs) {
                    result.renegotiationAttempts += 1;
                    try {
                        const offer = await pc.createOffer();
                        await pc.setLocalDescription(offer);
                        result.renegotiationOk += 1;
                    } catch (e) {
                        result.renegotiationErrors.push(String((e && e.message) || e));
                    }
                }
                if (mic && mic.enabled !== true) {
                    try { mic.enabled = true; result.enabledOurTrack = 1; } catch (_) {}
                }
                if (vid && vid.enabled !== true) {
                    try { vid.enabled = true; result.enabledOurTrack += 1; } catch (_) {}
                }
                // Trigger pierwszy frame na encoder. Niektore wersje Chromium
                // dla canvas captureStream nie wysylaja frames dopoki ktos
                // nie wymusi requestFrame() lub dopoki canvas backbuffer nie
                // jest "dirty". Wywolujemy ZARAZ po replaceTrack, w 200ms
                // odstepie pare razy.
                if (vid && typeof vid.requestFrame === 'function') {
                    for (let i = 0; i < 5; i++) {
                        try { vid.requestFrame(); } catch (_) {}
                        await new Promise((r) => setTimeout(r, 200));
                    }
                }
                result.sendersAfter = dumpSenders(pcs);
                // Dump getStats() per video sender — zobaczymy czy Teams
                // faktycznie enkoduje frames z naszego canvas. Jesli
                // framesEncoded > 0 i bytesSent > 0 -> pipeline dziala,
                // problem renderingu po stronie Teams. Jesli 0 -> Teams
                // dropuje track przed encoderem.
                result.videoStats = [];
                for (const pc of pcs) {
                    for (const s of pc.getSenders()) {
                        if (!s.track || s.track.kind !== 'video') continue;
                        try {
                            const stats = await s.getStats();
                            const out = {};
                            stats.forEach(function (rep) {
                                if (rep.type === 'outbound-rtp') {
                                    out.framesEncoded = rep.framesEncoded;
                                    out.framesSent = rep.framesSent;
                                    out.bytesSent = rep.bytesSent;
                                    out.frameWidth = rep.frameWidth;
                                    out.frameHeight = rep.frameHeight;
                                    out.framesPerSecond = rep.framesPerSecond;
                                }
                                if (rep.type === 'media-source') {
                                    out.mediaSourceWidth = rep.width;
                                    out.mediaSourceHeight = rep.height;
                                    out.mediaSourceFrames = rep.frames;
                                    out.mediaSourceFps = rep.framesPerSecond;
                                }
                            });
                            out.trackId = s.track.id;
                            // Direction transceivera jest kluczowa: jesli
                            // currentDirection != sendrecv -> SRTP video nie
                            // leci do Teams niezaleznie od mediaSource.
                            try {
                                for (const t of pc.getTransceivers()) {
                                    if (t.sender === s) {
                                        out.transceiverDirection = t.direction;
                                        out.transceiverCurrentDirection = t.currentDirection;
                                        out.transceiverMid = t.mid;
                                        break;
                                    }
                                }
                            } catch (_) {}
                            result.videoStats.push(out);
                        } catch (_) {}
                    }
                }
                // Wyciaga sekcje m=video z SDP wraz z pierwszymi 5 a= linii
                // dotyczacymi direction. To jedyny pewny artefakt diagnostyczny
                // mowiacy czy Teams negocjuje sendrecv czy recvonly/inactive.
                function extractVideoMSection(sdp) {
                    if (!sdp) return null;
                    const lines = sdp.split(/\r?\n/);
                    let start = -1;
                    for (let i = 0; i < lines.length; i++) {
                        if (lines[i].startsWith('m=video')) { start = i; break; }
                    }
                    if (start < 0) return null;
                    let end = lines.length;
                    for (let i = start + 1; i < lines.length; i++) {
                        if (lines[i].startsWith('m=')) { end = i; break; }
                    }
                    const section = lines.slice(start, end);
                    const directionLines = [];
                    for (const ln of section) {
                        if (/^a=(sendrecv|sendonly|recvonly|inactive)/.test(ln)) {
                            directionLines.push(ln);
                            if (directionLines.length >= 5) break;
                        }
                    }
                    return { mLine: section[0], directionLines: directionLines };
                }
                for (const pc of pcs) {
                    try {
                        result.sdpDump.push({
                            localType: pc.localDescription ? pc.localDescription.type : null,
                            localSdpVideo: extractVideoMSection(pc.localDescription && pc.localDescription.sdp),
                            remoteSdpVideo: extractVideoMSection(pc.remoteDescription && pc.remoteDescription.sdp),
                        });
                    } catch (_) {}
                }
                return JSON.stringify(result);
            })()
            "#,
        )
        .await
        .map(|v| v.into_value::<String>().unwrap_or_default())
        .unwrap_or_default();
    tracing::info!(report = %unmute_report, "Post-join unmute + senders dump");

    Ok((page, observer))
}

/// Polluje wywolanie JS co 500ms az zwroci `true` albo minie timeout.
/// Zwraca Ok(()) jesli się powiodło, Err z timeout opisem w przeciwnym razie.
async fn click_when_present(
    page: &Page,
    js_expr: &str,
    timeout: Duration,
    label: &str,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let ok = page.evaluate(js_expr)
            .await
            .map(|v| v.into_value::<bool>().unwrap_or(false))
            .unwrap_or(false);
        if ok {
            tracing::info!(selector = label, "Element znaleziony i klikniety");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    anyhow::bail!("Timeout czekajac na element: {}", label)
}

/// Klika "Leave" / "Hang up" w aktywnej page Teams i daje stosowi WebRTC
/// chwile (1.5s) na wyslanie BYE/RTCP leave do serwerow konferencji. Bez tego
/// zwykly browser.close()/SIGKILL zostawia bota w ghost-state — Teams nadal
/// pokazuje go w roster przez kilka-kilkanascie sekund po zniknieciu kontenera.
///
/// JS najpierw szuka po `#hangup-button` / `data-tid="hangup-button"`, potem po
/// aria-label (kilka jezykow + warianty), na koncu po widocznym tekscie. Zwraca
/// `true` gdy klikniecie zostalo wykonane.
pub async fn click_leave_in_teams(page: &Page) -> Result<bool> {
    let clicked = page
        .evaluate(
            r#"
            (function() {
                const sels = [
                    '#hangup-button',
                    '[data-tid="hangup-button"]',
                    '[data-tid="call-end"]',
                    'button[aria-label*="leave" i]',
                    'button[aria-label*="hang up" i]',
                    'button[aria-label*="wyjdz" i]',
                    'button[aria-label*="rozlacz" i]',
                    'button[title*="leave" i]',
                ];
                for (const sel of sels) {
                    const btn = document.querySelector(sel);
                    if (btn && !btn.disabled) {
                        try { btn.click(); return true; } catch (_) {}
                    }
                }
                const txtBtn = Array.from(document.querySelectorAll('button'))
                    .find(el => {
                        if (el.disabled) return false;
                        const t = (el.textContent || '').trim().toLowerCase();
                        return t === 'leave' || t === 'hang up'
                            || t === 'wyjdz' || t === 'rozlacz';
                    });
                if (txtBtn) { try { txtBtn.click(); return true; } catch (_) {} }
                return false;
            })()
            "#,
        )
        .await
        .map(|v| v.into_value::<bool>().unwrap_or(false))
        .unwrap_or(false);
    if clicked {
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    Ok(clicked)
}

/// Sprawdza czy Teams przekierowalo na strone logowania (sesja wygasla)
pub async fn detect_auth_expired(page: &Page) -> Result<bool> {
    let url = page.url().await?.unwrap_or_default();

    // Przekierowanie na login.microsoftonline.com oznacza wygasniecie sesji
    let expired = url.contains("login.microsoftonline.com")
        || url.contains("login.live.com");

    if expired {
        tracing::warn!("Autoryzacja Teams wygasla — wymagane odnowienie cookies");
    }

    Ok(expired)
}

/// Uruchamia w tle task forwardujacy komunikaty z Chromium DevTools Console
/// do naszych logow Rust. Pozwala widziec bledy wstrzyknietego JS bez potrzeby
/// otwierania DevTools przez VNC.
async fn spawn_console_forwarder(page: &Page) {
    use chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled;

    match page.event_listener::<EventConsoleApiCalled>().await {
        Ok(mut stream) => {
            tokio::spawn(async move {
                while let Some(event) = stream.next().await {
                    let level = format!("{:?}", event.r#type);
                    let args_text: Vec<String> = event
                        .args
                        .iter()
                        .map(|arg| {
                            arg.value
                                .as_ref()
                                .map(|v| v.to_string())
                                .or_else(|| arg.description.clone())
                                .unwrap_or_else(|| format!("{:?}", arg.r#type))
                        })
                        .collect();
                    let msg = args_text.join(" ");
                    // Filtruj szum (wiadomosci z naszego bridge maja prefix "[tentaflow]")
                    if msg.contains("[tentaflow]") {
                        tracing::info!(target: "js_console", level = %level, "{}", msg);
                    } else if level.to_lowercase().contains("error") {
                        tracing::warn!(target: "js_console", level = %level, "{}", msg);
                    } else {
                        tracing::debug!(target: "js_console", level = %level, "{}", msg);
                    }
                }
            });
            tracing::info!("Forwarder konsoli Chromium uruchomiony");
        }
        Err(e) => {
            tracing::warn!("Nie udalo sie uruchomic forwardera konsoli: {}", e);
        }
    }
}

/// Lokalizuje binarke Chromium/Chrome w zaleznosci od platformy. Sprawdza
/// kolejno: env `TENTAFLOW_CHROMIUM_PATH` → typowe sciezki systemowe → PATH.
/// Zwraca pierwszy istniejacy plik.
///
/// Linux (Docker i natywny): `/usr/bin/chromium`, `chromium-browser`, `google-chrome`.
/// macOS: `/Applications/Google Chrome.app`, Brave, Edge.
/// Windows: standardowe `Program Files` + PATH.
pub fn find_chromium_executable() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(p) = std::env::var("TENTAFLOW_CHROMIUM_PATH") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }

    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Chromium\Application\chrome.exe",
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        ]
    } else {
        // linux + reszta unixow
        &[
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/snap/bin/chromium",
            "/usr/bin/brave-browser",
            "/usr/bin/microsoft-edge",
        ]
    };
    for path in candidates {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    // Fallback — szukaj w PATH (uzywa `which`/`where`)
    let names: &[&str] = if cfg!(target_os = "windows") {
        &["chrome.exe", "chromium.exe", "msedge.exe", "brave.exe"]
    } else {
        &["chromium", "chromium-browser", "google-chrome", "brave-browser"]
    };
    for name in names {
        if let Some(found) = which_in_path(name) {
            return Some(found);
        }
    }
    None
}

fn which_in_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
