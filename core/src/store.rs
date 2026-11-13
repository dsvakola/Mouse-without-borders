//! Settings and saved pairings, kept as JSON in the app data folder.
//! Writes are atomic: a temporary file is written, then renamed over the real one.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AppError, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedPeer {
    pub id: String,
    pub name: String,
    /// Pairing secret, hex encoded.
    pub secret: String,
}

/// Which cell of the arrangement grid a computer sits in (3 columns x 2 rows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    pub id: String,
    pub col: u8,
    pub row: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub machine_id: String,
    pub machine_name: String,
    /// "master" or "secondary".
    pub role: String,
    /// Secondary only: connect to this address instead of searching.
    pub master_ip: String,
    pub two_row: bool,
    pub share_clipboard: bool,
    pub transfer_files: bool,
    pub start_with_windows: bool,
    /// Secondary only: where the master was last found (tried when search fails).
    pub last_master_addr: String,
    pub last_master_id: String,
    pub cells: Vec<Cell>,
    pub peers: Vec<PairedPeer>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::new("PC")
    }
}

impl Settings {
    pub fn new(machine_name: &str) -> Settings {
        Settings {
            machine_id: hex(&crate::crypto::random_secret()[..8]),
            machine_name: machine_name.to_string(),
            role: "master".into(),
            master_ip: String::new(),
            two_row: false,
            share_clipboard: true,
            transfer_files: true,
            start_with_windows: true,
            last_master_addr: String::new(),
            last_master_id: String::new(),
            cells: Vec::new(),
            peers: Vec::new(),
        }
    }

    pub fn secret_for(&self, id: &str) -> Option<[u8; 32]> {
        let p = self.peers.iter().find(|p| p.id == id)?;
        unhex32(&p.secret)
    }

    pub fn save_pairing(&mut self, id: &str, name: &str, secret: &[u8; 32]) {
        let entry = PairedPeer {
            id: id.into(),
            name: name.into(),
            secret: hex(secret),
        };
        match self.peers.iter_mut().find(|p| p.id == id) {
            Some(p) => *p = entry,
            None => self.peers.push(entry),
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.peers.retain(|p| p.id != id);
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

pub fn load(dir: &Path) -> Result<Option<Settings>> {
    let p = settings_path(dir);
    if !p.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&p).map_err(|e| AppError::Store(e.to_string()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| AppError::Store(e.to_string()))
}

pub fn save(dir: &Path, s: &Settings) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| AppError::Store(e.to_string()))?;
    let tmp = dir.join("settings.json.tmp");
    let json = serde_json::to_string_pretty(s).map_err(|e| AppError::Store(e.to_string()))?;
    fs::write(&tmp, json).map_err(|e| AppError::Store(e.to_string()))?;
    fs::rename(&tmp, settings_path(dir)).map_err(|e| AppError::Store(e.to_string()))
}

/// Save only what the running engine has learnt (pairings and, on the
/// master, the arrangement; on a secondary, where the master was found).
/// Everything else is taken from the file, so options the user changed in
/// the window meanwhile are never put back to old values.
pub fn save_learnt(dir: &Path, mine: &Settings, master: bool) -> Result<()> {
    let mut out = match load(dir) {
        Ok(Some(on_disk)) => on_disk,
        _ => mine.clone(),
    };
    out.peers = mine.peers.clone();
    if master {
        out.cells = mine.cells.clone();
        out.two_row = mine.two_row;
    } else {
        out.last_master_addr = mine.last_master_addr.clone();
        out.last_master_id = mine.last_master_id.clone();
    }
    save(dir, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_saves_do_not_undo_options_changed_in_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine_copy = Settings::new("PC-1");
        save(dir.path(), &engine_copy).unwrap();
        // The user switches two options off in the window...
        let mut window_copy = engine_copy.clone();
        window_copy.start_with_windows = false;
        window_copy.share_clipboard = false;
        save(dir.path(), &window_copy).unwrap();
        // ...and later the engine saves a new pairing from its older copy.
        engine_copy.save_pairing("abc", "PC-2", &[7u8; 32]);
        engine_copy.last_master_addr = "192.168.1.10:24800".into();
        save_learnt(dir.path(), &engine_copy, false).unwrap();
        let s = load(dir.path()).unwrap().unwrap();
        assert!(!s.start_with_windows);
        assert!(!s.share_clipboard);
        assert_eq!(s.secret_for("abc"), Some([7u8; 32]));
        assert_eq!(s.last_master_addr, "192.168.1.10:24800");
    }

    #[test]
    fn save_load_roundtrip_and_pairing_lookup() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
        let mut s = Settings::new("PC-A");
        s.save_pairing("b1", "PC-B", &[9u8; 32]);
        save(dir.path(), &s).unwrap();
        let back = load(dir.path()).unwrap().unwrap();
        assert_eq!(back, s);
        assert_eq!(back.secret_for("b1"), Some([9u8; 32]));
        assert_eq!(back.secret_for("zz"), None);
        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    #[test]
    fn re_pairing_replaces_and_forget_removes() {
        let mut s = Settings::new("PC-A");
        s.save_pairing("b1", "PC-B", &[1u8; 32]);
        s.save_pairing("b1", "PC-B2", &[2u8; 32]);
        assert_eq!(s.peers.len(), 1);
        assert_eq!(s.secret_for("b1"), Some([2u8; 32]));
        s.forget("b1");
        assert!(s.peers.is_empty());
    }

    #[test]
    fn settings_from_an_older_version_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let old = r#"{"machineId":"abc","machineName":"PC-A","role":"secondary","peers":[]}"#;
        fs::write(settings_path(dir.path()), old).unwrap();
        let s = load(dir.path()).unwrap().unwrap();
        assert_eq!(s.machine_id, "abc");
        assert_eq!(s.role, "secondary");
        assert!(s.start_with_windows);
        assert!(s.last_master_addr.is_empty());
    }

    #[test]
    fn corrupt_file_gives_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(settings_path(dir.path()), "{not json").unwrap();
        assert!(matches!(load(dir.path()), Err(AppError::Store(_))));
    }
}
