// =============================================================================
// Plik: net/iroh/pairing.rs
// Opis: Handler iroh protokolu parowania (ALPN `tentaflow-pairing/v1`). Przyj-
//       muje polaczenia inicjatora na pairing, odczytuje `PairingRequest`
//       z bidirectional streamu, weryfikuje `pin_proof` przy uzyciu
//       `MeshSecurity::derive_pin_proof`, zapisuje zaufanego peera, wysyla
//       `PairingConfirm` z wlasnym public_key_hex lub `PairingReject` gdy
//       PIN albo HKDF proof nie zgadza sie. Strumien binarny: len-prefixed
//       JSON dla requestu/responsu; rkyv zarezerwowane dla mesh core.
// =============================================================================

use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::protocol::ProtocolHandler;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::mesh::security::MeshSecurity;

const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Zadanie parowania wyslane przez inicjatora — node B → node A.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingRequest {
    /// Hex-enkodowany EndpointId noda B (Ed25519 pub).
    pub sender_node_id: String,
    /// Kombinowany klucz publiczny noda B (128 hex — Ed25519 + X25519).
    pub sender_public_key_hex: String,
    /// Hostname noda B — do wyswietlenia w logu zaufanych.
    pub sender_hostname: String,
    /// Hex-enkodowany 32-bajtowy pin_proof wyprowadzony przez
    /// `MeshSecurity::derive_pin_proof(pin, sender_node_id, receiver_node_id)`.
    pub pin_proof_hex: String,
}

/// Odpowiedz noda A potwierdzajaca lub odrzucajaca parowanie.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingResponse {
    Confirm {
        /// Klucz publiczny noda A do zapisu po stronie noda B (128 hex).
        receiver_public_key_hex: String,
        /// Hostname noda A.
        receiver_hostname: String,
        /// Lista (node_id, public_key_hex) juz zaufanych przez A.
        trusted_keys: Vec<(String, String)>,
    },
    Reject {
        reason: String,
    },
}

/// Obsluga przychodzacego parowania nad iroh stream.
#[derive(Clone)]
pub struct PairingHandler {
    security: Arc<MeshSecurity>,
    local_hostname: String,
}

impl std::fmt::Debug for PairingHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingHandler")
            .field("local_hostname", &self.local_hostname)
            .finish_non_exhaustive()
    }
}

impl PairingHandler {
    pub fn new(security: Arc<MeshSecurity>, local_hostname: impl Into<String>) -> Self {
        Self {
            security,
            local_hostname: local_hostname.into(),
        }
    }

    /// Weryfikacja requestu i zbudowanie odpowiedzi. Wydzielone zeby test
    /// mogl sprawdzic logike bez iroh endpoint.
    pub fn verify_request(&self, req: &PairingRequest) -> PairingResponse {
        if !self.security.check_pin_rate_limit(&req.sender_node_id) {
            return PairingResponse::Reject {
                reason: "przekroczony limit prob PIN".into(),
            };
        }

        let pending_pin = match self.security.get_pending_pin(&req.sender_node_id) {
            Ok(Some(pin)) => pin,
            Ok(None) => {
                return PairingResponse::Reject {
                    reason: "brak oczekujacego parowania dla tego noda".into(),
                };
            }
            Err(e) => {
                return PairingResponse::Reject {
                    reason: format!("blad bazy: {e}"),
                };
            }
        };

        if req.sender_public_key_hex.len() != 128 {
            return PairingResponse::Reject {
                reason: "klucz publiczny musi miec 128 hex znakow".into(),
            };
        }

        // X25519 pub noda B to druga polowa klucza publicznego.
        let remote_x25519_pub_hex = &req.sender_public_key_hex[64..128];
        let local_node_id = self.security.ed25519_public_key_hex();

        let expected_proof = match self.security.derive_pin_proof(
            remote_x25519_pub_hex,
            &pending_pin,
            &local_node_id,
            &req.sender_node_id,
        ) {
            Ok(p) => p,
            Err(e) => {
                return PairingResponse::Reject {
                    reason: format!("nie udalo sie wyprowadzic pin_proof: {e}"),
                };
            }
        };

        let actual_proof = match hex::decode(&req.pin_proof_hex) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                return PairingResponse::Reject {
                    reason: "pin_proof_hex musi byc 32-bajtowym hex".into(),
                };
            }
        };

        if !constant_time_eq(&expected_proof, &actual_proof) {
            return PairingResponse::Reject {
                reason: "pin_proof nie zgadza sie — niewlasciwy PIN lub klucz".into(),
            };
        }

        if let Err(e) = self.security.confirm_pairing(
            &req.sender_node_id,
            &req.sender_public_key_hex,
            &req.sender_hostname,
            "iroh-pairing",
        ) {
            return PairingResponse::Reject {
                reason: format!("zapis trusted_node nieudany: {e}"),
            };
        }

        info!(
            peer = %req.sender_node_id,
            hostname = %req.sender_hostname,
            "Parowanie zaakceptowane nad iroh transportem"
        );

        PairingResponse::Confirm {
            receiver_public_key_hex: self.security.public_key_hex(),
            receiver_hostname: self.local_hostname.clone(),
            trusted_keys: self.security.get_all_trusted_keys(),
        }
    }

    async fn handle_stream(
        &self,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
    ) -> anyhow::Result<()> {
        // Format: [u32 BE len][JSON PairingRequest].
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| anyhow::anyhow!("pairing: read len: {e}"))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            anyhow::bail!("pairing frame too large: {} bytes", len);
        }

        let mut body = vec![0u8; len];
        recv.read_exact(&mut body)
            .await
            .map_err(|e| anyhow::anyhow!("pairing: read body: {e}"))?;

        let request: PairingRequest = serde_json::from_slice(&body)
            .map_err(|e| anyhow::anyhow!("pairing: JSON decode: {e}"))?;

        let response = self.verify_request(&request);
        let response_bytes = serde_json::to_vec(&response)
            .map_err(|e| anyhow::anyhow!("pairing: JSON encode response: {e}"))?;

        send.write_all(&(response_bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("pairing: write len: {e}"))?;
        send.write_all(&response_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("pairing: write body: {e}"))?;
        send.finish()
            .map_err(|e| anyhow::anyhow!("pairing: finish send stream: {e}"))?;

        Ok(())
    }
}

impl ProtocolHandler for PairingHandler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let (send, recv) = match connection.accept_bi().await {
            Ok(v) => v,
            Err(e) => {
                warn!("pairing: accept_bi nieudane: {}", e);
                return Err(iroh::protocol::AcceptError::from_err(e));
            }
        };

        if let Err(e) = self.handle_stream(send, recv).await {
            warn!("pairing: obsluga streamu nieudana: {}", e);
        }
        Ok(())
    }
}

/// Klient uruchamiany przez inicjatora (node B): laczy sie do node A przez
/// `endpoint.connect(receiver_id, ALPN_PAIRING)`, buduje `PairingRequest`,
/// wysyla, odczytuje odpowiedz. Po `Confirm` zapisuje A jako trusted + sync
/// trusted_keys z odpowiedzi.
pub async fn initiate_pairing_over_iroh(
    endpoint: &iroh::Endpoint,
    receiver_id: iroh::EndpointId,
    receiver_node_id_hex: &str,
    security: &MeshSecurity,
    pin: &str,
    local_hostname: &str,
) -> anyhow::Result<()> {
    let sender_node_id = security.ed25519_public_key_hex();

    let pin_proof_bytes = security.derive_pin_proof(
        // Remote X25519 pub to polowa ich public_key_hex — ale tu jeszcze jej
        // nie mamy. Musimy derywowac PIN proof od PO odebraniu klucza A, czyli
        // faktyczny flow to dwuetapowy: najpierw wymiana kluczy, potem PIN
        // proof. W tej implementacji PairingRequest niesie dane od razu —
        // wiec zakladamy ze receiver_node_id + receiver_x25519_pub sa znane
        // wczesniej (np. z QR code razem z PIN-em).
        &receiver_node_id_hex[64..128],
        pin,
        &sender_node_id,
        // Uwaga: receiver_node_id_hex zawiera 128 hex (Ed25519+X25519), ale
        // derive_pin_proof przyjmuje tylko ed25519 czesc jako `remote_node_id`
        // w info_buf. Bezpieczniej jest uzywac pelnego hex — protokol musi
        // byc spojny po obu stronach. Implementacja handlera (verify_request)
        // uzywa `req.sender_node_id` (pelnego Ed25519 hex) — skoro my jestesmy
        // receiver dla handlera, a sender dla requestu, uzywamy ich Ed25519.
        &receiver_node_id_hex[..64],
    )?;
    let pin_proof_hex = hex::encode(pin_proof_bytes);

    let request = PairingRequest {
        sender_node_id: sender_node_id.clone(),
        sender_public_key_hex: security.public_key_hex(),
        sender_hostname: local_hostname.to_string(),
        pin_proof_hex,
    };
    let body = serde_json::to_vec(&request)
        .map_err(|e| anyhow::anyhow!("pairing: encode request: {e}"))?;

    let connection = endpoint
        .connect(receiver_id, super::ALPN_PAIRING)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: connect: {e}"))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("pairing: open_bi: {e}"))?;

    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("pairing: write len: {e}"))?;
    send.write_all(&body)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: write body: {e}"))?;
    send.finish()
        .map_err(|e| anyhow::anyhow!("pairing: finish: {e}"))?;

    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: read response len: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("pairing response too large: {} bytes", len);
    }
    let mut resp_bytes = vec![0u8; len];
    recv.read_exact(&mut resp_bytes)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: read response body: {e}"))?;

    let response: PairingResponse = serde_json::from_slice(&resp_bytes)
        .map_err(|e| anyhow::anyhow!("pairing: JSON decode response: {e}"))?;

    match response {
        PairingResponse::Confirm {
            receiver_public_key_hex,
            receiver_hostname,
            trusted_keys,
        } => {
            // Zapisujemy receiver jako trusted.
            security
                .add_trusted_key(
                    &receiver_node_id_hex[..64],
                    &receiver_public_key_hex,
                    &receiver_hostname,
                )
                .map_err(|e| anyhow::anyhow!("add_trusted_key receiver: {e}"))?;
            // Sync trusted_keys otrzymane od receiver.
            for (nid, pk) in trusted_keys {
                let _ = security.add_trusted_key(&nid, &pk, "mesh-sync");
            }
            Ok(())
        }
        PairingResponse::Reject { reason } => {
            anyhow::bail!("pairing rejected: {reason}")
        }
    }
}

/// Porownanie dwoch 32-bajtowych tablic w czasie stalym (odporne na timing attack).
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant_time_eq_sanity() {
        let a = [0u8; 32];
        let mut b = [0u8; 32];
        assert!(constant_time_eq(&a, &b));
        b[5] = 1;
        assert!(!constant_time_eq(&a, &b));
    }

    #[test]
    fn constant_time_eq_smoke() {
        constant_time_eq_sanity();
    }
}
