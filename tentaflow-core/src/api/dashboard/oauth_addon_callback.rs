// =============================================================================
// Plik: api/dashboard/oauth_addon_callback.rs
// Opis: REST endpoint GET /oauth/addon/callback?code=...&state=...
//       Obsluga drugiej polowki OAuth code flow addona: consume_oauth_state →
//       exchange_code → fetch_userinfo → encrypt tokens → upsert user_oauth_account.
//       Response to HTML ktory postMessage-uje wynik do window.opener i zamyka popup.
// =============================================================================

use anyhow::Result;

use crate::addon::{oauth, oauth_crypto};
use crate::db::{repository, DbPool};

/// Wynik procesowania callback (wysylany jako postMessage do window.opener w GUI).
pub struct CallbackResult {
    pub ok: bool,
    pub addon_id: String,
    pub provider_id: String,
    pub error: String,
}

/// Parsuje pojedynczy parametr z query stringu (x=a&y=b).
fn parse_query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        let v = it.next().unwrap_or("");
        if k == name {
            return Some(urlencoding::decode(v).ok()?.into_owned());
        }
    }
    None
}

/// Glowny handler callbacku. Deterministyczny — zero panik, zawsze zwraca `CallbackResult`.
pub async fn handle_callback(db: &DbPool, query: &str) -> CallbackResult {
    match handle_callback_inner(db, query).await {
        Ok(r) => r,
        Err(e) => CallbackResult {
            ok: false,
            addon_id: String::new(),
            provider_id: String::new(),
            error: format!("{}", e),
        },
    }
}

async fn handle_callback_inner(db: &DbPool, query: &str) -> Result<CallbackResult> {
    if let Some(err) = parse_query_param(query, "error") {
        let desc = parse_query_param(query, "error_description").unwrap_or_default();
        anyhow::bail!("provider error: {} — {}", err, desc);
    }
    let code =
        parse_query_param(query, "code").ok_or_else(|| anyhow::anyhow!("brak parametru 'code'"))?;
    let state = parse_query_param(query, "state")
        .ok_or_else(|| anyhow::anyhow!("brak parametru 'state'"))?;

    let pending = repository::consume_oauth_state(db, &state)?
        .ok_or_else(|| anyhow::anyhow!("state nieznany lub wygasniety"))?;

    let cfg = repository::get_oauth_config(db, &pending.addon_id, &pending.provider_id)?
        .ok_or_else(|| anyhow::anyhow!("brak konfiguracji OAuth"))?;
    let decl = repository::list_oauth_providers_decl(db, &pending.addon_id)?
        .into_iter()
        .find(|d| d.provider_id == pending.provider_id)
        .ok_or_else(|| anyhow::anyhow!("brak deklaracji providera"))?;

    let master_key = oauth_crypto::ensure_master_key(db)?;
    let client_secret = match cfg.client_secret_encrypted.as_deref() {
        Some(blob) => String::from_utf8(oauth_crypto::decrypt(&master_key, blob)?)?,
        None => String::new(),
    };

    let verifier = if decl.pkce && !pending.code_verifier.is_empty() {
        Some(pending.code_verifier.as_str())
    } else {
        None
    };
    let tokens = oauth::exchange_code(&cfg, &decl, &client_secret, &code, verifier)
        .await
        .map_err(|e| anyhow::anyhow!("exchange_code: {}", e))?;

    let (ext_id, display_name) = oauth::fetch_userinfo(&pending.provider_id, &tokens.access_token)
        .await
        .unwrap_or_else(|_| (String::new(), String::new()));

    let access_enc = oauth_crypto::encrypt(&master_key, tokens.access_token.as_bytes())?;
    let refresh_enc = match tokens.refresh_token.as_deref() {
        Some(rt) if !rt.is_empty() => Some(oauth_crypto::encrypt(&master_key, rt.as_bytes())?),
        _ => None,
    };

    let expires_at = tokens.expires_in_secs.map(|secs| {
        let dt = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    });

    repository::upsert_user_oauth_account(
        db,
        pending.user_id,
        &pending.addon_id,
        &pending.provider_id,
        &ext_id,
        &display_name,
        &access_enc,
        refresh_enc.as_deref(),
        &tokens.token_type,
        tokens.scope.as_deref().unwrap_or(""),
        expires_at.as_deref(),
    )?;

    Ok(CallbackResult {
        ok: true,
        addon_id: pending.addon_id,
        provider_id: pending.provider_id,
        error: String::new(),
    })
}

/// Generuje HTML zwracane klientowi — postMessage do window.opener + close.
/// Payload budowany przez `serde_json::to_string` — odporny na XSS (escape JSON),
/// dodatkowo zamieniamy `</` na `<\/` zeby `</script>` w danych nie zamknal bloku.
pub fn render_html(result: &CallbackResult) -> String {
    let payload = serde_json::json!({
        "type": "tf-oauth-result",
        "ok": result.ok,
        "addon_id": result.addon_id,
        "provider_id": result.provider_id,
        "error": result.error,
    });
    let json_str = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    let json_escaped = json_str.replace("</", "<\\/");

    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>OAuth</title></head><body><script>\
(function() {{\
  var msg = {payload};\
  if (window.opener) {{\
    window.opener.postMessage(msg, window.location.origin);\
  }}\
  window.close();\
}})();\
</script></body></html>",
        payload = json_escaped,
    )
}
