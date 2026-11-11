//! Encrypted, length-prefixed message channel over any byte stream.

use std::io::{Read, Write};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use crate::protocol::{decode, encode, Message, MAX_FRAME};
use crate::{AppError, Result};

pub fn write_frame<W: Write>(w: &mut W, data: &[u8]) -> Result<()> {
    if data.len() > MAX_FRAME {
        return Err(AppError::Protocol("frame too large".into()));
    }
    w.write_all(&(data.len() as u32).to_be_bytes())?;
    w.write_all(data)?;
    w.flush()?;
    Ok(())
}

pub fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME + 64 {
        return Err(AppError::Protocol("frame too large".into()));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

fn nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::from(n)
}

pub struct SecureChannel<S> {
    io: S,
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_ctr: u64,
    recv_ctr: u64,
}

impl<S: Read + Write> SecureChannel<S> {
    pub fn new(io: S, send_key: &[u8; 32], recv_key: &[u8; 32]) -> Self {
        SecureChannel {
            io,
            send: ChaCha20Poly1305::new(&Key::from(*send_key)),
            recv: ChaCha20Poly1305::new(&Key::from(*recv_key)),
            send_ctr: 0,
            recv_ctr: 0,
        }
    }

    pub fn send(&mut self, msg: &Message) -> Result<()> {
        let plain = encode(msg)?;
        let ct = self
            .send
            .encrypt(&nonce(self.send_ctr), plain.as_slice())
            .map_err(|_| AppError::Protocol("encryption failed".into()))?;
        self.send_ctr += 1;
        write_frame(&mut self.io, &ct)
    }

    pub fn recv(&mut self) -> Result<Message> {
        let ct = read_frame(&mut self.io)?;
        let plain = self
            .recv
            .decrypt(&nonce(self.recv_ctr), ct.as_slice())
            .map_err(|_| AppError::Protocol("message failed authentication".into()))?;
        self.recv_ctr += 1;
        decode(&plain)
    }

    pub fn into_inner(self) -> S {
        self.io
    }

    pub fn get_ref(&self) -> &S {
        &self.io
    }
}

/// Sending half of a split channel.
pub struct SecureSender<S> {
    io: S,
    cipher: ChaCha20Poly1305,
    ctr: u64,
}

/// Receiving half of a split channel.
pub struct SecureReceiver<S> {
    io: S,
    cipher: ChaCha20Poly1305,
    ctr: u64,
}

impl<S: Write> SecureSender<S> {
    pub fn send(&mut self, msg: &Message) -> Result<()> {
        let ct = self
            .cipher
            .encrypt(&nonce(self.ctr), encode(msg)?.as_slice())
            .map_err(|_| AppError::Protocol("encryption failed".into()))?;
        self.ctr += 1;
        write_frame(&mut self.io, &ct)
    }
}

impl<S: Read> SecureReceiver<S> {
    pub fn recv(&mut self) -> Result<Message> {
        let ct = read_frame(&mut self.io)?;
        let plain = self
            .cipher
            .decrypt(&nonce(self.ctr), ct.as_slice())
            .map_err(|_| AppError::Protocol("message failed authentication".into()))?;
        self.ctr += 1;
        decode(&plain)
    }
}

impl<S: Read + Write> SecureChannel<S> {
    /// Split into independent sending and receiving halves (for two threads).
    /// `clone` makes a second handle to the same stream, e.g. `TcpStream::try_clone`.
    pub fn split(
        self,
        clone: impl FnOnce(&S) -> std::io::Result<S>,
    ) -> Result<(SecureSender<S>, SecureReceiver<S>)> {
        let second = clone(&self.io)?;
        Ok((
            SecureSender {
                io: self.io,
                cipher: self.send,
                ctr: self.send_ctr,
            },
            SecureReceiver {
                io: second,
                cipher: self.recv,
                ctr: self.recv_ctr,
            },
        ))
    }
}
