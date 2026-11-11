//! Core engine of Mouse without borders: protocol, security, discovery,
//! screen layout, clipboard/file transfer and settings storage.

pub mod channel;
pub mod crypto;
pub mod discovery;
pub mod error;
pub mod files;
pub mod handshake;
pub mod layout;
pub mod limiter;
pub mod protocol;
pub mod router;
pub mod store;

pub use error::{AppError, Result};

/// The one shared PIN used by every PC (private-use software).
pub const SHARED_PIN: &str = "123456";

#[cfg(windows)]
pub mod platform;

#[cfg(windows)]
pub mod engine;
