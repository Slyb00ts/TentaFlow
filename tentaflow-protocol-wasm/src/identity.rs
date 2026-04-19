// =============================================================================
// Plik: tentaflow-protocol-wasm/src/identity.rs
// Opis: Browser-side Ed25519 keypair persistowany w localStorage. Browser jest
//       traktowany jak peer iroh: ma wlasny NodeId wyprowadzony z klucza
//       publicznego Ed25519. Przy pierwszym uruchomieniu generowany, zapisany
//       hex do localStorage pod kluczem `tentaflow_browser_keypair`.
// Przyklad:
//   import initWasm, { browserNodeId, browserSignHex } from './wasm_glue.js';
//   await initWasm();
//   const nodeIdHex = browserNodeId();   // 64-znakowy hex Ed25519 pub
//   const sigHex = browserSignHex(new Uint8Array([1,2,3]));
// =============================================================================

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand_core_06::OsRng;
use wasm_bindgen::prelude::*;
use web_sys::window;

const STORAGE_KEY: &str = "tentaflow_browser_keypair";

fn load_or_generate_keypair() -> Result<SigningKey, String> {
    let storage = window()
        .and_then(|w| w.local_storage().ok().flatten())
        .ok_or_else(|| "localStorage niedostepny".to_string())?;

    if let Some(hex_str) = storage.get_item(STORAGE_KEY).ok().flatten() {
        if !hex_str.is_empty() {
            let bytes = hex::decode(&hex_str).map_err(|e| format!("hex decode: {e}"))?;
            let key_bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| "klucz musi miec 32 bajty".to_string())?;
            return Ok(SigningKey::from_bytes(&key_bytes));
        }
    }

    // Wygeneruj nowy keypair i zapisz.
    let key = SigningKey::generate(&mut OsRng);
    let hex_str = hex::encode(key.to_bytes());
    storage
        .set_item(STORAGE_KEY, &hex_str)
        .map_err(|e| format!("set_item: {e:?}"))?;
    Ok(key)
}

/// Zwraca hex Ed25519 public key (64 znaki). Generuje keypair przy pierwszym
/// uzyciu i persistuje w localStorage.
#[wasm_bindgen(js_name = browserNodeId)]
pub fn browser_node_id() -> Result<String, JsError> {
    let key = load_or_generate_keypair().map_err(|e| JsError::new(&e))?;
    let vk: VerifyingKey = key.verifying_key();
    Ok(hex::encode(vk.to_bytes()))
}

/// Podpisuje `data` kluczem prywatnym browser-a. Zwraca signature (64 bajty)
/// jako hex string (128 znakow).
#[wasm_bindgen(js_name = browserSignHex)]
pub fn browser_sign_hex(data: &[u8]) -> Result<String, JsError> {
    let key = load_or_generate_keypair().map_err(|e| JsError::new(&e))?;
    let sig = key.sign(data);
    Ok(hex::encode(sig.to_bytes()))
}

/// Podpisuje `data` i zwraca raw bajty podpisu (64 B).
#[wasm_bindgen(js_name = browserSign)]
pub fn browser_sign(data: &[u8]) -> Result<Vec<u8>, JsError> {
    let key = load_or_generate_keypair().map_err(|e| JsError::new(&e))?;
    let sig = key.sign(data);
    Ok(sig.to_bytes().to_vec())
}

/// Usuwa keypair z localStorage (wylogowanie/reset tozsamosci browser).
/// Kolejne wywolanie `browserNodeId` wygeneruje nowy keypair.
#[wasm_bindgen(js_name = browserResetIdentity)]
pub fn browser_reset_identity() -> Result<(), JsError> {
    let storage = window()
        .and_then(|w| w.local_storage().ok().flatten())
        .ok_or_else(|| JsError::new("localStorage niedostepny"))?;
    storage
        .remove_item(STORAGE_KEY)
        .map_err(|e| JsError::new(&format!("remove_item: {e:?}")))?;
    Ok(())
}
