// =============================================================================
// File: unitree/go2/session.rs
// Purpose: Go2 session built on the generic WebRtcChannel. Holds the Go2-specific
//          protocol: con_notify/con_ing signaling, the data-channel validation
//          handshake, sport commands and telemetry subscriptions. Drives the
//          channel through the SAME pull/drain model a WASM addon will use, so
//          this doubles as the reference for the Chunk 4 addon port.
// =============================================================================

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::webrtc::{DcMessage, KeepaliveConfig, WebRtcChannel, WebRtcConfig};

use super::handshake;

/// Sport command api_ids (normal mode).
pub mod sport {
    pub const DAMP: u32 = 1001;
    pub const STAND_UP: u32 = 1004;
    pub const STAND_DOWN: u32 = 1005;
    pub const RECOVERY_STAND: u32 = 1006;
    pub const MOVE: u32 = 1008;
    pub const HELLO: u32 = 1016;
}

/// A validated Go2 control session.
pub struct Go2Session {
    channel: WebRtcChannel,
}

impl Go2Session {
    /// Connect over the LAN: build the channel, run con_notify/con_ing signaling,
    /// apply the answer, then complete the data-channel validation handshake.
    pub async fn connect(ip: String) -> Result<Go2Session> {
        let (channel, offer_sdp) = WebRtcChannel::create(WebRtcConfig {
            data_channel_label: "data".to_string(),
            want_video: true,
            disable_mdns: true, // Go2 signals by IP and rejects `.local` candidates
            // Heartbeat keepalive → precise RTT to the robot (it echoes heartbeats).
            keepalive: Some(KeepaliveConfig {
                text: json!({"type":"heartbeat","topic":"","data":{"timeInStr":"","timeInNum":0}})
                    .to_string(),
                interval: Duration::from_millis(1000),
                response_marker: "\"type\":\"heartbeat\"".to_string(),
            }),
            ..WebRtcConfig::default()
        })
        .await?;

        let ip_sig = ip.clone();
        let answer_sdp = tokio::task::spawn_blocking(move || -> Result<String> {
            let notify = handshake::con_notify(&ip_sig)?;
            let key = handshake::gen_session_key();
            handshake::send_offer(&ip_sig, &notify, &key, &offer_sdp)
        })
        .await
        .context("signaling task panicked")??;

        channel.set_answer(answer_sdp).await?;

        let session = Go2Session { channel };
        session.run_validation(Duration::from_secs(20)).await?;
        Ok(session)
    }

    /// Validation handshake via the pull model: drain inbound, answer the
    /// challenge (base64(md5("UnitreeGo2_"+key))), wait for "Validation Ok.".
    async fn run_validation(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            for msg in self.channel.dc_drain().await {
                if let DcMessage::Text(text) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("type").and_then(|t| t.as_str()) == Some("validation") {
                            let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
                            if data == "Validation Ok." {
                                return Ok(());
                            }
                            let resp = handshake::validation_response(data);
                            self.channel
                                .dc_send_text(
                                    json!({"type":"validation","topic":"","data":resp}).to_string(),
                                )
                                .await?;
                        }
                    }
                }
            }
            if Instant::now() >= deadline {
                bail!("data channel validation timed out (robot busy / another client?)");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Send a sport-mode request (api_id from `sport`; parameter JSON string or "").
    pub async fn send_sport(&self, api_id: u32, parameter: &str) -> Result<()> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let id = (now_ms % 2_147_483_648) + (rand::random::<u16>() as u64 % 1000);
        let payload = json!({
            "header": { "identity": { "id": id, "api_id": api_id } },
            "parameter": parameter,
        });
        self.channel
            .dc_send_text(json!({"type":"req","topic":"rt/api/sport/request","data":payload}).to_string())
            .await
    }

    pub async fn subscribe(&self, topic: &str) -> Result<()> {
        self.channel
            .dc_send_text(json!({"type":"subscribe","topic":topic}).to_string())
            .await
    }

    pub async fn switch_video(&self, on: bool) -> Result<()> {
        self.channel
            .dc_send_text(json!({"type":"vid","topic":"","data": if on {"on"} else {"off"}}).to_string())
            .await
    }

    /// Drain pending inbound data-channel messages (telemetry, responses).
    pub async fn drain(&self) -> Vec<DcMessage> {
        self.channel.dc_drain().await
    }

    /// Latest transport round-trip latency (ms) to the robot.
    pub fn rtt_ms(&self) -> Option<f64> {
        self.channel.rtt_ms()
    }

    /// Take the inbound video as an H.264 Annex-B byte stream (single consumer).
    /// Call before `switch_video(true)`.
    pub fn take_video_h264(&self) -> Option<mpsc::Receiver<bytes::Bytes>> {
        self.channel.take_h264_rx()
    }

    pub async fn close(&self) -> Result<()> {
        self.channel.close().await
    }
}
