//! Finding other PCs on the local network with small UDP broadcast beacons.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::protocol::{decode, encode, PROTOCOL_VERSION};
use crate::Result;

pub const DISCOVERY_PORT: u16 = 24801;
pub const CONNECT_PORT: u16 = 24800;
const MAGIC: &[u8; 4] = b"MWB1";

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
        let mut out = MAGIC.to_vec();
        out.extend(encode(self)?);
        Ok(out)
    }

    pub fn from_bytes(b: &[u8]) -> Option<Beacon> {
        let body = b.strip_prefix(MAGIC.as_slice())?;
        decode::<Beacon>(body)
            .ok()
            .filter(|x| x.version == PROTOCOL_VERSION)
    }
}

/// Bind the socket that listens for beacons.
pub fn listen_socket(port: u16) -> Result<UdpSocket> {
    let s = UdpSocket::bind(("0.0.0.0", port))?;
    s.set_broadcast(true)?;
    Ok(s)
}

/// Send one beacon to `target` (use 255.255.255.255:DISCOVERY_PORT on a real network).
pub fn announce(beacon: &Beacon, target: SocketAddr) -> Result<()> {
    let s = UdpSocket::bind(("0.0.0.0", 0))?;
    s.set_broadcast(true)?;
    s.send_to(&beacon.to_bytes()?, target)?;
    Ok(())
}

/// Wait up to `timeout` for one valid beacon; ignores our own id and junk packets.
pub fn wait_for_beacon(
    sock: &UdpSocket,
    my_id: &str,
    timeout: Duration,
) -> Result<Option<(Beacon, SocketAddr)>> {
    sock.set_read_timeout(Some(timeout))?;
    let mut buf = [0u8; 512];
    match sock.recv_from(&mut buf) {
        Ok((n, from)) => Ok(Beacon::from_bytes(&buf[..n])
            .filter(|b| b.id != my_id)
            .map(|b| (b, from))),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
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
    fn beacon_roundtrip_and_junk_rejected() {
        let b = Beacon::new("abc", "PC-1", true);
        assert_eq!(Beacon::from_bytes(&b.to_bytes().unwrap()), Some(b));
        assert_eq!(Beacon::from_bytes(b"hello"), None);
        assert_eq!(Beacon::from_bytes(b"MWB1\xff\xff"), None);
    }

    #[test]
    fn finds_peer_over_udp_and_ignores_itself() {
        let sock = listen_socket(0).unwrap();
        let port = sock.local_addr().unwrap().port();
        let target: SocketAddr = ([127, 0, 0, 1], port).into();
        let other = Beacon::new("other", "PC-2", false);
        announce(&other, target).unwrap();
        let (got, _) = wait_for_beacon(&sock, "me", Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(got, other);
        // Our own beacon is skipped.
        announce(&Beacon::new("me", "PC-1", true), target).unwrap();
        assert!(wait_for_beacon(&sock, "me", Duration::from_millis(300))
            .unwrap()
            .is_none());
    }
}
