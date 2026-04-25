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
    MeetingEventPayload, LIFECYCLE_BROWSER_LAUNCHED, LIFECYCLE_JOINED, LIFECYCLE_JOINING,
    LIFECYCLE_LOBBY_WAITING,
    LIFECYCLE_NAVIGATING, LIFECYCLE_PREJOIN_READY,
};
use tokio::sync::Mutex;

use crate::config::MeetingConfig;
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

/// Maksymalny czas oczekiwania na dolaczenie do spotkania (5 minut)
const JOIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Interwal pollingu stanu spotkania
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Skrypt wstrzykiwany do strony Teams — przechwytuje audio przez captureStream()
/// i wysyla PCM do Rust przez WebSocket ws://127.0.0.1:9999/bridge. Obsluguje tez
/// mic injection przez monkey-patch getUserMedia + MediaStreamTrackGenerator.
const AUDIO_BRIDGE_JS: &str = include_str!("browser_inject.js");

/// Uruchamia headless Chromium z wczytanymi cookies sesji.
/// Kazde uruchomienie tworzy UNIKALNY user_data_dir — bez tego Chromium
/// przy drugim launch'u blokuje sie na "profile locked".
pub async fn launch_chromium(config: &MeetingConfig) -> Result<Browser> {
    let instance_id = uuid::Uuid::new_v4().to_string();
    let user_data_dir = format!("/tmp/chromium-meeting-bot-{}", &instance_id[..8]);
    let user_data_dir = user_data_dir.as_str();
    let prefs_dir = format!("{}/Default", user_data_dir);
    std::fs::create_dir_all(&prefs_dir).ok();
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
        format!("{}/Preferences", prefs_dir),
        r#"{"profile":{"content_settings":{"exceptions":{"media_stream_mic":{"*,*":{"setting":1}},"media_stream_camera":{"*,*":{"setting":2}},"notifications":{"*,*":{"setting":2}},"loopback_network":{"https://teams.microsoft.com:443,*":{"setting":1},"https://teams.live.com:443,*":{"setting":1}}}}}}"#,
    ).ok();

    let browser_config = BrowserConfig::builder()
        .chrome_executable("/usr/bin/chromium")
        .with_head()
        .no_sandbox()
        .window_size(1920, 1080)
        .user_data_dir(user_data_dir)
        .arg("use-fake-ui-for-media-stream")
        .arg("autoplay-policy=no-user-gesture-required")
        .arg("enable-features=MediaStreamTrackGenerator")
        .arg("disable-gpu")
        .arg("disable-blink-features=AutomationControlled")
        .arg("ignore-certificate-errors")
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
) -> Result<Page> {
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

    page.evaluate_on_new_document(AUDIO_BRIDGE_JS).await
        .map_err(|e| anyhow::anyhow!("Blad evaluate_on_new_document: {}", e))?;
    tracing::info!("Zarejestrowano init script (audio bridge) dla wszystkich dokumentow");

    // Forward JS console messages z Chromium do naszych logow Rust
    // Dzieki temu bledy ze wstrzyknietego browser_inject.js widac w docker logs
    spawn_console_forwarder(&page).await;

    // NIE wywolujemy wait_for_navigation — Teams robi chain redirectow
    // (launcher.html → v2 → light-meetings) i event 'navigation done' nigdy
    // nie przychodzi. Nasz polling click_when_present sam poczeka na DOM.
    emit_lifecycle(router, meeting_key, LIFECYCLE_NAVIGATING, None).await;
    page.goto(url).await?;
    tracing::info!(url = url, "Nawigacja do spotkania Teams rozpoczeta");

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

    // Czekamy na potwierdzenie ze jestesmy w spotkaniu (po auto-join lub manualnym VNC).
    // Lobby state needs its own emit so the dashboard can distinguish "waiting
    // to be admitted" from "in the meeting"; we also de-bounce the lobby emit
    // so it only fires once per polling burst.
    let mut emitted_lobby = false;
    let deadline = tokio::time::Instant::now() + JOIN_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match detect_meeting_progress(&page).await? {
            MeetingProgress::InMeeting => {
                tracing::info!("Pomyslnie dolaczono do spotkania");
                emit_lifecycle(router, meeting_key, LIFECYCLE_JOINED, None).await;
                return Ok(page);
            }
            MeetingProgress::Lobby if !emitted_lobby => {
                tracing::info!("Bot w lobby — czekamy na admit hosta");
                emit_lifecycle(router, meeting_key, LIFECYCLE_LOBBY_WAITING, None).await;
                emitted_lobby = true;
            }
            _ => {}
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    anyhow::bail!("Timeout — nie udalo sie dolaczyc do spotkania w ciagu 5 minut")
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

/// Three-state classification of where the bot's Chromium currently sits in
/// the Teams join flow. We used to flatten this into a single is_in_meeting()
/// boolean, but the call controls (#hangup-button, #mic-button) are also
/// rendered inside the lobby waiting screen — flipping LIFECYCLE_JOINED the
/// moment those appeared made the dashboard report LIVE while the bot was
/// still waiting to be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeetingProgress {
    /// No call surface yet — still in prejoin / device picker / loading.
    NotYet,
    /// Lobby panel is showing. The "Waiting for someone to let you in"
    /// screen — the bot has clicked Join, the meeting page acknowledged
    /// it, but the host has not admitted us. Mic/Hangup buttons exist but
    /// are wired to lobby controls.
    Lobby,
    /// Real call surface — the stage layout is live and the bot is in the
    /// meeting proper.
    InMeeting,
}

async fn detect_meeting_progress(page: &Page) -> Result<MeetingProgress> {
    let raw = page
        .evaluate(
            r#"
            (function() {
                // Lobby probes first — Teams light-meetings shows a 'When the
                // meeting starts' / 'Someone will let you in soon' panel and
                // tags it with data-tid="prejoin-meet-now" or text on the
                // page. Classic Teams uses data-tid="lobby-screen" or aria-live
                // 'You're in the lobby'.
                const bodyText = (document.body && document.body.innerText) || '';
                const lobbyPhrases = [
                    'someone in the meeting will let you in',
                    "you're in the lobby",
                    'someone will let you in',
                    'when you are admitted',
                    'when you\'re admitted',
                    'waiting for the host',
                    'kogos w spotkaniu wpusci',
                    'oczekiwanie na wpuszczenie',
                    'gdy zostaniesz wpuszczony',
                ];
                const lower = bodyText.toLowerCase();
                const phraseHit = lobbyPhrases.some((p) => lower.indexOf(p) !== -1);
                const lobbyTids = ['lobby-screen', 'lobby-wait-screen',
                    'prejoin-meeting-info', 'lobby-waiting-room'];
                const lobbyTidHit = lobbyTids.some((t) =>
                    document.querySelector('[data-tid="' + t + '"]'));
                const inLobby = phraseHit || lobbyTidHit;

                // True call indicators — the stage wrapper plus visible
                // remote tiles or the participant roster reporting >1.
                const stage = document.querySelector('[data-tid="MixedStage-wrapper"]')
                    || document.querySelector('[data-tid="stage-layouts-renderer"]');
                let stageTiles = 0;
                if (stage) {
                    stageTiles = stage.querySelectorAll('[data-tid][data-stream-type]').length;
                }
                // Roster button shows participant badge; lobby usually shows '1' (only us).
                const rosterBadge = document.querySelector('#roster-button [data-tid="toolbar-item-badge"]');
                const rosterCount = rosterBadge ? parseInt(rosterBadge.textContent.trim(), 10) || 0 : 0;
                const remoteAudio = (function() {
                    const audios = document.querySelectorAll('audio');
                    for (const a of audios) {
                        if (a.srcObject && a.srcObject.getAudioTracks
                            && a.srcObject.getAudioTracks().length > 0) return true;
                    }
                    return false;
                })();

                // Real meeting if stage exists with multiple tiles, OR roster
                // shows more than just us, OR remote audio is wired up.
                const inCall = (!!stage && stageTiles >= 2)
                    || rosterCount >= 2
                    || remoteAudio;

                // Any prejoin/call surface at all.
                const anySurface = !!stage
                    || !!document.querySelector('#hangup-button')
                    || !!document.querySelector('#mic-button')
                    || !!document.querySelector('[data-tid="hangup-button"]')
                    || !!document.querySelector('[data-tid="calling-right-side-panel"]');

                if (inCall) return 'in_meeting';
                if (inLobby) return 'lobby';
                if (anySurface) return 'lobby';
                return 'not_yet';
            })()
            "#,
        )
        .await
        .map(|v| v.into_value::<String>().unwrap_or_default())
        .unwrap_or_default();
    Ok(match raw.as_str() {
        "in_meeting" => MeetingProgress::InMeeting,
        "lobby" => MeetingProgress::Lobby,
        _ => MeetingProgress::NotYet,
    })
}

/// Backwards-compatible alias for callers that only need the boolean.
async fn is_in_meeting(page: &Page) -> Result<bool> {
    Ok(detect_meeting_progress(page).await? == MeetingProgress::InMeeting)
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
