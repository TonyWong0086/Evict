//! Uninstall history: what Evict removed and when, in %APPDATA%\Evict\history.json.
use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub when: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub items: usize,
    pub bytes: u64,
}

fn path() -> PathBuf {
    PathBuf::from(env::var_os("APPDATA").unwrap_or_default()).join(r"Evict\history.json")
}

pub fn load() -> Vec<Entry> {
    fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(v: &[Entry]) -> io::Result<()> {
    let p = path();
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(p, serde_json::to_vec_pretty(v)?)
}
