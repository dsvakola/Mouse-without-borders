use serde::{Serialize, Serializer};

/// Errors returned by the core engine. Messages are written for end users.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Network or file problem: {0}")]
    Io(String),
    #[error("The other computer sent data this version cannot understand: {0}")]
    Protocol(String),
    #[error("The PIN is wrong. Please check it and try again.")]
    WrongPin,
    #[error("The saved pairing is no longer valid. Please pair again with the PIN.")]
    PairingRejected,
    #[error("A PIN is needed to connect to this computer for the first time.")]
    PinRequired,
    #[error("Too many wrong PINs. Try again in {0} seconds.")]
    LockedOut(u64),
    #[error("The PIN must be 4 to 6 digits (0-9 only).")]
    InvalidPin,
    #[error("This arrangement of computers is not valid: {0}")]
    InvalidLayout(String),
    #[error("Unsafe or empty file name: {0}")]
    UnsafeFileName(String),
    #[error("Could not read or save settings: {0}")]
    Store(String),
    #[error("This feature is not available on this operating system.")]
    Unsupported,
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Io(e.to_string())
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

/// PIN rule: 4 to 6 ASCII digits.
pub fn validate_pin(pin: &str) -> Result<()> {
    if (4..=6).contains(&pin.len()) && pin.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(AppError::InvalidPin)
    }
}
