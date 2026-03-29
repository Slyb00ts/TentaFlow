// =============================================================================
// Plik: api/dashboard/ws_metrics.rs
// Opis: Obsluga WebSocket do streamowania metryk dashboardu co sekunde.
// =============================================================================

use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use futures::{SinkExt, StreamExt};
use crate::metrics::RouterMetrics;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use serde::Serialize;
use tracing::debug;

/// Wiadomosc wysylana do frontendu przez WebSocket.
/// Mapuje pola z MetricsSnapshot na format oczekiwany przez ws-client.js.
#[derive(Serialize)]
struct WsDashboardMessage {
    tokens_in_per_sec: u64,
    tokens_out_per_sec: u64,
    active_services: usize,
    active_requests: u64,
    total_requests: u64,
    total_errors: u64,
    avg_latency_ms: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
}

/// Obsluguje upgrade'owany WebSocket - wysyla metryki co 1s
pub async fn handle_ws_connection<S>(stream: S, metrics: Arc<RouterMetrics>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let ws = WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;

    let (mut sink, mut stream) = ws.split();

    let mut ticker = interval(Duration::from_secs(1));
    let mut ping_ticker = interval(Duration::from_secs(15));

    debug!("WebSocket metryki: nowe polaczenie");

    loop {
        tokio::select! {
            // Ping co 15s — utrzymuje polaczenie przy zyciu przez proxy/load balancery
            _ = ping_ticker.tick() => {
                if sink.send(Message::Ping(vec![1, 2, 3, 4])).await.is_err() {
                    break;
                }
            }
            _ = ticker.tick() => {
                let snapshot = metrics.snapshot();

                // Oblicz srednia latencje ze wszystkich serwisow
                let stats = &snapshot.service_stats;
                let avg_latency = if stats.is_empty() {
                    0
                } else {
                    let sum: u64 = stats.iter().map(|s| s.avg_latency_ms).sum();
                    sum / stats.len() as u64
                };

                let msg = WsDashboardMessage {
                    tokens_in_per_sec: snapshot.input_tokens_per_second,
                    tokens_out_per_sec: snapshot.tokens_per_second,
                    active_services: snapshot.active_services as usize,
                    active_requests: snapshot.active_requests,
                    total_requests: snapshot.total_requests,
                    total_errors: snapshot.total_errors,
                    avg_latency_ms: avg_latency,
                    total_input_tokens: snapshot.total_input_tokens,
                    total_output_tokens: snapshot.total_output_tokens,
                };

                let json = serde_json::to_string(&msg).unwrap_or_default();
                if sink.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        if sink.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WebSocket metryki: polaczenie zamkniete");
}
