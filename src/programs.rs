use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use winreg::{enums::*, types::FromRegValue, RegKey};

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
    /// One value inside a key (a Run entry…): hive, key, value name
    Value(Hive, String, String),
}

impl Item {
    pub fn label(&self) -> String {
        match self {
            Item::Dir(p) | Item::File(p) => p.display().to_string(),
            Item::Key(h, k) => format!(r"{}\{k}", h.name()),
            Item::Value(h, k, n) => format!(r"{}\{k}\{n}", h.name()),
        }
    }
    pub fn exists(&self) -> bool {
        match self {
            Item::Dir(p) | Item::File(p) => p.exists(),
            Item::Key(h, k) => h.root().open_subkey(k).is_ok(),
            Item::Value(h, k, n) => h.root().open_subkey(k).and_then(|k| k.get_raw_value(n)).is_ok(),
        }
    }
    pub fn delete(&self) -> io::Result<()> {
        match self {
            Item::Dir(p) => fs::remove_dir_all(p),
            Item::File(p) => fs::remove_file(p),
            Item::Key(h, k) => h.root().delete_subkey_all(k),
            Item::Value(h, k, n) => h.root().open_subkey_with_flags(k, KEY_SET_VALUE)?.delete_value(n),
        }
    }
    pub fn is_registry(&self) -> bool {
        matches!(self, Item::Key(..) | Item::Value(..))
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
    /// Entry left behind by a program that's already gone (see [`ghost`])
    pub ghost: bool,
}

impl Program {
    pub fn id(&self) -> String {
        format!(r"{:?}\{}", self.hive, self.key)
    }
    pub fn folder(&self) -> PathBuf {
        PathBuf::from(self.location.trim().trim_matches('"'))
    }

    /// Icon sources for when DisplayIcon is empty or stale, in this order: the MSI
    /// product icon, an exe in the install folder, a Start Menu shortcut with the program's name,
    /// then a generic one.
    pub fn icon_fallbacks(&self, all: &[Program]) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(packed) = msi_packed(self.key.trim_matches(['{', '}'])) {
            for (hive, key) in [(Hive::LM, r"SOFTWARE\Classes\Installer\Products"), (Hive::CU, r"Software\Microsoft\Installer\Products")] {
                v.extend(hive.root().open_subkey(format!(r"{key}\{packed}")).and_then(|k| k.get_value::<String, _>("ProductIcon")));
            }
        }
        v.extend(self.icon_exe(all));
        v.extend(start_menu().get(&lower_name(&self.name)).map(|p| p.display().to_string()));
        // runtimes and language packs ship no icon at all: the generic application one
        v.extend(env::var("SystemRoot").map(|r| format!(r"{r}\System32\shell32.dll,2")));
        v
    }

    /// An exe in the install folder: one named after the program, else the shallowest non-uninstaller.
    fn icon_exe(&self, all: &[Program]) -> Option<String> {
        let dir = self.install_dir(all, false)?;
        let want = lower_name(&self.name);
        let (mut level, mut named, mut first) = (vec![dir], None, None);
        // ponytail: 4 levels, 300 folders; deep enough for Epic's Launcher\Portal\Binaries\Win64
        for _ in 0..5 {
            let mut next = Vec::new();
            for d in level.iter().take(300) {
                for e in fs::read_dir(d).into_iter().flatten().flatten() {
                    let (p, n) = (e.path(), e.file_name().to_string_lossy().to_lowercase());
                    // "__Installer", "_CommonRedist" and setup / crash helpers aren't the program
                    let helper = ["unins", "setup", "install", "cleanup", "crash", "redist"].iter().any(|h| n.contains(h));
                    if p.is_dir() && !n.starts_with('_') {
                        next.push(p);
                    } else if n.ends_with(".exe") && !helper && named.is_none() {
                        if lower_name(&n[..n.len() - 4]) == want {
                            named = Some(p);
                        } else if first.is_none() {
                            first = Some(p);
                        }
                    }
                }
            }
            if named.is_some() {
                break;
            }
            level = next;
        }
        named.or(first).map(|p| p.display().to_string())
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

/// The exe in `"C:\x\app.exe" --args` or `C:\x\app.exe,0`; launchers with URLs don't count.
pub fn exe_path(cmd: &str) -> Option<PathBuf> {
    let s = cmd.trim();
    if s.contains("://") {
        return None;
    }
    let path = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => &s[..s.to_lowercase().find(".exe")? + 4],
    };
    let path = Path::new(path);
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")).then(|| path.to_path_buf())
}

fn exe_dir(cmd: &str) -> Option<PathBuf> {
    exe_path(cmd)?.parent().map(Path::to_path_buf)
}

/// A ghost entry: its uninstaller is gone (or Windows Installer no longer knows the MSI product)
/// and so is its install folder, so the program can't be uninstalled; only the entry is left.
fn ghost(uninstall: &str, location: &str) -> bool {
    let loc = location.trim().trim_matches('"');
    if !loc.is_empty() && Path::new(loc).exists() {
        return false;
    }
    let u = uninstall.trim();
    if u.to_lowercase().contains("msiexec") {
        let guid = u.split_once('{').and_then(|(_, r)| r.split_once('}')).map(|(g, _)| g);
        return guid.and_then(msi_packed).is_some_and(|p| {
            let known = |h: Hive, k: &str| h.root().open_subkey(format!(r"{k}\{p}")).is_ok();
            !known(Hive::LM, r"SOFTWARE\Classes\Installer\Products") && !known(Hive::CU, r"Software\Microsoft\Installer\Products")
        });
    }
    // relative paths (rundll32.exe …) are found on PATH; can't tell
    exe_path(u).is_some_and(|p| p.is_absolute() && !p.exists())
}

/// Windows Installer's "packed" product code: 90160000-008C-… → 00006109C800…
pub fn msi_packed(guid: &str) -> Option<String> {
    let h: Vec<char> = guid.chars().filter(|c| *c != '-').collect();
    if h.len() != 32 || !h.iter().all(char::is_ascii_hexdigit) {
        return None;
    }
    let rev = |a: usize, b: usize| h[a..b].iter().rev().collect::<String>();
    let pairs: String = h[16..].chunks(2).flat_map(|p| [p[1], p[0]]).collect();
    Some((rev(0, 8) + &rev(8, 12) + &rev(12, 16) + &pairs).to_uppercase())
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
            // No InstallDate → when the uninstall entry was last written
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
                // The program's own exe decides, not which registry view the installer used
                is32: pe_is_x86(&s("DisplayIcon")).unwrap_or_else(|| {
                    let x86 = env::var_os("ProgramFiles(x86)").map(|v| lower(Path::new(&v)) + "\\");
                    x86.is_some_and(|x| lower(Path::new(s("InstallLocation").trim().trim_matches('"'))).starts_with(&x))
                }),
                icon: s("DisplayIcon"),
                ghost: ghost(&s("UninstallString"), &s("InstallLocation")),
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
    /// fixed drive (D:\Roblox, E:\Games\OMSI…): fine for sizing, too loose for deletion.
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

/// The user's Desktop, wherever it's been redirected (OneDrive…).
pub fn desktop() -> PathBuf {
    let shell = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders";
    Hive::CU.root().open_subkey(shell).and_then(|k| k.get_value::<String, _>("Desktop")).map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env::var_os("USERPROFILE").unwrap_or_default()).join("Desktop")
    })
}

/// Core name without spaces or punctuation, lowercase: "Epic Games Launcher" → "epicgameslauncher".
fn lower_name(s: &str) -> String {
    core_name(s).chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Start Menu shortcuts by `lower_name` of their file name, read once.
fn start_menu() -> &'static HashMap<String, PathBuf> {
    static M: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    M.get_or_init(|| {
        let dirs = ["APPDATA", "ProgramData"].into_iter().filter_map(env::var_os).map(|p| PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs"));
        dirs.flat_map(|d| shortcuts(&d, 2)).filter_map(|f| Some((lower_name(&f.file_stem()?.to_string_lossy()), f))).collect()
    })
}

/// `.lnk` files under `dir`, `depth` folders deep.
fn shortcuts(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() && depth > 0 => v.extend(shortcuts(&p, depth - 1)),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) => v.push(p),
            _ => {}
        }
    }
    v
}

/// Does the shortcut's target path contain `needle` (lowercase)? Shell links store it as ANSI and/or UTF-16.
fn lnk_points(lnk: &Path, needle: &str) -> bool {
    let Ok(mut bytes) = fs::read(lnk) else { return false };
    bytes.make_ascii_lowercase();
    let wide: Vec<u8> = needle.encode_utf16().flat_map(u16::to_le_bytes).collect();
    [needle.as_bytes(), &wide].iter().any(|n| bytes.windows(n.len()).any(|w| w == *n))
}

/// Deep scan: what uninstallers commonly leave pointing at the old install folder (shortcuts, Run
/// entries, App Paths, services), plus shortcuts and temp folders named after the program.
/// `dir` is the install folder as it was *before* uninstalling.
pub fn references(p: &Program, dir: Option<&Path>) -> Vec<Item> {
    let (name, publ) = (core_name(&p.name), core_name(&p.publisher));
    let named = |s: &str| matchable(&name, &publ) && core_name(s) == name;
    let dir = dir.map(|d| lower(d) + "\\");
    let points = |s: &str| dir.as_ref().is_some_and(|d| s.to_lowercase().contains(d.as_str()));
    let e = |k: &str| env::var_os(k).map(PathBuf::from);
    let mut v = Vec::new();

    let mut places: Vec<PathBuf> = ["APPDATA", "ProgramData"].into_iter().filter_map(e).map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs")).collect();
    places.extend(e("PUBLIC").map(|p| p.join("Desktop")));
    places.push(desktop());
    for f in places.iter().flat_map(|d| shortcuts(d, 2)) {
        let stem = f.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        if named(&stem) || dir.as_ref().is_some_and(|d| lnk_points(&f, d)) {
            v.push(Item::File(f));
        }
    }
    if let Some(t) = e("TEMP") {
        v.extend(subdirs(&t).filter(|(c, _)| matchable(&name, &publ) && *c == name).map(|(_, d)| Item::Dir(d)));
    }
    if dir.is_none() {
        return v;
    }
    for hive in [Hive::LM, Hive::CU] {
        for key in [r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run", r"SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce", r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run"] {
            let Ok(k) = hive.root().open_subkey(key) else { continue };
            v.extend(k.enum_values().flatten().filter(|(_, val)| points(&val.to_string())).map(|(n, _)| Item::Value(hive, key.into(), n)));
        }
        let app_paths = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";
        let Ok(k) = hive.root().open_subkey(app_paths) else { continue };
        for sub in k.enum_keys().flatten() {
            if k.open_subkey(&sub).and_then(|s| s.get_value::<String, _>("")).is_ok_and(|t| points(&t)) {
                v.push(Item::Key(hive, format!(r"{app_paths}\{sub}")));
            }
        }
    }
    let services = r"SYSTEM\CurrentControlSet\Services";
    if let Ok(k) = Hive::LM.root().open_subkey(services) {
        for sub in k.enum_keys().flatten() {
            if k.open_subkey(&sub).and_then(|s| s.get_value::<String, _>("ImagePath")).is_ok_and(|t| points(&t)) {
                v.push(Item::Key(Hive::LM, format!(r"{services}\{sub}")));
            }
        }
    }
    v
}

/// Shared file protection: leftovers other software still needs are held back. That's a folder
/// holding (or inside) another program's install folder, or holding a file SharedDLLs says
/// more than one program uses.
pub struct Shared {
    dirs: Vec<String>,
    dlls: Vec<String>,
}

impl Shared {
    pub fn new(all: &[Program], removing: &[Program]) -> Self {
        let dirs = all
            .iter()
            .filter(|o| !removing.iter().any(|r| r.key == o.key) && Item::Key(o.hive, o.key.clone()).exists())
            .filter_map(|o| o.install_dir(all, true))
            .map(|d| lower(&d))
            .collect();
        let mut dlls = Vec::new();
        for key in [r"SOFTWARE\Microsoft\Windows\CurrentVersion\SharedDLLs", r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\SharedDLLs"] {
            let Ok(k) = Hive::LM.root().open_subkey(key) else { continue };
            // ponytail: refcount >= 2 means "someone else too"; uninstallers that forget to decrement leave false positives
            for (file, val) in k.enum_values().flatten() {
                if u32::from_reg_value(&val).is_ok_and(|n| n >= 2) && Path::new(&file).exists() {
                    dlls.push(lower(Path::new(&file)));
                }
            }
        }
        Shared { dirs, dlls }
    }

    pub fn holds(&self, i: &Item) -> bool {
        let (Item::Dir(p) | Item::File(p)) = i else { return false };
        let l = lower(p);
        let within = |a: &str, b: &str| a == b || a.starts_with(&format!("{b}\\"));
        self.dirs.iter().any(|d| within(d, &l) || within(&l, d)) || self.dlls.iter().any(|f| within(f, &l))
    }
}
