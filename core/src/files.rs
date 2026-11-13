//! Receiving files copied on another PC into "Shared Files" on the Desktop.
//! Names from the network are never trusted: paths are stripped, Windows-illegal
//! characters replaced, and existing files are never overwritten.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{AppError, Result};

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn sanitize_file_name(name: &str) -> Result<String> {
    // Keep only the last path component, whichever slash style was used.
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned
        .trim_end_matches(['.', ' '])
        .trim_start()
        .to_string();
    if cleaned.is_empty() {
        return Err(AppError::UnsafeFileName(name.to_string()));
    }
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return Ok(format!("_{cleaned}"));
    }
    Ok(cleaned)
}

/// `name`, or `name (1)`, `name (2)` ... if it already exists.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("endless range")
}

struct Incoming {
    file: File,
    path: PathBuf,
    expected: u64,
    got: u64,
}

/// Receives files; several may arrive at once (each has its own id).
pub struct FileReceiver {
    dir: PathBuf,
    current: HashMap<u64, Incoming>,
}

impl FileReceiver {
    pub fn new(dir: PathBuf) -> Self {
        FileReceiver {
            dir,
            current: HashMap::new(),
        }
    }

    pub fn begin(&mut self, id: u64, name: &str, size: u64) -> Result<()> {
        self.abort(id);
        let safe = sanitize_file_name(name)?;
        fs::create_dir_all(&self.dir)?;
        let path = unique_path(&self.dir, &safe);
        let file = File::create(&path)?;
        self.current.insert(
            id,
            Incoming {
                file,
                path,
                expected: size,
                got: 0,
            },
        );
        Ok(())
    }

    pub fn chunk(&mut self, id: u64, data: &[u8]) -> Result<()> {
        let cur = self
            .current
            .get_mut(&id)
            .ok_or_else(|| AppError::Protocol("file data without an offer".into()))?;
        cur.got += data.len() as u64;
        if cur.got > cur.expected {
            self.abort(id);
            return Err(AppError::Protocol("file is larger than announced".into()));
        }
        cur.file.write_all(data)?;
        Ok(())
    }

    /// Returns the saved path.
    pub fn finish(&mut self, id: u64) -> Result<PathBuf> {
        match self.current.remove(&id) {
            Some(mut c) if c.got == c.expected => {
                c.file.flush()?;
                Ok(c.path)
            }
            Some(c) => {
                drop(c.file);
                let _ = fs::remove_file(&c.path);
                Err(AppError::Protocol("file transfer was incomplete".into()))
            }
            None => Err(AppError::Protocol("no file in progress".into())),
        }
    }

    /// Drop one half-received file.
    pub fn abort(&mut self, id: u64) {
        if let Some(c) = self.current.remove(&id) {
            drop(c.file);
            let _ = fs::remove_file(&c.path);
        }
    }
}

impl Drop for FileReceiver {
    /// Connection lost: never leave half-written files behind.
    fn drop(&mut self) {
        let ids: Vec<u64> = self.current.keys().copied().collect();
        for id in ids {
            self.abort(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(
            sanitize_file_name("..\\..\\Windows\\evil.exe").unwrap(),
            "evil.exe"
        );
        assert_eq!(sanitize_file_name("/etc/passwd").unwrap(), "passwd");
        assert_eq!(sanitize_file_name("a:b?.txt").unwrap(), "a_b_.txt");
        assert_eq!(sanitize_file_name("report.docx. ").unwrap(), "report.docx");
        assert_eq!(sanitize_file_name("nul.txt").unwrap(), "_nul.txt");
        assert!(sanitize_file_name("..").is_err());
        assert!(sanitize_file_name("").is_err());
        assert!(sanitize_file_name("folder/").is_err());
    }

    #[test]
    fn receives_file_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("Shared Files");
        let mut r = FileReceiver::new(shared.clone());
        for round in 0..2 {
            r.begin(round, "notes.txt", 11).unwrap();
            r.chunk(round, b"hello ").unwrap();
            r.chunk(round, b"world").unwrap();
            r.finish(round).unwrap();
        }
        assert_eq!(fs::read(shared.join("notes.txt")).unwrap(), b"hello world");
        assert_eq!(
            fs::read(shared.join("notes (1).txt")).unwrap(),
            b"hello world"
        );
    }

    #[test]
    fn bad_transfers_leave_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = FileReceiver::new(dir.path().to_path_buf());
        r.begin(1, "a.bin", 4).unwrap();
        r.chunk(1, b"ab").unwrap();
        assert!(r.finish(1).is_err()); // incomplete
        assert!(!dir.path().join("a.bin").exists());
        r.begin(2, "b.bin", 2).unwrap();
        assert!(r.chunk(2, b"abc").is_err()); // too big
        assert!(!dir.path().join("b.bin").exists());
        assert!(r.chunk(9, b"x").is_err()); // no offer
    }

    #[test]
    fn two_files_at_once_and_cleanup_when_connection_drops() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = FileReceiver::new(dir.path().to_path_buf());
        r.begin(1, "one.txt", 4).unwrap();
        r.begin(2, "two.txt", 4).unwrap();
        r.chunk(1, b"11").unwrap();
        r.chunk(2, b"2222").unwrap();
        r.chunk(1, b"11").unwrap();
        r.finish(2).unwrap();
        r.finish(1).unwrap();
        assert_eq!(fs::read(dir.path().join("one.txt")).unwrap(), b"1111");
        assert_eq!(fs::read(dir.path().join("two.txt")).unwrap(), b"2222");
        r.begin(3, "half.bin", 10).unwrap();
        r.chunk(3, b"abc").unwrap();
        drop(r);
        assert!(!dir.path().join("half.bin").exists());
    }
}
