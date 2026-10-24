//! Fonts, spacing and shapes. Colours are egui's own and stay that way.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId, RichText, Rounding, Shadow, TextStyle};

pub const AMBER: egui::Color32 = egui::Color32::from_rgb(240, 180, 60);
pub const DANGER: egui::Color32 = egui::Color32::from_rgb(196, 54, 54);
const RADIUS: f32 = 4.0;

fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Text in Segoe UI Semibold (headings, the active tab).
pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(semibold_family())
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut load = |name: &str| {
        let b = std::fs::read(format!(r"C:\Windows\Fonts\{name}")).ok()?;
        fonts.font_data.insert(name.into(), FontData::from_owned(b));
        Some(name.to_string())
    };
    let body = load("segoeui.ttf");
    let bold = load("seguisb.ttf");
    // CJK / symbol fallback so non-English program names don't render as boxes
    let fallback: Vec<String> = ["msyh.ttc", "seguisym.ttf"].into_iter().filter_map(&mut load).collect();
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    if let Some(b) = &body {
        prop.insert(0, b.clone());
    }
    let base = prop.clone();
    let mut semi = base.clone();
    if let Some(b) = bold {
        semi.insert(0, b);
    }
    fonts.families.insert(semibold_family(), semi);
    for fam in fonts.families.values_mut() {
        fam.extend(fallback.iter().cloned());
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|s| {
        use FontFamily::Proportional as P;
        s.text_styles = [
            (TextStyle::Small, FontId::new(11.5, P)),
            (TextStyle::Body, FontId::new(14.0, P)),
            (TextStyle::Button, FontId::new(14.0, P)),
            (TextStyle::Heading, FontId::new(18.0, semibold_family())),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.window_margin = egui::Margin::same(12.0);
        let v = &mut s.visuals;
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.rounding = Rounding::same(RADIUS);
        }
        v.window_rounding = Rounding::same(RADIUS);
        v.menu_rounding = Rounding::same(RADIUS);
        v.window_shadow = Shadow::NONE;
    });
}

/// Placeholder text: weak, so it never reads as something already typed.
pub fn hint(ui: &egui::Ui, text: &str) -> RichText {
    RichText::new(text).color(ui.visuals().weak_text_color())
}

/// A one-line text box as tall as the buttons beside it.
pub fn field<'t>(ui: &egui::Ui, text: &'t mut String, placeholder: &str, width: f32) -> egui::TextEdit<'t> {
    let s = ui.spacing();
    let h = s.interact_size.y.max(ui.text_style_height(&TextStyle::Button) + 2.0 * s.button_padding.y);
    egui::TextEdit::singleline(text)
        .hint_text(hint(ui, placeholder))
        .desired_width(width)
        .min_size(egui::vec2(0.0, h))
        .vertical_align(egui::Align::Center)
}
