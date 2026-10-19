//! Design system for the library window, following Microsoft Fluent 2 and
//! the Windows 11 app design guidelines:
//!
//! - Color: the WinUI theme resources (dark "Default" and "Light"), with the
//!   app's brand accent from the icon instead of the system blue.
//! - Type: Segoe UI with the Windows type ramp (Caption 12/16, Body 14/20,
//!   Body large 18/24, Subtitle 20/28, Title 28/36), sentence case.
//! - Geometry: 4 px grid, 32 px controls, 4 px corners on controls,
//!   8 px on cards and overlays, 24 px page margins.
//! - Layering: a base layer (navigation) and a content layer (pages, cards).
//! - Icons: Segoe Fluent Icons where the system has it.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response, RichText, Sense, Shadow,
    Stroke, TextStyle, Theme, Ui, Vec2, WidgetText,
};

/// Fluent color tokens for one theme.
#[derive(Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    /// Base layer (navigation), SolidBackgroundFillColorBase.
    pub base: Color32,
    /// Content layer, LayerFillColorDefault over the base.
    pub layer: Color32,
    pub layer_stroke: Color32,
    /// CardBackgroundFillColorDefault / Secondary.
    pub card: Color32,
    pub card_alt: Color32,
    pub card_stroke: Color32,
    pub card_hover: Color32,
    /// ControlFillColorDefault / Secondary / Tertiary.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_pressed: Color32,
    pub control_stroke: Color32,
    /// Edges of inputs, combo boxes and buttons: 3:1 against the page.
    pub edge: Color32,
    /// SubtleFillColorSecondary / Tertiary (hover/press on transparent controls).
    pub subtle_hover: Color32,
    pub subtle_pressed: Color32,
    pub strong: Color32,
    pub divider: Color32,
    /// Menus, dialogs and toasts.
    pub flyout: Color32,
    /// Behind video: pure black.
    pub well: Color32,
    /// TextFillColorPrimary / Secondary / Tertiary / Disabled.
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub text_disabled: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    /// Recording, and nothing else.
    pub recording: Color32,
    pub success: Color32,
    pub caution: Color32,
    pub critical: Color32,
    pub smoke: Color32,
}

const fn argb(v: u32) -> Color32 {
    let a = (v >> 24) as u8;
    let r = (v >> 16) as u8;
    let g = (v >> 8) as u8;
    let b = v as u8;
    // Color32 stores premultiplied alpha.
    Color32::from_rgba_premultiplied(
        ((r as u32 * a as u32) / 255) as u8,
        ((g as u32 * a as u32) / 255) as u8,
        ((b as u32 * a as u32) / 255) as u8,
        a,
    )
}

/// Dark theme: neutral, untinted surfaces so footage colors read true
/// (see docs/design.md). Values follow WinUI's dark theme resources.
pub const DARK: Tokens = Tokens {
    dark: true,
    base: argb(0xFF1C_1C1C),
    layer: argb(0xFF20_2020),
    layer_stroke: argb(0x14FF_FFFF),
    card: argb(0xFF2B_2B2B),
    card_alt: argb(0xFF26_2626),
    card_stroke: argb(0x14FF_FFFF),
    card_hover: argb(0xFF32_3232),
    control: argb(0x0FFF_FFFF),
    control_hover: argb(0x17FF_FFFF),
    control_pressed: argb(0x0AFF_FFFF),
    control_stroke: argb(0x17FF_FFFF),
    edge: argb(0xFF6A_6A6A),
    subtle_hover: argb(0x0FFF_FFFF),
    subtle_pressed: argb(0x0AFF_FFFF),
    strong: argb(0x8BFF_FFFF),
    divider: argb(0x14FF_FFFF),
    flyout: argb(0xFF2C_2C2C),
    well: argb(0xFF00_0000),
    text: argb(0xFFFF_FFFF),
    text2: argb(0xFFC8_C8C8),
    text3: argb(0xFF9E_9E9E),
    text_disabled: argb(0xFF5C_5C5C),
    // Brand accent, shifted toward magenta so it never reads as the red
    // recording dot. 5.5:1 on the page layer.
    accent: argb(0xFFF2_649A),
    accent_hover: argb(0xFFF5_7DAA),
    on_accent: argb(0xFF1A_0B12),
    recording: argb(0xFFFF_4D4F),
    success: argb(0xFF6C_CB5F),
    caution: argb(0xFFFC_E100),
    critical: argb(0xFFFF_99A4),
    smoke: argb(0x4D00_0000),
};

pub const LIGHT: Tokens = Tokens {
    dark: false,
    base: argb(0xFFF3_F3F3),
    layer: argb(0xFFF9_F9F9),
    layer_stroke: argb(0x0F00_0000),
    card: argb(0xFFFF_FFFF),
    card_alt: argb(0xFFF6_F6F6),
    card_stroke: argb(0x0F00_0000),
    card_hover: argb(0xFFF0_F0F0),
    control: argb(0xB3FF_FFFF),
    control_hover: argb(0x80F9_F9F9),
    control_pressed: argb(0x4DF9_F9F9),
    control_stroke: argb(0x2900_0000),
    edge: argb(0xFF8A_8A8A),
    subtle_hover: argb(0x0900_0000),
    subtle_pressed: argb(0x0600_0000),
    strong: argb(0x7200_0000),
    divider: argb(0x0F00_0000),
    flyout: argb(0xFFF9_F9F9),
    well: argb(0xFF00_0000),
    text: argb(0xE400_0000),
    text2: argb(0x9E00_0000),
    text3: argb(0x7200_0000),
    text_disabled: argb(0x5C00_0000),
    accent: argb(0xFFC2_2E6A),
    accent_hover: argb(0xFFD0_437B),
    on_accent: argb(0xFFFF_FFFF),
    recording: argb(0xFFD1_3438),
    success: argb(0xFF0F_7B0F),
    caution: argb(0xFF9D_5D00),
    critical: argb(0xFFC4_2B1C),
    smoke: argb(0x4D00_0000),
};

pub fn tokens(ctx: &egui::Context) -> Tokens {
    if ctx.theme() == Theme::Light { LIGHT } else { DARK }
}

/// Corner radii (Windows 11 geometry).
pub const R_CONTROL: u8 = 4;
pub const R_CARD: u8 = 8;
/// Page margin for windows wider than 640 px.
pub const GUTTER: f32 = 24.0;
pub const CONTROL_H: f32 = 32.0;
/// Width of the control column in settings rows; controls end on the right
/// edge and start on the same left edge.
pub const CONTROL_COL: f32 = 260.0;

pub const SEMIBOLD: &str = "semibold";
pub const ICONS: &str = "icons";

/// Segoe Fluent Icons code points used by the app.
pub mod icon {
    pub const PLAY: char = '\u{E768}';
    pub const PAUSE: char = '\u{E769}';
    pub const BACK: char = '\u{E72B}';
    pub const CUT: char = '\u{E8C6}';
    pub const COPY: char = '\u{E8C8}';
    pub const UPLOAD: char = '\u{E898}';
    pub const FOLDER: char = '\u{ED25}';
    pub const DELETE: char = '\u{E74D}';
    pub const SETTINGS: char = '\u{E713}';
    pub const LIBRARY: char = '\u{E8F1}';
    pub const SEARCH: char = '\u{E721}';
    pub const REFRESH: char = '\u{E72C}';
    pub const SAVE: char = '\u{E74E}';
    pub const LINK: char = '\u{E71B}';
    pub const VIDEO: char = '\u{E714}';
    pub const VOLUME: char = '\u{E767}';
    pub const MIC: char = '\u{E720}';
    pub const GAME: char = '\u{E7FC}';
    pub const KEYBOARD: char = '\u{E765}';
    pub const SHARE: char = '\u{E72D}';
    pub const UPDATE: char = '\u{E895}';
    pub const POWER: char = '\u{E7E8}';
    pub const RING: char = '\u{EA8F}';
    pub const JOIN: char = '\u{E71D}';
    pub const CHECK: char = '\u{E73E}';
    pub const SKIP_BACK: char = '\u{E892}';
    pub const SKIP_FWD: char = '\u{E893}';
    pub const INFO: char = '\u{E946}';
    pub const ERROR: char = '\u{E783}';
    pub const CLOCK: char = '\u{E823}';
    pub const ADD: char = '\u{E710}';
    pub const RECORD: char = '\u{E7C8}';
    pub const CANCEL: char = '\u{E711}';
    pub const STAR: char = '\u{E734}';
    pub const STOP: char = '\u{E71A}';
    pub const MORE: char = '\u{E712}';
    pub const SORT: char = '\u{E8CB}';
    pub const TAG: char = '\u{E8EC}';
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
    pub const CHEVRON_UP: char = '\u{E70E}';
    pub const STOPWATCH: char = '\u{E916}';
    pub const MONITOR: char = '\u{E7F4}';
    pub const PICTURE: char = '\u{E8B9}';
    pub const MOUSE: char = '\u{E962}';
    pub const BRIGHTNESS: char = '\u{E706}';
    pub const GAUGE: char = '\u{E9D9}';
    pub const OPEN: char = '\u{E8A7}';
    pub const UP: char = '\u{E74A}';
    pub const DOWN: char = '\u{E74B}';
    pub const EDIT: char = '\u{E70F}';
    pub const LOOP: char = '\u{E8EE}';
    pub const STAR_FILLED: char = '\u{E735}';
}

/// Loads Segoe UI (regular and semibold) and Segoe Fluent Icons on Windows,
/// the system font on macOS, and DejaVu/Noto on Linux when present.
/// Returns true when the icon font is available.
pub fn install_fonts(ctx: &egui::Context) -> bool {
    let mut fonts = FontDefinitions::default();
    let read = |paths: &[&str]| paths.iter().find_map(|p| std::fs::read(p).ok());
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let win = |f: &str| format!("{windir}\\Fonts\\{f}");
    let (regular, semibold, icons) = if cfg!(windows) {
        (read(&[&win("segoeui.ttf")]), read(&[&win("seguisb.ttf")]), read(&[&win("SegoeIcons.ttf"), &win("segmdl2.ttf")]))
    } else if cfg!(target_os = "macos") {
        (
            read(&["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"]),
            read(&["/System/Library/Fonts/SFNS.ttf"]),
            None,
        )
    } else {
        (
            read(&[
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ]),
            read(&[
                "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
                "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
                "/usr/share/fonts/google-noto/NotoSans-SemiBold.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ]),
            None,
        )
    };
    let proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(r) = regular {
        fonts.font_data.insert("ui".into(), Arc::new(FontData::from_owned(r)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    let mut semi = proportional.clone();
    if let Some(s) = semibold {
        fonts.font_data.insert("ui-semibold".into(), Arc::new(FontData::from_owned(s)));
        semi.insert(0, "ui-semibold".into());
    } else if fonts.font_data.contains_key("ui") {
        semi.insert(0, "ui".into());
    }
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);
    let has_icons = icons.is_some();
    if let Some(i) = icons {
        fonts.font_data.insert("icons".into(), Arc::new(FontData::from_owned(i)));
        fonts.families.insert(FontFamily::Name(ICONS.into()), vec!["icons".into()]);
    } else {
        fonts.families.insert(FontFamily::Name(ICONS.into()), proportional);
    }
    ctx.set_fonts(fonts);
    has_icons
}

fn visuals(t: &Tokens) -> egui::Visuals {
    let mut v = if t.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = t.base;
    v.window_fill = t.flyout;
    v.window_stroke = Stroke::new(1.0, if t.dark { argb(0x33FF_FFFF) } else { argb(0x1F00_0000) });
    v.window_corner_radius = CornerRadius::same(R_CARD);
    v.menu_corner_radius = CornerRadius::same(R_CARD);
    v.window_shadow = Shadow { offset: [0, 8], blur: 32, spread: 0, color: argb(0x4000_0000) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: argb(0x3000_0000) };
    v.extreme_bg_color = if t.dark { argb(0xFF1C_1C1C) } else { argb(0xFFFF_FFFF) };
    v.text_edit_bg_color = Some(if t.dark { argb(0xB31E_1E1E) } else { argb(0xFFFF_FFFF) });
    v.faint_bg_color = t.card_alt;
    v.hyperlink_color = t.accent;
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0, t.accent);
    v.warn_fg_color = t.caution;
    v.error_fg_color = t.critical;
    v.override_text_color = None;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    v.indent_has_left_vline = false;

    let r = CornerRadius::same(R_CONTROL);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.divider);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    w.noninteractive.corner_radius = r;
    // Slider rails are neutral; the filled part (selection) and the thumb
    // on hover use the accent. Control edges reach 3:1 against the page.
    w.inactive.bg_fill = if t.dark { argb(0xFF8A_8A8A) } else { argb(0xFF86_8686) };
    w.inactive.weak_bg_fill = t.control;
    w.inactive.bg_stroke = Stroke::new(1.0, t.edge);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = t.accent_hover;
    w.hovered.weak_bg_fill = t.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, t.edge);
    w.hovered.fg_stroke = Stroke::new(1.0, t.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = t.accent;
    w.active.weak_bg_fill = t.control_pressed;
    w.active.bg_stroke = Stroke::new(1.0, t.edge);
    w.active.fg_stroke = Stroke::new(1.0, t.text2);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open = w.hovered;
    v
}

fn style(base: &egui::Style) -> egui::Style {
    let mut s = base.clone();
    use FontFamily::Proportional;
    let semi = FontFamily::Name(SEMIBOLD.into());
    s.text_styles = [
        (TextStyle::Small, FontId::new(12.0, Proportional)),
        (TextStyle::Body, FontId::new(14.0, Proportional)),
        (TextStyle::Button, FontId::new(14.0, Proportional)),
        (TextStyle::Monospace, FontId::new(14.0, Proportional)),
        (TextStyle::Heading, FontId::new(28.0, semi)),
    ]
    .into();
    s.spacing.item_spacing = Vec2::new(8.0, 8.0);
    s.spacing.button_padding = Vec2::new(12.0, 6.0);
    s.spacing.interact_size = Vec2::new(40.0, CONTROL_H);
    s.spacing.combo_height = 320.0;
    s.spacing.slider_width = 200.0;
    s.spacing.slider_rail_height = 4.0;
    s.spacing.icon_width = 20.0;
    s.spacing.icon_width_inner = 10.0;
    s.spacing.window_margin = Margin::same(16);
    s.spacing.menu_margin = Margin::same(4);
    s.spacing.combo_width = 220.0;
    s.spacing.text_edit_width = 280.0;
    s.spacing.scroll.bar_width = 6.0;
    s.spacing.scroll.floating = true;
    s.interaction.tooltip_delay = 0.4;
    s
}

pub fn apply(ctx: &egui::Context) {
    for (theme, t) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        let st = style(&ctx.style_of(theme));
        ctx.set_style_of(theme, st);
        ctx.set_visuals_of(theme, visuals(&t));
    }
}

// ------------------------------------------------------------- type ramp

pub fn title(s: impl Into<String>) -> RichText {
    RichText::new(s).family(FontFamily::Name(SEMIBOLD.into())).size(28.0)
}
pub fn subtitle(s: impl Into<String>) -> RichText {
    RichText::new(s).family(FontFamily::Name(SEMIBOLD.into())).size(20.0)
}
pub fn body_large(s: impl Into<String>) -> RichText {
    RichText::new(s).size(18.0)
}
pub fn body_strong(s: impl Into<String>) -> RichText {
    RichText::new(s).family(FontFamily::Name(SEMIBOLD.into())).size(14.0)
}
pub fn caption(s: impl Into<String>) -> RichText {
    RichText::new(s).size(12.0)
}
pub fn glyph(c: char, size: f32) -> RichText {
    RichText::new(c.to_string()).family(FontFamily::Name(ICONS.into())).size(size)
}

// ---------------------------------------------------------------- widgets

/// A card on the content layer (8 px radius, 1 px stroke).
pub fn card(t: &Tokens) -> egui::Frame {
    egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_stroke))
        .corner_radius(CornerRadius::same(R_CARD))
        .inner_margin(Margin::same(16))
}

/// A settings card row: compact padding, 4 px radius, like Windows Settings.
pub fn setting_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_stroke))
        .corner_radius(CornerRadius::same(R_CONTROL))
        .inner_margin(Margin { left: 16, right: 16, top: 12, bottom: 12 })
}

/// Button content: an icon glyph (when the icon font exists) and a label.
pub fn label_with_icon(icons: bool, c: char, text: &str) -> WidgetText {
    icon_label(icons, c, text, Color32::PLACEHOLDER)
}

/// Like `label_with_icon`, in a fixed color (for text on the accent).
pub fn icon_label(icons: bool, c: char, text: &str, color: Color32) -> WidgetText {
    if !icons {
        return RichText::new(text).color(color).into();
    }
    let mut job = egui::text::LayoutJob::default();
    job.append(&c.to_string(), 0.0, egui::TextFormat { font_id: FontId::new(16.0, FontFamily::Name(ICONS.into())), color, valign: egui::Align::Center, ..Default::default() });
    if !text.is_empty() {
        job.append(text, 8.0, egui::TextFormat { font_id: FontId::new(14.0, FontFamily::Proportional), color, valign: egui::Align::Center, ..Default::default() });
    }
    job.into()
}

/// Accent ("primary") button: one per view, for the main action.
pub fn primary(ui: &mut Ui, t: &Tokens, text: impl Into<WidgetText>, enabled: bool) -> Response {
    let text: WidgetText = text.into();
    let b = egui::Button::new(text.color(if enabled { t.on_accent } else { t.text_disabled }))
        .fill(if enabled { t.accent } else { t.control })
        .stroke(Stroke::NONE)
        .min_size(Vec2::new(0.0, CONTROL_H));
    ui.add_enabled(enabled, b)
}

/// Standard button (ControlFill with a 1 px stroke).
pub fn button(ui: &mut Ui, text: impl Into<WidgetText>, enabled: bool) -> Response {
    ui.add_enabled(enabled, egui::Button::new(text).min_size(Vec2::new(0.0, CONTROL_H)))
}

/// Transparent ("subtle") button, for toolbars and icon buttons.
pub fn subtle(ui: &mut Ui, text: impl Into<WidgetText>) -> Response {
    ui.add(egui::Button::new(text).frame_when_inactive(false).min_size(Vec2::new(CONTROL_H, CONTROL_H)))
}

/// Fluent ToggleSwitch: 40 x 20 track, 12 px knob (14 px on hover).
pub fn toggle(ui: &mut Ui, t: &Tokens, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(40.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let how_on = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let p = ui.painter();
    let r = rect.height() / 2.0;
    if *on {
        let fill = if resp.hovered() { t.accent_hover } else { t.accent };
        p.rect_filled(rect, r, fill);
    } else {
        let fill = if resp.hovered() { t.control_hover } else { t.control_pressed };
        p.rect_filled(rect, r, fill);
        p.rect_stroke(rect, r, Stroke::new(1.0, t.strong), egui::StrokeKind::Inside);
    }
    let knob_r = if resp.hovered() { 7.0 } else { 6.0 };
    let x = egui::lerp((rect.left() + r)..=(rect.right() - r), how_on);
    let knob = if *on { t.on_accent } else { t.strong };
    p.circle_filled(egui::pos2(x, rect.center().y), knob_r, knob);
    focus_ring(ui, t, &resp, rect, r);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CheckBox, ui.is_enabled(), *on, if *on { "On" } else { "Off" }));
    resp
}

/// One row of a settings group: title and description on the left, the
/// control on the right.
pub fn setting(
    ui: &mut Ui,
    t: &Tokens,
    icons: bool,
    glyph_char: Option<char>,
    title_text: &str,
    description: &str,
    control: impl FnOnce(&mut Ui),
) {
    setting_frame(t).show(ui, |ui| {
        ui.set_min_height(40.0);
        ui.horizontal(|ui| {
            // Every row keeps the icon column, so all titles line up.
            if icons {
                let (r, _) = ui.allocate_exact_size(Vec2::new(28.0, 40.0), Sense::hover());
                if let Some(c) = glyph_char {
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, c, FontId::new(18.0, FontFamily::Name(ICONS.into())), t.text);
                }
                ui.add_space(4.0);
            }
            // Text column: leaves room for the control column on the right.
            let text_w = (ui.available_width() - CONTROL_COL - 16.0).max(160.0);
            let (text_rect, _) = ui.allocate_exact_size(Vec2::new(text_w, 40.0), Sense::hover());
            let mut tu = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::top_down(egui::Align::Min)));
            tu.spacing_mut().item_spacing.y = 2.0;
            let wrap = |s: RichText| egui::Label::new(s).wrap();
            if description.is_empty() {
                // One line: centre it on the control.
                tu.add_space(10.0);
                tu.add(wrap(RichText::new(title_text).color(t.text)));
            } else {
                tu.add(wrap(RichText::new(title_text).color(t.text)));
                tu.add(wrap(caption(description).color(t.text2)));
            }
            let used = tu.min_rect().height();
            if used > 40.0 {
                ui.allocate_exact_size(Vec2::new(0.0, used), Sense::hover());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
        });
    });
}

/// Navigation item for the left rail (selected item has an accent pill).
/// In the compact rail only the icon shows, with the label as a tooltip.
pub fn nav_item(ui: &mut Ui, t: &Tokens, icons: bool, c: char, label: &str, selected: bool, compact: bool) -> Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 36.0), Sense::click());
    let p = ui.painter();
    let fill = if selected {
        t.subtle_hover
    } else if resp.is_pointer_button_down_on() {
        t.subtle_pressed
    } else if resp.hovered() {
        t.subtle_hover
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, R_CONTROL, fill);
    if selected {
        let pill = egui::Rect::from_center_size(egui::pos2(rect.left() + 1.5, rect.center().y), Vec2::new(3.0, 16.0));
        p.rect_filled(pill, 1.5, t.accent);
    }
    focus_ring(ui, t, &resp, rect, f32::from(R_CONTROL));
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label));
    let p = ui.painter();
    let icon_font = FontId::new(16.0, FontFamily::Name(ICONS.into()));
    if compact {
        let g = if icons { c.to_string() } else { label.chars().next().unwrap_or('?').to_string() };
        p.text(rect.center(), egui::Align2::CENTER_CENTER, g, if icons { icon_font } else { FontId::proportional(14.0) }, t.text);
        return resp.on_hover_text(label).on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    let mut x = rect.left() + 14.0;
    if icons {
        p.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, c, icon_font, t.text);
        x += 30.0;
    }
    p.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, label, FontId::new(14.0, FontFamily::Proportional), t.text);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A standard button of width `w`.
pub fn button_w(ui: &mut Ui, text: impl Into<WidgetText>, w: f32) -> Response {
    ui.add(egui::Button::new(text).min_size(Vec2::new(w, CONTROL_H)))
}

/// A filter chip: rounded, accent-filled when on.
pub fn chip(ui: &mut Ui, t: &Tokens, text: impl Into<String>, on: bool) -> Response {
    let b = egui::Button::new(RichText::new(text).size(14.0).color(if on { t.text } else { t.text2 }))
        .fill(if on { if t.dark { argb(0xFF3A_3A3A) } else { argb(0xFFE6_E6E6) } } else { t.control })
        .stroke(if on { Stroke::new(1.0, t.accent) } else { Stroke::new(1.0, t.control_stroke) })
        .corner_radius(CornerRadius::same(14))
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(b)
}

/// A ComboBox with a Fluent chevron instead of egui's filled triangle.
pub fn combo<'a>(id: impl egui::AsIdSalt + std::fmt::Debug) -> egui::ComboBox<'a> {
    egui::ComboBox::from_id_salt(id).icon(|ui, rect, _visuals, _open| {
        let c = egui::pos2(rect.center().x, rect.center().y + 1.0);
        let col = ui.visuals().widgets.inactive.fg_stroke.color.gamma_multiply(0.8);
        let s = Stroke::new(1.3, col);
        ui.painter().line_segment([egui::pos2(c.x - 4.0, c.y - 2.0), egui::pos2(c.x, c.y + 2.0)], s);
        ui.painter().line_segment([egui::pos2(c.x, c.y + 2.0), egui::pos2(c.x + 4.0, c.y - 2.0)], s);
    })
}

/// A slider with its value right-aligned in a fixed box, filling the
/// control column.
pub fn slider_row(ui: &mut Ui, t: &Tokens, value: String, v: &mut u32, range: std::ops::RangeInclusive<u32>, log: bool) -> Response {
    let (r, _) = ui.allocate_exact_size(Vec2::new(60.0, 20.0), Sense::hover());
    ui.painter().text(egui::pos2(r.right(), r.center().y), egui::Align2::RIGHT_CENTER, value, FontId::proportional(14.0), t.text2);
    slider(ui, t, v, range, log, CONTROL_COL - 68.0)
}

/// Fluent slider: a 4 px rail (accent up to the thumb, neutral after) and a
/// 20 px dark thumb with an accent dot that grows on hover.
pub fn slider(ui: &mut Ui, t: &Tokens, v: &mut u32, range: std::ops::RangeInclusive<u32>, log: bool, width: f32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::click_and_drag());
    let (lo, hi) = (f64::from(*range.start()), f64::from(*range.end()).max(f64::from(*range.start()) + 1.0));
    let to_t = |x: f64| if log { (x.max(lo).ln() - lo.max(1.0).ln()) / (hi.ln() - lo.max(1.0).ln()) } else { (x - lo) / (hi - lo) };
    let from_t = |f: f64| if log { (lo.max(1.0).ln() + f * (hi.ln() - lo.max(1.0).ln())).exp() } else { lo + f * (hi - lo) };
    let (x0, x1) = (rect.left() + 10.0, rect.right() - 10.0);
    if resp.is_pointer_button_down_on()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let f = f64::from((p.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
        let nv = from_t(f).round().clamp(lo, hi) as u32;
        if nv != *v {
            *v = nv;
            resp.mark_changed();
        }
    }
    if resp.has_focus() {
        let step = ((hi - lo) / 100.0).max(1.0);
        let (l, r) = ui.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
        if l || r {
            let nv = (f64::from(*v) + if r { step } else { -step }).round().clamp(lo, hi) as u32;
            if nv != *v {
                *v = nv;
                resp.mark_changed();
            }
        }
    }
    let f = to_t(f64::from(*v)).clamp(0.0, 1.0) as f32;
    let cx = x0 + (x1 - x0) * f;
    let cy = rect.center().y;
    let p = ui.painter();
    let rail = |a: f32, b: f32, c: Color32| p.rect_filled(egui::Rect::from_min_max(egui::pos2(a, cy - 2.0), egui::pos2(b, cy + 2.0)), 2.0, c);
    rail(x0, x1, if t.dark { argb(0xFF8A_8A8A) } else { argb(0xFF86_8686) });
    rail(x0, cx, if ui.is_enabled() { t.accent } else { t.text_disabled });
    let outer = if t.dark { argb(0xFF45_4545) } else { argb(0xFFFF_FFFF) };
    p.circle(egui::pos2(cx, cy), 10.0, outer, Stroke::new(1.0, t.control_stroke));
    let inner = if resp.is_pointer_button_down_on() { 5.0 } else if resp.hovered() { 7.0 } else { 6.0 };
    p.circle_filled(egui::pos2(cx, cy), inner, if ui.is_enabled() { t.accent } else { t.text_disabled });
    focus_ring(ui, t, &resp, rect, 4.0);
    let value = f64::from(*v);
    resp.widget_info(|| egui::WidgetInfo::slider(true, value, ""));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A fixed-width button with its label centred (dialog footers).
pub fn button_centered(ui: &mut Ui, t: &Tokens, text: &str, w: f32, primary: bool) -> Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, CONTROL_H), Sense::click());
    let hovered = resp.hovered();
    let (fill, stroke, color) = if primary {
        (if hovered { t.accent_hover } else { t.accent }, Stroke::NONE, t.on_accent)
    } else {
        (if hovered { t.control_hover } else { t.control }, Stroke::new(1.0, t.edge), t.text)
    };
    ui.painter().rect(r, R_CONTROL, fill, stroke, egui::StrokeKind::Inside);
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(14.0), color);
    focus_ring(ui, t, &resp, r, f32::from(R_CONTROL));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A button in the navigation rail, laid out like `nav_item` (icon at 14 px,
/// label at 44 px) so the rail keeps one icon and one label column. `right`
/// is a quiet hint at the right edge (a hotkey).
#[allow(clippy::too_many_arguments)]
pub fn rail_button(ui: &mut Ui, t: &Tokens, icons: bool, c: char, label: &str, right: &str, icon_color: Color32, enabled: bool, compact: bool) -> Response {
    let w = ui.available_width();
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 36.0), sense);
    let fill = if !enabled {
        t.control_pressed
    } else if resp.is_pointer_button_down_on() {
        t.control_pressed
    } else if resp.hovered() {
        t.control_hover
    } else {
        t.control
    };
    let p = ui.painter();
    if compact {
        if enabled && resp.hovered() {
            p.rect_filled(rect, R_CONTROL, t.subtle_hover);
        }
    } else {
        p.rect(rect, R_CONTROL, fill, Stroke::new(1.0, t.control_stroke), egui::StrokeKind::Inside);
    }
    let fg = if enabled { t.text } else { t.text_disabled };
    let ic = if enabled { icon_color } else { t.text_disabled };
    let icon_font = FontId::new(16.0, FontFamily::Name(ICONS.into()));
    if compact {
        if icons {
            p.text(rect.center(), egui::Align2::CENTER_CENTER, c, icon_font, ic);
        }
    } else {
        if icons {
            p.text(egui::pos2(rect.left() + 14.0, rect.center().y), egui::Align2::LEFT_CENTER, c, icon_font, ic);
        }
        p.text(egui::pos2(rect.left() + 44.0, rect.center().y), egui::Align2::LEFT_CENTER, label, FontId::proportional(14.0), fg);
        if !right.is_empty() {
            p.text(egui::pos2(rect.right() - 12.0, rect.center().y), egui::Align2::RIGHT_CENTER, right, FontId::proportional(12.0), t.text3);
        }
    }
    focus_ring(ui, t, &resp, rect, f32::from(R_CONTROL));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A 2 px focus ring around `rect` when `resp` has keyboard focus.
pub fn focus_ring(ui: &Ui, t: &Tokens, resp: &Response, rect: egui::Rect, radius: f32) {
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(3.0), radius + 3.0, Stroke::new(2.0, t.text), egui::StrokeKind::Outside);
    }
}

/// Section header inside a page (Body strong, Windows Settings style).
/// The first section header on a page: no extra space above it.
pub fn section_first(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add_space(4.0);
    ui.label(body_strong(text).color(t.text));
    ui.add_space(4.0);
}

pub fn section(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add_space(24.0);
    ui.label(body_strong(text).color(t.text));
    ui.add_space(4.0);
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0) as u64;
    let sec = s - m as f64 * 60.0;
    format!("{m}:{sec:05.2}")
}

/// A bitrate for people: "0.8 Mbit/s", "4.5 Mbit/s", "25 Mbit/s".
pub fn fmt_mbps(kbps: u32) -> String {
    let m = f64::from(kbps) / 1000.0;
    if m < 10.0 && kbps % 1000 != 0 { format!("{m:.1} Mbit/s") } else { format!("{m:.0} Mbit/s") }
}

/// A memory size for people: "860 MB", "1.4 GB".
pub fn fmt_bytes(b: u64) -> String {
    if b >= 1_000_000_000 { format!("{:.1} GB", b as f64 / 1e9) } else { format!("{} MB", (b + 500_000) / 1_000_000) }
}

/// A running timer: "4:05", "1:02:03".
pub fn fmt_clock(secs: u64) -> String {
    if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) }
}

pub fn fmt_short(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
