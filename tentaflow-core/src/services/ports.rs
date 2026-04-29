// ============ File: services/ports.rs — runtime TCP port allocator for deployed services ============

use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::sync::Mutex;

/// Allocates TCP ports inside a configured range, skipping ports already
/// bound by other processes. Allocated ports are tracked until released.
pub struct PortAllocator {
    range: (u16, u16),
    inner: Mutex<Inner>,
}

struct Inner {
    cursor: u16,
    leased: HashSet<u16>,
    excluded: HashSet<u16>,
}

impl PortAllocator {
    /// Builds a fresh allocator. `range` is inclusive on both ends; `excluded`
    /// is a set of ports the caller wants to reserve (e.g. dashboard, prometheus).
    pub fn new(range: (u16, u16), excluded: HashSet<u16>) -> Result<Self> {
        let (lo, hi) = range;
        if lo == 0 || hi == 0 || lo > hi {
            return Err(anyhow!("invalid port range: {}..={}", lo, hi));
        }
        Ok(Self {
            range,
            inner: Mutex::new(Inner {
                cursor: lo,
                leased: HashSet::new(),
                excluded,
            }),
        })
    }

    /// Acquires a single free port. Skips ports already bound by other
    /// processes (probed via `TcpListener::bind` on 127.0.0.1) and ports
    /// previously leased or explicitly excluded. Returns an error if the
    /// entire range is exhausted.
    pub fn acquire(&self) -> Result<u16> {
        let (lo, hi) = self.range;
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| anyhow!("port allocator mutex poisoned: {}", e))?;
        let span = (hi - lo + 1) as u32;
        for _ in 0..span {
            let candidate = inner.cursor;
            // Advance cursor for next call (wraps back to lo).
            inner.cursor = if inner.cursor >= hi {
                lo
            } else {
                inner.cursor + 1
            };

            if inner.leased.contains(&candidate) || inner.excluded.contains(&candidate) {
                continue;
            }
            if !is_port_free(candidate) {
                continue;
            }
            inner.leased.insert(candidate);
            return Ok(candidate);
        }
        Err(anyhow!(
            "no free port in range {}..={} (all leased or in use)",
            lo,
            hi
        ))
    }

    /// Acquires `n` distinct ports in one call. On any failure no partial
    /// state is leaked: every port already taken in this call is released.
    pub fn acquire_many(&self, n: usize) -> Result<Vec<u16>> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            match self.acquire() {
                Ok(p) => out.push(p),
                Err(e) => {
                    for p in &out {
                        let _ = self.release(*p);
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// Releases a previously acquired port so future calls may hand it out
    /// again. Releasing an unleased port is a no-op (logged via Result Ok).
    pub fn release(&self, port: u16) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| anyhow!("port allocator mutex poisoned: {}", e))?;
        inner.leased.remove(&port);
        Ok(())
    }
}

/// Probes whether a TCP port on 127.0.0.1 can be bound right now.
fn is_port_free(port: u16) -> bool {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    TcpListener::bind(addr).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{SocketAddr, TcpListener};

    fn alloc(lo: u16, hi: u16) -> PortAllocator {
        PortAllocator::new((lo, hi), HashSet::new()).unwrap()
    }

    #[test]
    fn acquire_returns_port_in_range() {
        let a = alloc(45_000, 45_010);
        let p = a.acquire().unwrap();
        assert!((45_000..=45_010).contains(&p));
    }

    #[test]
    fn acquire_release_acquire_returns_same_port() {
        let a = alloc(45_100, 45_100); // single-element range
        let p1 = a.acquire().unwrap();
        a.release(p1).unwrap();
        let p2 = a.acquire().unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn acquire_pair_returns_two_distinct_ports() {
        let a = alloc(45_200, 45_210);
        let pair = a.acquire_many(2).unwrap();
        assert_eq!(pair.len(), 2);
        assert_ne!(pair[0], pair[1]);
    }

    #[test]
    fn acquire_skips_port_already_bound_by_other_process() {
        // Range covers 3 ports; bind the middle one with our own listener
        // so the allocator must skip it.
        let lo = 45_300;
        let hi = 45_302;
        let blocked = lo + 1;
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, blocked))).unwrap();
        let allocator = alloc(lo, hi);

        let mut handed_out = Vec::new();
        for _ in 0..2 {
            handed_out.push(allocator.acquire().unwrap());
        }
        assert!(
            !handed_out.contains(&blocked),
            "allocator returned a port that is currently bound: {:?}",
            handed_out
        );
        drop(listener);
    }

    #[test]
    fn acquire_excludes_initial_excluded_set() {
        let lo = 45_400;
        let hi = 45_402;
        let mut excluded = HashSet::new();
        excluded.insert(lo);
        excluded.insert(hi);
        let a = PortAllocator::new((lo, hi), excluded).unwrap();
        let p = a.acquire().unwrap();
        assert_eq!(p, lo + 1, "only the middle port should be eligible");
    }

    #[test]
    fn invalid_range_rejected() {
        assert!(PortAllocator::new((0, 100), HashSet::new()).is_err());
        assert!(PortAllocator::new((200, 100), HashSet::new()).is_err());
    }
}
