use crate::programs::{self, Hive};
use std::{
    collections::HashMap,
    env, fs, io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use winreg::{enums::*, RegValue};

const RUN: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager stores the enabled/disabled switch.
const APPROVED: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

pub enum Where {
    Reg(&'static str),
    Folder(PathBuf),
}

pub struct Entry {
    pub name: String,
    pub command: String,
    pub hive: Hive,
    pub at: Where,
    approved: &'static str,
    pub enabled: bool,
    /// Task Manager's rating; empty when the traces can't be read (needs admin)
    pub impact: &'static str,
}

impl Entry {
    pub fn location(&self) -> String {
        match &self.at {
            Where::Reg(k) => format!(r"{}\{k}", self.hive.name()),
            Where::Folder(p) => p.display().to_string(),
        }
    }

    pub fn set_enabled(&mut self, on: bool) -> io::Result<()> {
        let (k, _) = self.hive.root().create_subkey(format!(r"{APPROVED}\{}", self.approved))?;
        // 12 bytes: flag (2 = on, 3 = off) + reserved + FILETIME of when it was disabled
        let mut bytes = vec![0u8; 12];
        bytes[0] = if on { 2 } else { 3 };
        if !on {
            let ft = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64 / 100
                + 116_444_736_000_000_000;
            bytes[4..].copy_from_slice(&ft.to_le_bytes());
        }
        k.set_raw_value(&self.name, &RegValue { bytes, vtype: REG_BINARY })?;
        self.enabled = on;
        Ok(())
    }

    pub fn delete(&self) -> io::Result<()> {
        match &self.at {
            Where::Reg(k) => self.hive.root().open_subkey_with_flags(k, KEY_SET_VALUE)?.delete_value(&self.name)?,
            Where::Folder(p) => fs::remove_file(p.join(&self.name))?,
        }
        if let Ok(k) = self.hive.root().open_subkey_with_flags(format!(r"{APPROVED}\{}", self.approved), KEY_SET_VALUE) {
            let _ = k.delete_value(&self.name);
        }
        Ok(())
    }
}

fn approved(hive: Hive, sub: &str, name: &str) -> bool {
    hive.root()
        .open_subkey(format!(r"{APPROVED}\{sub}"))
        .and_then(|k| k.get_raw_value(name))
        .map_or(true, |v| v.bytes.first().map_or(true, |b| b & 1 == 0))
}

/// Worst sign-in cost per exe (lowercase path) as (CPU µs, disk bytes), from the traces Windows
/// keeps for Task Manager. None without admin rights.
fn traces() -> Option<HashMap<String, (u64, u64)>> {
    let dir = PathBuf::from(env::var_os("SystemRoot")?).join(r"System32\wdi\LogFiles\StartupInfo");
    let mut worst: HashMap<String, (u64, u64)> = HashMap::new();
    for e in fs::read_dir(dir).ok()?.flatten() {
        let Ok(b) = fs::read(e.path()) else { continue };
        let xml = String::from_utf16_lossy(&b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
        let tag = |p: &str, t: &str| p.split(&format!("<{t} ")).nth(1)?.split('>').nth(1)?.split('<').next()?.parse::<u64>().ok();
        let mut sum: HashMap<String, (u64, u64)> = HashMap::new();
        for p in xml.split("<Process Name=\"").skip(1) {
            let Some(name) = p.split('"').next() else { continue };
            let s = sum.entry(name.to_lowercase()).or_default();
            s.0 += tag(p, "CpuUsage").unwrap_or(0);
            s.1 += tag(p, "DiskUsage").unwrap_or(0);
        }
        for (k, (c, d)) in sum {
            let w = worst.entry(k).or_default();
            *w = (w.0.max(c), w.1.max(d));
        }
    }
    Some(worst)
}

/// Task Manager's thresholds: High over 1 s CPU or 3 MB disk, Medium over 300 ms or 300 KB.
fn rate(cpu_us: u64, disk: u64) -> &'static str {
    if cpu_us > 1_000_000 || disk > 3 << 20 {
        "High"
    } else if cpu_us > 300_000 || disk > 300 << 10 {
        "Medium"
    } else {
        "Low"
    }
}

pub fn list() -> Vec<Entry> {
    let mut v = Vec::new();
    for (hive, key, ap) in [(Hive::CU, RUN, "Run"), (Hive::LM, RUN, "Run"), (Hive::LM, RUN32, "Run32")] {
        let Ok(k) = hive.root().open_subkey(key) else { continue };
        for (name, _) in k.enum_values().flatten() {
            v.push(Entry {
                enabled: approved(hive, ap, &name),
                command: k.get_value::<String, _>(&name).unwrap_or_default(),
                name,
                hive,
                at: Where::Reg(key),
                approved: ap,
                impact: "",
            });
        }
    }
    for (hive, var) in [(Hive::CU, "APPDATA"), (Hive::LM, "ProgramData")] {
        let Some(base) = env::var_os(var) else { continue };
        let dir = PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs\Startup");
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            v.push(Entry {
                enabled: approved(hive, "StartupFolder", &name),
                command: e.path().display().to_string(),
                name,
                hive,
                at: Where::Folder(dir.clone()),
                approved: "StartupFolder",
                impact: "",
            });
        }
    }
    if let Some(t) = traces() {
        for e in &mut v {
            // ponytail: shortcuts in the Startup folder aren't resolved to their target, so they stay unmeasured
            let exe = programs::exe_path(&e.command).map(|p| p.display().to_string().to_lowercase());
            e.impact = exe.and_then(|x| t.get(&x)).map_or("Not measured", |&(c, d)| rate(c, d));
        }
    }
    v
}

#[test]
fn ratings() {
    assert_eq!(rate(1_200_000, 0), "High");
    assert_eq!(rate(0, 4 << 20), "High");
    assert_eq!(rate(500_000, 0), "Medium");
    assert_eq!(rate(100_000, 1000), "Low");
}
