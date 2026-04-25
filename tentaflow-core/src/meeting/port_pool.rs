// =============================================================================
// Plik: meeting/port_pool.rs
// Opis: Alokator portów dla ephemeralnych kontenerów Meeting Bot. Trzy zakresy:
//       QUIC UDP 7000-7999, VNC TCP 6100-6999, noVNC TCP 6800-7099. Rezerwacja
//       jest atomowa przez UNIQUE(port, kind) w `meeting_port_allocations`:
//       INSERT OR IGNORE zwraca changed=1 przy sukcesie. Zwolnienie kasuje
//       wszystkie wiersze sesji. Dodatkowo sprawdzamy czy port nie jest zajęty
//       na systemie poprzez `TcpListener::bind` (TCP) lub `UdpSocket::bind`
//       (UDP) — chroni przed kolizją z procesem spoza tentaflow.
// =============================================================================

use anyhow::{anyhow, Result};
use rand::prelude::*;
use std::net::{TcpListener, UdpSocket};

use crate::db::{repository, DbPool};

/// Zakresy portów (inclusive start, exclusive end). VNC i noVNC nie mogą
/// nakładać się — wcześniejsza para 6100-7000 / 6800-7100 współdzieliła
/// 6800-6999 i pula realnie kurczyła się do ~700 + ~200 zamiast deklarowanego
/// rozmiaru. Teraz każdy zakres jest wyłączny.
pub const QUIC_RANGE: (u16, u16) = (7000, 8000);
pub const VNC_RANGE: (u16, u16) = (5100, 6100);
pub const NOVNC_RANGE: (u16, u16) = (6100, 7000);

pub const KIND_QUIC: &str = "quic";
pub const KIND_VNC: &str = "vnc";
pub const KIND_NOVNC: &str = "novnc";

/// Trójka portów zaalokowanych dla jednej sesji.
#[derive(Debug, Clone, Copy)]
pub struct AllocatedPorts {
    pub quic: u16,
    pub vnc: u16,
    pub novnc: u16,
}

/// Alokuje trzy niezależne porty, zapisuje rezerwację w DB pod `session_id`.
/// Jeśli nie uda się znaleźć wolnej trójki w ciągu 64 prób dla któregoś portu,
/// zwraca błąd i cofa rezerwacje już dokonane dla tej sesji.
pub fn allocate_for_session(pool: &DbPool, session_id: i64) -> Result<AllocatedPorts> {
    let quic = match reserve_one(pool, session_id, KIND_QUIC, QUIC_RANGE, PortKind::Udp) {
        Ok(p) => p,
        Err(e) => {
            let _ = repository::transcripts::release_session_ports(pool, session_id);
            return Err(e);
        }
    };
    let vnc = match reserve_one(pool, session_id, KIND_VNC, VNC_RANGE, PortKind::Tcp) {
        Ok(p) => p,
        Err(e) => {
            let _ = repository::transcripts::release_session_ports(pool, session_id);
            return Err(e);
        }
    };
    let novnc = match reserve_one(pool, session_id, KIND_NOVNC, NOVNC_RANGE, PortKind::Tcp) {
        Ok(p) => p,
        Err(e) => {
            let _ = repository::transcripts::release_session_ports(pool, session_id);
            return Err(e);
        }
    };
    Ok(AllocatedPorts { quic, vnc, novnc })
}

/// Zwalnia wszystkie porty przypisane do sesji.
pub fn release_for_session(pool: &DbPool, session_id: i64) -> Result<()> {
    repository::transcripts::release_session_ports(pool, session_id)
}

#[derive(Copy, Clone)]
enum PortKind {
    Tcp,
    Udp,
}

fn port_available(port: u16, kind: PortKind) -> bool {
    match kind {
        PortKind::Tcp => TcpListener::bind(("0.0.0.0", port)).is_ok(),
        PortKind::Udp => UdpSocket::bind(("0.0.0.0", port)).is_ok(),
    }
}

fn reserve_one(
    pool: &DbPool,
    session_id: i64,
    kind_db: &str,
    range: (u16, u16),
    kind_sys: PortKind,
) -> Result<u16> {
    let (start, end) = range;
    let span = (end - start) as u32;
    let mut rng = rand::rng();
    for _ in 0..64 {
        let candidate = start + ((rng.random::<u32>() % span) as u16);
        if !port_available(candidate, kind_sys) {
            continue;
        }
        if repository::transcripts::try_reserve_port(pool, candidate, kind_db, session_id)? {
            return Ok(candidate);
        }
    }
    // Fallback — sekwencyjne skanowanie zakresu, gdyby randomizacja nie trafiła.
    for candidate in start..end {
        if !port_available(candidate, kind_sys) {
            continue;
        }
        if repository::transcripts::try_reserve_port(pool, candidate, kind_db, session_id)? {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "nie udalo sie zaalokowac portu {} w zakresie {}..{}",
        kind_db,
        start,
        end
    ))
}
