//! The Games page: every game Replay has found, whether it is recording
//! right now, and a switch to turn recording on or off for each one.

use eframe::egui::{self, Color32, CornerRadius, RichText, Sense, Stroke, Vec2};

use super::LibraryApp;
use super::theme::{self, Tokens, icon};
use crate::config::{GameSettings, Resolution};
use crate::games::{self, GameDb, GameRecord};

enum RowAction {
    Toggle { exe: String, name: String, on: bool, by_user: bool },
    Forget { exe: String, name: String },
    Expand(String),
    Set { exe: String, name: String, settings: GameSettings },
}

const FPS_CHOICES: [u32; 5] = [30, 60, 120, 144, 240];
const MBPS_CHOICES: [u32; 9] = [10, 15, 20, 25, 35, 50, 75, 100, 150];
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
    /// Saves a detection change right away and tells the recorder.
    fn apply_detection(&mut self, change: impl FnOnce(&mut crate::config::Config), message: String) {
        let mut cfg = self.shared.cfg.clone();
        change(&mut cfg);
        let cfg = cfg.sanitized();
        match cfg.save() {
            Ok(()) => {
                self.shared.cfg = cfg;
                self.settings.reset(&self.shared.cfg);
                crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::Reload);
                self.shared.toast(message, false);
            }
            Err(e) => self.shared.toast(format!("Could not save: {e}"), true),
        }
    }

    pub(super) fn games_page(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let icons = self.shared.icons;
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
                    self.apply_detection(move |c| games::set_enabled(c, &e, true, true), format!("{pretty} will be recorded."));
                }
            });
        });
        ui.label(
            RichText::new("Replay finds games by itself. Turn off any game you don't want recorded, or add one it missed.")
                .color(t.text2),
        );
        ui.add_space(12.0);

        // Games on the lists that were never seen running still get a row.
        let mut list: Vec<GameRecord> = self.games.games.clone();
        for exe in self.shared.cfg.detection.include.iter().chain(self.shared.cfg.detection.exclude.iter()) {
            if !list.iter().any(|g| &g.exe == exe) {
                let how = if self.shared.cfg.detection.include.contains(exe) { "Added by you" } else { "Turned off by you" };
                list.push(GameRecord {
                    exe: exe.clone(),
                    name: exe.trim_end_matches(".exe").to_string(),
                    path: String::new(),
                    how: how.into(),
                    first_seen: 0,
                    last_seen: 0,
                    recording: false,
                    capturing: false,
                });
            }
        }
        list.sort_by(|a, b| b.recording.cmp(&a.recording).then(b.last_seen.cmp(&a.last_seen)).then(a.name.cmp(&b.name)));

        if list.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                if icons {
                    ui.label(theme::glyph(icon::GAME, 40.0).color(t.text3));
                }
                ui.add_space(8.0);
                ui.label(theme::subtitle("No games found yet").color(t.text));
                ui.label(RichText::new("Start a game and it shows up here. You can also add one yourself.").color(t.text2));
            });
            return;
        }

        let recording: Vec<&GameRecord> = list.iter().filter(|g| g.recording).collect();
        let mut action: Option<RowAction> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(980.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            if !recording.is_empty() {
                theme::section(ui, t, "Recording now");
                for g in &recording {
                    self.game_row(ui, t, g, &mut action);
                }
            }
            let others: Vec<&GameRecord> = list.iter().filter(|g| !g.recording).collect();
            if !others.is_empty() {
                theme::section(ui, t, if recording.is_empty() { "All games" } else { "Other games" });
                for g in others {
                    self.game_row(ui, t, g, &mut action);
                }
            }
            ui.add_space(24.0);
        });

        match action {
            Some(RowAction::Toggle { exe, name, on, by_user }) => {
                let msg = if on { format!("{name} will be recorded.") } else { format!("{name} will not be recorded.") };
                self.apply_detection(move |c| games::set_enabled(c, &exe, on, by_user), msg);
            }
            Some(RowAction::Forget { exe, name }) => self.forget_game(exe, name),
            Some(RowAction::Expand(exe)) => {
                self.game_open = if self.game_open.as_deref() == Some(exe.as_str()) { None } else { Some(exe) };
            }
            Some(RowAction::Set { exe, name, settings }) => {
                let msg = if self.games.games.iter().any(|g| g.exe == exe && g.recording) {
                    format!("{name}: saved. Recording restarts with the new settings.")
                } else {
                    format!("{name}: saved.")
                };
                self.apply_detection(
                    move |c| {
                        if settings.is_empty() {
                            c.games.remove(&exe);
                        } else {
                            c.games.insert(exe, settings);
                        }
                    },
                    msg,
                );
            }
            None => {}
        }
    }

    fn forget_game(&mut self, exe: String, name: String) {
        let mut db = GameDb::load();
        db.forget(&exe);
        db.save();
        self.games = db;
        self.apply_detection(
            move |c| {
                c.detection.include.retain(|e| e != &exe);
                c.detection.exclude.retain(|e| e != &exe);
                c.games.remove(&exe);
            },
            format!("{name} was removed from the list."),
        );
    }

    fn game_row(&self, ui: &mut egui::Ui, t: &Tokens, g: &GameRecord, action: &mut Option<RowAction>) {
        let icons = self.shared.icons;
        let enabled = games::is_enabled(&self.shared.cfg.detection, &g.exe);
        let custom = self.shared.cfg.games.get(&g.exe).cloned().unwrap_or_default();
        let open = self.game_open.as_deref() == Some(g.exe.as_str());
        theme::setting_frame(t).show(ui, |ui| {
            ui.set_min_height(44.0);
            ui.horizontal(|ui| {
                // Letter tile in the accent tint, like an app avatar.
                let (r, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
                let tint = if enabled { t.accent.gamma_multiply(0.25) } else { t.control };
                ui.painter().rect_filled(r, CornerRadius::same(theme::R_CONTROL), tint);
                let letter = g.name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    letter,
                    egui::FontId::new(16.0, egui::FontFamily::Name(theme::SEMIBOLD.into())),
                    if enabled { t.accent } else { t.text3 },
                );
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&g.name).color(if enabled { t.text } else { t.text2 }));
                        if g.recording {
                            pill(ui, t.success, "Recording");
                        } else if !enabled {
                            pill(ui, t.text3, "Off");
                        }
                        if !custom.is_empty() {
                            pill(ui, t.accent, "Own settings");
                        }
                    });
                    let mut parts = vec![g.exe.clone(), g.how.clone()];
                    if g.last_seen > 0 {
                        parts.push(format!("last played {}", when(g.last_seen)));
                    }
                    ui.label(theme::caption(parts.join("  ·  ")).color(t.text2));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let forget_btn = theme::subtle(ui, theme::label_with_icon(icons, icon::DELETE, if icons { "" } else { "Forget" }));
                    if forget_btn.on_hover_text("Remove from this list").clicked() {
                        *action = Some(RowAction::Forget { exe: g.exe.clone(), name: g.name.clone() });
                    }
                    let gear = theme::subtle(ui, theme::label_with_icon(icons, icon::SETTINGS, if icons { "" } else { "Settings" }));
                    if gear.on_hover_text("Quality settings for this game").clicked() {
                        *action = Some(RowAction::Expand(g.exe.clone()));
                    }
                    ui.add_space(8.0);
                    let mut on = enabled;
                    if theme::toggle(ui, t, &mut on).on_hover_text(if enabled { "Recording is on" } else { "Recording is off" }).changed() {
                        *action = Some(RowAction::Toggle { exe: g.exe.clone(), name: g.name.clone(), on, by_user: g.how == "Added by you" });
                    }
                    ui.label(theme::caption(if enabled { "Record" } else { "Off" }).color(t.text2));
                });
            });
            if open {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                if let Some(settings) = self.game_settings(ui, t, &g.exe, &custom) {
                    *action = Some(RowAction::Set { exe: g.exe.clone(), name: g.name.clone(), settings });
                }
            }
        });
    }

    /// The per-game settings panel. Returns new settings when one changed.
    fn game_settings(&self, ui: &mut egui::Ui, t: &Tokens, exe: &str, current: &GameSettings) -> Option<GameSettings> {
        let general = &self.shared.cfg;
        let mut s = current.clone();
        ui.label(theme::caption("Settings for this game only. \"Same as general\" follows Settings > Recording.").color(t.text2));
        ui.add_space(4.0);
        egui::Grid::new(("game-settings", exe)).num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("Frame rate").color(t.text));
            let label = |v: Option<u32>| v.map(|f| format!("{f} fps")).unwrap_or_else(|| format!("Same as general ({} fps)", general.video.fps));
            egui::ComboBox::from_id_salt(("g-fps", exe)).width(240.0).selected_text(label(s.fps)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.fps, None, label(None));
                for f in FPS_CHOICES {
                    ui.selectable_value(&mut s.fps, Some(f), label(Some(f)));
                }
            });
            ui.end_row();

            ui.label(RichText::new("Resolution").color(t.text));
            let label = |v: Option<Resolution>| match v {
                Some(r) => r.label().to_string(),
                None => format!("Same as general ({})", general.video.resolution.label()),
            };
            egui::ComboBox::from_id_salt(("g-res", exe)).width(240.0).selected_text(label(s.resolution)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.resolution, None, label(None));
                for r in Resolution::ALL {
                    ui.selectable_value(&mut s.resolution, Some(r), label(Some(r)));
                }
            });
            ui.end_row();

            ui.label(RichText::new("Quality").color(t.text));
            let label = |v: Option<u32>| match v {
                Some(k) => format!("{} Mbit/s", k / 1000),
                None => format!("Same as general ({} Mbit/s)", general.video.bitrate_kbps / 1000),
            };
            egui::ComboBox::from_id_salt(("g-br", exe)).width(240.0).selected_text(label(s.bitrate_kbps)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.bitrate_kbps, None, label(None));
                for m in MBPS_CHOICES {
                    ui.selectable_value(&mut s.bitrate_kbps, Some(m * 1000), label(Some(m * 1000)));
                }
            });
            ui.end_row();

            ui.label(RichText::new("Replay length").color(t.text));
            let label = |v: Option<u32>| match v {
                Some(secs) => theme::fmt_short(f64::from(secs)),
                None => format!("Same as general ({})", theme::fmt_short(f64::from(general.replay_seconds))),
            };
            egui::ComboBox::from_id_salt(("g-len", exe)).width(240.0).selected_text(label(s.replay_seconds)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.replay_seconds, None, label(None));
                for secs in LENGTH_CHOICES {
                    ui.selectable_value(&mut s.replay_seconds, Some(secs), label(Some(secs)));
                }
            });
            ui.end_row();
        });
        if !current.is_empty() {
            ui.add_space(4.0);
            if theme::subtle(ui, theme::caption("Use the general settings")).clicked() {
                s = GameSettings::default();
            }
        }
        (s != *current).then_some(s)
    }
}

/// A small status pill (dot and label).
fn pill(ui: &mut egui::Ui, color: Color32, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(12.0), color);
    let size = galley.size() + Vec2::new(22.0, 4.0);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(r, CornerRadius::same(10), color.gamma_multiply(0.15), Stroke::NONE, egui::StrokeKind::Inside);
    ui.painter().circle_filled(egui::pos2(r.left() + 9.0, r.center().y), 3.0, color);
    ui.painter().galley(egui::pos2(r.left() + 16.0, r.top() + 2.0), galley, color);
}
