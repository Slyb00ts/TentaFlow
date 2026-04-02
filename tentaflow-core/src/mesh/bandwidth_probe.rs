// =============================================================================
// Plik: mesh/bandwidth_probe.rs
// Opis: Probe przepustowosci sieci — multi-stream TCP z nonce auth.
//       Serwer nasuchuje na ephemeral porcie, klient laczy sie i wysyla dane.
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

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub bytes_transferred: u64,
    pub duration_ms: u64,
    pub bandwidth_mbps: f64,
    pub latency_us: u64,
    pub streams_completed: u8,
    pub streams_total: u8,
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

    for _ in 0..num_streams {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match timeout(remaining, listener.accept()).await {
            Ok(Ok((stream, _addr))) => {
                let expected_nonce = *nonce;
                handles.push(tokio::spawn(async move {
                    handle_stream(stream, &expected_nonce).await
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

// Ponizej jest run_client ktory mierzy latency

async fn handle_stream(
    mut stream: TcpStream,
    expected_nonce: &[u8; NONCE_SIZE],
) -> Result<u64> {
    // Weryfikuj nonce (pierwsze 32 bajty)
    let mut nonce_buf = [0u8; NONCE_SIZE];
    stream.read_exact(&mut nonce_buf).await?;
    if nonce_buf != *expected_nonce {
        return Err(anyhow!("Niepoprawny nonce"));
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
    // Pomiar latency — mierzymy czas pierwszego stream connect (nie oddzielne polaczenie)
    let latency_us = 0u64; // TODO: mierzyc w ramach pierwszego streamu

    let start = Instant::now();
    let mut handles = Vec::new();

    for _ in 0..num_streams {
        let nonce_copy = *nonce;
        let iface = bind_interface.to_string();
        let target = addr;
        let dur_ms = duration_ms;

        handles.push(tokio::spawn(async move {
            send_stream(target, &iface, &nonce_copy, dur_ms).await
        }));
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
        latency_us,
        streams_completed: completed,
        streams_total: num_streams,
    })
}

/// Pomiar TCP RTT — czas polaczenia TCP do serwera (SYN-SYNACK-ACK)
async fn measure_tcp_rtt(addr: SocketAddr) -> Result<u64> {
    let start = Instant::now();
    let stream = TcpStream::connect(addr).await?;
    let connect_us = start.elapsed().as_micros() as u64;
    drop(stream);
    Ok(connect_us)
}

async fn send_stream(
    addr: SocketAddr,
    bind_interface: &str,
    nonce: &[u8; NONCE_SIZE],
    duration_ms: u32,
) -> Result<u64> {
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

    // Wysylaj dane az minie deadline
    let data = vec![0xABu8; CHUNK_SIZE];
    let deadline = Instant::now() + Duration::from_millis(duration_ms as u64);
    let mut total: u64 = 0;

    while Instant::now() < deadline {
        match stream.write_all(&data).await {
            Ok(()) => total += CHUNK_SIZE as u64,
            Err(_) => break,
        }
    }

    Ok(total)
}
