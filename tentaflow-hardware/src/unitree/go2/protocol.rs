// =============================================================================
// File: unitree/go2/protocol.rs
// Purpose: Pure Go2 LAN signaling crypto + framing (data2=2 legacy). NO HTTP, NO
//          WebRTC, NO std::net — compiles to native AND wasm32 so the WASM addon
//          and the native driver share ONE implementation. The caller supplies
//          the transport (raw TCP natively, http.request host fn in the addon).
// =============================================================================

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ecb::cipher::block_padding::Pkcs7;
use ecb::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit as EcbKeyInit};
use md5::{Digest, Md5};
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};

// Static AES-128-GCM key baked into the Unitree apk (AESGCMUtil.keyBytes).
// Used to decrypt the con_notify `data1` on firmware < 1.1.15 (data2=2).
const LEGACY_GCM_KEY: [u8; 16] = [
    232, 86, 130, 189, 22, 84, 155, 0, 142, 4, 166, 104, 43, 179, 235, 227,
];

type Aes256EcbEnc = ecb::Encryptor<aes::Aes256>;
type Aes256EcbDec = ecb::Decryptor<aes::Aes256>;

/// Heartbeat keepalive ping + the substring identifying the robot's reply.
/// Used to configure precise RTT measurement (the robot echoes heartbeats).
pub const HEARTBEAT_TEXT: &str =
    r#"{"type":"heartbeat","topic":"","data":{"timeInStr":"","timeInNum":0}}"#;
pub const HEARTBEAT_MARKER: &str = "\"type\":\"heartbeat\"";

/// Robot identity extracted from the con_notify response.
pub struct RobotIdentity {
    pub robot_pubkey: RsaPublicKey,
    pub path_ending: String,
    pub data2: i64,
    pub pubkey_bits: usize,
}

fn decrypt_data1_legacy(data1_b64: &str) -> Result<String> {
    let raw = B64.decode(data1_b64.trim()).context("data1 is not valid base64")?;
    if raw.len() < 28 {
        bail!("data1 too short for legacy GCM decrypt ({} bytes)", raw.len());
    }
    let n = raw.len();
    let nonce = &raw[n - 28..n - 16];
    let mut ct_with_tag = raw[..n - 28].to_vec();
    ct_with_tag.extend_from_slice(&raw[n - 16..]); // aes-gcm wants ciphertext||tag
    let cipher = Aes128Gcm::new_from_slice(&LEGACY_GCM_KEY).expect("16-byte key");
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce), ct_with_tag.as_ref())
        .map_err(|_| anyhow!("legacy GCM decrypt failed (wrong key or corrupt data1)"))?;
    String::from_utf8(pt).context("decrypted data1 is not UTF-8")
}

/// last 10 chars of data1, pairs, each pair's second char (A..J) → 0..9 index.
fn calc_path_ending(data1: &str) -> String {
    const MAP: [char; 10] = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J'];
    let chars: Vec<char> = data1.chars().collect();
    let start = chars.len().saturating_sub(10);
    let mut out = String::new();
    for pair in chars[start..].chunks(2) {
        if pair.len() > 1 {
            if let Some(idx) = MAP.iter().position(|&c| c == pair[1]) {
                out.push_str(&idx.to_string());
            }
        }
    }
    out
}

fn parse_robot_pubkey(data1: &str) -> Result<RsaPublicKey> {
    if data1.len() < 20 {
        bail!("decrypted data1 too short to hold a pubkey");
    }
    let pem_b64 = &data1[10..data1.len() - 10];
    let der = B64.decode(pem_b64.trim()).context("robot pubkey is not valid base64 DER")?;
    RsaPublicKey::from_public_key_der(&der).context("robot pubkey is not valid SPKI DER")
}

fn aes256_ecb_encrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    Aes256EcbEnc::new_from_slice(key)
        .expect("32-byte key")
        .encrypt_padded_vec::<Pkcs7>(data)
}

fn aes256_ecb_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    Aes256EcbDec::new_from_slice(key)
        .expect("32-byte key")
        .decrypt_padded_vec::<Pkcs7>(data)
        .map_err(|_| anyhow!("AES-256-ECB unpad failed"))
}

fn rsa_encrypt_pkcs1v15(pk: &RsaPublicKey, data: &[u8]) -> Result<Vec<u8>> {
    // RSA (0.9) speaks rand_core 0.6; use its OS CSPRNG directly instead of rand 0.10's
    // ThreadRng (rand_core 0.9), which doesn't implement rsa's `CryptoRngCore`.
    let mut rng = rsa::rand_core::OsRng;
    pk.encrypt(&mut rng, Pkcs1v15Encrypt, data)
        .map_err(|e| anyhow!("RSA encrypt failed: {e}"))
}

/// Random 32-hex-char session key (used verbatim as 32 ASCII bytes = AES-256 key).
pub fn gen_session_key() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

/// Validation response for the data-channel challenge: base64(MD5("UnitreeGo2_"+key)).
pub fn validation_response(challenge: &str) -> String {
    let mut h = Md5::new();
    h.update(format!("UnitreeGo2_{challenge}").as_bytes());
    B64.encode(h.finalize())
}

/// Parse the con_notify HTTP response body (base64(JSON{data1,data2})) into the
/// robot identity. Pure — the caller fetches the body over its own transport.
pub fn parse_con_notify(body_text: &str) -> Result<RobotIdentity> {
    let decoded = B64.decode(body_text.trim()).context("con_notify body is not base64")?;
    let json: serde_json::Value =
        serde_json::from_slice(&decoded).context("con_notify payload is not JSON")?;
    let data2 = json.get("data2").and_then(|v| v.as_i64()).unwrap_or(1);
    let data1_field = json
        .get("data1")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("con_notify JSON missing data1"))?;
    let data1 = match data2 {
        2 => decrypt_data1_legacy(data1_field)?,
        1 => data1_field.to_string(),
        3 => bail!("robot speaks data2=3 (firmware >= 1.1.15) — per-device AES key required"),
        other => bail!("unexpected data2={other} in con_notify"),
    };
    let robot_pubkey = parse_robot_pubkey(&data1)?;
    let pubkey_bits = robot_pubkey.n().bits();
    Ok(RobotIdentity {
        robot_pubkey,
        path_ending: calc_path_ending(&data1),
        data2,
        pubkey_bits,
    })
}

/// Build the con_ing request: returns (path_suffix, body_json). The offer is
/// wrapped in the JSON envelope the robot expects, AES-256-ECB encrypted, and the
/// session key RSA-wrapped with the robot pubkey.
pub fn build_con_ing(
    id: &RobotIdentity,
    session_key_hex: &str,
    offer_sdp: &str,
) -> Result<(String, String)> {
    let key_bytes = session_key_hex.as_bytes(); // 32 ASCII chars = AES-256 key
    let offer_envelope = serde_json::json!({
        "id": "STA_localNetwork",
        "sdp": offer_sdp,
        "type": "offer",
        "token": "",
    })
    .to_string();
    let enc_sdp = B64.encode(aes256_ecb_encrypt(key_bytes, offer_envelope.as_bytes()));
    let enc_key = B64.encode(rsa_encrypt_pkcs1v15(&id.robot_pubkey, session_key_hex.as_bytes())?);
    let body = serde_json::json!({ "data1": enc_sdp, "data2": enc_key }).to_string();
    let path = format!("con_ing_{}", id.path_ending);
    Ok((path, body))
}

/// Decrypt + parse the con_ing answer → SDP. `sdp == "reject"` means the robot
/// is busy (another WebRTC client connected).
pub fn parse_con_ing_answer(resp_text: &str, session_key_hex: &str) -> Result<String> {
    let key_bytes = session_key_hex.as_bytes();
    let answer = aes256_ecb_decrypt(
        key_bytes,
        &B64.decode(resp_text.trim()).context("answer not base64")?,
    )?;
    let answer_str = String::from_utf8(answer).context("SDP answer is not UTF-8")?;
    let answer_json: serde_json::Value =
        serde_json::from_str(&answer_str).context("SDP answer is not JSON")?;
    let sdp = answer_json
        .get("sdp")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("answer JSON missing sdp"))?;
    if sdp == "reject" {
        bail!("robot rejected the offer — another WebRTC client is connected (close the Unitree app)");
    }
    Ok(sdp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes256_ecb_roundtrip() {
        let key = gen_session_key();
        let pt = b"v=0\r\no=- 123 2 IN IP4 0.0.0.0\r\n";
        let ct = aes256_ecb_encrypt(key.as_bytes(), pt);
        let back = aes256_ecb_decrypt(key.as_bytes(), &ct).unwrap();
        assert_eq!(&back, pt);
    }

    #[test]
    fn validation_matches_reference() {
        let got = validation_response("abc");
        let mut h = Md5::new();
        h.update(b"UnitreeGo2_abc");
        assert_eq!(got, B64.encode(h.finalize()));
    }

    #[test]
    fn path_ending_maps_pairs() {
        assert_eq!(calc_path_ending("prefix____xAxBxCxDxE"), "01234");
    }

    #[test]
    fn session_key_is_32_hex() {
        let k = gen_session_key();
        assert_eq!(k.len(), 32);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
