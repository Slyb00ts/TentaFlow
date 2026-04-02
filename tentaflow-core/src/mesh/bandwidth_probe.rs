// =============================================================================
// Plik: mesh/bandwidth_probe.rs
// Opis: Probe przepustowosci sieci — multi-stream TCP z nonce auth.
//       Pomiar po stronie klienta z flush+shutdown (uwzglednia bufory TCP).
//       Latency mierzona jako RTT ping-pong na pierwszym strumieniu.
// =============================================================================

use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration, Instant};
use std::net::SocketAddr;
use anyhow::{Result, anyhow};

const NONCE_SIZE: usize = 32;
const CHUNK_SIZE: usize = 4 * 1024 * 1024;
const SERVER_TIMEOUT_SECS: u64 = 30;
const CLIENT_TIMEOUT_SECS: u64 = 20;

/// Bajt wysylany przez klienta w ramach ping latency
const LATENCY_PING: u8 = 0xAC;
/// Bajt wysylany przez serwer jako pong latency
const LATENCY_PONG: u8 = 0xCA;

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub bytes_transferred: u64,
    pub duration_ms: u64,
    pub bandwidth_mbps: f64,
    pub latency_us: u64,
    pub streams_completed: u8,
    pub streams_total: u8,
}

/// Wynik pojedynczego strumienia po stronie klienta
struct StreamResult {
    bytes_sent: u64,
    elapsed: Duration,
    latency_us: u64,
}

/// Uruchom serwer probing na podanym IP. Binduje port 0 (OS przydziela).
/// Zwraca (port, JoinHandle z ProbeResult po zakonczeniu).
/// Serwer akceptuje `num_streams` polaczen, weryfikuje nonce na kazdym.
/// Auto-cleanup po SERVER_TIMEOUT_SECS.
pub async fn start_probe_server(
    bind_ip: &str,
    nonce: &[u8; NONCE_SIZE],
    num_streams: u8,
    duration_ms: u32,
) -> Result<(u16, tokio::task::JoinHandle<Result<ProbeResult>>)> {
    let addr: SocketAddr = format!("{}:0", bind_ip).parse()?;

    let socket = TcpSocket::new_v4()?;
    socket.set_recv_buffer_size(64 * 1024 * 1024)?;
    socket.set_send_buffer_size(64 * 1024 * 1024)?;
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    let listener = socket.listen(num_streams as u32 + 1)?;
    let port = listener.local_addr()?.port();

    let nonce_copy = *nonce;

    let handle = tokio::spawn(async move {
        let result = timeout(
            Duration::from_secs(SERVER_TIMEOUT_SECS),
            run_server(listener, &nonce_copy, num_streams, duration_ms),
        )
        .await;

        match result {
            Ok(r) => r,
            Err(_) => Ok(ProbeResult {
                bytes_transferred: 0,
                duration_ms: 0,
                bandwidth_mbps: 0.0,
                latency_us: 0,
                streams_completed: 0,
                streams_total: num_streams,
            }),
        }
    });

    Ok((port, handle))
}

async fn run_server(
    listener: TcpListener,
    nonce: &[u8; NONCE_SIZE],
    num_streams: u8,
    duration_ms: u32,
) -> Result<ProbeResult> {
    let start = Instant::now();
    // Dodatkowe 3s na setup polaczen
    let deadline = start + Duration::from_millis(duration_ms as u64 + 3000);
    let mut handles = Vec::new();
    let mut first = true;

    for _ in 0..num_streams {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match timeout(remaining, listener.accept()).await {
            Ok(Ok((stream, _addr))) => {
                let expected_nonce = *nonce;
                let is_first = first;
                first = false;
                handles.push(tokio::spawn(async move {
                    handle_stream(stream, &expected_nonce, is_first).await
                }));
            }
            _ => break,
        }
    }

    let mut total_bytes: u64 = 0;
    let mut completed: u8 = 0;

    for h in handles {
        if let Ok(Ok(bytes)) = h.await {
            total_bytes += bytes;
            completed += 1;
        }
    }

    let elapsed = start.elapsed().as_millis() as u64;
    let bandwidth_mbps = if elapsed > 0 {
        (total_bytes as f64 * 8.0) / (elapsed as f64) / 1000.0
    } else {
        0.0
    };

    Ok(ProbeResult {
        bytes_transferred: total_bytes,
        duration_ms: elapsed,
        bandwidth_mbps,
        latency_us: 0,
        streams_completed: completed,
        streams_total: num_streams,
    })
}

async fn handle_stream(
    mut stream: TcpStream,
    expected_nonce: &[u8; NONCE_SIZE],
    is_first_stream: bool,
) -> Result<u64> {
    // Weryfikuj nonce (pierwsze 32 bajty)
    let mut nonce_buf = [0u8; NONCE_SIZE];
    stream.read_exact(&mut nonce_buf).await?;
    if nonce_buf != *expected_nonce {
        return Err(anyhow!("Niepoprawny nonce"));
    }

    // Latency ping-pong na pierwszym strumieniu
    if is_first_stream {
        let mut ping = [0u8; 1];
        stream.read_exact(&mut ping).await?;
        if ping[0] == LATENCY_PING {
            stream.write_all(&[LATENCY_PONG]).await?;
            stream.flush().await?;
        }
    }

    // Odbieraj dane az klient zamknie polaczenie
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total: u64 = 0;
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => total += n as u64,
            Err(_) => break,
        }
    }
    Ok(total)
}

/// Uruchom klienta probing — laczy sie z serwerem, wysyla nonce + dane przez duration_ms.
pub async fn start_probe_client(
    target_addr: &str,
    target_port: u16,
    bind_interface: &str,
    nonce: &[u8; NONCE_SIZE],
    num_streams: u8,
    duration_ms: u32,
) -> Result<ProbeResult> {
    let addr: SocketAddr = format!("{}:{}", target_addr, target_port).parse()?;

    let result = timeout(
        Duration::from_secs(CLIENT_TIMEOUT_SECS),
        run_client(addr, bind_interface, nonce, num_streams, duration_ms),
    )
    .await
    .map_err(|_| anyhow!("Timeout klienta po {}s", CLIENT_TIMEOUT_SECS))??;

    Ok(result)
}

async fn run_client(
    addr: SocketAddr,
    bind_interface: &str,
    nonce: &[u8; NONCE_SIZE],
    num_streams: u8,
    duration_ms: u32,
) -> Result<ProbeResult> {
    let mut handles = Vec::new();

    for i in 0..num_streams {
        let nonce_copy = *nonce;
        let iface = bind_interface.to_string();
        let target = addr;
        let dur_ms = duration_ms;
        let is_first = i == 0;

        handles.push(tokio::spawn(async move {
            send_stream(target, &iface, &nonce_copy, dur_ms, is_first).await
        }));
    }

    let mut total_bytes: u64 = 0;
    let mut total_elapsed = Duration::ZERO;
    let mut completed: u8 = 0;
    let mut latency_us: u64 = 0;

    for h in handles {
        if let Ok(Ok(result)) = h.await {
            total_bytes += result.bytes_sent;
            if result.elapsed > total_elapsed {
                total_elapsed = result.elapsed;
            }
            if result.latency_us > 0 {
                latency_us = result.latency_us;
            }
            completed += 1;
        }
    }

    let elapsed_ms = total_elapsed.as_millis() as u64;
    let bandwidth_mbps = if elapsed_ms > 0 {
        (total_bytes as f64 * 8.0) / (elapsed_ms as f64) / 1000.0
    } else {
        0.0
    };

    Ok(ProbeResult {
        bytes_transferred: total_bytes,
        duration_ms: elapsed_ms,
        bandwidth_mbps,
        latency_us,
        streams_completed: completed,
        streams_total: num_streams,
    })
}

async fn send_stream(
    addr: SocketAddr,
    bind_interface: &str,
    nonce: &[u8; NONCE_SIZE],
    duration_ms: u32,
    is_first_stream: bool,
) -> Result<StreamResult> {
    let socket = TcpSocket::new_v4()?;
    socket.set_send_buffer_size(64 * 1024 * 1024)?;
    socket.set_recv_buffer_size(64 * 1024 * 1024)?;

    // Bindowanie do interfejsu sieciowego (tylko Linux — SO_BINDTODEVICE)
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = socket.as_raw_fd();
        let iface = std::ffi::CString::new(bind_interface)?;
        unsafe {
            let ret = libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                iface.as_ptr() as *const libc::c_void,
                iface.as_bytes_with_nul().len() as libc::socklen_t,
            );
            if ret != 0 {
                tracing::warn!(
                    "SO_BINDTODEVICE nie powiodlo sie dla {}: {}",
                    bind_interface,
                    std::io::Error::last_os_error()
                );
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    let _ = bind_interface;

    let mut stream = match socket.connect(addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("TCP connect failed {}: {}", addr, e);
            return Err(anyhow!("TCP connect failed {}: {}", addr, e));
        }
    };
    stream.set_nodelay(true)?;

    // Wyslij nonce jako autoryzacje
    stream.write_all(nonce).await?;

    // Pomiar latency na pierwszym strumieniu (ping-pong)
    let mut latency_us: u64 = 0;
    if is_first_stream {
        let ping_start = Instant::now();
        stream.write_all(&[LATENCY_PING]).await?;
        stream.flush().await?;
        let mut pong = [0u8; 1];
        stream.read_exact(&mut pong).await?;
        if pong[0] == LATENCY_PONG {
            latency_us = ping_start.elapsed().as_micros() as u64 / 2;
        }
    }

    // Wysylaj dane az minie deadline
    let data = vec![0xABu8; CHUNK_SIZE];
    let start = Instant::now();
    let deadline = start + Duration::from_millis(duration_ms as u64);
    let mut total: u64 = 0;

    while Instant::now() < deadline {
        match stream.write_all(&data).await {
            Ok(()) => total += CHUNK_SIZE as u64,
            Err(_) => break,
        }
    }

    // Flush i shutdown — czekamy az TCP dostarczy wszystkie buforowane dane
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;

    let elapsed = start.elapsed();

    Ok(StreamResult {
        bytes_sent: total,
        elapsed,
        latency_us,
    })
}
