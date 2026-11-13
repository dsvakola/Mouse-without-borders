//! Finding other PCs on the local network with small UDP packets.
//!
//! The master sends a beacon every two seconds. A secondary also sends a
//! "probe", and the master answers it directly. That answer is a reply to
//! something the secondary sent, so the secondary's firewall lets it in even
//! when it blocks unrequested packets. Packets go to every network adapter,
//! not only the default one (PCs with VPN or virtual adapters).

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::protocol::{decode, encode, PROTOCOL_VERSION};
use crate::Result;

pub const DISCOVERY_PORT: u16 = 24801;
pub const CONNECT_PORT: u16 = 24800;
const BEACON_MAGIC: &[u8; 4] = b"MWB1";
const PROBE_MAGIC: &[u8; 4] = b"MWBP";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beacon {
    pub id: String,
    pub name: String,
    /// TCP port to connect to.
    pub port: u16,
    /// True if this PC is acting as the master.
    pub is_master: bool,
    pub version: u16,
}

impl Beacon {
    pub fn new(id: &str, name: &str, is_master: bool) -> Beacon {
        Beacon {
            id: id.into(),
            name: name.into(),
            port: CONNECT_PORT,
            is_master,
            version: PROTOCOL_VERSION,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = BEACON_MAGIC.to_vec();
        out.extend(encode(self)?);
        Ok(out)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Beacon> {
        let body = b.strip_prefix(BEACON_MAGIC.as_slice())?;
        decode::<Beacon>(body).ok()
    }
}

/// What can arrive on the discovery port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Beacon(Beacon),
    /// A secondary with this id is looking for a master.
    Probe {
        id: String,
    },
}

pub fn probe_bytes(id: &str) -> Vec<u8> {
    let mut out = PROBE_MAGIC.to_vec();
    out.extend(id.as_bytes());
    out
}

pub fn parse_packet(b: &[u8]) -> Option<Packet> {
    if let Some(id) = b.strip_prefix(PROBE_MAGIC.as_slice()) {
        let id = std::str::from_utf8(id).ok()?;
        return (!id.is_empty() && id.len() <= 64).then(|| Packet::Probe { id: id.to_string() });
    }
    Beacon::from_bytes(b).map(Packet::Beacon)
}

/// Bind the discovery socket.
pub fn listen_socket(port: u16) -> Result<UdpSocket> {
    let s = UdpSocket::bind(("0.0.0.0", port))?;
    s.set_broadcast(true)?;
    Ok(s)
}

/// IPv4 addresses of this PC's network adapters (empty if they cannot be read).
pub fn local_ipv4s(hostname: &str) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = (hostname, 0u16)
        .to_socket_addrs()
        .map(|it| {
            it.filter_map(|a| match a {
                SocketAddr::V4(v4) if !v4.ip().is_loopback() && !v4.ip().is_link_local() => {
                    Some(*v4.ip())
                }
                _ => None,
            })
            .collect()
        })
        .unwrap_or_default();
    // The adapter used for normal traffic (nothing is sent by this).
    if let Ok(s) = UdpSocket::bind(("0.0.0.0", 0)) {
        if s.connect(("192.0.2.1", 9)).is_ok() {
            if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                if !a.ip().is_loopback() && !a.ip().is_unspecified() {
                    out.push(*a.ip());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Where to send a broadcast so it reaches every adapter: the general
/// broadcast address plus the broadcast address of each adapter's network
/// (assuming the usual 255.255.255.0 mask of home and office networks).
pub fn broadcast_targets(local: &[Ipv4Addr], port: u16) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = vec![(Ipv4Addr::BROADCAST, port).into()];
    for ip in local {
        let o = ip.octets();
        let t: SocketAddr = (Ipv4Addr::new(o[0], o[1], o[2], 255), port).into();
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Send `data` to every target; errors on single adapters are ignored.
/// Returns how many sends worked.
pub fn send_to_all(sock: &UdpSocket, data: &[u8], targets: &[SocketAddr]) -> usize {
    targets
        .iter()
        .filter(|t| sock.send_to(data, t).is_ok())
        .count()
}

/// Wait up to `timeout` for one valid packet. Junk and our own packets give `None`.
pub fn recv_packet(
    sock: &UdpSocket,
    my_id: &str,
    timeout: Duration,
) -> Result<Option<(Packet, SocketAddr)>> {
    sock.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
    let mut buf = [0u8; 512];
    match sock.recv_from(&mut buf) {
        Ok((n, from)) => Ok(parse_packet(&buf[..n])
            .filter(|p| match p {
                Packet::Beacon(b) => b.id != my_id,
                Packet::Probe { id } => id != my_id,
            })
            .map(|p| (p, from))),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
                    // Windows reports a previous send to a closed port this way.
                    | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_roundtrip_and_junk_rejected() {
        let b = Beacon::new("abc", "PC-1", true);
        assert_eq!(
            parse_packet(&b.to_bytes().unwrap()),
            Some(Packet::Beacon(b))
        );
        assert_eq!(
            parse_packet(&probe_bytes("xyz")),
            Some(Packet::Probe { id: "xyz".into() })
        );
        assert_eq!(parse_packet(b"hello"), None);
        assert_eq!(parse_packet(b"MWB1\xff\xff"), None);
        assert_eq!(parse_packet(b"MWBP"), None);
    }

    #[test]
    fn broadcast_reaches_every_adapter_network_once() {
        let local = [
            Ipv4Addr::new(192, 168, 1, 7),
            Ipv4Addr::new(10, 0, 5, 20),
            Ipv4Addr::new(192, 168, 1, 9),
        ];
        let t = broadcast_targets(&local, 24801);
        let want: Vec<SocketAddr> = vec![
            "255.255.255.255:24801".parse().unwrap(),
            "192.168.1.255:24801".parse().unwrap(),
            "10.0.5.255:24801".parse().unwrap(),
        ];
        assert_eq!(t, want);
    }

    #[test]
    fn probe_is_answered_directly_and_own_packets_are_ignored() {
        // "master" and "secondary" sockets on loopback.
        let master = listen_socket(0).unwrap();
        let secondary = listen_socket(0).unwrap();
        let m_addr: SocketAddr = ([127, 0, 0, 1], master.local_addr().unwrap().port()).into();

        // Secondary probes; master sees it and answers to where it came from.
        assert_eq!(send_to_all(&secondary, &probe_bytes("sec"), &[m_addr]), 1);
        let (p, from) = recv_packet(&master, "mas", Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(p, Packet::Probe { id: "sec".into() });
        let beacon = Beacon::new("mas", "MASTER", true);
        master.send_to(&beacon.to_bytes().unwrap(), from).unwrap();
        let (p, from_master) = recv_packet(&secondary, "sec", Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(p, Packet::Beacon(beacon));
        assert_eq!(from_master.port(), m_addr.port());

        // A PC never reacts to its own packets.
        send_to_all(&secondary, &probe_bytes("mas"), &[m_addr]);
        assert!(recv_packet(&master, "mas", Duration::from_millis(300))
            .unwrap()
            .is_none());
    }

    #[test]
    fn local_addresses_never_include_loopback() {
        for ip in local_ipv4s("localhost") {
            assert!(!ip.is_loopback());
        }
    }
}
