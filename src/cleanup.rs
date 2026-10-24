//! The two uninstall dialogs: a progress window while the uninstaller runs and leftovers are
//! scanned, then a tree of what was found to tick and remove.
use crate::{fmt_bytes, programs::Item, programs::Program, theme, win};
use eframe::egui::{self, collapsing_header::CollapsingState, Align, Align2, Checkbox, Color32, Id, Layout, RichText, Sense};
use std::{
    collections::HashMap,
    fs,
    sync::{atomic::AtomicBool, Arc, Mutex},
};
use winreg::RegKey;

const GREEN: Color32 = Color32::from_rgb(60, 170, 80);
const STAGES: [&str; 3] = ["Uninstall", "Scan leftovers", "Review"];

#[derive(Default)]
pub struct Progress {
    pub title: String,
    /// which programs, shown under the title
    pub what: String,
    /// index into STAGES
    pub stage: usize,
    pub log: Vec<String>,
    pub frac: f32,
}

pub struct Task {
    pub state: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
}

impl Task {
    pub fn new(title: &str, what: String) -> Self {
        let state = Progress { title: title.into(), what, ..Default::default() };
        Task { state: Arc::new(Mutex::new(state)), cancel: Arc::default() }
    }

    /// Returns true when Cancel is clicked.
    pub fn ui(&self, ctx: &egui::Context) -> bool {
        let s = self.state.lock().unwrap();
        let mut cancel = false;
        dialog("Uninstall", "evict_progress", [640.0, 330.0]).show(ctx, |ui| {
            ui.label(theme::semibold(&s.title).size(16.0));
            ui.label(RichText::new(&s.what).weak());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 22.0;
                for (i, name) in STAGES.iter().enumerate() {
                    let text = match i.cmp(&s.stage) {
                        std::cmp::Ordering::Less => RichText::new(*name),
                        std::cmp::Ordering::Equal => theme::semibold(*name),
                        std::cmp::Ordering::Greater => RichText::new(*name).color(ui.visuals().weak_text_color()),
                    };
                    let r = ui.label(text);
                    if i == s.stage {
                        ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 3.0, egui::Stroke::new(2.0_f32, ui.visuals().hyperlink_color));
                    }
                }
            });
            ui.add_space(8.0);
            ui.add(egui::ProgressBar::new(s.frac).fill(GREEN).animate(true).desired_height(18.0));
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).max_height(130.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in &s.log {
                        ui.label(RichText::new(line).monospace().size(12.0));
                    }
                });
            });
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| cancel = ui.button("  Cancel  ").clicked());
        });
        cancel
    }
}

fn dialog<'a>(title: &str, id: &'a str, size: [f32; 2]) -> egui::Window<'a> {
    egui::Window::new(title.to_string())
        .id(Id::new(id))
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .fixed_size(size)
}

/// Items inside a folder or key (counting itself) and bytes on disk.
pub fn measure(item: &Item) -> (usize, u64) {
    fn key(k: &RegKey) -> usize {
        let subs: usize = k.enum_keys().flatten().filter_map(|s| k.open_subkey(&s).ok()).map(|c| key(&c)).sum();
        1 + k.enum_values().count() + subs
    }
    match item {
        Item::File(p) => (1, fs::metadata(p).map_or(0, |m| m.len())),
        Item::Dir(p) => fs::read_dir(p).into_iter().flatten().flatten().fold((1, 0), |(n, b), e| {
            let (n2, b2) = match e.file_type() {
                Ok(t) if t.is_dir() => measure(&Item::Dir(e.path())),
                _ => (1, e.metadata().map_or(0, |m| m.len())),
            };
            (n + n2, b + b2)
        }),
        Item::Key(h, k) => (h.root().open_subkey(k).map_or(0, |k| key(&k)), 0),
        Item::Value(..) => (1, 0),
    }
}

#[derive(Clone)]
enum Node {
    Branch(Item),
    Leaf(String),
}

/// One level of a folder or key, for expanding it in the tree.
fn children(item: &Item) -> Vec<Node> {
    const CAP: usize = 300;
    let mut v: Vec<(bool, String, Node)> = match item {
        Item::Dir(p) => fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let dir = e.file_type().is_ok_and(|t| t.is_dir());
                (dir, name.clone(), if dir { Node::Branch(Item::Dir(e.path())) } else { Node::Leaf(name) })
            })
            .collect(),
        Item::Key(h, k) => {
            let Ok(key) = h.root().open_subkey(k) else { return Vec::new() };
            let subs = key.enum_keys().flatten().map(|s| (true, s.clone(), Node::Branch(Item::Key(*h, format!(r"{k}\{s}")))));
            let vals = key.enum_values().flatten().map(|(n, _)| {
                let n = if n.is_empty() { "(Default)".to_string() } else { n };
                (false, n.clone(), Node::Leaf(n))
            });
            subs.chain(vals).collect()
        }
        Item::File(_) | Item::Value(..) => Vec::new(),
    };
    v.sort_by_key(|(dir, name, _)| (!dir, name.to_lowercase()));
    let more = v.len().saturating_sub(CAP);
    let mut out: Vec<Node> = v.into_iter().take(CAP).map(|(_, _, n)| n).collect();
    if more > 0 {
        out.push(Node::Leaf(format!("… {more} more")));
    }
    out
}

struct Found {
    item: Item,
    count: usize,
    bytes: u64,
    on: bool,
}

pub enum Outcome {
    /// ticked items with their size on disk
    Finish(Vec<(Item, u64)>),
    Cancel,
}

pub struct Results {
    found: Vec<Found>,
    /// held back by shared file protection
    kept: Vec<Item>,
    note: Option<String>,
    filter: String,
    kids: HashMap<String, Vec<Node>>,
    /// what was uninstalled, for the history
    pub progs: Vec<Program>,
}

impl Results {
    /// Measures every item, so call it off the UI thread.
    pub fn new(items: Vec<Item>, kept: Vec<Item>, ticked: bool, note: Option<String>, progs: Vec<Program>) -> Self {
        let found = items
            .into_iter()
            .map(|item| {
                let (count, bytes) = measure(&item);
                Found { item, count, bytes, on: ticked }
            })
            .collect();
        Results { found, kept, note, filter: String::new(), kids: HashMap::new(), progs }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Outcome> {
        let mut out = None;
        let (ticked, ticked_bytes) = self.found.iter().filter(|f| f.on).fold((0, 0), |(n, b), f| (n + 1, b + f.bytes));
        let total: u64 = self.found.iter().map(|f| f.bytes).sum();
        dialog("Leftover scan", "evict_results", [780.0, 540.0]).show(ctx, |ui| {
            // header
            let heading = if self.found.is_empty() { "Nothing left behind" } else { "Some leftover traces found!" };
            ui.label(theme::semibold(heading).size(16.0));
            let names: Vec<&str> = self.progs.iter().map(|p| p.name.as_str()).collect();
            if !names.is_empty() {
                ui.label(names.join(", "));
            }
            let mut sub = format!("{} item{} found", self.found.len(), if self.found.len() == 1 { "" } else { "s" });
            if total > 0 {
                sub += &format!(", {}", fmt_bytes(total));
            }
            ui.label(RichText::new(sub).weak());
            if let Some(n) = &self.note {
                ui.add_space(6.0);
                // slim notice: an amber bar on the left, plain text
                let r = egui::Frame::none().inner_margin(egui::Margin { left: 10.0, right: 6.0, top: 4.0, bottom: 4.0 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(n).color(theme::AMBER));
                });
                let rect = r.response.rect;
                ui.painter().vline(rect.left() + 1.0, rect.y_range(), egui::Stroke::new(3.0_f32, theme::AMBER));
            }
            ui.add_space(6.0);

            // toolbar
            let f = self.filter.to_lowercase();
            let visible: Vec<usize> = (0..self.found.len()).filter(|&i| f.is_empty() || self.found[i].item.label().to_lowercase().contains(&f)).collect();
            ui.horizontal(|ui| {
                for (label, on) in [("Select all", true), ("Select none", false)] {
                    if ui.add_enabled(!visible.is_empty(), egui::Button::new(label)).clicked() {
                        visible.iter().for_each(|&i| self.found[i].on = on);
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(theme::field(ui, &mut self.filter, "Filter", 200.0));
                });
            });
            ui.add_space(2.0);

            // tree
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).max_height(ui.available_height() - 52.0).show(ui, |ui| {
                    if self.found.is_empty() && self.kept.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| ui.label(RichText::new("The program didn't leave anything behind.").weak()));
                    }
                    let icons = icons(ctx);
                    for (group, registry) in [("File System", false), ("Registry", true)] {
                        let idx: Vec<usize> = visible.iter().copied().filter(|&i| self.found[i].item.is_registry() == registry).collect();
                        if idx.is_empty() {
                            continue;
                        }
                        let count: usize = idx.iter().map(|&i| self.found[i].count).sum();
                        let bytes: u64 = idx.iter().map(|&i| self.found[i].bytes).sum();
                        let on = idx.iter().filter(|&&i| self.found[i].on).count();
                        CollapsingState::load_with_default_open(ctx, Id::new(("evict_group", group)), true)
                            .show_header(ui, |ui| {
                                let mut all = on == idx.len();
                                let tri = Checkbox::without_text(&mut all).indeterminate(on > 0 && on < idx.len());
                                if ui.add(tri).changed() {
                                    idx.iter().for_each(|&i| self.found[i].on = all);
                                }
                                ui.label(theme::semibold(group));
                                ui.label(RichText::new(summary(count, bytes)).weak());
                            })
                            .body(|ui| {
                                for &i in &idx {
                                    let f = &mut self.found[i];
                                    let size = fmt_bytes(f.bytes);
                                    node(ui, &f.item, Some(&mut f.on), &size, &icons, &mut self.kids);
                                }
                            });
                    }
                    if !self.kept.is_empty() {
                        CollapsingState::load_with_default_open(ctx, Id::new("evict_kept"), false)
                            .show_header(ui, |ui| {
                                ui.label(theme::semibold("Kept, other programs still use these"));
                                ui.label(RichText::new(format!("({})", self.kept.len())).weak());
                            })
                            .body(|ui| {
                                for k in &self.kept {
                                    ui.horizontal(|ui| {
                                        ui.add_space(18.0);
                                        ui.add(egui::Label::new(RichText::new(k.label()).weak()).truncate())
                                            .on_hover_text("Shared file protection: another installed program still uses this, so it's not offered for removal.");
                                    });
                                }
                            });
                    }
                });
            });

            // footer
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if !self.found.is_empty() {
                    ui.label(RichText::new("Removed items are deleted permanently, not sent to the Recycle Bin.").weak().small());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.found.is_empty() {
                        if ui.button("  Close  ").clicked() {
                            out = Some(Outcome::Cancel);
                        }
                        return;
                    }
                    let mut label = format!("Remove {ticked} item{}", if ticked == 1 { "" } else { "s" });
                    if ticked_bytes > 0 {
                        label += &format!(" ({})", fmt_bytes(ticked_bytes));
                    }
                    let remove = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(theme::DANGER).min_size(egui::vec2(0.0, 28.0));
                    if ui.add_enabled(ticked > 0, remove).clicked() {
                        out = Some(Outcome::Finish(self.found.iter().filter(|f| f.on).map(|f| (f.item.clone(), f.bytes)).collect()));
                    }
                    if ui.add(egui::Button::new("  Cancel  ").min_size(egui::vec2(0.0, 28.0))).clicked() {
                        out = Some(Outcome::Cancel);
                    }
                });
            });
        });
        out
    }
}

fn summary(count: usize, bytes: u64) -> String {
    match bytes {
        0 => format!("{count} items"),
        b => format!("{count} items, {}", fmt_bytes(b)),
    }
}

/// Right-click menu on a leftover: show it in Explorer / Registry Editor, or copy its path.
fn item_menu(r: egui::Response, item: &Item) {
    r.context_menu(|ui| {
        if ui.button("Open location").clicked() {
            match item {
                Item::Dir(p) => win::open(&p.display().to_string()),
                Item::File(p) => win::reveal(p),
                Item::Key(h, k) | Item::Value(h, k, _) => crate::regedit(&format!(r"{}\{k}", h.name())),
            }
            ui.close_menu();
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(item.label());
            ui.close_menu();
        }
    });
}

/// A folder / key row that expands to show what's inside. Only top-level rows get a checkbox and size.
fn node(ui: &mut egui::Ui, item: &Item, check: Option<&mut bool>, size: &str, icons: &Icons, kids: &mut HashMap<String, Vec<Node>>) {
    let full = item.label();
    let text = match (&check, item) {
        (Some(_), Item::Dir(_)) => format!("{full}\\"),
        (Some(_), _) => full.clone(),
        (None, _) => full.rsplit('\\').next().unwrap_or(&full).to_string(),
    };
    let icon = icons.0[match item {
        Item::Dir(_) => 0,
        Item::File(_) => 1,
        Item::Key(..) | Item::Value(..) => 2,
    }]
    .clone();
    let header = |ui: &mut egui::Ui, check: Option<&mut bool>| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(size).weak());
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                if let Some(c) = check {
                    ui.add(Checkbox::without_text(c));
                }
                if let Some(t) = &icon {
                    ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(16.0, 16.0)));
                }
                let r = ui.add(egui::Label::new(&text).truncate().sense(Sense::click())).on_hover_text(&full);
                item_menu(r, item);
            });
        });
    };
    if let Item::File(_) | Item::Value(..) = item {
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            header(ui, check);
        });
        return;
    }
    CollapsingState::load_with_default_open(ui.ctx(), Id::new(("evict_node", &full)), false)
        .show_header(ui, |ui| header(ui, check))
        .body(|ui| {
            let list = kids.entry(full.clone()).or_insert_with(|| children(item)).clone();
            for n in &list {
                match n {
                    Node::Branch(it) => node(ui, it, None, "", icons, kids),
                    Node::Leaf(name) => drop(ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.label(name);
                    })),
                }
            }
        });
}

/// Folder, file and registry-key icons from Windows itself, loaded once.
#[derive(Clone)]
struct Icons([Option<egui::TextureHandle>; 3]);

fn icons(ctx: &egui::Context) -> Icons {
    let id = Id::new("evict_tree_icons");
    if let Some(i) = ctx.data(|d| d.get_temp::<Icons>(id)) {
        return i;
    }
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let load = |name: &str, src: String| win::icon(&src).map(|img| ctx.load_texture(name, img, Default::default()));
    let i = Icons([
        load("evict_folder", format!(r"{sys}\System32\shell32.dll,3")),
        load("evict_file", format!(r"{sys}\System32\shell32.dll,0")),
        load("evict_regkey", format!(r"{sys}\regedit.exe,1")),
    ]);
    ctx.data_mut(|d| d.insert_temp(id, i.clone()));
    i
}
