#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod cleanup;
mod history;
mod monitor;
mod programs;
mod startup;
mod win;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use programs::{Item, Program};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    os::windows::process::CommandExt,
    process::{Command, ExitStatus},
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
    thread,
    time::Duration,
};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

const NO_WINDOW: u32 = 0x0800_0000;

enum Msg {
    Store(Vec<StoreApp>),
    Leftovers(cleanup::Results),
    Cancelled,
    Monitor(Result<monitor::Session, String>),
    Trace(monitor::Trace),
    /// status line, history entry to log
    Deleted(String, Option<history::Entry>),
    // background fill-ins, not jobs
    Icon(String, egui::ColorImage),
    /// id, bytes on disk, resolved install folder
    Size(String, u64, String),
}

#[derive(PartialEq, Clone, Copy)]
enum Col {
    Name,
    Version,
    Publisher,
    Size,
    Date,
    Folder,
}

#[derive(Clone, Copy)]
enum Act {
    Uninstall,
    Force,
    RemoveEntry,
    Modify,
    Folder,
    Registry,
    Website,
    Google,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Programs,
    Store,
    Startup,
    Monitor,
    History,
}

struct StoreApp {
    name: String,
    full: String,
    version: String,
    bloat: bool,
}

/// Preinstalled Store apps most people never use; prefix match on the package name.
const BLOAT: &[&str] = &[
    "Microsoft.BingNews", "Microsoft.BingWeather", "Microsoft.BingFinance", "Microsoft.BingSports", "Microsoft.BingSearch",
    "Microsoft.GetHelp", "Microsoft.Getstarted", "Microsoft.MicrosoftSolitaireCollection", "Microsoft.People",
    "Microsoft.WindowsFeedbackHub", "Microsoft.ZuneMusic", "Microsoft.ZuneVideo", "Microsoft.MicrosoftOfficeHub",
    "Microsoft.SkypeApp", "Microsoft.MixedReality.Portal", "Microsoft.WindowsMaps", "Microsoft.3DBuilder",
    "Microsoft.Microsoft3DViewer", "Microsoft.Print3D", "Microsoft.Messaging", "Microsoft.OneConnect",
    "Microsoft.PowerAutomateDesktop", "Microsoft.Todos", "Microsoft.549981C3F5F10", "Microsoft.WindowsCommunicationsApps",
    "Microsoft.MicrosoftJournal", "Microsoft.Wallet", "MicrosoftTeams", "Clipchamp.Clipchamp", "MicrosoftCorporationII.MicrosoftFamily",
    "king.com.", "Disney.", "SpotifyAB.SpotifyMusic", "Facebook.", "AmazonVideo.PrimeVideo", "BytedancePte.Ltd.TikTok",
    "4DF9E0F8.Netflix", "5A894077.McAfeeSecurity",
];

struct App {
    tab: Tab,
    filter: String,
    programs: Vec<Program>,
    sel: BTreeSet<String>,
    sort: (Col, bool),
    sizes: HashMap<String, u64>,
    folders: HashMap<String, String>,
    col_w: [f32; 5],
    icons: HashMap<String, egui::TextureHandle>,
    loading: HashSet<String>,
    store: Vec<StoreApp>,
    store_loaded: bool,
    store_sel: HashSet<String>,
    ghosts_only: bool,
    history: Vec<history::Entry>,
    startup: Vec<startup::Entry>,
    traces: Vec<monitor::Trace>,
    task: Option<cleanup::Task>,
    leftovers: Option<cleanup::Results>,
    confirm: Option<(Tab, usize)>,
    monitor: Option<monitor::Session>,
    installer: String,
    busy: usize,
    status: String,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

fn shell(cmd: &str) -> std::io::Result<ExitStatus> {
    Command::new("cmd").raw_arg(format!("/S /C \"{cmd}\"")).creation_flags(NO_WINDOW).status()
}

fn powershell(script: &str) -> String {
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

pub(crate) fn regedit(path: &str) {
    let applet = r"Software\Microsoft\Windows\CurrentVersion\Applets\Regedit";
    if let Ok((k, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(applet) {
        let _ = k.set_value("LastKey", &format!(r"Computer\{path}"));
    }
    let _ = Command::new("regedit").arg("-m").spawn();
}

fn store_list() -> Vec<StoreApp> {
    let script = "Get-AppxPackage | Where-Object { -not $_.IsFramework -and -not $_.NonRemovable -and $_.SignatureKind -ne 'System' } | ForEach-Object { \"$($_.Name)`t$($_.PackageFullName)`t$($_.Version)\" }";
    let mut v: Vec<StoreApp> = powershell(script)
        .lines()
        .filter_map(|l| {
            let mut s = l.split('\t');
            let name: String = s.next()?.into();
            let bloat = BLOAT.iter().any(|b| name.starts_with(b));
            Some(StoreApp { name, full: s.next()?.into(), version: s.next()?.trim().into(), bloat })
        })
        .collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    v
}

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Three significant digits like Geek: "106 GB", "72.8 GB", "8.66 GB".
pub(crate) fn fmt_bytes(b: u64) -> String {
    if b == 0 {
        return String::new();
    }
    let (mut v, mut unit) = (b as f64, "B");
    for u in ["KB", "MB", "GB", "TB"] {
        if v < 1024.0 {
            break;
        }
        v /= 1024.0;
        unit = u;
    }
    match v {
        v if v >= 100.0 => format!("{v:.0} {unit}"),
        v if v >= 10.0 => format!("{v:.1} {unit}"),
        v => format!("{v:.2} {unit}"),
    }
}

fn bytes_or_zero(b: u64) -> String {
    if b == 0 { "0 B".into() } else { fmt_bytes(b) }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            b' ' => "+".into(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Geek's Action menu, shown on right-click anywhere in a row.
fn actions(ui: &mut egui::Ui, first: Option<&Program>, count: usize, act: &mut Option<Act>) {
    let one = first.filter(|_| count == 1);
    let has = |f: fn(&Program) -> &String| one.is_some_and(|p| !f(p).trim().is_empty());
    let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, a: Act| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            *act = Some(a);
            ui.close_menu();
        }
    };
    item(ui, count > 0, "Uninstall...", Act::Uninstall);
    item(ui, count > 0, "Force Removal", Act::Force);
    item(ui, count > 0, "Remove Entry", Act::RemoveEntry);
    item(ui, has(|p| &p.modify), "Modify...", Act::Modify);
    ui.separator();
    item(ui, one.is_some(), "Installation Folder", Act::Folder);
    item(ui, one.is_some(), "Registry Entry", Act::Registry);
    item(ui, has(|p| &p.url), "Program Website", Act::Website);
    item(ui, one.is_some(), "Google for", Act::Google);
}

/// Version, Publisher, Size and Installed On are centered, like Geek.
fn centered(col: usize) -> bool {
    (1..=4).contains(&col)
}

/// Non-selectable label, so clicks fall through to the table row.
fn cell(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.add(egui::Label::new(text).selectable(false).truncate());
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.into() } else { s.chars().take(n).collect::<String>() + "…" }
}

/// Two-click delete: first click arms, second click confirms.
fn confirm_button(ui: &mut egui::Ui, armed: &mut Option<(Tab, usize)>, key: (Tab, usize), label: &str) -> bool {
    if *armed == Some(key) {
        if ui.button(egui::RichText::new("Sure?").color(egui::Color32::RED)).clicked() {
            *armed = None;
            return true;
        }
    } else if ui.small_button(label).clicked() {
        *armed = Some(key);
    }
    false
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        // CJK / symbol fallback so non-English program names don't render as boxes
        let mut fonts = egui::FontDefinitions::default();
        for f in ["msyh.ttc", "seguisym.ttf"] {
            if let Ok(b) = std::fs::read(format!(r"C:\Windows\Fonts\{f}")) {
                fonts.font_data.insert(f.into(), egui::FontData::from_owned(b));
                fonts.families.values_mut().for_each(|fam| fam.push(f.into()));
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let (tx, rx) = mpsc::channel();
        let mut app = App {
            tab: Tab::Programs,
            filter: String::new(),
            programs: Vec::new(),
            sel: BTreeSet::new(),
            sort: (Col::Name, true),
            sizes: HashMap::new(),
            folders: HashMap::new(),
            col_w: [340.0, 110.0, 190.0, 85.0, 110.0],
            icons: HashMap::new(),
            loading: HashSet::new(),
            store: Vec::new(),
            store_loaded: false,
            store_sel: HashSet::new(),
            ghosts_only: false,
            history: history::load(),
            startup: startup::list(),
            traces: monitor::load_all(),
            task: None,
            leftovers: None,
            confirm: None,
            monitor: None,
            installer: String::new(),
            busy: 0,
            status: String::new(),
            tx,
            rx,
        };
        app.reload(&cc.egui_ctx);
        app
    }

    /// Re-read the program list; icons and folder sizes fill in from background threads.
    fn reload(&mut self, ctx: &egui::Context) {
        self.programs = programs::list();
        let ids: HashSet<String> = self.programs.iter().map(Program::id).collect();
        self.sel.retain(|id| ids.contains(id));
        let todo: Vec<Program> = self.programs.iter().filter(|p| self.loading.insert(p.id())).cloned().collect();
        if todo.is_empty() {
            return;
        }
        let icons: Vec<(String, String)> =
            todo.iter().filter(|p| !p.icon.trim().is_empty()).map(|p| (p.id(), p.icon.clone())).collect();
        let (tx, c) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            for (id, path) in icons {
                if let Some(img) = win::icon(&path) {
                    let _ = tx.send(Msg::Icon(id, img));
                    c.request_repaint();
                }
            }
        });
        let all = self.programs.clone();
        let (tx, c) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            // Other programs installed inside a footprint (games inside Steam…) are carved out of it.
            let skip: HashSet<String> = all.iter().filter_map(|p| p.install_dir(&all, true)).map(|d| programs::lower(&d)).collect();
            let index = programs::DirIndex::build(true);
            let jobs: Vec<_> = todo
                .iter()
                .map(|p| (p.id(), p.install_dir(&all, false).map(|d| d.display().to_string()).unwrap_or_default(), p.footprint(&all, &index)))
                .collect();
            let (jobs, skip) = (Arc::new(Mutex::new(jobs)), Arc::new(skip));
            for _ in 0..4 {
                let (jobs, skip, tx, c) = (jobs.clone(), skip.clone(), tx.clone(), c.clone());
                thread::spawn(move || loop {
                    let Some((id, dir, dirs)) = jobs.lock().unwrap().pop() else { break };
                    let _ = tx.send(Msg::Size(id, dirs.iter().map(|d| programs::folder_size(d, &skip)).sum(), dir));
                    c.request_repaint();
                });
            }
        });
    }

    /// Measured size on disk (install folder + data folders), else the size the installer declared.
    fn size_of(&self, p: &Program) -> u64 {
        self.sizes.get(&p.id()).copied().filter(|&s| s > 0).unwrap_or(p.size_kb as u64 * 1024)
    }

    /// Resolved install folder (Steam library, vendor subfolder…), else what the registry says.
    fn folder_of(&self, p: &Program) -> String {
        self.folders.get(&p.id()).filter(|d| !d.is_empty()).cloned().unwrap_or_else(|| p.folder().display().to_string())
    }

    fn job(&mut self, ctx: &egui::Context, f: impl FnOnce() -> Msg + Send + 'static) {
        self.busy += 1;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    /// Run each uninstaller to completion (if asked), then scan for what it left behind.
    fn scan(&mut self, ctx: &egui::Context, progs: Vec<Program>, force: bool, run_uninstaller: bool) {
        const SCANNING: &str = "Scanning for leftover files, folders and registry items...";
        let traces = self.traces.clone();
        let all = self.programs.clone();
        let names = progs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
        let task = cleanup::Task::new(SCANNING, names);
        let (state, cancel) = (task.state.clone(), task.cancel.clone());
        self.task = Some(task);
        self.job(ctx, move || {
            let step = |title: &str, stage: usize, line: String, frac: f32| {
                let mut s = state.lock().unwrap();
                s.title = title.into();
                s.stage = stage;
                s.log.push(line);
                s.frac = frac;
            };
            // Before the uninstaller deletes it: the deep scan looks for things still pointing into it.
            let dirs: Vec<_> = progs.iter().map(|p| p.install_dir(&all, true)).collect();
            let n = progs.len() as f32;
            let mut items: Vec<Item> = Vec::new();
            for (i, p) in progs.iter().enumerate() {
                let at = i as f32 / n;
                if run_uninstaller && !p.uninstall.is_empty() {
                    step("Waiting for the uninstaller to finish...", 0, format!("Running the {} uninstaller…", p.name), at);
                    if let Err(e) = win::run_and_wait(&format!("cmd /S /C \"{}\"", p.uninstall), &cancel) {
                        step(SCANNING, 1, format!("Couldn't start it: {e}"), at);
                    }
                }
                if cancel.load(Ordering::Relaxed) {
                    return Msg::Cancelled;
                }
                step(SCANNING, 1, format!("Scanning for {} leftovers…", p.name), at + 0.3 / n);
                let traced = traces.iter().filter(|t| t.matches(p)).flat_map(|t| t.items.clone());
                let deep = programs::references(p, dirs[i].as_deref());
                for i in programs::leftovers(p, &all).into_iter().chain(deep).chain(traced) {
                    if i.exists() && !items.contains(&i) {
                        items.push(i);
                    }
                }
            }
            // A shortcut inside a Start Menu folder that's already listed goes with the folder.
            let folders: Vec<String> = items.iter().filter_map(|i| if let Item::Dir(d) = i { Some(programs::lower(d) + "\\") } else { None }).collect();
            items.retain(|i| !matches!(i, Item::File(f) if folders.iter().any(|d| programs::lower(f).starts_with(d))));
            step(SCANNING, 1, "Checking which files other programs still use…".into(), 0.85);
            let shared = programs::Shared::new(&all, &progs);
            let (kept, items): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|i| shared.holds(i));
            step(SCANNING, 1, format!("Measuring {} items…", items.len()), 0.95);
            // An uninstall entry that survived means the uninstaller was probably cancelled: don't pre-tick anything.
            let survived = progs.iter().any(|p| Item::Key(p.hive, p.key.clone()).exists());
            let note = (!force && survived).then(|| "The program still looks installed (uninstall cancelled?). Nothing is pre-ticked.".into());
            let results = cleanup::Results::new(items, kept, force || !survived, note, progs);
            if cancel.load(Ordering::Relaxed) {
                return Msg::Cancelled;
            }
            Msg::Leftovers(results)
        });
    }

    /// Show `items` in the leftovers dialog; `progs` are logged to the history if removed.
    fn review(&mut self, ctx: &egui::Context, items: Vec<Item>, progs: Vec<Program>) {
        self.job(ctx, move || Msg::Leftovers(cleanup::Results::new(items, Vec::new(), true, None, progs)));
    }

    /// Write the program list to the Desktop as CSV or JSON.
    fn export(&self, json: bool) -> std::io::Result<std::path::PathBuf> {
        #[derive(serde::Serialize)]
        struct Row<'a> {
            name: &'a str,
            version: &'a str,
            publisher: &'a str,
            size_bytes: u64,
            installed_on: &'a str,
            folder: String,
            is_32bit: bool,
            ghost: bool,
            uninstall: &'a str,
        }
        let rows: Vec<Row> = self
            .programs
            .iter()
            .map(|p| Row {
                name: &p.name,
                version: &p.version,
                publisher: &p.publisher,
                size_bytes: self.size_of(p),
                installed_on: &p.date,
                folder: self.folder_of(p),
                is_32bit: p.is32,
                ghost: p.ghost,
                uninstall: &p.uninstall,
            })
            .collect();
        let data = if json {
            serde_json::to_string_pretty(&rows)?
        } else {
            // BOM so Excel reads it as UTF-8
            let mut s = String::from("\u{feff}Name,Version,Publisher,Size (bytes),Installed On,Folder,32-bit,Ghost,Uninstall\r\n");
            for r in &rows {
                let f = [r.name, r.version, r.publisher].map(csv_field).join(",");
                let g = [r.installed_on, &r.folder].map(csv_field).join(",");
                s += &format!("{f},{},{g},{},{},{}\r\n", r.size_bytes, r.is_32bit, r.ghost, csv_field(r.uninstall));
            }
            s
        };
        let path = programs::desktop().join(format!("Evict programs {}.{}", &win::now()[..10], if json { "json" } else { "csv" }));
        std::fs::write(&path, data)?;
        Ok(path)
    }

    fn selected(&self) -> Vec<Program> {
        self.programs.iter().filter(|p| self.sel.contains(&p.id())).cloned().collect()
    }

    fn run(&mut self, ctx: &egui::Context, act: Act) {
        let sel = self.selected();
        let Some(p) = sel.first().cloned() else { return };
        match act {
            Act::Uninstall => self.scan(ctx, sel, false, true),
            Act::Force => self.scan(ctx, sel, true, false),
            Act::RemoveEntry => self.review(ctx, sel.iter().map(|p| Item::Key(p.hive, p.key.clone())).collect(), sel),
            Act::Modify => drop(thread::spawn(move || shell(&p.modify))),
            Act::Folder => win::open(&self.folder_of(&p)),
            Act::Registry => regedit(&format!(r"{}\{}", p.hive.name(), p.key)),
            Act::Website => win::open(&p.url),
            Act::Google => win::open(&format!("https://www.google.com/search?q={}", url_encode(&p.name))),
        }
    }

    fn programs_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut act = None;
        let ghosts: Vec<Program> = self.programs.iter().filter(|p| p.ghost).cloned().collect();
        let mut export = None;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("🔎 Type to find a program").desired_width(240.0));
            if ui.button("⟳ Refresh").clicked() {
                self.reload(&ctx);
            }
            ui.menu_button("⤓ Export", |ui| {
                if ui.button("CSV (Excel)").clicked() {
                    export = Some(false);
                    ui.close_menu();
                }
                if ui.button("JSON").clicked() {
                    export = Some(true);
                    ui.close_menu();
                }
            });
            if !ghosts.is_empty() || self.ghosts_only {
                ui.separator();
                ui.toggle_value(&mut self.ghosts_only, format!("👻 Ghosts ({})", ghosts.len()))
                    .on_hover_text("Entries whose program is already gone: the uninstaller and install folder no longer exist.");
                if self.ghosts_only && ui.add_enabled(!ghosts.is_empty(), egui::Button::new("Remove all ghost entries")).clicked() {
                    self.review(&ctx, ghosts.iter().map(|p| Item::Key(p.hive, p.key.clone())).collect(), ghosts.clone());
                }
            }
        });
        if let Some(json) = export {
            self.status = match self.export(json) {
                Ok(path) => {
                    win::reveal(&path);
                    format!("Exported {} programs to {}", self.programs.len(), path.display())
                }
                Err(e) => format!("Export failed: {e}"),
            };
        }
        ui.separator();

        let f = self.filter.to_lowercase();
        let mut rows: Vec<&Program> = self
            .programs
            .iter()
            .filter(|p| !self.ghosts_only || p.ghost)
            .filter(|p| f.is_empty() || p.name.to_lowercase().contains(&f) || p.publisher.to_lowercase().contains(&f))
            .collect();
        let (col, asc) = self.sort;
        rows.sort_by(|a, b| {
            let o = match col {
                Col::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Col::Version => a.version.cmp(&b.version),
                Col::Publisher => a.publisher.to_lowercase().cmp(&b.publisher.to_lowercase()),
                Col::Size => self.size_of(a).cmp(&self.size_of(b)),
                Col::Date => a.date.cmp(&b.date),
                Col::Folder => self.folder_of(a).to_lowercase().cmp(&self.folder_of(b).to_lowercase()),
            };
            if asc { o } else { o.reverse() }
        });

        let (mut new_sort, mut click, mut resize) = (None, None, None);
        let count = self.sel.len();
        // Columns resize from grips in the header only, so the list has no divider lines.
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for w in self.col_w {
            table = table.column(Column::exact(w).clip(true));
        }
        table
            .column(Column::remainder().at_least(150.0).clip(true))
            .header(24.0, |mut h| {
                let heads = [
                    (Col::Name, "Program Name"),
                    (Col::Version, "Version"),
                    (Col::Publisher, "Publisher"),
                    (Col::Size, "Size"),
                    (Col::Date, "Installed On"),
                    (Col::Folder, "Folder"),
                ];
                for (i, (c, label)) in heads.into_iter().enumerate() {
                    h.col(|ui| {
                        let rect = ui.max_rect();
                        let arrow = match (col == c, asc) {
                            (false, _) => "",
                            (true, true) => " ▲",
                            (true, false) => " ▼",
                        };
                        let text = egui::RichText::new(format!("{label}{arrow}")).strong();
                        let label = egui::Label::new(text).selectable(false).sense(egui::Sense::click());
                        let r = if centered(i) { ui.centered_and_justified(|ui| ui.add(label)).inner } else { ui.add(label) };
                        if r.clicked() {
                            new_sort = Some(c);
                        }
                        if i < 5 {
                            let grip = egui::Rect::from_x_y_ranges(rect.right() - 6.0..=rect.right(), rect.y_range());
                            let g = ui.interact(grip, egui::Id::new(("evict_col", i)), egui::Sense::drag());
                            if g.hovered() || g.dragged() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                            }
                            if g.dragged() {
                                resize = Some((i, g.drag_delta().x));
                            }
                            ui.painter().vline(rect.right() - 1.0, rect.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, rows.len(), |mut row| {
                    let p = rows[row.index()];
                    let id = p.id();
                    row.set_selected(self.sel.contains(&id));
                    let mut cells = vec![row
                        .col(|ui| {
                            match self.icons.get(&id) {
                                Some(t) => drop(ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(18.0, 18.0)))),
                                None => ui.add_space(22.0),
                            }
                            cell(ui, p.name.as_str());
                            if p.ghost {
                                cell(ui, egui::RichText::new("(ghost)").color(egui::Color32::from_rgb(240, 180, 60)));
                            }
                            if p.is32 {
                                cell(ui, egui::RichText::new("(32-bit)").weak());
                            }
                            if self.traces.iter().any(|t| t.matches(p)) {
                                cell(ui, egui::RichText::new("[traced]").weak());
                            }
                        })
                        .1];
                    let mid = |ui: &mut egui::Ui, t: String| drop(ui.centered_and_justified(|ui| cell(ui, t)));
                    cells.push(row.col(|ui| mid(ui, p.version.clone())).1);
                    cells.push(row.col(|ui| mid(ui, p.publisher.clone())).1);
                    cells.push(row.col(|ui| mid(ui, fmt_bytes(self.size_of(p)))).1);
                    cells.push(row.col(|ui| mid(ui, programs::pretty_date(&p.date))).1);
                    cells.push(row.col(|ui| cell(ui, self.folder_of(p))).1);

                    let r = row.response();
                    if r.clicked() || r.double_clicked() {
                        click = Some((id.clone(), r.ctx.input(|i| i.modifiers.command)));
                    }
                    if r.double_clicked() {
                        act = Some(Act::Uninstall);
                    }
                    if r.secondary_clicked() && !self.sel.contains(&id) {
                        click = Some((id.clone(), false));
                    }
                    let n = if self.sel.contains(&id) { count } else { 1 };
                    // The row's combined response keeps only the first cell's id, so the menu goes on every cell.
                    for c in cells {
                        c.context_menu(|ui| actions(ui, Some(p), n, &mut act));
                    }
                });
            });

        if let Some((id, ctrl)) = click {
            if !ctrl {
                self.sel.clear();
            }
            if !self.sel.remove(&id) || !ctrl {
                self.sel.insert(id);
            }
        }
        if let Some((i, dx)) = resize {
            self.col_w[i] = (self.col_w[i] + dx).max(40.0);
        }
        if let Some(c) = new_sort {
            let default_desc = matches!(c, Col::Size | Col::Date);
            self.sort = if self.sort.0 == c { (c, !self.sort.1) } else { (c, !default_desc) };
        }
        if let Some(a) = act {
            self.run(&ctx, a);
        }
    }

    fn store_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let bloat = self.store.iter().filter(|a| a.bloat).count();
        let mut remove = false;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("🔎 Search").desired_width(200.0));
            if ui.button("⟳ Refresh").clicked() {
                self.store_loaded = false;
            }
            ui.separator();
            if ui.add_enabled(bloat > 0, egui::Button::new(format!("Select bloatware ({bloat})"))).on_hover_text("Tick the preinstalled apps most people never use").clicked() {
                self.store_sel = self.store.iter().filter(|a| a.bloat).map(|a| a.full.clone()).collect();
            }
            if ui.add_enabled(!self.store_sel.is_empty(), egui::Button::new("Clear")).clicked() {
                self.store_sel.clear();
            }
            let n = self.store_sel.len();
            ui.add_enabled_ui(n > 0, |ui| remove = confirm_button(ui, &mut self.confirm, (Tab::Store, 0), &format!("Remove {n} selected")));
            ui.label(egui::RichText::new(format!("{} apps", self.store.len())).weak());
        });
        ui.separator();
        let f = self.filter.to_lowercase();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("store").striped(true).num_columns(4).spacing([16.0, 6.0]).show(ui, |ui| {
                for h in ["", "Name", "Version", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for a in &self.store {
                    if !f.is_empty() && !a.name.to_lowercase().contains(&f) {
                        continue;
                    }
                    let mut on = self.store_sel.contains(&a.full);
                    if ui.checkbox(&mut on, "").changed() {
                        if on { self.store_sel.insert(a.full.clone()) } else { self.store_sel.remove(&a.full) };
                    }
                    ui.label(&a.name);
                    ui.label(&a.version);
                    if a.bloat {
                        ui.label(egui::RichText::new("bloatware").color(egui::Color32::from_rgb(240, 180, 60)).small());
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        });
        if remove {
            let full: Vec<String> = self.store_sel.drain().collect();
            self.status = format!("Removing {} app(s)…", full.len());
            self.job(&ctx, move || {
                let list = full.iter().map(|f| format!("'{}'", f.replace('\'', "''"))).collect::<Vec<_>>().join(",");
                powershell(&format!("{list} | ForEach-Object {{ Remove-AppxPackage -Package $_ }}"));
                Msg::Store(store_list())
            });
        }
    }

    fn startup_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.startup = startup::list();
            }
            ui.label("Untick to stop a program from running at sign-in (same switch Task Manager uses).");
        });
        ui.separator();
        let (mut del, mut err) = (None, None);
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("startup").striped(true).num_columns(5).show(ui, |ui| {
                for h in ["On", "Name", "Command", "Location", ""] {
                    ui.strong(h);
                }
                ui.end_row();
                for (i, e) in self.startup.iter_mut().enumerate() {
                    let mut on = e.enabled;
                    if ui.checkbox(&mut on, "").changed() {
                        if let Err(x) = e.set_enabled(on) {
                            err = Some(format!("{}: {x}", e.name));
                        }
                    }
                    ui.label(&e.name);
                    ui.label(short(&e.command, 70)).on_hover_text(&e.command);
                    ui.label(short(&e.location(), 60)).on_hover_text(e.location());
                    if confirm_button(ui, &mut self.confirm, (Tab::Startup, i), "Delete") {
                        del = Some(i);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(i) = del {
            match self.startup[i].delete() {
                Ok(()) => drop(self.startup.remove(i)),
                Err(x) => err = Some(format!("{}: {x}", self.startup[i].name)),
            }
        }
        if let Some(e) = err {
            self.status = e;
        }
    }

    fn monitor_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.heading("Real-time installation monitor");
        ui.label("Evict snapshots the registry, watches the disk while an installer runs, and saves everything it added as a trace. Uninstalling a traced program later removes exactly what was installed.");
        ui.add_space(6.0);
        let mut stop = false;
        if let Some(m) = &self.monitor {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Watching… {} new paths so far", m.created.len()));
                stop = ui.button("Stop & save trace").clicked();
            });
            ui.small("Finish the installer completely before stopping.");
            egui::ScrollArea::vertical().max_height(220.0).stick_to_bottom(true).show(ui, |ui| {
                for p in m.created.iter().rev().take(300) {
                    ui.monospace(p.display().to_string());
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("Installer:");
                ui.add(egui::TextEdit::singleline(&mut self.installer).hint_text("drop an .exe / .msi here, or leave empty").desired_width(420.0));
                if ui.add_enabled(self.busy == 0, egui::Button::new("Start monitoring")).clicked() {
                    let inst = self.installer.clone();
                    self.status = "Taking registry snapshot…".into();
                    self.job(&ctx, move || Msg::Monitor(monitor::start(inst)));
                }
            });
        }
        if stop {
            let s = self.monitor.take().unwrap();
            self.status = "Comparing registry…".into();
            self.job(&ctx, move || Msg::Trace(s.finish()));
        }

        ui.separator();
        ui.heading("Traces");
        let (mut clean, mut del) = (None, None);
        egui::ScrollArea::vertical().id_salt("traces").auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("traces").striped(true).num_columns(4).show(ui, |ui| {
                for (i, t) in self.traces.iter().enumerate() {
                    ui.label(&t.name);
                    ui.label(format!("{} items", t.items.len()));
                    if ui.small_button("Remove installed items").clicked() {
                        clean = Some(i);
                    }
                    if confirm_button(ui, &mut self.confirm, (Tab::Monitor, i), "Forget trace") {
                        del = Some(i);
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(i) = clean {
            let items = self.traces[i].items.iter().filter(|x| x.exists()).cloned().collect();
            self.review(&ctx, items, Vec::new());
        }
        if let Some(i) = del {
            let _ = self.traces.remove(i).remove();
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(t) = &self.task {
            if t.ui(ctx) {
                t.cancel.store(true, Ordering::Relaxed);
            }
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        let Some(results) = &mut self.leftovers else { return };
        match results.ui(ctx) {
            Some(cleanup::Outcome::Finish(items)) => {
                let progs = self.leftovers.take().unwrap().progs;
                self.status = format!("Removing {} item(s)…", items.len());
                self.job(ctx, move || {
                    let (mut errs, mut bytes) = (Vec::new(), 0);
                    for (i, b) in &items {
                        match i.delete() {
                            Ok(()) => bytes += b,
                            Err(e) => errs.push(format!("{}: {e}", i.label())),
                        }
                    }
                    let done = items.len() - errs.len();
                    let entry = (!progs.is_empty()).then(|| {
                        let join = |f: fn(&Program) -> &str| progs.iter().map(f).collect::<Vec<_>>().join(", ");
                        history::Entry {
                            when: win::now(),
                            name: join(|p| &p.name),
                            version: join(|p| &p.version),
                            publisher: join(|p| &p.publisher),
                            items: done,
                            bytes,
                        }
                    });
                    let status = match errs.first() {
                        None => format!("Removed {done} item(s), {} freed.", bytes_or_zero(bytes)),
                        Some(e) => format!("{} of {} failed, e.g. {e}", errs.len(), items.len()),
                    };
                    Msg::Deleted(status, entry)
                });
            }
            Some(cleanup::Outcome::Cancel) => self.leftovers = None,
            None => {}
        }
    }

    fn history_ui(&mut self, ui: &mut egui::Ui) {
        let (freed, items) = self.history.iter().fold((0, 0), |(b, n), e| (b + e.bytes, n + e.items));
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} uninstalls · {items} items removed · {} freed", self.history.len(), bytes_or_zero(freed))).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!self.history.is_empty(), |ui| clear = confirm_button(ui, &mut self.confirm, (Tab::History, 0), "Clear history"));
            });
        });
        ui.separator();
        if self.history.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new("Programs you uninstall with Evict are listed here.").weak()));
        }
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("history").striped(true).num_columns(6).spacing([20.0, 6.0]).show(ui, |ui| {
                if !self.history.is_empty() {
                    for h in ["When", "Program", "Version", "Publisher", "Items removed", "Freed"] {
                        ui.strong(h);
                    }
                    ui.end_row();
                }
                for e in self.history.iter().rev() {
                    ui.label(&e.when);
                    ui.label(&e.name);
                    ui.label(&e.version);
                    ui.label(&e.publisher);
                    ui.label(e.items.to_string());
                    ui.label(fmt_bytes(e.bytes));
                    ui.end_row();
                }
            });
        });
        if clear {
            self.history.clear();
            if let Err(e) = history::save(&self.history) {
                self.status = format!("Couldn't save history: {e}");
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        // egui's default text is grey; Geek's is plain white (black in light mode)
        ctx.style_mut(|s| {
            s.visuals.override_text_color = Some(if s.visuals.dark_mode { egui::Color32::WHITE } else { egui::Color32::BLACK })
        });
        while let Ok(m) = self.rx.try_recv() {
            if !matches!(m, Msg::Icon(..) | Msg::Size(..)) {
                self.busy = self.busy.saturating_sub(1);
            }
            match m {
                Msg::Icon(id, img) => {
                    let tex = ctx.load_texture(&id, img, Default::default());
                    self.icons.insert(id, tex);
                }
                Msg::Size(id, b, dir) => {
                    self.sizes.insert(id.clone(), b);
                    self.folders.insert(id, dir);
                }
                Msg::Store(s) => {
                    self.store = s;
                    self.status.clear();
                }
                Msg::Leftovers(results) => {
                    self.task = None;
                    self.status.clear();
                    self.leftovers = Some(results);
                    self.reload(ctx);
                }
                Msg::Cancelled => {
                    self.task = None;
                    self.status = "Cancelled.".into();
                }
                Msg::Monitor(Ok(s)) => {
                    self.monitor = Some(s);
                    self.status = "Monitoring. Run the installer to completion, then press Stop.".into();
                }
                Msg::Monitor(Err(e)) => self.status = e,
                Msg::Trace(t) => {
                    self.status = match t.save() {
                        Ok(()) => format!("Saved trace \"{}\" ({} items).", t.name, t.items.len()),
                        Err(e) => format!("Couldn't save trace: {e}"),
                    };
                    self.traces.retain(|x| x.name != t.name);
                    self.traces.push(t);
                    self.reload(ctx);
                }
                Msg::Deleted(s, entry) => {
                    self.status = s;
                    if let Some(e) = entry {
                        self.history.push(e);
                        if let Err(e) = history::save(&self.history) {
                            self.status = format!("Couldn't save history: {e}");
                        }
                    }
                    self.reload(ctx);
                }
            }
        }
        if let Some(m) = &mut self.monitor {
            m.poll();
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if let Some(p) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
            self.installer = p.display().to_string();
            self.tab = Tab::Monitor;
        }
        if self.tab == Tab::Store && !self.store_loaded {
            self.store_loaded = true;
            self.status = "Loading Store apps…".into();
            self.job(ctx, || Msg::Store(store_list()));
        }

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (t, l) in [(Tab::Programs, "Programs"), (Tab::Store, "Store apps"), (Tab::Startup, "Startup"), (Tab::Monitor, "Install monitor"), (Tab::History, "History")] {
                    if ui.selectable_label(self.tab == t, l).clicked() {
                        self.tab = t;
                        self.confirm = None;
                    }
                }
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let total: u64 = self.programs.iter().map(|p| self.size_of(p)).sum();
                ui.label(format!("{} programs of {} size in total", self.programs.len(), fmt_bytes(total)));
                ui.separator();
                if self.busy > 0 {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
        });
        self.dialogs(ctx);
        let modal = self.task.is_some() || self.leftovers.is_some();
        egui::CentralPanel::default().show(ctx, |ui| {
            // dims and locks the list while a dialog is up
            if modal {
                ui.disable();
            }
            match self.tab {
            Tab::Programs => self.programs_ui(ui),
            Tab::Store => self.store_ui(ui),
            Tab::Startup => self.startup_ui(ui),
            Tab::Monitor => self.monitor_ui(ui),
            Tab::History => self.history_ui(ui),
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1220.0, 780.0]).with_min_inner_size([640.0, 400.0]).with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Evict", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::{
        monitor::topmost,
        programs::{core_name, iso_date, pretty_date},
    };

    #[test]
    fn matching_and_collapse() {
        assert_eq!(core_name("Mozilla Firefox (x64 en-US) 131.0"), "mozillafirefox");
        assert_eq!(core_name("7-Zip 24.08 (x64)"), "7zip");
        assert_eq!(core_name("Foo Co., Ltd."), "foo");
        assert_eq!(core_name("VLC media player v3.0"), "vlcmediaplayer");
        assert_eq!(core_name("Adobe Photoshop 2026"), "adobephotoshop2026");
        assert_eq!(iso_date("6/27/2026").as_deref(), Some("2026-06-27"));
        assert_eq!(iso_date("20250114").as_deref(), Some("2025-01-14"));
        assert_eq!(pretty_date("2025-09-29"), "Sep 29, 2025");
        assert_eq!(super::programs::msi_packed("90160000-008C-0000-0000-0000000FF1CE").as_deref(), Some("00006109C80000000000000000F01FEC"));
        assert_eq!(super::programs::msi_packed("nope"), None);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            topmost(s(&[r"C:\A\b", r"C:\A\b c", r"C:\a\B\c.txt", r"C:\X\y"])),
            s(&[r"C:\A\b", r"C:\A\b c", r"C:\X\y"])
        );
    }

    /// The parent exits at once, like an Inno/NSIS uninstaller handing off to its %TEMP% copy.
    #[test]
    fn waits_for_child_processes() {
        let t = std::time::Instant::now();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        super::win::run_and_wait(r#"cmd /S /C "start "" /b ping -n 3 127.0.0.1 >nul""#, &cancel).unwrap();
        assert!(t.elapsed().as_secs_f32() > 1.5, "returned after {:?}", t.elapsed());
    }

    /// Read-only run against this machine: `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_readonly() {
        let progs = super::programs::list();
        println!("{} programs, {} startup entries", progs.len(), super::startup::list().len());
        for p in progs.iter().filter(|p| p.location.is_empty()).take(5) {
            println!("{} 32bit={} dir={:?} footprint={:?}", p.name, p.is32, p.install_dir(&progs, false), p.footprint(&progs, &super::programs::DirIndex::build(true)));
        }
        for p in progs.iter().take(400).filter(|p| !p.location.is_empty()).take(5) {
            println!("{} -> {:?}", p.name, super::programs::leftovers(p, &progs).iter().map(|i| i.label()).collect::<Vec<_>>());
        }
        for p in progs.iter().filter(|p| p.ghost) {
            println!("ghost: {} | {}", p.name, p.uninstall);
        }
        let shared = super::programs::Shared::new(&progs, &[]);
        for p in progs.iter().take(400).filter(|p| !p.location.is_empty()).take(8) {
            let refs = super::programs::references(p, p.install_dir(&progs, true).as_deref());
            println!("deep {} -> {:?}", p.name, refs.iter().map(|i| (i.label(), shared.holds(i))).collect::<Vec<_>>());
        }
        let t = std::time::Instant::now();
        let s = super::monitor::start(String::new()).unwrap();
        println!("snapshot+watch: {:?}", t.elapsed());
        let tr = s.finish();
        println!("diff: {:?} total, {} items", t.elapsed(), tr.items.len());
    }
}
