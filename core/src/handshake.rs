//! Connection set-up. The secondary PC connects to the master.
//!
//! First time: both sides prove they know the PIN (SPAKE2), then the master
//! sends a random pairing secret that both save. Next times: the saved secret
//! is used instead of the PIN, so the PIN is never asked again.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::channel::{read_frame, write_frame, SecureChannel};
use crate::crypto::{random_secret, tags_equal, Pake};
use crate::protocol::{decode, encode, Message, PROTOCOL_VERSION};
use crate::{AppError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Method {
    Pin,
    Secret,
}

#[derive(Serialize, Deserialize)]
enum Hs {
    ClientHello {
        version: u16,
        id: String,
        name: String,
        has_secret: bool,
    },
    ServerHello {
        id: String,
        name: String,
        method: Method,
    },
    Pake(Vec<u8>),
    Confirm([u8; 32]),
}

pub struct Established<S> {
    pub channel: SecureChannel<S>,
    pub peer: PeerInfo,
    /// Set when a new pairing was made: save it for next time.
    pub new_secret: Option<[u8; 32]>,
}

fn send_hs<S: Write>(io: &mut S, m: &Hs) -> Result<()> {
    write_frame(io, &encode(m)?)
}

fn recv_hs<S: Read>(io: &mut S) -> Result<Hs> {
    decode(&read_frame(io)?)
}

/// Secondary side. `target_id` is the master's id (from discovery);
/// `secret` is the saved pairing secret for it, if any;
/// `pin` is what the user typed, if anything.
pub fn connect<S: Read + Write>(
    mut io: S,
    me: &PeerInfo,
    target_id: &str,
    secret: Option<[u8; 32]>,
    pin: Option<&str>,
) -> Result<Established<S>> {
    send_hs(
        &mut io,
        &Hs::ClientHello {
            version: PROTOCOL_VERSION,
            id: me.id.clone(),
            name: me.name.clone(),
            has_secret: secret.is_some(),
        },
    )?;
    let (peer, method) = match recv_hs(&mut io)? {
        Hs::ServerHello { id, name, method } => (PeerInfo { id, name }, method),
        _ => return Err(AppError::Protocol("unexpected reply".into())),
    };
    if peer.id != target_id {
        return Err(AppError::Protocol(
            "connected to a different computer than expected".into(),
        ));
    }
    let password: Vec<u8> = match method {
        Method::Secret => secret.ok_or(AppError::PairingRejected)?.to_vec(),
        Method::Pin => {
            let pin = pin.ok_or(AppError::PinRequired)?;
            crate::error::validate_pin(pin)?;
            pin.as_bytes().to_vec()
        }
    };
    let pake = Pake::start(&password);
    send_hs(&mut io, &Hs::Pake(pake.message().to_vec()))?;
    let peer_msg = match recv_hs(&mut io)? {
        Hs::Pake(m) => m,
        _ => return Err(AppError::Protocol("unexpected reply".into())),
    };
    let keys = pake.finish(&peer_msg)?;
    send_hs(&mut io, &Hs::Confirm(keys.my_confirm))?;
    // The master only answers when our proof was right.
    let wrong = || match method {
        Method::Pin => AppError::WrongPin,
        Method::Secret => AppError::PairingRejected,
    };
    let theirs = match recv_hs(&mut io) {
        Ok(Hs::Confirm(t)) => t,
        _ => return Err(wrong()),
    };
    if !tags_equal(&theirs, &keys.peer_confirm) {
        return Err(wrong());
    }
    let mut channel = SecureChannel::new(io, &keys.send_key, &keys.recv_key);
    let new_secret = if method == Method::Pin {
        match channel.recv()? {
            Message::PairingSecret(s) => Some(s),
            _ => return Err(AppError::Protocol("pairing secret missing".into())),
        }
    } else {
        None
    };
    Ok(Established {
        channel,
        peer,
        new_secret,
    })
}

/// Master side. `lookup` returns the saved secret for a known secondary id.
pub fn accept<S: Read + Write>(
    mut io: S,
    me: &PeerInfo,
    lookup: &dyn Fn(&str) -> Option<[u8; 32]>,
    pin: &str,
) -> Result<Established<S>> {
    let (peer, has_secret) = match recv_hs(&mut io)? {
        Hs::ClientHello {
            version,
            id,
            name,
            has_secret,
        } => {
            if version != PROTOCOL_VERSION {
                return Err(AppError::Protocol(format!(
                    "version {version} not supported"
                )));
            }
            (PeerInfo { id, name }, has_secret)
        }
        _ => return Err(AppError::Protocol("unexpected first message".into())),
    };
    let saved = if has_secret { lookup(&peer.id) } else { None };
    let method = if saved.is_some() {
        Method::Secret
    } else {
        Method::Pin
    };
    send_hs(
        &mut io,
        &Hs::ServerHello {
            id: me.id.clone(),
            name: me.name.clone(),
            method,
        },
    )?;
    let password: Vec<u8> = match saved {
        Some(s) => s.to_vec(),
        None => pin.as_bytes().to_vec(),
    };
    let pake = Pake::start(&password);
    let peer_msg = match recv_hs(&mut io)? {
        Hs::Pake(m) => m,
        _ => return Err(AppError::Protocol("unexpected message".into())),
    };
    send_hs(&mut io, &Hs::Pake(pake.message().to_vec()))?;
    let keys = pake.finish(&peer_msg)?;
    let theirs = match recv_hs(&mut io)? {
        Hs::Confirm(t) => t,
        _ => return Err(AppError::Protocol("unexpected message".into())),
    };
    if !tags_equal(&theirs, &keys.peer_confirm) {
        return Err(match method {
            Method::Pin => AppError::WrongPin,
            Method::Secret => AppError::PairingRejected,
        });
    }
    send_hs(&mut io, &Hs::Confirm(keys.my_confirm))?;
    let mut channel = SecureChannel::new(io, &keys.send_key, &keys.recv_key);
    let new_secret = if method == Method::Pin {
        let s = random_secret();
        channel.send(&Message::PairingSecret(s))?;
        Some(s)
    } else {
        None
    };
    Ok(Established {
        channel,
        peer,
        new_secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn master() -> PeerInfo {
        PeerInfo {
            id: "master-1".into(),
            name: "MASTER".into(),
        }
    }
    fn laptop() -> PeerInfo {
        PeerInfo {
            id: "laptop-1".into(),
            name: "LAPTOP".into(),
        }
    }

    /// Run one connection attempt over real loopback TCP.
    fn attempt(
        saved_on_master: Option<[u8; 32]>,
        master_pin: &'static str,
        client_secret: Option<[u8; 32]>,
        client_pin: Option<&'static str>,
    ) -> (
        Result<Established<TcpStream>>,
        Result<Established<TcpStream>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            accept(
                s,
                &master(),
                &|id| {
                    if id == "laptop-1" {
                        saved_on_master
                    } else {
                        None
                    }
                },
                master_pin,
            )
        });
        let c = TcpStream::connect(addr).unwrap();
        let client = connect(c, &laptop(), "master-1", client_secret, client_pin);
        (server.join().unwrap(), client)
    }

    #[test]
    fn first_pairing_then_secret_reconnect() {
        let (s, c) = attempt(None, "123456", None, Some("123456"));
        let (mut s, mut c) = (s.unwrap(), c.unwrap());
        assert_eq!(s.new_secret, c.new_secret);
        let secret = s.new_secret.expect("secret issued");
        assert_eq!(c.peer.name, "MASTER");
        assert_eq!(s.peer.name, "LAPTOP");
        // Encrypted traffic flows both ways.
        c.channel
            .send(&Message::ClipboardText("hello".into()))
            .unwrap();
        assert_eq!(
            s.channel.recv().unwrap(),
            Message::ClipboardText("hello".into())
        );
        s.channel.send(&Message::Enter { x: 5, y: 6 }).unwrap();
        assert_eq!(c.channel.recv().unwrap(), Message::Enter { x: 5, y: 6 });

        // Second time: no PIN needed on either side.
        let (s2, c2) = attempt(Some(secret), "123456", Some(secret), None);
        assert!(s2.unwrap().new_secret.is_none());
        assert!(c2.unwrap().new_secret.is_none());
    }

    #[test]
    fn wrong_pin_is_rejected_on_both_sides() {
        let (s, c) = attempt(None, "123456", None, Some("654321"));
        assert!(matches!(s, Err(AppError::WrongPin)));
        assert!(matches!(c, Err(AppError::WrongPin)));
    }

    #[test]
    fn pin_is_requested_when_not_paired() {
        let (_s, c) = attempt(None, "123456", None, None);
        assert!(matches!(c, Err(AppError::PinRequired)));
    }

    #[test]
    fn master_forgot_pairing_asks_for_pin_again() {
        let old = [7u8; 32];
        // Client still has a secret, master does not: master asks for the PIN.
        let (s, c) = attempt(None, "123456", Some(old), Some("123456"));
        assert!(s.unwrap().new_secret.is_some());
        assert!(c.unwrap().new_secret.is_some());
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let (s, c) = attempt(Some([1u8; 32]), "123456", Some([2u8; 32]), None);
        assert!(matches!(s, Err(AppError::PairingRejected)));
        assert!(matches!(c, Err(AppError::PairingRejected)));
    }

    #[test]
    fn tampered_frame_is_rejected() {
        let (s, c) = attempt(None, "123456", None, Some("123456"));
        let (mut s, c) = (s.unwrap(), c.unwrap());
        // Send a garbage frame instead of a valid encrypted one.
        let mut raw = c.channel.into_inner();
        write_frame(&mut raw, &[9u8; 40]).unwrap();
        assert!(s.channel.recv().is_err());
    }
}
