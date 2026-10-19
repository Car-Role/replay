//! The Games page: a list of every game Replay has found (icon, clips,
//! quality, record switch). A row opens that game's page with its own
//! quality settings, memory use and clips.

use std::path::Path;

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke, StrokeKind, Vec2};

use super::theme::{self, Tokens, icon};
use super::{LibraryApp, Page};
use crate::config::{GameSettings, Resolution};
use crate::games::{self, GameDb, GameRecord};

const FPS_CHOICES: [u32; 5] = [30, 60, 120, 144, 240];
const KBPS_CHOICES: [u32; 14] = [1_000, 2_000, 3_000, 4_000, 6_000, 8_000, 10_000, 15_000, 20_000, 25_000, 35_000, 50_000, 75_000, 150_000];
const LENGTH_CHOICES: [u32; 8] = [30, 60, 120, 180, 300, 600, 900, 1200];

fn when(secs: u64) -> String {
    let now = games::now_secs();
    let days = now.saturating_sub(secs) / 86_400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        d if d < 30 => format!("{d} days ago"),
        d if d < 365 => format!("{} months ago", d / 30),
        _ => "over a year ago".into(),
    }
}

impl LibraryApp {
    /// Saves a config change right away and tells the recorder.
    fn apply_config(&mut self, change: impl FnOnce(&mut crate::config::Config), message: Option<String>) {
        let mut cfg = self.shared.cfg.clone();
        change(&mut cfg);
        let cfg = cfg.sanitized();
        match cfg.save() {
            Ok(()) => {
                self.shared.cfg = cfg;
                self.settings.reset(&self.shared.cfg);
                crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::Reload);
                if let Some(m) = message {
                    self.shared.toast(m, false);
                }
            }
            Err(e) => self.shared.toast(format!("Could not save: {e}"), true),
        }
    }

    /// Every game: found ones, plus ones on the lists that were never seen.
    fn game_list(&self) -> Vec<GameRecord> {
        let mut list: Vec<GameRecord> = self.games.games.clone();
        let d = &self.shared.cfg.detection;
        for exe in d.include.iter().chain(d.exclude.iter()) {
            if !list.iter().any(|g| &g.exe == exe) {
                list.push(GameRecord {
                    exe: exe.clone(),
                    name: exe.trim_end_matches(".exe").to_string(),
                    path: String::new(),
                    how: if d.include.contains(exe) { "Added by you" } else { "Turned off by you" }.into(),
                    first_seen: 0,
                    last_seen: 0,
                    recording: false,
                    capturing: false,
                    capture_started: 0,
                });
            }
        }
        list.sort_by(|a, b| b.recording.cmp(&a.recording).then(b.last_seen.cmp(&a.last_seen)).then(a.name.cmp(&b.name)));
        list
    }

    /// (clips, bytes) saved for a game.
    fn game_clips(&self, g: &GameRecord) -> (usize, u64) {
        let folder = crate::util::sanitize_file_name(&g.name);
        let mine = self.clips.iter().filter(|c| c.game == folder || c.game == g.name);
        mine.fold((0, 0), |(n, b), c| (n + 1, b + c.size))
    }

    /// One line about how a game records: "60 fps · 1080p · 4 Mbit/s · 2:00".
    fn quality_line(&self, exe: &str) -> String {
        let c = self.shared.cfg.for_game(exe);
        format!(
            "{} fps  \u{00b7}  {}  \u{00b7}  {}  \u{00b7}  {}",
            c.video.fps,
            c.video.resolution.label(),
            theme::fmt_mbps(c.video.bitrate_kbps),
            theme::fmt_short(f64::from(c.replay_seconds))
        )
    }

    /// The game's icon (from its program file) or its first letter.
    fn game_icon(&mut self, ui: &mut egui::Ui, t: &Tokens, g: &GameRecord, size: f32, enabled: bool) {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        let tex = if !g.path.is_empty() && Path::new(&g.path).exists() { self.thumb(Path::new(&g.path)).map(|t| t.id()) } else { None };
        match tex {
            Some(id) => {
                let tint = if enabled { Color32::WHITE } else { Color32::from_white_alpha(110) };
                ui.painter().image(id, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
            }
            None => {
                ui.painter().rect_filled(r, CornerRadius::same(theme::R_CONTROL), t.control_hover);
                let letter = g.name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    letter,
                    egui::FontId::new(size * 0.45, egui::FontFamily::Name(theme::SEMIBOLD.into())),
                    if enabled { t.text } else { t.text3 },
                );
            }
        }
    }

    pub(super) fn games_page(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        if let Some(exe) = self.game_open.clone() {
            match self.game_list().into_iter().find(|g| g.exe == exe) {
                Some(g) => return self.game_detail(ui, t, &g),
                None => self.game_open = None,
            }
        }
        let icons = self.shared.icons;
        let list = self.game_list();
        ui.horizontal(|ui| {
            ui.label(theme::title("Games").color(t.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::button(ui, theme::label_with_icon(icons, icon::ADD, "Add a game"), true).clicked()
                    && let Some(p) = crate::sys::shell::pick_exe()
                    && let Some(name) = p.file_name()
                {
                    let exe = name.to_string_lossy().to_ascii_lowercase();
                    let pretty = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    let mut db = GameDb::load();
                    db.seen(&exe, &pretty, &p.display().to_string(), "Added by you");
                    db.save();
                    self.games = db;
                    let e = exe.clone();
                    self.apply_config(move |c| games::set_enabled(c, &e, true, true), Some(format!("{pretty} will be recorded.")));
                }
            });
        });
        // Memory in one sentence: every enabled game recording at once.
        let enabled: Vec<&str> = list.iter().filter(|g| games::is_enabled(&self.shared.cfg.detection, &g.exe)).map(|g| g.exe.as_str()).collect();
        let sentence = match enabled.len() {
            0 => "Replay finds games by itself. Turn off any you don't want recorded.".to_string(),
            1 => format!("While a replay is on, Replay uses about {} of memory.", theme::fmt_bytes(self.shared.cfg.recording_memory(&enabled))),
            n => format!(
                "One game with a replay on uses up to {} of memory; all {n} at once would use {}.",
                theme::fmt_bytes(enabled.iter().map(|e| self.shared.cfg.recording_memory(&[e])).max().unwrap_or(0)),
                theme::fmt_bytes(self.shared.cfg.recording_memory(&enabled))
            ),
        };
        ui.label(RichText::new(sentence).color(t.text2));
        ui.add_space(8.0);

        if list.is_empty() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                if icons {
                    ui.label(theme::glyph(icon::GAME, 40.0).color(t.text3));
                }
                ui.add_space(8.0);
                ui.label(theme::subtitle("No games yet").color(t.text));
                ui.label(RichText::new("Start a game and it shows up here. You can also add one.").color(t.text2));
            });
            return;
        }

        let recording: Vec<&GameRecord> = list.iter().filter(|g| g.recording).collect();
        let on: Vec<&GameRecord> = list.iter().filter(|g| !g.recording && games::is_enabled(&self.shared.cfg.detection, &g.exe)).collect();
        let off: Vec<&GameRecord> = list.iter().filter(|g| !games::is_enabled(&self.shared.cfg.detection, &g.exe)).collect();
        let mut toggle = None;
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
            let mut first = true;
            for (title, group) in [("Replay on", &recording), ("Your games", &on), ("Turned off", &off)] {
                if group.is_empty() {
                    continue;
                }
                if std::ptr::eq(group, &off) {
                    // Apps turned off are rarely needed: keep them folded.
                    let id = ui.id().with("show-off");
                    let mut open = ui.data_mut(|d| *d.get_temp_mut_or(id, false));
                    if !first {
                        ui.add_space(24.0);
                    }
                    let label = format!("{} turned off ({})", if open { "Hide" } else { "Show" }, group.len());
                    let chevron = if open { icon::CHEVRON_UP } else { icon::CHEVRON_DOWN };
                    if ui.add(egui::Button::new(theme::icon_label(self.shared.icons, chevron, &label, t.text2)).frame_when_inactive(false)).clicked() {
                        open = !open;
                        ui.data_mut(|d| d.insert_temp(id, open));
                    }
                    if !open {
                        continue;
                    }
                } else if first {
                    theme::section_first(ui, t, title);
                } else {
                    theme::section(ui, t, title);
                }
                first = false;
                for g in group.iter() {
                    match self.game_row(ui, t, g) {
                        Some(RowClick::Toggle(v)) => toggle = Some((g.exe.clone(), g.name.clone(), v, g.how == "Added by you")),
                        Some(RowClick::Open) => open = Some(g.exe.clone()),
                        None => {}
                    }
                }
            }
            ui.add_space(24.0);
        });
        if let Some((exe, name, v, by_user)) = toggle {
            let msg = if v { format!("{name} will be recorded.") } else { format!("{name} will not be recorded.") };
            self.apply_config(move |c| games::set_enabled(c, &exe, v, by_user), Some(msg));
        }
        if let Some(exe) = open {
            self.game_open = Some(exe);
        }
    }

    fn game_row(&mut self, ui: &mut egui::Ui, t: &Tokens, g: &GameRecord) -> Option<RowClick> {
        let enabled = games::is_enabled(&self.shared.cfg.detection, &g.exe);
        let (clips, bytes) = self.game_clips(g);
        let own = self.shared.cfg.games.contains_key(&g.exe);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 64.0), Sense::click());
        let hovered = resp.hovered();
        ui.painter().rect(rect, CornerRadius::same(theme::R_CONTROL), if hovered { t.card_hover } else { t.card }, Stroke::new(1.0, t.card_stroke), StrokeKind::Inside);
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(16.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let mut click = None;
        let ui = &mut inner;
        self.game_icon(ui, t, g, 32.0, enabled);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.label(theme::body_strong(&g.name).color(if enabled { t.text } else { t.text2 }));
                if g.capturing {
                    pill(ui, t.recording, "Recording session");
                }
                if own {
                    pill(ui, t.text2, "Own settings");
                }
            });
            let mut parts = Vec::new();
            if clips > 0 {
                parts.push(format!("{clips} {}  \u{00b7}  {}", if clips == 1 { "clip" } else { "clips" }, theme::fmt_bytes(bytes)));
            }
            if g.recording {
                let secs = self.shared.cfg.for_game(&g.exe).replay_seconds;
                parts.insert(0, format!("Keeping {}", theme::fmt_short(f64::from(secs))));
            }
            if enabled && g.recording {
                // "Buffering 2:00" already says the length.
                let c = self.shared.cfg.for_game(&g.exe);
                parts.push(format!("{} fps  \u{00b7}  {}  \u{00b7}  {}", c.video.fps, c.video.resolution.label(), theme::fmt_mbps(c.video.bitrate_kbps)));
            } else if enabled {
                parts.push(self.quality_line(&g.exe));
            } else {
                parts.push(g.how.clone());
            }
            ui.label(theme::caption(parts.join("  \u{00b7}  ")).color(t.text2));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if self.shared.icons {
                ui.label(theme::glyph(icon::CHEVRON_RIGHT, 12.0).color(t.text3));
            }
            ui.add_space(8.0);
            let mut v = enabled;
            if theme::toggle(ui, t, &mut v).on_hover_text(format!("Record {}: {}", g.name, if enabled { "on" } else { "off" })).changed() {
                click = Some(RowClick::Toggle(v));
            }
        });
        if click.is_none() && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            click = Some(RowClick::Open);
        }
        click
    }

    /// One game's page: record switch, quality, memory and clips.
    fn game_detail(&mut self, ui: &mut egui::Ui, t: &Tokens, g: &GameRecord) {
        let icons = self.shared.icons;
        let enabled = games::is_enabled(&self.shared.cfg.detection, &g.exe);
        ui.horizontal(|ui| {
            if theme::subtle(ui, theme::label_with_icon(icons, icon::BACK, if icons { "" } else { "Back" })).on_hover_text("All games").clicked() {
                self.game_open = None;
            }
            ui.add_space(4.0);
            self.game_icon(ui, t, g, 40.0, enabled);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(theme::subtitle(&g.name).color(t.text));
                let mut parts = vec![g.exe.clone(), g.how.clone()];
                if g.last_seen > 0 {
                    parts.push(format!("last played {}", when(g.last_seen)));
                }
                ui.label(theme::caption(parts.join("  \u{00b7}  ")).color(t.text2));
            });
        });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.game_open = None;
        }
        ui.add_space(12.0);
        let current = self.shared.cfg.games.get(&g.exe).cloned().unwrap_or_default();
        let general = self.shared.cfg.clone();
        let mut s = current.clone();
        let mut toggle = None;
        let mut forget = false;
        let mut show_clips = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
            theme::setting(ui, t, false, Some(icon::GAME), "Replay for this game", "Keep the last few minutes while it runs", |ui| {
                let mut v = enabled;
                if theme::toggle(ui, t, &mut v).changed() {
                    toggle = Some(v);
                }
            });

            theme::section(ui, t, "Video");
            let combo_w = theme::CONTROL_COL;
            theme::setting(ui, t, false, Some(icon::VIDEO), "Frame rate", "", |ui| {
                let label = |v: Option<u32>| v.map(|f| format!("{f} fps")).unwrap_or_else(|| format!("Same as general ({} fps)", general.video.fps));
                theme::combo("g-fps").width(combo_w).selected_text(label(s.fps)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.fps, None, label(None));
                    for f in FPS_CHOICES {
                        ui.selectable_value(&mut s.fps, Some(f), label(Some(f)));
                    }
                });
            });
            theme::setting(ui, t, false, None, "Resolution", "", |ui| {
                let label = |v: Option<Resolution>| match v {
                    Some(r) => r.label().to_string(),
                    None => format!("Same as general ({})", general.video.resolution.label()),
                };
                theme::combo("g-res").width(combo_w).selected_text(label(s.resolution)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.resolution, None, label(None));
                    for r in Resolution::ALL {
                        ui.selectable_value(&mut s.resolution, Some(r), label(Some(r)));
                    }
                });
            });
            theme::setting(ui, t, false, None, "Bitrate", "Higher is sharper; lower uses less memory and disk", |ui| {
                let label = |v: Option<u32>| match v {
                    Some(k) => theme::fmt_mbps(k),
                    None => format!("Same as general ({})", theme::fmt_mbps(general.video.bitrate_kbps)),
                };
                theme::combo("g-br").width(combo_w).selected_text(label(s.bitrate_kbps)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.bitrate_kbps, None, label(None));
                    for k in KBPS_CHOICES {
                        ui.selectable_value(&mut s.bitrate_kbps, Some(k), label(Some(k)));
                    }
                });
            });
            theme::setting(ui, t, false, Some(icon::CLOCK), "Replay length", "", |ui| {
                let label = |v: Option<u32>| match v {
                    Some(secs) => theme::fmt_short(f64::from(secs)),
                    None => format!("Same as general ({})", theme::fmt_short(f64::from(general.replay_seconds))),
                };
                theme::combo("g-len").width(combo_w).selected_text(label(s.replay_seconds)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.replay_seconds, None, label(None));
                    for secs in LENGTH_CHOICES {
                        ui.selectable_value(&mut s.replay_seconds, Some(secs), label(Some(secs)));
                    }
                });
            });
            if !current.is_empty() && ui.add(egui::Button::new(theme::caption("Use the general settings").color(t.accent)).frame_when_inactive(false)).clicked() {
                s = GameSettings::default();
            }

            theme::section(ui, t, "Memory and storage");
            let mut preview = general.clone();
            if s.is_empty() {
                preview.games.remove(&g.exe);
            } else {
                preview.games.insert(g.exe.clone(), s.clone());
            }
            let preview = preview.sanitized();
            let buffer = preview.for_game(&g.exe).buffer_bytes();
            let total = preview.recording_memory(&[g.exe.as_str()]);
            theme::setting(ui, t, false, Some(icon::INFO), "Memory while the replay is on", &format!("{} for the replay, the rest for capture and encoding", theme::fmt_bytes(buffer)), |ui| {
                ui.label(RichText::new(theme::fmt_bytes(total)).color(t.text));
            });
            let (clips, bytes) = self.game_clips(g);
            let desc = if clips == 0 { "No clips yet".to_string() } else { format!("{clips} {} use {}", if clips == 1 { "clip" } else { "clips" }, theme::fmt_bytes(bytes)) };
            theme::setting(ui, t, false, Some(icon::LIBRARY), "Clips", &desc, |ui| {
                if clips > 0 && theme::button(ui, "Show clips", true).clicked() {
                    show_clips = true;
                }
            });

            ui.add_space(24.0);
            theme::setting(ui, t, false, None, "Remove from this list", "Replay adds it again if you play it and it is detected", |ui| {
                if ui.add(egui::Button::new(theme::label_with_icon(icons, icon::DELETE, "Remove")).min_size(Vec2::new(0.0, theme::CONTROL_H))).clicked() {
                    forget = true;
                }
            });
            ui.add_space(24.0);
        });

        if s != current {
            let exe = g.exe.clone();
            let msg = if g.recording { Some(format!("{}: recording restarts with the new settings.", g.name)) } else { None };
            self.apply_config(
                move |c| {
                    if s.is_empty() {
                        c.games.remove(&exe);
                    } else {
                        c.games.insert(exe, s);
                    }
                },
                msg,
            );
        }
        if let Some(v) = toggle {
            let (exe, by_user) = (g.exe.clone(), g.how == "Added by you");
            let msg = if v { format!("{} will be recorded.", g.name) } else { format!("{} will not be recorded.", g.name) };
            self.apply_config(move |c| games::set_enabled(c, &exe, v, by_user), Some(msg));
        }
        if show_clips {
            self.gallery.chip = super::clips::Chip::Game(crate::util::sanitize_file_name(&g.name));
            self.page = Page::Clips;
        }
        if forget {
            let exe = g.exe.clone();
            let mut db = GameDb::load();
            db.forget(&exe);
            db.save();
            self.games = db;
            self.game_open = None;
            self.apply_config(
                move |c| {
                    c.detection.include.retain(|e| e != &exe);
                    c.detection.exclude.retain(|e| e != &exe);
                    c.games.remove(&exe);
                },
                Some(format!("{} was removed from the list.", g.name)),
            );
        }
    }
}

enum RowClick {
    Toggle(bool),
    Open,
}

/// A small status pill (dot and label).
fn pill(ui: &mut egui::Ui, color: Color32, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), color);
    let size = galley.size() + Vec2::new(22.0, 4.0);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(r, CornerRadius::same(10), color.gamma_multiply(0.15), Stroke::NONE, StrokeKind::Inside);
    ui.painter().circle_filled(egui::pos2(r.left() + 9.0, r.center().y), 3.0, color);
    ui.painter().galley(egui::pos2(r.left() + 16.0, r.top() + 2.0), galley, color);
}
