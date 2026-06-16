// =============================================================================
// File: handshake.rs
// Purpose: Native HTTP transport for the Go2 LAN signaling (con_notify / con_ing
//          over raw TCP). All crypto/framing lives in `protocol` (shared with the
//          WASM addon, which uses the http.request host fn instead of std::net).
// =============================================================================

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

pub use super::protocol::{gen_session_key, validation_response, RobotIdentity};
use super::protocol;

/// Minimal HTTP/1.0 POST over raw TCP. The robot's embedded signaling server
/// does not handle hyper's HTTP/1.1 keep-alive on bodied POSTs (con_ing closes
/// the connection mid-request), so we speak HTTP/1.0 + `Connection: close` and
/// read the whole response until EOF — exactly what the firmware expects.
fn http_post(ip: &str, port: u16, path: &str, content_type: Option<&str>, body: &[u8]) -> Result<String> {
    let mut stream = TcpStream::connect((ip, port)).context("tcp connect to robot")?;
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    let mut head = format!("POST {path} HTTP/1.0\r\nHost: {ip}\r\nConnection: close\r\n");
    if let Some(ct) = content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).context("read robot response")?;
    let (idx, sep_len) = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .ok_or_else(|| {
            anyhow!(
                "no HTTP header terminator ({} bytes): {:?}",
                raw.len(),
                String::from_utf8_lossy(&raw[..raw.len().min(256)])
            )
        })?;
    let header = String::from_utf8_lossy(&raw[..idx]);
    let body_bytes = &raw[idx + sep_len..];
    let code = header
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    if code != 200 {
        bail!("HTTP {code} from {path}: {}", String::from_utf8_lossy(body_bytes));
    }
    Ok(String::from_utf8_lossy(body_bytes).trim().to_string())
}

/// POST con_notify and parse the robot identity (legacy data2=2 path).
pub fn con_notify(ip: &str) -> Result<RobotIdentity> {
    let body = http_post(ip, 9991, "/con_notify", None, b"").context("con_notify failed")?;
    protocol::parse_con_notify(&body)
}

/// Send the SDP offer via con_ing_<path>, return the decrypted SDP answer.
pub fn send_offer(
    ip: &str,
    id: &RobotIdentity,
    session_key_hex: &str,
    offer_sdp: &str,
) -> Result<String> {
    let (path, body) = protocol::build_con_ing(id, session_key_hex, offer_sdp)?;
    let text = http_post(
        ip,
        9991,
        &format!("/{path}"),
        Some("application/x-www-form-urlencoded"),
        body.as_bytes(),
    )
    .context("con_ing failed (robot busy / another WebRTC client?)")?;
    protocol::parse_con_ing_answer(&text, session_key_hex)
}
