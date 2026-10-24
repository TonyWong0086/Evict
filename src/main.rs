#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod cleanup;
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
    /// found items with their item counts, pre-ticked?, note
    Leftovers(Vec<(Item, usize)>, bool, Option<String>),
    Cancelled,
    Monitor(Result<monitor::Session, String>),
    Trace(monitor::Trace),
    Deleted(String),
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
}

struct StoreApp {
    name: String,
    full: String,
    version: String,
}

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

fn regedit(path: &str) {
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
            Some(StoreApp { name: s.next()?.into(), full: s.next()?.into(), version: s.next()?.trim().into() })
        })
        .collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    v
}

/// Three significant digits like Geek: "106 GB", "72.8 GB", "8.66 GB".
fn fmt_bytes(b: u64) -> String {
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

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            b' ' => "+".into(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Geek's Action menu; rendered both as the right-click menu and as the toolbar.
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

fn with_counts(items: Vec<Item>) -> Vec<(Item, usize)> {
    items.into_iter().map(|i| (cleanup::count(&i), i)).map(|(c, i)| (i, c)).collect()
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
        let task = cleanup::Task::new(SCANNING);
        let (state, cancel) = (task.state.clone(), task.cancel.clone());
        self.task = Some(task);
        self.job(ctx, move || {
            let step = |title: &str, line: String, frac: f32| {
                let mut s = state.lock().unwrap();
                s.title = title.into();
                s.log.push(line);
                s.frac = frac;
            };
            let n = progs.len() as f32;
            let mut items = Vec::new();
            for (i, p) in progs.iter().enumerate() {
                let at = i as f32 / n;
                if run_uninstaller && !p.uninstall.is_empty() {
                    step("Waiting for the uninstaller to finish...", format!("Running the {} uninstaller…", p.name), at);
                    if let Err(e) = win::run_and_wait(&format!("cmd /S /C \"{}\"", p.uninstall), &cancel) {
                        step(SCANNING, format!("Couldn't start it: {e}"), at);
                    }
                }
                if cancel.load(Ordering::Relaxed) {
                    return Msg::Cancelled;
                }
                step(SCANNING, format!("Scanning for {} leftovers…", p.name), at + 0.3 / n);
                let traced = traces.iter().filter(|t| t.matches(p)).flat_map(|t| t.items.clone());
                for i in programs::leftovers(p, &all).into_iter().chain(traced) {
                    if i.exists() && !items.contains(&i) {
                        items.push(i);
                    }
                }
            }
            step(SCANNING, format!("Counting {} items…", items.len()), 0.95);
            let found = with_counts(items);
            if cancel.load(Ordering::Relaxed) {
                return Msg::Cancelled;
            }
            // An uninstall entry that survived means the uninstaller was probably cancelled: don't pre-tick anything.
            let survived = progs.iter().any(|p| Item::Key(p.hive, p.key.clone()).exists());
            let note = (!force && survived).then(|| "The program still looks installed (uninstall cancelled?). Nothing is pre-ticked.".into());
            Msg::Leftovers(found, force || !survived, note)
        });
    }

    /// Show `items` in the leftovers dialog (item counts are worked out in the background).
    fn review(&mut self, ctx: &egui::Context, items: Vec<Item>) {
        self.job(ctx, move || Msg::Leftovers(with_counts(items), true, None));
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
            Act::RemoveEntry => self.review(ctx, sel.into_iter().map(|p| Item::Key(p.hive, p.key)).collect()),
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
        let selected = self.selected();
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Type to find a program").desired_width(200.0));
            if ui.button("Refresh").clicked() {
                self.reload(&ctx);
            }
            ui.separator();
            actions(ui, selected.first(), selected.len(), &mut act);
        });
        ui.separator();

        let f = self.filter.to_lowercase();
        let mut rows: Vec<&Program> = self
            .programs
            .iter()
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
                    row.col(|ui| {
                        match self.icons.get(&id) {
                            Some(t) => drop(ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(18.0, 18.0)))),
                            None => ui.add_space(22.0),
                        }
                        cell(ui, p.name.as_str());
                        if p.is32 {
                            cell(ui, egui::RichText::new("(32-bit)").weak());
                        }
                        if self.traces.iter().any(|t| t.matches(p)) {
                            cell(ui, egui::RichText::new("[traced]").weak());
                        }
                    });
                    let mid = |ui: &mut egui::Ui, t: String| drop(ui.centered_and_justified(|ui| cell(ui, t)));
                    row.col(|ui| mid(ui, p.version.clone()));
                    row.col(|ui| mid(ui, p.publisher.clone()));
                    row.col(|ui| mid(ui, fmt_bytes(self.size_of(p))));
                    row.col(|ui| mid(ui, programs::pretty_date(&p.date)));
                    row.col(|ui| cell(ui, self.folder_of(p)));

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
                    r.context_menu(|ui| actions(ui, Some(p), n, &mut act));
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
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search").desired_width(180.0));
            if ui.button("Refresh").clicked() {
                self.store_loaded = false;
            }
            ui.label(format!("{} apps", self.store.len()));
        });
        ui.separator();
        let f = self.filter.to_lowercase();
        let mut remove = None;
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("store").striped(true).num_columns(3).show(ui, |ui| {
                for (i, a) in self.store.iter().enumerate() {
                    if !f.is_empty() && !a.name.to_lowercase().contains(&f) {
                        continue;
                    }
                    ui.label(&a.name);
                    ui.label(&a.version);
                    if confirm_button(ui, &mut self.confirm, (Tab::Store, i), "Uninstall") {
                        remove = Some(a.full.clone());
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(full) = remove {
            self.status = format!("Removing {full}…");
            self.job(&ctx, move || {
                powershell(&format!("Remove-AppxPackage -Package '{}'", full.replace('\'', "''")));
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
            self.review(&ctx, items);
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
                self.leftovers = None;
                if !items.is_empty() {
                    self.job(ctx, move || {
                        let errs: Vec<String> = items.iter().filter_map(|i| i.delete().err().map(|e| format!("{}: {e}", i.label()))).collect();
                        Msg::Deleted(match errs.first() {
                            None => format!("Removed {} item(s).", items.len()),
                            Some(e) => format!("{} of {} failed, e.g. {e}", errs.len(), items.len()),
                        })
                    });
                }
            }
            Some(cleanup::Outcome::Cancel) => self.leftovers = None,
            None => {}
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
                Msg::Leftovers(found, ticked, note) => {
                    self.task = None;
                    self.status.clear();
                    self.leftovers = Some(cleanup::Results::new(found, ticked, note));
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
                Msg::Deleted(s) => {
                    self.status = s;
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
                for (t, l) in [(Tab::Programs, "Programs"), (Tab::Store, "Store apps"), (Tab::Startup, "Startup"), (Tab::Monitor, "Install monitor")] {
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
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Programs => self.programs_ui(ui),
            Tab::Store => self.store_ui(ui),
            Tab::Startup => self.startup_ui(ui),
            Tab::Monitor => self.monitor_ui(ui),
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
        let t = std::time::Instant::now();
        let s = super::monitor::start(String::new()).unwrap();
        println!("snapshot+watch: {:?}", t.elapsed());
        let tr = s.finish();
        println!("diff: {:?} total, {} items", t.elapsed(), tr.items.len());
    }
}
