//! PIN-authenticated key exchange (SPAKE2) and key derivation.
//!
//! A short PIN is safe to use here because SPAKE2 does not let an eavesdropper
//! test PIN guesses offline: each guess needs a live attempt on the master,
//! and those are rate limited (see `limiter`).

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::{AppError, Result};

type HmacSha256 = Hmac<Sha256>;

const IDENTITY: &[u8] = b"mouse-without-borders-v1";

pub struct Pake {
    state: Spake2<Ed25519Group>,
    my_msg: Vec<u8>,
}

/// Keys for one authenticated session.
pub struct SessionKeys {
    pub send_key: [u8; 32],
    pub recv_key: [u8; 32],
    /// Tag this side sends to prove it knows the password.
    pub my_confirm: [u8; 32],
    /// Tag expected from the other side.
    pub peer_confirm: [u8; 32],
}

impl Pake {
    /// `password` is the PIN (first pairing) or the saved 32-byte pairing secret.
    pub fn start(password: &[u8]) -> Pake {
        let (state, my_msg) = Spake2::<Ed25519Group>::start_symmetric(
            &Password::new(password),
            &Identity::new(IDENTITY),
        );
        Pake { state, my_msg }
    }

    pub fn message(&self) -> &[u8] {
        &self.my_msg
    }

    pub fn finish(self, peer_msg: &[u8]) -> Result<SessionKeys> {
        if peer_msg == self.my_msg.as_slice() {
            return Err(AppError::Protocol("reflected handshake".into()));
        }
        let shared = self
            .state
            .finish(peer_msg)
            .map_err(|_| AppError::Protocol("bad handshake message".into()))?;

        // Order the two messages so both sides compute the same transcript.
        let i_am_lo = self.my_msg.as_slice() < peer_msg;
        let (lo, hi) = if i_am_lo {
            (self.my_msg.as_slice(), peer_msg)
        } else {
            (peer_msg, self.my_msg.as_slice())
        };
        let mut transcript = Sha256::new();
        transcript.update(lo);
        transcript.update(hi);
        let transcript = transcript.finalize();

        let hk = Hkdf::<Sha256>::new(Some(&transcript), &shared);
        let mut key_lo = [0u8; 32];
        let mut key_hi = [0u8; 32];
        let mut conf_lo = [0u8; 32];
        let mut conf_hi = [0u8; 32];
        for (label, out) in [
            (&b"mwb key lo"[..], &mut key_lo),
            (&b"mwb key hi"[..], &mut key_hi),
            (&b"mwb confirm lo"[..], &mut conf_lo),
            (&b"mwb confirm hi"[..], &mut conf_hi),
        ] {
            hk.expand(label, out)
                .map_err(|_| AppError::Protocol("key derivation failed".into()))?;
        }
        let tag = |key: &[u8; 32]| -> Result<[u8; 32]> {
            let mut m = HmacSha256::new_from_slice(key)
                .map_err(|_| AppError::Protocol("mac key".into()))?;
            m.update(&transcript);
            Ok(m.finalize().into_bytes().into())
        };
        let (send_key, recv_key, mine, theirs) = if i_am_lo {
            (key_lo, key_hi, tag(&conf_lo)?, tag(&conf_hi)?)
        } else {
            (key_hi, key_lo, tag(&conf_hi)?, tag(&conf_lo)?)
        };
        Ok(SessionKeys {
            send_key,
            recv_key,
            my_confirm: mine,
            peer_confirm: theirs,
        })
    }
}

/// Constant-time comparison of two confirmation tags.
pub fn tags_equal(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 32 random bytes for a new pairing secret.
pub fn random_secret() -> [u8; 32] {
    let mut s = [0u8; 32];
    rand::fill(&mut s);
    s
}
