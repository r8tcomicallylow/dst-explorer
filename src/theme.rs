use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui::{
    self, pos2, vec2, Color32, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, Response,
    Rounding, Sense, Shape, Stroke, TextStyle,
};

pub const BG: Color32 = Color32::from_rgb(11, 12, 17);
pub const SIDEBAR: Color32 = Color32::from_rgb(15, 16, 23);
pub const PANEL: Color32 = Color32::from_rgb(17, 18, 26);
pub const CARD: Color32 = Color32::from_rgb(23, 25, 35);
pub const CARD_HOVER: Color32 = Color32::from_rgb(30, 32, 44);
pub const BORDER: Color32 = Color32::from_rgb(37, 39, 53);
pub const BORDER_SOFT: Color32 = Color32::from_rgb(30, 32, 44);
pub const TEXT: Color32 = Color32::from_rgb(236, 234, 245);
pub const MUTED: Color32 = Color32::from_rgb(145, 147, 165);
pub const DIM: Color32 = Color32::from_rgb(102, 104, 122);
pub const DANGER: Color32 = Color32::from_rgb(255, 118, 132);
pub const SUCCESS: Color32 = Color32::from_rgb(106, 219, 164);
pub const WARNING: Color32 = Color32::from_rgb(255, 196, 112);

static ACCENT: AtomicU32 = AtomicU32::new(0xB7A5FF);

pub const ACCENTS: [(&str, Color32); 5] = [
    ("Lavender", Color32::from_rgb(183, 165, 255)),
    ("Mint", Color32::from_rgb(140, 226, 192)),
    ("Peach", Color32::from_rgb(255, 180, 143)),
    ("Sky", Color32::from_rgb(140, 199, 255)),
    ("Rose", Color32::from_rgb(255, 157, 199)),
];

pub fn set_accent(index: usize) {
    let color = ACCENTS.get(index).map(|(_, c)| *c).unwrap_or(ACCENTS[0].1);
    let packed = ((color.r() as u32) << 16) | ((color.g() as u32) << 8) | color.b() as u32;
    ACCENT.store(packed, Ordering::Relaxed);
}

pub fn accent() -> Color32 {
    let packed = ACCENT.load(Ordering::Relaxed);
    Color32::from_rgb(
        ((packed >> 16) & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        (packed & 0xFF) as u8,
    )
}

pub fn accent_dim(alpha: u8) -> Color32 {
    let a = accent();
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), alpha)
}

pub fn accent_dark() -> Color32 {
    let a = accent();
    Color32::from_rgb(
        (a.r() as f32 * 0.24) as u8,
        (a.g() as f32 * 0.22) as u8,
        (a.b() as f32 * 0.30) as u8,
    )
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_string(),
        FontData::from_static(include_bytes!("../assets/Inter-Variable.ttf")),
    );
    fonts.font_data.insert(
        "mplus".to_string(),
        FontData::from_static(include_bytes!("../assets/MPLUS1p-Regular.ttf")),
    );

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let list = fonts.families.entry(family).or_default();
        if !list.iter().any(|name| name == "inter") {
            list.insert(0, "inter".to_string());
        }
        if !list.iter().any(|name| name == "mplus") {
            list.push("mplus".to_string());
        }
    }
    ctx.set_fonts(fonts);
}

pub fn configure(ctx: &egui::Context, accent_index: usize) {
    set_accent(accent_index);
    let mut style = (*ctx.style()).clone();
    style.visuals = visuals();
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(14.0, 8.0);
    style.spacing.menu_margin = egui::Margin::same(10.0);
    style.spacing.window_margin = egui::Margin::same(16.0);
    style.spacing.scroll.bar_width = 9.0;
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_inner_margin = 3.0;
    style.spacing.scroll.bar_outer_margin = 2.0;
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(28.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
        (
            TextStyle::Button,
            FontId::new(13.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
    ]
    .into();
    ctx.set_style(style);
}

fn visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(9, 10, 14);
    visuals.faint_bg_color = CARD;
    visuals.code_bg_color = CARD;
    visuals.override_text_color = Some(TEXT);
    visuals.window_rounding = Rounding::same(14.0);
    visuals.menu_rounding = Rounding::same(10.0);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: vec2(0.0, 10.0),
        blur: 30.0,
        spread: 0.0,
        color: Color32::from_black_alpha(140),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: vec2(0.0, 4.0),
        blur: 18.0,
        spread: 0.0,
        color: Color32::from_black_alpha(120),
    };
    visuals.selection.bg_fill = accent_dim(80);
    visuals.selection.stroke = Stroke::new(1.0_f32, accent());
    visuals.hyperlink_color = accent();
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER_SOFT);
    visuals.widgets.noninteractive.rounding = Rounding::same(9.0);
    visuals.widgets.inactive.bg_fill = CARD;
    visuals.widgets.inactive.weak_bg_fill = CARD;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER_SOFT);
    visuals.widgets.inactive.rounding = Rounding::same(9.0);
    visuals.widgets.hovered.bg_fill = CARD_HOVER;
    visuals.widgets.hovered.weak_bg_fill = CARD_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.widgets.hovered.rounding = Rounding::same(9.0);
    visuals.widgets.active.bg_fill = accent_dark();
    visuals.widgets.active.weak_bg_fill = accent_dark();
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent());
    visuals.widgets.active.rounding = Rounding::same(9.0);
    visuals.widgets.open.bg_fill = CARD_HOVER;
    visuals.widgets.open.weak_bg_fill = CARD_HOVER;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.widgets.open.rounding = Rounding::same(9.0);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    visuals
}

pub fn primary_button(ui: &mut egui::Ui, text: &str) -> Response {
    primary_button_w(ui, text, 0.0)
}

pub fn primary_button_w(ui: &mut egui::Ui, text: &str, width: f32) -> Response {
    let button = egui::Button::new(
        egui::RichText::new(text)
            .size(13.0)
            .strong()
            .color(Color32::from_rgb(14, 15, 21)),
    )
    .fill(accent())
    .stroke(Stroke::NONE)
    .rounding(Rounding::same(9.0))
    .min_size(vec2(width, 36.0));
    ui.add(button)
}

pub fn small_button(ui: &mut egui::Ui, text: &str, tint: Color32) -> Response {
    let button = egui::Button::new(egui::RichText::new(text).size(11.0).color(tint))
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER_SOFT))
        .rounding(Rounding::same(7.0))
        .min_size(vec2(0.0, 26.0));
    ui.add(button)
}

pub fn secondary_button(ui: &mut egui::Ui, text: &str) -> Response {
    let button = egui::Button::new(egui::RichText::new(text).size(13.0).color(TEXT))
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(Rounding::same(9.0))
        .min_size(vec2(0.0, 34.0));
    ui.add(button)
}

pub fn chip(ui: &mut egui::Ui, text: &str, selected: bool) -> Response {
    let color = if selected {
        Color32::from_rgb(14, 15, 21)
    } else {
        MUTED
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(12.0), color);
    let size = vec2(galley.size().x + 22.0, 29.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let bg = if selected {
        accent()
    } else if hovered {
        CARD_HOVER
    } else {
        CARD
    };
    ui.painter().rect_filled(rect, Rounding::same(8.0), bg);
    if !selected {
        ui.painter().rect_stroke(
            rect,
            Rounding::same(8.0),
            Stroke::new(1.0_f32, if hovered { BORDER } else { BORDER_SOFT }),
        );
    }
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, color);
    response
}

pub fn badge(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(10.5), fg);
    let size = vec2(galley.size().x + 12.0, 20.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, Rounding::same(5.0), bg);
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, fg);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Compass,
    Search,
    Star,
    StarFilled,
    Download,
    Settings,
    Refresh,
    Close,
    Check,
    External,
    Trash,
    Clock,
    Alert,
    Grid,
    Folder,
}

pub fn icon_button(ui: &mut egui::Ui, icon: Icon, size: f32, tint: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let bg = if response.hovered() {
        CARD_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, Rounding::same(8.0), bg);
    }
    paint_icon(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(size * 0.46, size * 0.46)),
        icon,
        tint,
        (size * 0.075).clamp(1.2, 2.0),
    );
    response
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32, width: f32) {
    let stroke = Stroke::new(width, color);
    let pt = |x: f32, y: f32| {
        pos2(
            rect.min.x + rect.width() * x,
            rect.min.y + rect.height() * y,
        )
    };
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5;
    match icon {
        Icon::Compass => {
            painter.circle_stroke(center, radius * 0.9, stroke);
            let points = vec![
                pt(0.5, 0.2),
                pt(0.6, 0.61),
                pt(0.5, 0.8),
                pt(0.4, 0.39),
                pt(0.5, 0.2),
            ];
            painter.add(Shape::line(points, stroke));
        }
        Icon::Search => {
            painter.circle_stroke(pt(0.44, 0.44), radius * 0.58, stroke);
            painter.line_segment([pt(0.68, 0.68), pt(0.9, 0.9)], stroke);
        }
        Icon::Star | Icon::StarFilled => {
            let points = star_points(center, radius * 0.95, radius * 0.43);
            if icon == Icon::StarFilled {
                for index in 0..points.len() {
                    let next = points[(index + 1) % points.len()];
                    painter.add(Shape::convex_polygon(
                        vec![center, points[index], next],
                        color,
                        Stroke::NONE,
                    ));
                }
            } else {
                let mut points = points;
                if let Some(first) = points.first().copied() {
                    points.push(first);
                }
                painter.add(Shape::line(points, stroke));
            }
        }
        Icon::Download => {
            painter.line_segment([pt(0.5, 0.12), pt(0.5, 0.62)], stroke);
            painter.add(Shape::line(
                vec![pt(0.3, 0.45), pt(0.5, 0.65), pt(0.7, 0.45)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![pt(0.2, 0.7), pt(0.2, 0.86), pt(0.8, 0.86), pt(0.8, 0.7)],
                stroke,
            ));
        }
        Icon::Settings => {
            let r = radius * 0.96;
            for index in 0..6 {
                let angle = index as f32 * std::f32::consts::TAU / 6.0;
                let inner = center + vec2(angle.cos(), angle.sin()) * (r * 0.55);
                let outer = center + vec2(angle.cos(), angle.sin()) * r;
                painter.line_segment([inner, outer], stroke);
                painter.circle_filled(outer, width * 0.95, color);
            }
            painter.circle_stroke(center, r * 0.42, stroke);
        }
        Icon::Refresh => {
            let mut points = Vec::new();
            let start = -2.2_f32;
            let end = 1.3_f32;
            for step in 0..=28 {
                let t = step as f32 / 28.0;
                let angle = start + (end - start) * t;
                points.push(center + vec2(angle.cos(), angle.sin()) * radius * 0.66);
            }
            painter.add(Shape::line(points, stroke));
            let tip = center + vec2(end.cos(), end.sin()) * radius * 0.66;
            painter.add(Shape::line(
                vec![
                    tip + vec2(-radius * 0.02, -radius * 0.3),
                    tip,
                    tip + vec2(-radius * 0.32, radius * 0.02),
                ],
                stroke,
            ));
        }
        Icon::Close => {
            painter.line_segment([pt(0.2, 0.2), pt(0.8, 0.8)], stroke);
            painter.line_segment([pt(0.8, 0.2), pt(0.2, 0.8)], stroke);
        }
        Icon::Check => {
            painter.add(Shape::line(
                vec![pt(0.16, 0.52), pt(0.4, 0.76), pt(0.85, 0.24)],
                Stroke::new(width * 1.15, color),
            ));
        }
        Icon::External => {
            painter.add(Shape::line(
                vec![pt(0.55, 0.16), pt(0.85, 0.16), pt(0.85, 0.46)],
                stroke,
            ));
            painter.line_segment([pt(0.85, 0.16), pt(0.48, 0.53)], stroke);
            painter.add(Shape::line(
                vec![
                    pt(0.72, 0.58),
                    pt(0.72, 0.86),
                    pt(0.14, 0.86),
                    pt(0.14, 0.28),
                    pt(0.42, 0.28),
                ],
                stroke,
            ));
        }
        Icon::Trash => {
            painter.add(Shape::line(vec![pt(0.2, 0.28), pt(0.8, 0.28)], stroke));
            painter.line_segment([pt(0.4, 0.18), pt(0.6, 0.18)], stroke);
            painter.add(Shape::line(
                vec![
                    pt(0.28, 0.36),
                    pt(0.34, 0.84),
                    pt(0.66, 0.84),
                    pt(0.72, 0.36),
                ],
                stroke,
            ));
        }
        Icon::Clock => {
            painter.circle_stroke(center, radius * 0.82, stroke);
            painter.add(Shape::line(
                vec![pt(0.5, 0.28), pt(0.5, 0.52), pt(0.7, 0.62)],
                stroke,
            ));
        }
        Icon::Alert => {
            painter.add(Shape::line(
                vec![pt(0.5, 0.16), pt(0.9, 0.84), pt(0.1, 0.84), pt(0.5, 0.16)],
                stroke,
            ));
            painter.line_segment([pt(0.5, 0.42), pt(0.5, 0.62)], stroke);
            painter.circle_filled(pt(0.5, 0.72), width * 0.9, color);
        }
        Icon::Grid => {
            for (x, y) in [(0.16, 0.16), (0.56, 0.16), (0.16, 0.56), (0.56, 0.56)] {
                painter.rect_stroke(
                    Rect::from_min_size(pt(x, y), vec2(rect.width() * 0.28, rect.height() * 0.28)),
                    Rounding::same(2.0),
                    stroke,
                );
            }
        }
        Icon::Folder => {
            painter.add(Shape::line(
                vec![
                    pt(0.12, 0.28),
                    pt(0.12, 0.82),
                    pt(0.88, 0.82),
                    pt(0.88, 0.34),
                    pt(0.46, 0.34),
                    pt(0.36, 0.22),
                    pt(0.12, 0.22),
                    pt(0.12, 0.28),
                ],
                stroke,
            ));
        }
    }
}

fn star_points(center: Pos2, outer: f32, inner: f32) -> Vec<Pos2> {
    let mut points = Vec::with_capacity(10);
    for index in 0..10 {
        let radius = if index % 2 == 0 { outer } else { inner };
        let angle = -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::PI / 5.0;
        points.push(center + vec2(angle.cos(), angle.sin()) * radius);
    }
    points
}

pub fn progress_bar(ui: &mut egui::Ui, fraction: Option<f32>, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(height * 0.5), BORDER_SOFT);
    if let Some(fraction) = fraction {
        if fraction > 0.0 {
            let fill = Rect::from_min_size(
                rect.min,
                vec2(
                    (rect.width() * fraction.clamp(0.0, 1.0)).max(height),
                    height,
                ),
            );
            ui.painter()
                .rect_filled(fill, Rounding::same(height * 0.5), accent());
        }
    }
}
