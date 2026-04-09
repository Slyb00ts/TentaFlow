// =============================================================================
// Plik: browser.rs
// Opis: Automatyzacja przegladarki Chromium — dolaczanie do spotkan Teams,
//       wykrywanie aktywnego mowcy i stanu autoryzacji.
// =============================================================================

use anyhow::Result;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt;

use crate::config::MeetingConfig;

/// Uruchamia headless Chromium z wczytanymi cookies sesji
pub async fn launch_chromium(config: &MeetingConfig) -> Result<Browser> {
    let browser_config = BrowserConfig::builder()
        .no_sandbox()
        .arg("--use-fake-ui-for-media-stream")
        .arg("--use-fake-device-for-media-stream")
        .arg("--disable-gpu")
        .arg("--headless=new")
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

/// Nawiguje do URL spotkania Teams i dolacza klikajac "Join Now"
pub async fn join_meeting(browser: &Browser, url: &str) -> Result<Page> {
    let page = browser.new_page(url).await?;
    page.wait_for_navigation().await?;

    // TODO: Pelna automatyzacja dolaczania do spotkania:
    // 1. Poczekaj na zaladowanie strony Teams
    // 2. Kliknij "Continue on this browser" jesli pojawi sie prompt
    // 3. Wylacz kamera i mikrofon w pre-join lobby
    // 4. Kliknij "Join now"
    //
    // Selektory CSS Teams czesto sie zmieniaja — wymagaja utrzymania.
    // Przykladowe selektory (moga byc nieaktualne):
    //   - Przycisk "Join now": [data-tid="prejoin-join-button"]
    //   - Przycisk mikrofonu: [data-tid="toggle-mute"]

    tracing::info!(url = url, "Dolaczanie do spotkania (TODO: pelna automatyzacja)");

    Ok(page)
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
