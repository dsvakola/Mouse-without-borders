use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
/// Largest frame accepted from the network (file chunks are far smaller).
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
/// File data is sent in chunks of this size.
pub const FILE_CHUNK: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// Everything sent over an established (encrypted) connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Sent once by the master right after a successful PIN pairing.
    PairingSecret([u8; 32]),
    Ping,
    Pong,
    /// A secondary reports its screen size.
    ScreenInfo {
        width: i32,
        height: i32,
    },
    /// Master's cursor now enters this computer at (x, y).
    Enter {
        x: i32,
        y: i32,
    },
    /// Master's cursor leaves this computer.
    Leave,
    /// Place the cursor exactly here (screen pixels). Used while the master controls this PC,
    /// so pointer speed settings on this PC cannot make the two drift apart.
    MouseAbs {
        x: i32,
        y: i32,
    },
    MouseMove {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: Button,
        down: bool,
    },
    Wheel {
        dx: i32,
        dy: i32,
    },
    /// Windows scan code (neutral key identifier; Mac mapping comes later).
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
    ClipboardText(String),
    /// One file of a batch: `index` counts from 0, `total` is the batch size.
    FileOffer {
        id: u64,
        name: String,
        size: u64,
        index: u32,
        total: u32,
    },
    FileChunk {
        id: u64,
        data: Vec<u8>,
    },
    FileEnd {
        id: u64,
    },
}

pub fn encode<T: Serialize>(v: &T) -> crate::Result<Vec<u8>> {
    postcard::to_allocvec(v).map_err(|e| crate::AppError::Protocol(e.to_string()))
}

pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> crate::Result<T> {
    postcard::from_bytes(b).map_err(|e| crate::AppError::Protocol(e.to_string()))
}
