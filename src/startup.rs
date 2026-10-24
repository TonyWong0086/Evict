use crate::programs::Hive;
use std::{
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
            });
        }
    }
    v
}
