use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use winreg::{enums::*, RegKey};

pub const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
pub const UNINSTALL32: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall";

/// Names that are never treated as "belonging" to one program.
const BLOCK: &[&str] = &[
    "microsoft", "windows", "commonfiles", "classes", "policies", "wow6432node", "clients",
    "registeredapplications", "packages", "programs", "temp", "startmenu",
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Hive {
    LM,
    CU,
}

impl Hive {
    pub fn root(self) -> RegKey {
        RegKey::predef(match self {
            Hive::LM => HKEY_LOCAL_MACHINE,
            Hive::CU => HKEY_CURRENT_USER,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Hive::LM => "HKEY_LOCAL_MACHINE",
            Hive::CU => "HKEY_CURRENT_USER",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Item {
    Dir(PathBuf),
    File(PathBuf),
    Key(Hive, String),
}

impl Item {
    pub fn label(&self) -> String {
        match self {
            Item::Dir(p) | Item::File(p) => p.display().to_string(),
            Item::Key(h, k) => format!(r"{}\{k}", h.name()),
        }
    }
    pub fn exists(&self) -> bool {
        match self {
            Item::Dir(p) | Item::File(p) => p.exists(),
            Item::Key(h, k) => h.root().open_subkey(k).is_ok(),
        }
    }
    pub fn delete(&self) -> io::Result<()> {
        match self {
            Item::Dir(p) => fs::remove_dir_all(p),
            Item::File(p) => fs::remove_file(p),
            Item::Key(h, k) => h.root().delete_subkey_all(k),
        }
    }
}

#[derive(Clone)]
pub struct Program {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub size_kb: u32,
    pub date: String,
    pub location: String,
    pub uninstall: String,
    pub modify: String,
    pub url: String,
    pub hive: Hive,
    pub key: String,
    pub icon: String,
    pub is32: bool,
}

impl Program {
    pub fn id(&self) -> String {
        format!(r"{:?}\{}", self.hive, self.key)
    }
    pub fn folder(&self) -> PathBuf {
        PathBuf::from(self.location.trim().trim_matches('"'))
    }

    /// Where the program actually lives. `strict` sticks to what the registry (or Steam) states,
    /// which is what deletion uses; sizing also accepts folders inferred from the icon / uninstaller.
    pub fn install_dir(&self, all: &[Program], strict: bool) -> Option<PathBuf> {
        let name = core_name(&self.name);
        // A folder another program also claims (e.g. "Program Files\Adobe") isn't this program's.
        let taken = |d: &Path| {
            let l = lower(d);
            all.iter().any(|o| o.key != self.key && !o.location.trim().is_empty() && lower(&o.folder()) == l)
        };
        let mut c = Vec::new();
        if !self.location.trim().is_empty() {
            let loc = self.folder();
            if taken(&loc) {
                c.extend(subdirs(&loc).filter(|(n, _)| *n == name).map(|(_, d)| d));
            } else {
                c.push(loc);
            }
        }
        c.extend(steam_app_dir(&self.key));
        if !strict {
            c.extend(exe_dir(&self.icon));
            c.extend(exe_dir(&self.uninstall));
        }
        let dir = c.into_iter().find(|d| d.is_dir() && !protected(d) && !lower(d).contains(r"\package cache") && !taken(d))?;
        Some(if strict { dir } else { climb(dir) })
    }

    /// The install folder, if it's safe to delete: nothing else is installed inside it.
    pub fn own_folder(&self, all: &[Program]) -> Option<PathBuf> {
        let d = self.install_dir(all, true)?;
        let prefix = lower(&d) + "\\";
        let nested = all.iter().any(|o| o.key != self.key && !o.location.trim().is_empty() && lower(&o.folder()).starts_with(&prefix));
        (!nested).then_some(d)
    }

    /// Folders whose size counts toward this program: install folder plus data/cache folders named after it.
    pub fn footprint(&self, all: &[Program], index: &DirIndex) -> Vec<PathBuf> {
        let install = self.install_dir(all, false);
        let inside = install.as_ref().map(|d| lower(d)).unwrap_or_default();
        // A same-named folder *containing* the install dir (Program Files\Unity holds every editor) is a parent, not data.
        let named = index.named(self).into_iter().filter(|d| !inside.starts_with(&(lower(d) + "\\")));
        let dirs = install.into_iter().chain(named).map(|d| d.display().to_string()).collect();
        crate::monitor::topmost(dirs).into_iter().map(PathBuf::from).collect()
    }
}

/// Bytes on disk under `p`, not following junctions/symlinks, skipping subfolders in `skip`
/// (lowercased paths: other programs installed inside this one, e.g. games inside Steam).
pub fn folder_size(p: &Path, skip: &HashSet<String>) -> u64 {
    fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() && !skip.contains(&lower(&e.path())) => folder_size(&e.path(), skip),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// Folder of the exe in `"C:\x\app.exe" --args` or `C:\x\app.exe,0`; launchers with URLs don't count.
fn exe_dir(cmd: &str) -> Option<PathBuf> {
    let s = cmd.trim();
    if s.contains("://") {
        return None;
    }
    let path = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => &s[..s.to_lowercase().find(".exe")? + 4],
    };
    let path = Path::new(path);
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        return None;
    }
    path.parent().map(Path::to_path_buf)
}

/// Folders that are a layer inside an app, not the app: "version-4310…", "app-1.0.9", "Versions", "Application", "bin", "26.0".
fn versionish(name: &str) -> bool {
    let n = name.to_lowercase();
    let layers = ["versions", "version", "application", "app", "current", "bin", "bin64", "x64", "x86", "win64", "win32"];
    layers.contains(&n.as_str())
        || n.starts_with("version-")
        || n.starts_with("app-")
        || (n.starts_with(|c: char| c.is_ascii_digit()) && n.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Roblox\Versions\version-4310… → Roblox, so caches next to the versions are counted.
fn climb(mut d: PathBuf) -> PathBuf {
    while d.file_name().is_some_and(|n| versionish(&n.to_string_lossy())) {
        match d.parent() {
            Some(p) if !protected(p) => d = p.to_path_buf(),
            _ => break,
        }
    }
    d
}

/// True if the PE file is x86 (32-bit). None if it isn't a readable PE.
fn pe_is_x86(path: &str) -> Option<bool> {
    let path = path.trim().trim_matches('"');
    let path = path.rsplit_once(',').filter(|(_, i)| i.trim().parse::<i32>().is_ok()).map_or(path, |(p, _)| p);
    let mut buf = Vec::new();
    fs::File::open(path.trim_matches('"')).ok()?.take(4096).read_to_end(&mut buf).ok()?;
    let off = u32::from_le_bytes(buf.get(0x3c..0x40)?.try_into().ok()?) as usize;
    (buf.get(off..off + 4)? == b"PE\0\0").then_some(())?;
    Some(u16::from_le_bytes(buf.get(off + 4..off + 6)?.try_into().ok()?) == 0x14c)
}

/// Steam games ("Steam App 252530") don't record InstallLocation; ask Steam's own manifests.
fn steam_app_dir(key: &str) -> Option<PathBuf> {
    let id = key.rsplit('\\').next()?.strip_prefix("Steam App ")?;
    steam_libraries().iter().find_map(|lib| {
        let acf = fs::read_to_string(lib.join(format!(r"steamapps\appmanifest_{id}.acf"))).ok()?;
        let dir = vdf_values(&acf, "installdir").next()?;
        Some(lib.join(r"steamapps\common").join(dir))
    })
}

// ponytail: read once per run; libraries added while Evict is open need a restart
fn steam_libraries() -> &'static [PathBuf] {
    static LIBS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    LIBS.get_or_init(|| {
        let Ok(root) = Hive::CU.root().open_subkey(r"Software\Valve\Steam").and_then(|k| k.get_value::<String, _>("SteamPath")) else {
            return Vec::new();
        };
        let root = PathBuf::from(root.replace('/', "\\"));
        let vdf = fs::read_to_string(root.join(r"steamapps\libraryfolders.vdf")).unwrap_or_default();
        let mut libs: Vec<PathBuf> = vdf_values(&vdf, "path").map(PathBuf::from).collect();
        libs.push(root);
        libs
    })
}

/// Values of `"key"  "value"` lines in Valve's KeyValues text files.
fn vdf_values<'a>(text: &'a str, key: &'a str) -> impl Iterator<Item = String> + 'a {
    text.lines().filter_map(move |l| {
        let p: Vec<&str> = l.split('"').collect();
        (p.len() >= 4 && p[1].eq_ignore_ascii_case(key)).then(|| p[3].replace(r"\\", r"\"))
    })
}

pub fn list() -> Vec<Program> {
    let mut out = Vec::new();
    for (hive, path) in [(Hive::LM, UNINSTALL), (Hive::LM, UNINSTALL32), (Hive::CU, UNINSTALL)] {
        let Ok(root) = hive.root().open_subkey(path) else { continue };
        for sub in root.enum_keys().flatten() {
            let Ok(k) = root.open_subkey(&sub) else { continue };
            // some installers (Roblox) leave a NUL at the end of their strings
            let s = |n: &str| k.get_value::<String, _>(n).unwrap_or_default().trim_end_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
            let d = |n: &str| k.get_value::<u32, _>(n).ok();
            let name = s("DisplayName").trim().to_string();
            if name.trim().is_empty() || d("SystemComponent") == Some(1) || !s("ParentKeyName").is_empty() {
                continue;
            }
            // No InstallDate → when the uninstall entry was last written, like Geek
            let date = iso_date(&s("InstallDate"))
                .or_else(|| {
                    let t = k.query_info().ok()?.get_last_write_time_system();
                    Some(format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay))
                })
                .unwrap_or_default();
            out.push(Program {
                name,
                publisher: s("Publisher"),
                version: s("DisplayVersion"),
                size_kb: d("EstimatedSize").unwrap_or(0),
                date,
                location: s("InstallLocation"),
                uninstall: s("UninstallString"),
                modify: if d("NoModify") == Some(1) { String::new() } else { s("ModifyPath") },
                url: s("URLInfoAbout"),
                hive,
                key: format!(r"{path}\{sub}"),
                // Geek's rule: the program's own exe decides, not which registry view the installer used
                is32: pe_is_x86(&s("DisplayIcon")).unwrap_or_else(|| {
                    let x86 = env::var_os("ProgramFiles(x86)").map(|v| lower(Path::new(&v)) + "\\");
                    x86.is_some_and(|x| lower(Path::new(s("InstallLocation").trim().trim_matches('"'))).starts_with(&x))
                }),
                icon: s("DisplayIcon"),
            });
        }
    }
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// InstallDate is "20250929" by spec, but some installers write "9/29/2025".
pub fn iso_date(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..]));
    }
    let n: Vec<u32> = s.split('/').map(|x| x.trim().parse().ok()).collect::<Option<_>>()?;
    match n[..] {
        [m, d, y] if (1..=12).contains(&m) && (1..=31).contains(&d) => Some(format!("{y:04}-{m:02}-{d:02}")),
        _ => None,
    }
}

/// "2025-09-29" -> "Sep 29, 2025"
pub fn pretty_date(iso: &str) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut it = iso.split('-').map(|x| x.parse::<usize>().ok());
    match (it.next().flatten(), it.next().flatten(), it.next().flatten()) {
        (Some(y), Some(m @ 1..=12), Some(d)) => format!("{} {d:02}, {y}", M[m - 1]),
        _ => iso.into(),
    }
}

/// "Mozilla Firefox (x64 en-US) 131.0" -> "mozillafirefox", "Foo Co., Ltd." -> "foo"
pub fn core_name(s: &str) -> String {
    const NOISE: &[&str] = &[
        "x64", "x86", "64-bit", "32-bit", "inc", "inc.", "llc", "ltd", "ltd.", "co.", "corporation",
        "corp", "corp.", "gmbh", "s.a.",
    ];
    s.split(['(', '['])
        .next()
        .unwrap_or(s)
        .split_whitespace()
        .filter(|w| {
            // "24.08" / "v3" are versions; a bare "2026" is part of the product name
            let v = w.trim_start_matches(['v', 'V']);
            let numeric = v.starts_with(|c: char| c.is_ascii_digit()) && v.chars().all(|c| c.is_ascii_digit() || c == '.');
            !(numeric && (v.contains('.') || v.len() < w.len()))
        })
        .filter(|w| !NOISE.contains(&w.to_lowercase().trim_end_matches(',')))
        .flat_map(|w| w.chars())
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn search_dirs() -> Vec<PathBuf> {
    let e = |k: &str| env::var_os(k).map(PathBuf::from);
    let mut v: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData", "APPDATA", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(e)
        .collect();
    v.extend(e("LOCALAPPDATA").map(|l| l.join("Programs")));
    v.extend(e("USERPROFILE").map(|u| u.join(r"AppData\LocalLow")));
    for k in ["APPDATA", "ProgramData"] {
        v.extend(e(k).map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs")));
    }
    v.sort();
    v.dedup();
    v
}

pub fn lower(p: &Path) -> String {
    p.to_string_lossy().trim_end_matches('\\').to_lowercase()
}

/// Folders we refuse to delete even if a program claims them as InstallLocation.
fn protected(p: &Path) -> bool {
    let l = lower(p);
    let env_dir = |k: &str| env::var_os(k).map(|v| lower(Path::new(&v)));
    p.components().count() <= 2
        || search_dirs().iter().any(|d| lower(d) == l)
        || env_dir("SystemRoot").is_some_and(|w| l.starts_with(&w))
        || env_dir("USERPROFILE").is_some_and(|u| u == l)
        || p.file_name().is_some_and(|n| BLOCK.contains(&core_name(&n.to_string_lossy()).as_str()))
}

fn subdirs(base: &Path) -> impl Iterator<Item = (String, PathBuf)> {
    fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (core_name(&e.file_name().to_string_lossy()), e.path()))
}

// ponytail: exact core-name match only; fuzzy matching finds more but deletes the wrong things
fn matchable(name: &str, publ: &str) -> bool {
    name.chars().count() >= 3 && name != publ && !BLOCK.contains(&name)
}

/// Candidate folders (core name, path) to match programs against, listed once.
pub struct DirIndex(Vec<(String, PathBuf)>);

impl DirIndex {
    /// Children of Program Files / AppData / ProgramData. `all_drives` adds the top two levels of every
    /// fixed drive (D:\Roblox, E:\Games\OMSI…) — fine for sizing, too loose for deletion.
    pub fn build(all_drives: bool) -> Self {
        let mut bases = search_dirs();
        if all_drives {
            let system = ["windows", "programfiles", "programfilesx86", "programdata", "users", "recyclebin", "systemvolumeinformation", "recovery", "perflogs"];
            for drive in crate::win::fixed_drives() {
                bases.extend(subdirs(&drive).filter(|(c, _)| !system.contains(&c.as_str())).map(|(_, d)| d));
                bases.push(drive);
            }
        }
        DirIndex(bases.iter().flat_map(|b| subdirs(b)).collect())
    }

    /// Folders named after the program, or after it inside a folder named after its publisher.
    pub fn named(&self, p: &Program) -> Vec<PathBuf> {
        let (name, publ) = (core_name(&p.name), core_name(&p.publisher));
        let mut v = Vec::new();
        if !matchable(&name, &publ) {
            return v;
        }
        for (c, d) in &self.0 {
            if *c == name {
                v.push(d.clone());
            } else if !publ.is_empty() && *c == publ {
                v.extend(subdirs(d).filter(|(c2, _)| *c2 == name).map(|(_, d2)| d2));
            }
        }
        v
    }
}

/// Everything a program probably left behind: its uninstall entry, install folder,
/// and folders / registry keys named after it (optionally under its publisher).
pub fn leftovers(p: &Program, all: &[Program]) -> Vec<Item> {
    let mut v = vec![Item::Key(p.hive, p.key.clone())];
    v.extend(p.own_folder(all).map(Item::Dir));
    v.extend(DirIndex::build(false).named(p).into_iter().map(Item::Dir));
    let (name, publ) = (core_name(&p.name), core_name(&p.publisher));
    if matchable(&name, &publ) {
        for hive in [Hive::LM, Hive::CU] {
            for soft in ["SOFTWARE", r"SOFTWARE\WOW6432Node"] {
                let Ok(k) = hive.root().open_subkey(soft) else { continue };
                for sub in k.enum_keys().flatten() {
                    let c = core_name(&sub);
                    if c == name {
                        v.push(Item::Key(hive, format!(r"{soft}\{sub}")));
                    } else if !publ.is_empty() && c == publ {
                        let Ok(pk) = k.open_subkey(&sub) else { continue };
                        for s2 in pk.enum_keys().flatten().filter(|s2| core_name(s2) == name) {
                            v.push(Item::Key(hive, format!(r"{soft}\{sub}\{s2}")));
                        }
                    }
                }
            }
        }
    }
    let mut out: Vec<Item> = Vec::new();
    for i in v {
        if i.exists() && !out.contains(&i) {
            out.push(i);
        }
    }
    out
}
