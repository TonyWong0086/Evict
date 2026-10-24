//! Window and list layout, remembered in %APPDATA%\Evict\settings.json.
use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window: [f32; 2],
    pub col_w: [f32; 5],
    /// column index into `Col::ALL`, ascending?
    pub sort: (usize, bool),
    pub tab: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { window: [1220.0, 780.0], col_w: [340.0, 110.0, 190.0, 85.0, 110.0], sort: (0, true), tab: 0 }
    }
}

fn path() -> PathBuf {
    PathBuf::from(env::var_os("APPDATA").unwrap_or_default()).join(r"Evict\settings.json")
}

pub fn load() -> Settings {
    fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(s: &Settings) -> io::Result<()> {
    let p = path();
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(p, serde_json::to_vec_pretty(s)?)
}
