//! Settings, laid out like Windows 11 Settings: grouped cards with the
//! setting's name and description on the left and its control on the right.
//! A save bar appears only when something changed.

use std::time::{Duration, Instant};

use eframe::egui::{self, Margin, RichText, Vec2};

use super::Shared;
use super::theme::{self, Tokens, icon};
use crate::config::{Codec, Config, OverlayCorner, RateControl, Resolution, UploadTarget};

/// What the Updates card shows.
#[derive(Clone)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Ready(String),
    Available { version: String, page: String, why: String },
    Failed(String),
}

/// Settings categories, in the order of the list.
#[derive(PartialEq, Clone, Copy)]
pub enum Category {
    General,
    Recording,
    Audio,
    Webcam,
    Hotkeys,
    Detection,
    Storage,
    Sharing,
    About,
}

impl Category {
    const ALL: [(Category, char); 9] = [
        (Category::General, icon::SETTINGS),
        (Category::Recording, icon::VIDEO),
        (Category::Audio, icon::VOLUME),
        (Category::Webcam, icon::RECORD),
        (Category::Hotkeys, icon::KEYBOARD),
        (Category::Detection, icon::GAME),
        (Category::Storage, icon::FOLDER),
        (Category::Sharing, icon::SHARE),
        (Category::About, icon::INFO),
    ];

    fn label(self) -> &'static str {
        match self {
            Category::General => "General",
            Category::Recording => "Recording",
            Category::Audio => "Audio",
            Category::Webcam => "Webcam",
            Category::Hotkeys => "Hotkeys",
            Category::Detection => "Game detection",
            Category::Storage => "Storage",
            Category::Sharing => "Sharing",
            Category::About => "About",
        }
    }
}

pub struct SettingsPage {
    pub update: UpdateState,
    category: Category,
    /// When the draft first differed from the saved settings.
    changed_at: Option<Instant>,
    saved_at: Option<Instant>,
    draft: Config,
    include_text: String,
    exclude_text: String,
    password: String,
    /// Custom uploader headers as typed; saved encrypted.
    custom_headers: String,
    custom_fields: String,
    mics: Vec<(String, String)>,
    encoders: Vec<(Codec, bool)>,
    /// Clips in the folder and their total size, for the Storage section.
    usage: (usize, u64),
    cameras: Vec<(String, String)>,
}

const FPS: [u32; 5] = [30, 60, 120, 144, 240];
const COMBO_W: f32 = theme::CONTROL_COL;

impl SettingsPage {
    pub fn new(cfg: &Config) -> Self {
        Self {
            update: match crate::update::staged() {
                Some(s) => UpdateState::Ready(s.version),
                None => UpdateState::Idle,
            },
            draft: cfg.clone(),
            category: Category::General,
            changed_at: None,
            saved_at: None,
            include_text: cfg.detection.include.join("\n"),
            exclude_text: cfg.detection.exclude.join("\n"),
            password: String::new(),
            custom_headers: crate::sys::secret::unprotect(&cfg.share.custom.headers_protected).unwrap_or_default(),
            custom_fields: cfg.share.custom.fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n"),
            mics: crate::sys::list_microphones(),
            encoders: crate::sys::available_codecs(),
            usage: folder_usage(&cfg.output_dir),
            cameras: crate::sys::list_cameras(),
        }
    }

    /// Starts again from a saved config (after another page changed it).
    pub fn reset(&mut self, cfg: &Config) {
        let mics = std::mem::take(&mut self.mics);
        let encoders = std::mem::take(&mut self.encoders);
        let cameras = std::mem::take(&mut self.cameras);
        let update = self.update.clone();
        let category = self.category;
        *self = Self { mics, encoders, cameras, update, category, ..Self::new(cfg) };
    }

    /// Returns true when the clips folder changed (the list must rescan).
    pub fn show(&mut self, c: Category) {
        self.category = c;
    }

    /// Returns true when the clips folder changed (the list must rescan).
    /// Changes save by themselves a moment after the last edit.
    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &mut Shared) -> bool {
        let t = theme::tokens(ui.ctx());
        let icons = shared.icons;
        ui.horizontal(|ui| {
            ui.label(theme::title("Settings").color(t.text));
            if self.saved_at.is_some_and(|s| s.elapsed() < Duration::from_secs(2)) {
                ui.add_space(8.0);
                if icons {
                    ui.label(theme::glyph(icon::CHECK, 14.0).color(t.success));
                }
                ui.label(theme::caption("Saved").color(t.text2));
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        });
        ui.add_space(16.0);
        let area = ui.available_rect_before_wrap();
        let list_w = 200.0;
        let list = egui::Rect::from_min_size(area.min, Vec2::new(list_w, area.height()));
        let body = egui::Rect::from_min_max(egui::pos2(area.left() + list_w + 24.0, area.top()), area.max);
        let mut lu = ui.new_child(egui::UiBuilder::new().max_rect(list).layout(egui::Layout::top_down(egui::Align::Min)));
        lu.spacing_mut().item_spacing.y = 2.0;
        for (c, glyph) in Category::ALL {
            if theme::nav_item(&mut lu, &t, icons, glyph, c.label(), self.category == c, false).clicked() {
                self.category = c;
            }
        }
        let mut bu = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
        egui::ScrollArea::vertical().id_salt(self.category.label()).auto_shrink([false, false]).show(&mut bu, |ui| {
            ui.set_max_width(ui.available_width().min(1000.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(theme::subtitle(self.category.label()).color(t.text));
            ui.add_space(4.0);
            match self.category {
                Category::General => self.general(ui, &t, icons),
                Category::Recording => self.recording(ui, &t, icons),
                Category::Audio => self.audio(ui, &t, icons),
                Category::Webcam => self.webcam(ui, &t, icons),
                Category::Hotkeys => self.hotkeys(ui, &t, icons),
                Category::Detection => self.detection(ui, &t, icons),
                Category::Storage => self.storage(ui, &t, icons),
                Category::Sharing => self.sharing(ui, &t, icons, shared),
                Category::About => self.updates(ui, &t, icons, shared),
            }
            ui.add_space(32.0);
        });
        ui.allocate_rect(area, egui::Sense::hover());

        // Save a moment after the last change, once no control is held.
        let mut folder_changed = false;
        if self.autosave_dirty(&shared.cfg) {
            let since = *self.changed_at.get_or_insert_with(Instant::now);
            let holding = ui.input(|i| i.pointer.any_down());
            if since.elapsed() > Duration::from_millis(600) && !holding && self.hotkeys_valid() {
                folder_changed = self.save(shared);
                self.changed_at = None;
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(200));
            }
        } else {
            self.changed_at = None;
        }
        folder_changed
    }

    fn hotkeys_valid(&self) -> bool {
        let h = &self.draft.hotkeys;
        [&h.save_focused, &h.save_all, &h.toggle_game, &h.record].iter().filter(|h| !h.trim().is_empty()).all(|h| crate::hotkey::parse(h).is_ok())
    }

    fn recording(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool) {
        let d = &mut self.draft;
        theme::setting(ui, t, false, Some(icon::CLOCK), "Replay length", "How much recent gameplay a saved clip contains", |ui| {
            let label = theme::fmt_short(f64::from(d.replay_seconds));
            theme::slider_row(ui, t, label, &mut d.replay_seconds, 15..=1200, true);
        });
        let encoders = self.encoders.clone();
        theme::setting(ui, t, false, Some(icon::VIDEO), "Video codec", "AV1 makes the smallest files; H.264 plays everywhere", |ui| {
            theme::combo("codec").width(COMBO_W).selected_text(d.video.codec.label()).show_ui(ui, |ui| {
                for (c, ok) in &encoders {
                    let label = if *ok { c.label().to_string() } else { format!("{} (no hardware encoder)", c.label()) };
                    ui.add_enabled_ui(*ok, |ui| ui.selectable_value(&mut d.video.codec, *c, label));
                }
            });
        });
        theme::setting(ui, t, false, None, "Frame rate", "Frames per second in saved clips", |ui| {
            theme::combo("fps").width(COMBO_W).selected_text(format!("{} fps", d.video.fps)).show_ui(ui, |ui| {
                for f in FPS {
                    ui.selectable_value(&mut d.video.fps, f, format!("{f} fps"));
                }
            });
        });
        theme::setting(ui, t, false, None, "Resolution", "Clips are never scaled up", |ui| {
            theme::combo("res").width(COMBO_W).selected_text(d.video.resolution.label()).show_ui(ui, |ui| {
                for r in Resolution::ALL {
                    ui.selectable_value(&mut d.video.resolution, r, r.label());
                }
            });
        });
        let one = theme::fmt_bytes(d.recording_memory(&["(any game)"]));
        let desc = format!("Higher is sharper. About {one} of memory while a game records");
        theme::setting(ui, t, false, None, "Quality", &desc, |ui| {
            let mut kbps = d.video.bitrate_kbps;
            let label = theme::fmt_mbps(kbps);
            if theme::slider_row(ui, t, label, &mut kbps, 500..=150_000, true).changed() {
                // Fine steps for low bitrates, whole Mbit/s above 10.
                let step = if kbps < 10_000 { 100 } else { 1000 };
                d.video.bitrate_kbps = ((kbps + step / 2) / step * step).max(500);
            }
        });
        theme::setting(ui, t, false, None, "Rate control", "Variable gives better quality for the same size", |ui| {
            let label = if d.video.rate_control == RateControl::Vbr { "Variable" } else { "Constant" };
            theme::combo("rc").width(COMBO_W).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.video.rate_control, RateControl::Vbr, "Variable");
                ui.selectable_value(&mut d.video.rate_control, RateControl::Cbr, "Constant");
            });
        });
        theme::setting(ui, t, false, None, "Show the mouse cursor", "Draw the pointer into clips", |ui| {
            theme::toggle(ui, t, &mut d.video.capture_cursor);
        });
        theme::setting(ui, t, false, None, "HDR games", "When Windows HDR is on, convert to normal colors so clips look right on every screen", |ui| {
            theme::toggle(ui, t, &mut d.video.hdr_to_sdr);
        });
    }

    fn storage(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool) {
        let d = &mut self.draft;
        let folder = d.output_dir.display().to_string();
        theme::setting(ui, t, false, Some(icon::FOLDER), "Clips folder", &folder, |ui| {
            if ui.add(egui::Button::new("Choose folder\u{2026}").min_size(Vec2::new(COMBO_W, theme::CONTROL_H))).clicked()
                && let Some(p) = crate::sys::shell::pick_folder()
            {
                d.output_dir = p;
            }
        });
        let (n, bytes) = self.usage;
        let used = format!("The clips folder holds {n} {} ({:.1} GB). Favorites are never deleted.", if n == 1 { "clip" } else { "clips" }, bytes as f64 / 1e9);
        theme::setting(ui, t, false, Some(icon::FOLDER), "Limit the clips folder size", &used, |ui| {
            let label = |gb: u32| if gb == 0 { "No limit".to_string() } else if gb >= 1000 { format!("{} TB", gb / 1000) } else { format!("{gb} GB") };
            theme::combo("max-gb").width(COMBO_W).selected_text(label(d.storage.max_gb)).show_ui(ui, |ui| {
                for gb in [0, 10, 25, 50, 100, 250, 500, 1000] {
                    ui.selectable_value(&mut d.storage.max_gb, gb, label(gb));
                }
            });
        });
        theme::setting(ui, t, false, Some(icon::CLOCK), "Delete old clips", "Clips over the limits are deleted for good, oldest first", |ui| {
            let label = |days: u32| match days {
                0 => "Never".to_string(),
                365 => "After 1 year".to_string(),
                d => format!("After {d} days"),
            };
            theme::combo("max-age").width(COMBO_W).selected_text(label(d.storage.max_age_days)).show_ui(ui, |ui| {
                for days in [0, 7, 14, 30, 60, 90, 180, 365] {
                    ui.selectable_value(&mut d.storage.max_age_days, days, label(days));
                }
            });
        });
    }

    fn audio(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool) {
        let d = &mut self.draft;
        theme::setting(ui, t, false, Some(icon::GAME), "Game audio", "Record only the game's own sound, not chat or music", |ui| {
            theme::toggle(ui, t, &mut d.audio.game_audio);
        });
        theme::setting(ui, t, false, Some(icon::MIC), "Microphone", "Record your voice on its own track", |ui| {
            theme::toggle(ui, t, &mut d.audio.microphone);
        });
        let mics = self.mics.clone();
        theme::setting(ui, t, false, None, "Microphone device", "", |ui| {
            let current = mics.iter().find(|(id, _)| *id == d.audio.microphone_device).map(|(_, n)| n.clone()).unwrap_or_else(|| "System default".into());
            theme::combo("mic").width(300.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.audio.microphone_device, String::new(), "System default");
                for (id, name) in &mics {
                    ui.selectable_value(&mut d.audio.microphone_device, id.clone(), name);
                }
            });
        });
        theme::setting(ui, t, false, Some(icon::VOLUME), "Mixed track", "Add game and mic together as the first track, so Discord and web players hear both", |ui| {
            theme::toggle(ui, t, &mut d.audio.mixed_track);
        });
        for (label, v) in [("Game volume in the mix", &mut d.audio.game_volume), ("Mic volume in the mix", &mut d.audio.microphone_volume)] {
            theme::setting(ui, t, false, None, label, "", |ui| {
                let label = format!("{v}%");
                theme::slider_row(ui, t, label, v, 0..=200, false);
            });
        }
        theme::setting(ui, t, false, None, "Audio quality", "AAC bitrate", |ui| {
            theme::combo("abr").width(COMBO_W).selected_text(format!("{} kbit/s", d.audio.bitrate_kbps)).show_ui(ui, |ui| {
                for b in [96, 128, 160, 192] {
                    ui.selectable_value(&mut d.audio.bitrate_kbps, b, format!("{b} kbit/s"));
                }
            });
        });
    }

    fn webcam(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool) {
        let d = &mut self.draft;
        let desc = if self.cameras.is_empty() { "No camera was found" } else { "Show your camera in a corner of every recording. The camera is on only while a game records" };
        theme::setting(ui, t, false, Some(icon::VIDEO), "Webcam in recordings", desc, |ui| {
            theme::toggle(ui, t, &mut d.webcam.enabled);
        });
        if !d.webcam.enabled {
            return;
        }
        let cams = self.cameras.clone();
        theme::setting(ui, t, false, None, "Camera", "", |ui| {
            let current = cams.iter().find(|(id, _)| *id == d.webcam.device).map(|(_, n)| n.clone()).unwrap_or_else(|| "First camera".into());
            theme::combo("cam").width(300.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.webcam.device, String::new(), "First camera");
                for (id, name) in &cams {
                    ui.selectable_value(&mut d.webcam.device, id.clone(), name);
                }
            });
        });
        theme::setting(ui, t, false, None, "Position", "", |ui| {
            theme::combo("cam-corner").width(COMBO_W).selected_text(d.webcam.corner.label()).show_ui(ui, |ui| {
                for c in OverlayCorner::ALL {
                    ui.selectable_value(&mut d.webcam.corner, c, c.label());
                }
            });
        });
        theme::setting(ui, t, false, None, "Size", "Height of the camera picture", |ui| {
            let label = format!("{}%", d.webcam.size);
            theme::slider_row(ui, t, label, &mut d.webcam.size, 10..=50, false);
        });
    }

    fn hotkeys(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        for (label, desc, value) in [
            ("Save a replay", "Saves the game in front", &mut d.hotkeys.save_focused),
            ("Save all games", "Saves every game being recorded", &mut d.hotkeys.save_all),
            ("Start or stop recording", "Records the whole session of the game in front until you press it again", &mut d.hotkeys.record),
            ("Mark as game", "Starts or stops recording the app in front", &mut d.hotkeys.toggle_game),
        ] {
            theme::setting(ui, t, false, Some(icon::KEYBOARD), label, desc, |ui| {
                let ok = crate::hotkey::parse(value);
                ui.add(egui::TextEdit::singleline(value).desired_width(COMBO_W - 44.0).margin(Margin::symmetric(10, 7)));
                match ok {
                    Ok(_) => {
                        if icons {
                            ui.label(theme::glyph(icon::CHECK, 14.0).color(t.success));
                        }
                    }
                    Err(e) => {
                        ui.label(theme::caption(e).color(t.critical));
                    }
                }
            });
        }
        ui.label(theme::caption("Examples: Alt+F10, Ctrl+Shift+S, F9. If the NVIDIA app uses the same keys, turn off its Instant Replay.").color(t.text3));
    }

    fn detection(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool) {
        let d = &mut self.draft;
        theme::setting(ui, t, false, Some(icon::GAME), "Find games automatically", "Games from launchers, game folders and the Windows Game Bar list", |ui| {
            theme::toggle(ui, t, &mut d.detection.enabled);
        });
        theme::setting(ui, t, false, None, "Fullscreen apps", "Also record other 3D apps that fill a whole screen", |ui| {
            theme::toggle(ui, t, &mut d.detection.fullscreen_apps);
        });
        let (inc, exc) = (&mut self.include_text, &mut self.exclude_text);
        theme::setting_frame(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.columns(2, |cols| {
                cols[0].label(RichText::new("Always record").color(t.text));
                cols[0].label(theme::caption("Program names, one per line").color(t.text2));
                cols[0].add(egui::TextEdit::multiline(inc).desired_rows(3).desired_width(f32::INFINITY).hint_text("game.exe"));
                if cols[0].add(egui::Button::new("Add a program...")).clicked()
                    && let Some(p) = crate::sys::shell::pick_exe()
                    && let Some(name) = p.file_name()
                {
                    if !inc.is_empty() && !inc.ends_with('\n') {
                        inc.push('\n');
                    }
                    inc.push_str(&name.to_string_lossy().to_ascii_lowercase());
                }
                cols[1].label(RichText::new("Never record").color(t.text));
                cols[1].label(theme::caption("Program names, one per line").color(t.text2));
                cols[1].add(egui::TextEdit::multiline(exc).desired_rows(3).desired_width(f32::INFINITY).hint_text("app.exe"));
            });
        });
    }

    fn sharing(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool, shared: &mut Shared) {
        let d = &mut self.draft;
        theme::setting(ui, t, false, Some(icon::SHARE), "Upload to", d.share.target.describe(), |ui| {
            theme::combo("target").width(COMBO_W).selected_text(d.share.target.label()).show_ui(ui, |ui| {
                for u in UploadTarget::ALL {
                    ui.selectable_value(&mut d.share.target, u, u.label());
                }
            });
        });
        match d.share.target {
            UploadTarget::Streamable => {
                theme::setting(ui, t, false, None, "Streamable email", "Used to upload clips to your Streamable account", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.share.streamable_email).desired_width(COMBO_W).margin(Margin::symmetric(10, 7)));
                });
                let saved = !shared.cfg.share.streamable_password_protected.is_empty();
                let hint = if saved { "Saved" } else { "" };
                let mut save_pw = false;
                theme::setting(ui, t, false, None, "Streamable password", "Stored encrypted for your user account, and sent only to Streamable", |ui| {
                    if ui.add_enabled(!self.password.is_empty(), egui::Button::new("Save").min_size(Vec2::new(64.0, theme::CONTROL_H))).clicked() {
                        save_pw = true;
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text(hint).desired_width(COMBO_W - 72.0).margin(Margin::symmetric(10, 7)));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.password.is_empty() {
                        save_pw = true;
                    }
                });
                if save_pw {
                    self.save(shared);
                    shared.toast("Password saved.", false);
                }
            }
            UploadTarget::Catbox => {}
            UploadTarget::Litterbox => {
                theme::setting(ui, t, false, None, "Keep files for", "Litterbox deletes them after this time", |ui| {
                    let label = |h: u32| if h == 1 { "1 hour".to_string() } else { format!("{h} hours") };
                    theme::combo("litter").width(COMBO_W).selected_text(label(d.share.litterbox_hours)).show_ui(ui, |ui| {
                        for h in [1, 12, 24, 72] {
                            ui.selectable_value(&mut d.share.litterbox_hours, h, label(h));
                        }
                    });
                });
            }
            UploadTarget::Custom => {
                let mut import = false;
                theme::setting(ui, t, false, None, "ShareX uploader", "Import a .sxcu file, or fill in the fields below", |ui| {
                    if theme::button(ui, "Import .sxcu...", true).clicked() {
                        import = true;
                    }
                });
                if import
                    && let Some(p) = crate::sys::shell::pick_file("ShareX uploader", "*.sxcu;*.json")
                {
                    match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| crate::share::import_sxcu(&t)) {
                        Ok((up, headers)) => {
                            self.custom_fields = up.fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");
                            d.share.custom = up;
                            self.custom_headers = headers;
                            shared.toast("Uploader imported. Save to use it.", false);
                        }
                        Err(e) => shared.toast(e, true),
                    }
                }
                let c = &mut d.share.custom;
                theme::setting_frame(t).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Grid::new("custom-up").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, label: &str, v: &mut String, hint: &str| {
                            ui.label(RichText::new(label).color(t.text));
                            ui.add(egui::TextEdit::singleline(v).hint_text(hint).desired_width(420.0).margin(Margin::symmetric(10, 6)));
                            ui.end_row();
                        };
                        row(ui, "Upload URL", &mut c.url, "https://example.com/api/upload");
                        row(ui, "File field", &mut c.file_field, "file");
                        row(ui, "Link in reply", &mut c.link, "data.link (empty: the whole reply)");
                        ui.label(RichText::new("Form fields").color(t.text));
                        ui.add(egui::TextEdit::multiline(&mut self.custom_fields).hint_text("name=value, one per line").desired_rows(2).desired_width(420.0));
                        ui.end_row();
                        ui.label(RichText::new("Headers").color(t.text));
                        ui.add(egui::TextEdit::multiline(&mut self.custom_headers).hint_text("Authorization: Bearer ...").desired_rows(2).desired_width(420.0));
                        ui.end_row();
                    });
                    ui.label(theme::caption("Headers often hold an API key, so they are stored encrypted for your user account.").color(t.text3));
                });
            }
        }
        let d = &mut self.draft;
        let target = d.share.target.label();
        theme::setting(ui, t, false, Some(icon::UPLOAD), "Upload every replay", &format!("Send each saved clip to {target} automatically"), |ui| {
            theme::toggle(ui, t, &mut d.share.auto_upload);
        });
        theme::setting(ui, t, false, Some(icon::LINK), "Copy the link after an upload", "", |ui| {
            theme::toggle(ui, t, &mut d.share.copy_link_after_upload);
        });
    }

    fn updates(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool, shared: &mut Shared) {
        theme::setting(ui, t, false, Some(icon::INFO), "Replay", &format!("Version {}  \u{00b7}  Instant replay for Windows games. Free and open source (MIT)", crate::update::CURRENT), |ui| {
            if theme::button(ui, "Report a problem", true).clicked() {
                crate::sys::shell::open_url("https://github.com/Car-Role/replay/issues");
            }
            if ui.link("Website").on_hover_text("car-role.github.io/replay").clicked() {
                crate::sys::shell::open_url("https://car-role.github.io/replay/");
            }
        });
        let d = &mut self.draft;
        theme::setting(ui, t, false, Some(icon::UPDATE), "Automatic updates", "Check once a day and download new versions in the background", |ui| {
            theme::toggle(ui, t, &mut d.updates.auto_check);
        });
        let version = format!("You have version {}", crate::update::CURRENT);
        let state = self.update.clone();
        let (title, desc): (String, String) = match &state {
            UpdateState::Checking => ("Checking for updates...".into(), version.clone()),
            UpdateState::UpToDate => ("You're up to date".into(), version.clone()),
            UpdateState::Ready(v) => (format!("Version {v} is ready to install"), "Replay restarts to finish the update".into()),
            UpdateState::Available { version: v, why, .. } => (format!("Version {v} is available"), why.clone()),
            UpdateState::Failed(e) => ("The update check failed".into(), e.clone()),
            UpdateState::Idle => ("Updates".into(), "Releases are signed; only verified updates are installed".into()),
        };
        let mut check = false;
        let mut restart = false;
        let mut open_page = None;
        theme::setting(ui, t, false, None, &title, &desc, |ui| match &state {
            UpdateState::Checking => {
                ui.spinner();
            }
            UpdateState::Ready(_) => {
                if theme::primary(ui, t, "Restart now", true).clicked() {
                    restart = true;
                }
            }
            UpdateState::Available { page, .. } => {
                if theme::button(ui, "Open download page", true).clicked() {
                    open_page = Some(page.clone());
                }
            }
            _ => {
                if theme::button(ui, "Check for updates", true).clicked() {
                    check = true;
                }
            }
        });
        if let Some(p) = open_page {
            crate::sys::shell::open_url(&p);
        }
        if restart {
            restart_to_update(ui.ctx(), shared);
        }
        if check {
            self.update = UpdateState::Checking;
            shared.spawn_job(ui.ctx(), super::JobKind::Update, "Checking for updates".into(), |p| {
                let r = (|| -> Result<UpdateState, String> {
                    let Some(rel) = crate::update::check()? else { return Ok(UpdateState::UpToDate) };
                    if let Err(why) = crate::update::can_self_update() {
                        return Ok(UpdateState::Available { version: rel.version, page: rel.page, why });
                    }
                    let s = crate::update::download(&rel, &|a, b| p(a as f32 / b.max(1) as f32))?;
                    crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::UpdateStaged);
                    Ok(UpdateState::Ready(s.version))
                })();
                super::JobDone::Update(r.unwrap_or_else(UpdateState::Failed))
            });
        }
    }

    fn general(&mut self, ui: &mut egui::Ui, t: &Tokens, _icons: bool) {
        let d = &mut self.draft;
        theme::setting(ui, t, false, Some(icon::INFO), "Notifications", "Show a Windows notification when a replay is saved", |ui| {
            theme::toggle(ui, t, &mut d.notify);
        });
        theme::setting(ui, t, false, Some(icon::VIDEO), "In-game notice", "A small card over the game confirms each save. It never shows up in clips", |ui| {
            theme::toggle(ui, t, &mut d.overlay.enabled);
        });
        let on = d.overlay.enabled;
        theme::setting(ui, t, false, None, "Notice position", "Corner of the screen for the in-game notice", |ui| {
            if !on {
                ui.disable();
            }
            theme::combo("corner").width(COMBO_W).selected_text(d.overlay.corner.label()).show_ui(ui, |ui| {
                for c in OverlayCorner::ALL {
                    ui.selectable_value(&mut d.overlay.corner, c, c.label());
                }
            });
        });
        theme::setting(ui, t, false, Some(icon::VOLUME), "Sound", "Play a short sound when a replay is saved", |ui| {
            theme::toggle(ui, t, &mut d.sound);
        });
        theme::setting(ui, t, false, Some(icon::POWER), "Start with your computer", "Run the recorder when you sign in", |ui| {
            theme::toggle(ui, t, &mut d.start_with_windows);
        });
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect()
    }

    /// The draft as a config, without touching the password store.
    fn built(&self, current: &Config) -> Config {
        let mut c = self.draft.clone();
        c.detection.include = Self::lines(&self.include_text);
        c.detection.exclude = Self::lines(&self.exclude_text);
        c.share.streamable_password_protected = current.share.streamable_password_protected.clone();
        c.share.custom.fields = self
            .custom_fields
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .filter(|(k, _)| !k.is_empty())
            .collect();
        // Headers are compared as typed; they are encrypted only when saved.
        c.share.custom.headers_protected = current.share.custom.headers_protected.clone();
        c.sanitized()
    }

    /// True when something other than the password changed. The password
    /// saves only with its button, so a half-typed one is never stored.
    fn autosave_dirty(&self, current: &Config) -> bool {
        let saved_headers = crate::sys::secret::unprotect(&current.share.custom.headers_protected).unwrap_or_default();
        self.custom_headers.trim() != saved_headers.trim() || self.built(current) != *current
    }

    fn save(&mut self, shared: &mut Shared) -> bool {
        let mut new = self.built(&shared.cfg);
        if let Err(e) = [&new.hotkeys.save_focused, &new.hotkeys.save_all, &new.hotkeys.toggle_game, &new.hotkeys.record]
            .iter()
            .filter(|h| !h.trim().is_empty())
            .try_for_each(|h| crate::hotkey::parse(h).map(|_| ()))
        {
            shared.toast(format!("Fix the hotkey first: {e}"), true);
            return false;
        }
        new.share.custom.headers_protected =
            if self.custom_headers.trim().is_empty() { String::new() } else { crate::sys::secret::protect(self.custom_headers.trim()) };
        if !self.password.is_empty() {
            new.share.streamable_password_protected = crate::sys::secret::protect(&self.password);
            if new.share.streamable_password_protected.is_empty() {
                shared.toast("The password could not be stored securely.", true);
                return false;
            }
        }
        if new.start_with_windows != shared.cfg.start_with_windows && !crate::sys::shell::set_autostart(new.start_with_windows) {
            shared.toast("Could not change the startup setting.", true);
        }
        match new.save() {
            Ok(()) => {
                let folder_changed = new.output_dir != shared.cfg.output_dir;
                shared.cfg = new.clone();
                self.draft = new;
                self.password.clear();
                crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::Reload);
                self.saved_at = Some(Instant::now());
                folder_changed
            }
            Err(e) => {
                shared.toast(format!("Could not save settings: {e}"), true);
                false
            }
        }
    }
}

fn folder_usage(dir: &std::path::Path) -> (usize, u64) {
    let clips = crate::library::clips_on_disk(dir);
    (clips.len(), clips.iter().map(|c| c.size).sum())
}

/// Installs the downloaded update. The recorder restarts itself when it
/// runs; otherwise this window does it. Either way this window closes.
fn restart_to_update(ctx: &egui::Context, shared: &mut Shared) {
    if crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::InstallUpdate) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    match crate::update::staged() {
        Some(s) => match crate::update::apply_and_restart(&s, &["--library"]) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => shared.toast(format!("Update failed: {e}"), true),
        },
        None => shared.toast("The downloaded update is missing. Check again.", true),
    }
}

