// =============================================================================
// Plik: browser.rs
// Opis: Automatyzacja przegladarki Chromium — dolaczanie do spotkan Teams,
//       wykrywanie aktywnego mowcy i stanu autoryzacji.
// =============================================================================

use std::time::Duration;

use anyhow::Result;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt;

use crate::config::MeetingConfig;

/// Maksymalny czas oczekiwania na dolaczenie do spotkania (5 minut)
const JOIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Interwal pollingu stanu spotkania
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Uruchamia headless Chromium z wczytanymi cookies sesji
pub async fn launch_chromium(config: &MeetingConfig) -> Result<Browser> {
    // Ustawienie preferencji Chromium — auto-grant mikrofon i kamera
    let user_data_dir = "/tmp/chromium-meeting-bot";
    let prefs_dir = format!("{}/Default", user_data_dir);
    std::fs::create_dir_all(&prefs_dir).ok();
    // content_settings: 1 = allow, pattern "*" = wszystkie strony
    std::fs::write(
        format!("{}/Preferences", prefs_dir),
        r#"{"profile":{"content_settings":{"exceptions":{"media_stream_mic":{"*,*":{"setting":1}},"media_stream_camera":{"*,*":{"setting":1}},"notifications":{"*,*":{"setting":2}}}}}}"#,
    ).ok();

    let browser_config = BrowserConfig::builder()
        .chrome_executable("/usr/bin/chromium")
        .with_head()
        .no_sandbox()
        .window_size(1920, 1080)
        .user_data_dir(user_data_dir)
        .arg("--use-fake-ui-for-media-stream")
        .arg("--autoplay-policy=no-user-gesture-required")
        .arg("--enable-features=PulseaudioLoopbackForCast")
        .arg("--disable-gpu")
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

/// Nawiguje do URL spotkania Teams i dolacza jako gosc lub czeka na logowanie VNC
pub async fn join_meeting(browser: &Browser, url: &str, config: &MeetingConfig) -> Result<Page> {
    let page = browser.new_page(url).await?;
    page.wait_for_navigation().await?;

    tracing::info!(url = url, "Nawigacja do spotkania Teams");

    // Czekamy na zaladowanie strony i zamykamy dialogi uprawnien
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Auto-klik na dialogi uprawnien Teams (mikrofon, kamera)
    // Teams wyswietla wlasne dialogi "Allow while visiting the site"
    for _ in 0..5 {
        let clicked = page.evaluate(
            r#"(function() {
                // Chromium permission prompt
                let allow = Array.from(document.querySelectorAll('button'))
                    .find(el => el.textContent.includes('Allow')
                        || el.textContent.includes('Zezwalaj')
                        || el.textContent.includes('Allow while visiting'));
                if (allow) { allow.click(); return true; }
                // Teams dialog "Continue without audio or video"
                let continueBtn = Array.from(document.querySelectorAll('button, a'))
                    .find(el => el.textContent.includes('Continue without audio')
                        || el.textContent.includes('Kontynuuj bez'));
                if (continueBtn) { continueBtn.click(); return true; }
                // Teams wlasny dialog — "Allow" / zamknij
                let close = document.querySelector('.ms-Dialog-button--close, [aria-label="Close"], [data-tid="close-button"]');
                if (close) { close.click(); return true; }
                return false;
            })()"#,
        )
        .await
        .map(|v| v.into_value::<bool>().unwrap_or(false))
        .unwrap_or(false);

        if clicked {
            tracing::info!("Zamknieto dialog uprawnien");
            tokio::time::sleep(Duration::from_secs(1)).await;
        } else {
            break;
        }
    }

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Sprawdzamy czy dostepna jest opcja dolaczenia jako gosc
    // TODO: selektory wymagaja weryfikacji z aktualnym klientem Teams
    let guest_button_exists = page
        .evaluate(
            r#"!!document.querySelector('[data-tid="prejoin-join-button-as-guest"]')
               || !!Array.from(document.querySelectorAll('button, a'))
                    .find(el => el.textContent.includes('Continue without signing in'))"#,
        )
        .await
        .map(|v| v.into_value::<bool>().unwrap_or(false))
        .unwrap_or(false);

    if guest_button_exists {
        tracing::info!(bot_name = %config.bot_name, "Opcja goscia dostepna — dolaczanie anonimowo");

        // Klikamy przycisk "Join as guest" / "Continue without signing in"
        // TODO: selektory do weryfikacji na zywo
        page.evaluate(
            r#"(function() {
                let btn = document.querySelector('[data-tid="prejoin-join-button-as-guest"]');
                if (!btn) {
                    btn = Array.from(document.querySelectorAll('button, a'))
                        .find(el => el.textContent.includes('Continue without signing in'));
                }
                if (btn) btn.click();
            })()"#,
        )
        .await?;

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Wpisujemy nazwe bota w pole imienia
        // TODO: selektory do weryfikacji na zywo
        let name_js = format!(
            r#"(function() {{
                let input = document.querySelector('[data-tid="prejoin-display-name-input"]')
                    || document.querySelector('input[name="displayName"]');
                if (input) {{
                    input.value = '';
                    input.focus();
                    const nativeSet = Object.getOwnPropertyDescriptor(
                        HTMLInputElement.prototype, 'value').set;
                    nativeSet.call(input, "{}");
                    input.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    input.dispatchEvent(new Event('change', {{ bubbles: true }}));
                }}
            }})()"#,
            config.bot_name.replace('"', r#"\""#)
        );
        page.evaluate(name_js).await?;

        tokio::time::sleep(Duration::from_secs(1)).await;

        // Klikamy "Join now"
        // TODO: selektor do weryfikacji na zywo
        page.evaluate(
            r#"(function() {
                let btn = document.querySelector('[data-tid="prejoin-join-button"]');
                if (btn) btn.click();
            })()"#,
        )
        .await?;
    } else {
        // Logowanie wymagane — uzytkownik musi zalogowac sie przez VNC
        tracing::warn!(
            "Logowanie wymagane — uzyj VNC na porcie 5900 zeby zalogowac sie recznie"
        );

        // Czekamy az uzytkownik zaloguje sie i dolaczyl do spotkania przez VNC
        let deadline = tokio::time::Instant::now() + JOIN_TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("Timeout — uzytkownik nie zalogowal sie w ciagu 5 minut");
            }

            if is_in_meeting(&page).await? {
                tracing::info!("Wykryto dolaczenie do spotkania po logowaniu VNC");
                break;
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // Czekamy na potwierdzenie ze jestesmy w spotkaniu
    let deadline = tokio::time::Instant::now() + JOIN_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if is_in_meeting(&page).await? {
            tracing::info!("Pomyslnie dolaczono do spotkania");
            return Ok(page);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    anyhow::bail!("Timeout — nie udalo sie dolaczyc do spotkania w ciagu 5 minut")
}

/// Sprawdza czy jestesmy w aktywnym spotkaniu (wykrywanie elementow UI)
async fn is_in_meeting(page: &Page) -> Result<bool> {
    // TODO: selektory do weryfikacji z aktualnym klientem Teams
    let in_meeting = page
        .evaluate(
            r#"!!document.querySelector('[data-tid="calling-bar"]')
               || !!document.querySelector('[data-tid="toggle-mute"]')
               || !!document.querySelector('[data-tid="hangup-button"]')"#,
        )
        .await
        .map(|v| v.into_value::<bool>().unwrap_or(false))
        .unwrap_or(false);

    Ok(in_meeting)
}

/// Pobiera nazwe aktywnego mowcy z DOM strony Teams
pub async fn get_active_speaker(page: &Page) -> Result<Option<String>> {
    // TODO: Scraping DOM Teams dla aktywnego mowcy
    // Teams oznacza aktywnego mowce podswietleniem ramki wideo
    // i wyswietleniem nazwy w elemencie z odpowiednia klasa CSS.
    //
    // Przykladowy selektor (moze wymagac aktualizacji):
    //   [data-tid="active-speaker-name"]
    //
    // Alternatywa: monitorowanie zdarzen DOM przez MutationObserver
    // wstrzykniety jako skrypt JavaScript.

    let _result = page
        .evaluate("document.querySelector('[data-tid=\"active-speaker-name\"]')?.textContent")
        .await;

    // Na razie zwracamy None — pelna implementacja wymaga testow z Teams
    Ok(None)
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
