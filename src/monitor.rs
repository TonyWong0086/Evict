use crate::programs::{Hive, Item, Program, UNINSTALL, UNINSTALL32};
use notify::{event::ModifyKind, EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashSet},
    env, fs, io,
    path::PathBuf,
    process::Command,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};
use winreg::RegKey;

type Keys = HashSet<(Hive, String)>;

/// Paths that change constantly no matter what you install.
// ponytail: substring blocklist; per-process attribution (ETW) is the real fix if noise hurts
const NOISE: &[&str] = &[
    r"\temp\", r"\appdata\local\microsoft\", r"\appdata\local\packages\", r"\appdata\local\google\chrome\",
    r"\appdata\local\mozilla\", r"\appdata\roaming\mozilla\", r"\programdata\microsoft\windows defender",
    r"\programdata\microsoft\windows\wer", r"\crashdumps\", r"\evict\traces",
];

#[derive(Serialize, Deserialize, Clone)]
pub struct Trace {
    pub name: String,
    pub uninstall_keys: Vec<(Hive, String)>,
    pub items: Vec<Item>,
}

pub fn dir() -> PathBuf {
    PathBuf::from(env::var_os("APPDATA").unwrap_or_default()).join(r"Evict\traces")
}

pub fn load_all() -> Vec<Trace> {
    let mut v: Vec<Trace> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok())
        .collect();
    v.sort_by_key(|t| t.name.to_lowercase());
    v
}

impl Trace {
    fn path(&self) -> PathBuf {
        let safe: String = self.name.chars().map(|c| if c.is_alphanumeric() || " -.".contains(c) { c } else { '_' }).collect();
        dir().join(format!("{safe}.json"))
    }
    pub fn save(&self) -> io::Result<()> {
        fs::create_dir_all(dir())?;
        fs::write(self.path(), serde_json::to_vec_pretty(self)?)
    }
    pub fn remove(&self) -> io::Result<()> {
        fs::remove_file(self.path())
    }
    pub fn matches(&self, p: &Program) -> bool {
        self.uninstall_keys.iter().any(|(h, k)| *h == p.hive && k.eq_ignore_ascii_case(&p.key))
    }
}

pub struct Session {
    _watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    before: Keys,
    pub label: String,
    pub created: BTreeSet<PathBuf>,
}

/// Snapshot the registry, start watching the disk, then launch the installer (if any).
pub fn start(installer: String) -> Result<Session, String> {
    let before = snapshot();
    let (tx, rx) = mpsc::channel();
    let mut w = notify::recommended_watcher(tx).map_err(|e| e.to_string())?;
    for r in roots() {
        let _ = w.watch(&r, RecursiveMode::Recursive);
    }
    let path = PathBuf::from(installer.trim().trim_matches('"'));
    let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if !label.is_empty() {
        let is_msi = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("msi"));
        let mut cmd = if is_msi { Command::new("msiexec") } else { Command::new(&path) };
        if is_msi {
            cmd.arg("/i").arg(&path);
        }
        cmd.spawn().map_err(|e| format!("Couldn't start installer: {e}"))?;
    }
    Ok(Session { _watcher: w, rx, before, label, created: BTreeSet::new() })
}

impl Session {
    pub fn poll(&mut self) {
        for ev in self.rx.try_iter().flatten() {
            if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))) {
                self.created.extend(ev.paths);
            }
        }
    }

    pub fn finish(mut self) -> Trace {
        self.poll();
        let new: Vec<(Hive, String)> = snapshot().difference(&self.before).cloned().collect();
        let uninstall_keys: Vec<_> = new
            .iter()
            .filter(|(_, k)| k.to_lowercase().contains(r"\currentversion\uninstall\"))
            .cloned()
            .collect();
        let mut items = Vec::new();
        for h in [Hive::LM, Hive::CU] {
            let keys = new.iter().filter(|(x, _)| *x == h).map(|(_, k)| k.clone()).collect();
            items.extend(topmost(keys).into_iter().map(|k| Item::Key(h, k)));
        }
        let files = self
            .created
            .iter()
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .filter(|s| {
                let l = s.to_lowercase();
                !NOISE.iter().any(|n| l.contains(n))
            })
            .collect();
        for f in topmost(files) {
            let p = PathBuf::from(f);
            items.push(if p.is_dir() { Item::Dir(p) } else { Item::File(p) });
        }
        let display = uninstall_keys
            .first()
            .and_then(|(h, k)| h.root().open_subkey(k).ok()?.get_value::<String, _>("DisplayName").ok());
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let name = display
            .or((!self.label.is_empty()).then(|| self.label.clone()))
            .unwrap_or_else(|| format!("trace-{secs}"));
        Trace { name, uninstall_keys, items }
    }
}

fn roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramData", "APPDATA", "LOCALAPPDATA", "PUBLIC"]
        .into_iter()
        .filter_map(|k| env::var_os(k).map(PathBuf::from))
        .collect();
    v.extend(env::var_os("USERPROFILE").map(|u| PathBuf::from(u).join("Desktop")));
    v.retain(|p| p.is_dir());
    v.sort();
    v.dedup();
    v
}

fn snapshot() -> Keys {
    let mut out = Keys::new();
    let roots = [
        (Hive::LM, "SOFTWARE", 4),
        (Hive::CU, "SOFTWARE", 4),
        (Hive::LM, UNINSTALL, 1),
        (Hive::LM, UNINSTALL32, 1),
        (Hive::CU, UNINSTALL, 1),
        (Hive::LM, r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths", 1),
    ];
    for (h, path, depth) in roots {
        if let Ok(k) = h.root().open_subkey(path) {
            walk(h, &k, path.to_string(), depth, &mut out);
        }
    }
    out
}

// ponytail: depth-limited registry walk; keys created deeper than 4 levels are missed
fn walk(h: Hive, key: &RegKey, path: String, depth: u32, out: &mut Keys) {
    if depth > 0 {
        for sub in key.enum_keys().flatten() {
            let Ok(child) = key.open_subkey(&sub) else { continue };
            // Classes is enormous; tracking its direct children catches new extensions / ProgIDs
            let d = if sub.eq_ignore_ascii_case("Classes") { 1 } else { depth - 1 };
            walk(h, &child, format!(r"{path}\{sub}"), d, out);
        }
    }
    out.insert((h, path));
}

/// Drop every path whose parent is also in the list (delete the folder, not each file).
pub fn topmost(v: Vec<String>) -> Vec<String> {
    let set: HashSet<String> = v.iter().map(|s| s.to_lowercase()).collect();
    let mut out: Vec<String> = v
        .into_iter()
        .filter(|s| {
            let l = s.to_lowercase();
            !l.match_indices('\\').any(|(i, _)| set.contains(&l[..i]))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}
