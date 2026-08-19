// ===== File: code_studio/egress/resolver.rs — the gateway does its own name resolution =====
//
// The sandbox never resolves anything: it has no route to a resolver, and a
// name it resolved itself would be a name the gateway did not check. So the
// gateway resolves, checks EVERY answer and pins the result for the operation.
//
// The trait exists for one reason beyond testing: resolution is where a
// rebinding attack lives, and a seam here lets the checks be exercised against
// an answer that changes between calls without depending on real DNS.

use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::{anyhow, Result};

pub trait Resolver: Send + Sync {
    /// Every address the name answers with, for that port. An empty vector is a
    /// refusal upstream, not an allowance.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>>;
}

/// The node's own resolver. Blocking, so async callers run it off the reactor.
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>> {
        let addresses: Vec<SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|e| anyhow!("cannot resolve {host}: {e}"))?
            .collect();
        if addresses.is_empty() {
            return Err(anyhow!("{host} resolved to no address"));
        }
        Ok(addresses)
    }
}
