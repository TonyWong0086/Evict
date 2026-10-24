//! The two uninstall dialogs, like Geek's: a progress window while the uninstaller runs and
//! leftovers are scanned, then a tree of what was found to tick and remove.
use crate::programs::Item;
use eframe::egui::{self, collapsing_header::CollapsingState, Align, Align2, Checkbox, Color32, Id, Layout, RichText};
use std::{
    collections::HashMap,
    fs,
    sync::{atomic::AtomicBool, Arc, Mutex},
};
use winreg::RegKey;

const FOLDER: Color32 = Color32::from_rgb(232, 184, 64);

#[derive(Default)]
pub struct Progress {
    pub title: String,
    pub log: Vec<String>,
    pub frac: f32,
}

pub struct Task {
    pub state: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
}

impl Task {
    pub fn new(title: &str) -> Self {
        let state = Progress { title: title.into(), ..Default::default() };
        Task { state: Arc::new(Mutex::new(state)), cancel: Arc::default() }
    }

    /// Returns true when Cancel is clicked.
    pub fn ui(&self, ctx: &egui::Context) -> bool {
        let s = self.state.lock().unwrap();
        let mut cancel = false;
        dialog(&s.title, "evict_progress").show(ctx, |ui| {
            let bar = egui::ProgressBar::new(s.frac).fill(Color32::from_rgb(30, 140, 40)).animate(true);
            ui.add(bar.desired_height(22.0));
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in &s.log {
                        ui.label(line);
                    }
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| cancel = ui.button("  Cancel  ").clicked());
        });
        cancel
    }
}

fn dialog<'a>(title: &str, id: &'a str) -> egui::Window<'a> {
    egui::Window::new(title.to_string())
        .id(Id::new(id))
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .fixed_size([620.0, 300.0])
}

/// Files + folders inside a folder, or subkeys + values inside a key, counting the item itself.
pub fn count(item: &Item) -> usize {
    fn key(k: &RegKey) -> usize {
        let subs: usize = k.enum_keys().flatten().filter_map(|s| k.open_subkey(&s).ok()).map(|c| key(&c)).sum();
        1 + k.enum_values().count() + subs
    }
    match item {
        Item::File(_) => 1,
        Item::Dir(p) => {
            let inner: usize = fs::read_dir(p)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| if e.file_type().is_ok_and(|t| t.is_dir()) { count(&Item::Dir(e.path())) } else { 1 })
                .sum();
            1 + inner
        }
        Item::Key(h, k) => h.root().open_subkey(k).map_or(0, |k| key(&k)),
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
        Item::File(_) => Vec::new(),
    };
    v.sort_by_key(|(dir, name, _)| (!dir, name.to_lowercase()));
    let more = v.len().saturating_sub(CAP);
    let mut out: Vec<Node> = v.into_iter().take(CAP).map(|(_, _, n)| n).collect();
    if more > 0 {
        out.push(Node::Leaf(format!("… {more} more")));
    }
    out
}

pub struct Found {
    pub item: Item,
    pub count: usize,
    pub on: bool,
}

pub enum Outcome {
    Finish(Vec<Item>),
    Cancel,
}

pub struct Results {
    found: Vec<Found>,
    note: Option<String>,
    kids: HashMap<String, Vec<Node>>,
}

impl Results {
    pub fn new(found: Vec<(Item, usize)>, ticked: bool, note: Option<String>) -> Self {
        let found = found.into_iter().map(|(item, count)| Found { item, count, on: ticked }).collect();
        Results { found, note, kids: HashMap::new() }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Outcome> {
        let title = if self.found.is_empty() { "No leftovers found" } else { "Some leftover traces found!" };
        let mut out = None;
        dialog(title, "evict_results").show(ctx, |ui| {
            if let Some(n) = &self.note {
                ui.colored_label(Color32::from_rgb(240, 180, 60), n);
            }
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::both().auto_shrink(false).max_height(220.0).show(ui, |ui| {
                    if self.found.is_empty() {
                        ui.label("The program left nothing behind.");
                    }
                    for (group, icon, files) in [("File System", "🖴", true), ("Registry", "🗄", false)] {
                        let idx: Vec<usize> =
                            (0..self.found.len()).filter(|&i| matches!(self.found[i].item, Item::Key(..)) != files).collect();
                        if idx.is_empty() {
                            continue;
                        }
                        let total: usize = idx.iter().map(|&i| self.found[i].count).sum();
                        let on = idx.iter().filter(|&&i| self.found[i].on).count();
                        CollapsingState::load_with_default_open(ctx, Id::new(("evict_group", group)), true)
                            .show_header(ui, |ui| {
                                let mut all = on == idx.len();
                                let tri = Checkbox::without_text(&mut all).indeterminate(on > 0 && on < idx.len());
                                if ui.add(tri).changed() {
                                    idx.iter().for_each(|&i| self.found[i].on = all);
                                }
                                ui.label(format!("{icon} {group} ({total} items)"));
                            })
                            .body(|ui| {
                                for &i in &idx {
                                    let f = &mut self.found[i];
                                    node(ui, &f.item, Some(&mut f.on), &mut self.kids);
                                }
                            });
                    }
                });
            });
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("  Cancel  ").clicked() {
                    out = Some(Outcome::Cancel);
                }
                if ui.button("  Finish  ").on_hover_text("Remove the ticked items").clicked() {
                    out = Some(Outcome::Finish(self.found.iter().filter(|f| f.on).map(|f| f.item.clone()).collect()));
                }
            });
        });
        out
    }
}

/// A folder / key row that expands to show what's inside. Only top-level rows get a checkbox.
fn node(ui: &mut egui::Ui, item: &Item, check: Option<&mut bool>, kids: &mut HashMap<String, Vec<Node>>) {
    let full = item.label();
    let text = match (&check, item) {
        (Some(_), Item::Dir(_)) => format!("{full}\\"),
        (Some(_), _) => full.clone(),
        (None, _) => full.rsplit('\\').next().unwrap_or(&full).to_string(),
    };
    let header = |ui: &mut egui::Ui, check: Option<&mut bool>, icon: &str| {
        if let Some(c) = check {
            ui.add(Checkbox::without_text(c));
        }
        ui.label(RichText::new(icon).color(FOLDER));
        ui.label(&text);
    };
    if let Item::File(_) = item {
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            header(ui, check, "🗋");
        });
        return;
    }
    CollapsingState::load_with_default_open(ui.ctx(), Id::new(("evict_node", &full)), false)
        .show_header(ui, |ui| header(ui, check, "📁"))
        .body(|ui| {
            let list = kids.entry(full.clone()).or_insert_with(|| children(item)).clone();
            for n in &list {
                match n {
                    Node::Branch(it) => node(ui, it, None, kids),
                    Node::Leaf(name) => drop(ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.label(name);
                    })),
                }
            }
        });
}
