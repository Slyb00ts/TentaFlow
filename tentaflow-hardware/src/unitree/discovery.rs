// =============================================================================
// File: unitree/discovery.rs
// Purpose: LAN discovery of Unitree robots by serial number over UDP multicast
//          (the query the official app sends). Maps serial -> IPv4 so a robot
//          that got a new DHCP lease (e.g. moved onto a node's Wi-Fi hotspot)
//          is found without anyone reading a lease table.
//
//          Firmware that speaks data2=3 may ignore an untargeted query and
//          answer only one that names its own serial, so both forms go out.
// =============================================================================

use anyhow::{Context, Result};
use socket2::{Domain, Protocol, SockAddr, SockRef, Socket, Type};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

const GO2_GROUP: Ipv4Addr = Ipv4Addr::new(231, 1, 1, 1);
const QUERY_PORT: u16 = 10131;
// Replies arrive here, both multicast and unicast to the query's source port,
// which is why the query leaves from a socket bound to this same port.
const REPLY_PORT: u16 = 10134;
const QUERY_NAME: &str = "unitree_dapengche";
const QUERY_BURST: usize = 3;
const QUERY_INTERVAL: Duration = Duration::from_millis(200);

/// Sends the Go2 discovery query out of every interface in `interfaces` and
/// listens for `timeout`. Returns every robot that answered (serial -> IP),
/// not only the requested `serials`, so the caller can tell "nothing on the
/// network" from "that serial is not here". Blocking — run it off the async
/// executor.
pub fn discover_go2(
    serials: &[String],
    interfaces: &[Ipv4Addr],
    timeout: Duration,
) -> Result<BTreeMap<String, Ipv4Addr>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .context("creating the discovery socket")?;
    socket.set_reuse_address(true)?;
    socket
        .bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, REPLY_PORT)))
        .with_context(|| format!("binding UDP {REPLY_PORT} for robot discovery"))?;
    let socket: UdpSocket = socket.into();
    for iface in interfaces {
        // An interface without multicast refuses the join; replies can still
        // arrive as unicast to the query's source port, so this is not fatal.
        let _ = socket.join_multicast_v4(&GO2_GROUP, iface);
    }

    let mut queries = vec![serde_json::json!({ "name": QUERY_NAME }).to_string()];
    queries.extend(
        serials
            .iter()
            .filter(|s| !s.is_empty())
            .map(|sn| serde_json::json!({ "name": QUERY_NAME, "sn": sn }).to_string()),
    );
    let target = SocketAddrV4::new(GO2_GROUP, QUERY_PORT);

    let mut found = BTreeMap::new();
    for round in 0..QUERY_BURST {
        for iface in interfaces {
            if SockRef::from(&socket).set_multicast_if_v4(iface).is_err() {
                continue;
            }
            for query in &queries {
                let _ = socket.send_to(query.as_bytes(), target);
            }
        }
        let wait = if round + 1 == QUERY_BURST { timeout } else { QUERY_INTERVAL };
        collect(&socket, wait, &mut found);
    }

    for iface in interfaces {
        let _ = socket.leave_multicast_v4(&GO2_GROUP, iface);
    }
    Ok(found)
}

fn collect(socket: &UdpSocket, wait: Duration, found: &mut BTreeMap<String, Ipv4Addr>) {
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || socket.set_read_timeout(Some(left)).is_err() {
            return;
        }
        let Ok((len, from)) = socket.recv_from(&mut buf) else {
            return;
        };
        let sender = match from.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            std::net::IpAddr::V6(_) => None,
        };
        if let Some((serial, ip)) = parse_reply(&buf[..len], sender) {
            found.entry(serial).or_insert(ip);
        }
    }
}

/// One reply: JSON carrying `sn` and usually `ip`; the datagram's source
/// address stands in when `ip` is absent. The port is shared with other
/// software, so anything else is ignored.
fn parse_reply(bytes: &[u8], sender: Option<Ipv4Addr>) -> Option<(String, Ipv4Addr)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let serial = value.get("sn")?.as_str()?.trim();
    if serial.is_empty() {
        return None;
    }
    let ip = value
        .get("ip")
        .and_then(|v| v.as_str())
        .and_then(|s| s.trim().parse::<Ipv4Addr>().ok())
        .or(sender)?;
    Some((serial.to_string(), ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_prefers_announced_ip() {
        let got = parse_reply(
            br#"{"sn":"B42D2000XXXX","ip":"192.168.50.250"}"#,
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        assert_eq!(got, Some(("B42D2000XXXX".into(), Ipv4Addr::new(192, 168, 50, 250))));
    }

    #[test]
    fn reply_without_ip_uses_sender() {
        let got = parse_reply(br#"{"sn":"B42D"}"#, Some(Ipv4Addr::new(192, 168, 50, 9)));
        assert_eq!(got, Some(("B42D".into(), Ipv4Addr::new(192, 168, 50, 9))));
    }

    #[test]
    fn foreign_datagrams_are_ignored() {
        assert_eq!(parse_reply(b"not json", Some(Ipv4Addr::LOCALHOST)), None);
        assert_eq!(parse_reply(br#"{"sn":""}"#, Some(Ipv4Addr::LOCALHOST)), None);
        assert_eq!(parse_reply(br#"{"name":"x"}"#, Some(Ipv4Addr::LOCALHOST)), None);
    }
}
