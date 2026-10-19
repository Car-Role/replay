//! Settings, laid out like Windows 11 Settings: grouped cards with the
//! setting's name and description on the left and its control on the right.
//! A save bar appears only when something changed.

use eframe::egui::{self, CornerRadius, Margin, RichText, Stroke};

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

pub struct SettingsPage {
    pub update: UpdateState,
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
const COMBO_W: f32 = 220.0;

impl SettingsPage {
    pub fn new(cfg: &Config) -> Self {
        Self {
            update: match crate::update::staged() {
                Some(s) => UpdateState::Ready(s.version),
                None => UpdateState::Idle,
            },
            draft: cfg.clone(),
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
        *self = Self { mics, encoders, cameras, update, ..Self::new(cfg) };
    }

    /// Returns true when the clips folder changed (the list must rescan).
    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &mut Shared) -> bool {
        let t = theme::tokens(ui.ctx());
        let icons = shared.icons;
        let mut folder_changed = false;
        let dirty = self.dirty(&shared.cfg);

        ui.label(theme::title("Settings").color(t.text));
        ui.add_space(8.0);
        let bar_h = if dirty { 64.0 } else { 0.0 };
        let scroll_h = (ui.available_height() - bar_h).max(100.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(scroll_h).show(ui, |ui| {
            ui.set_max_width(980.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            self.recording(ui, &t, icons);
            self.storage(ui, &t, icons);
            self.audio(ui, &t, icons);
            self.webcam(ui, &t, icons);
            self.hotkeys(ui, &t, icons);
            self.detection(ui, &t, icons);
            self.sharing(ui, &t, icons, shared);
            self.updates(ui, &t, icons, shared);
            self.general(ui, &t, icons);
            ui.add_space(24.0);
        });

        if dirty {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.card_stroke))
                .corner_radius(CornerRadius::same(theme::R_CARD))
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_max_width(980.0 - 32.0);
                    ui.horizontal(|ui| {
                        if icons {
                            ui.label(theme::glyph(icon::INFO, 16.0).color(t.accent));
                        }
                        ui.label(RichText::new("You have unsaved changes.").color(t.text));
                        ui.label(theme::caption("Video and audio changes restart recording, so the replay buffers start again.").color(t.text2));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::button(ui, "Discard", true).clicked() {
                                *self = Self { mics: std::mem::take(&mut self.mics), encoders: std::mem::take(&mut self.encoders), ..Self::new(&shared.cfg) };
                            }
                            if theme::primary(ui, &t, theme::label_with_icon(icons, icon::SAVE, "Save"), true).clicked() {
                                folder_changed = self.save(shared);
                            }
                        });
                    });
                });
        }
        folder_changed
    }

    fn recording(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "Recording");
        theme::setting(ui, t, icons, Some(icon::CLOCK), "Replay length", "How much recent gameplay a saved clip contains", |ui| {
            ui.label(RichText::new(theme::fmt_short(f64::from(d.replay_seconds))).color(t.text2));
            ui.spacing_mut().slider_width = 220.0;
            ui.add(egui::Slider::new(&mut d.replay_seconds, 15..=1200).logarithmic(true).show_value(false));
        });
        let folder = d.output_dir.display().to_string();
        theme::setting(ui, t, icons, Some(icon::FOLDER), "Clips folder", &folder, |ui| {
            if theme::button(ui, "Change...", true).clicked()
                && let Some(p) = crate::sys::shell::pick_folder()
            {
                d.output_dir = p;
            }
        });
        let encoders = self.encoders.clone();
        theme::setting(ui, t, icons, Some(icon::VIDEO), "Video codec", "AV1 makes the smallest files; H.264 plays everywhere", |ui| {
            egui::ComboBox::from_id_salt("codec").width(COMBO_W).selected_text(d.video.codec.label()).show_ui(ui, |ui| {
                for (c, ok) in &encoders {
                    let label = if *ok { c.label().to_string() } else { format!("{} (no hardware encoder)", c.label()) };
                    ui.add_enabled_ui(*ok, |ui| ui.selectable_value(&mut d.video.codec, *c, label));
                }
            });
        });
        theme::setting(ui, t, icons, None, "Frame rate", "Frames per second in saved clips", |ui| {
            egui::ComboBox::from_id_salt("fps").width(COMBO_W).selected_text(format!("{} fps", d.video.fps)).show_ui(ui, |ui| {
                for f in FPS {
                    ui.selectable_value(&mut d.video.fps, f, format!("{f} fps"));
                }
            });
        });
        theme::setting(ui, t, icons, None, "Resolution", "Clips are never scaled up", |ui| {
            egui::ComboBox::from_id_salt("res").width(COMBO_W).selected_text(d.video.resolution.label()).show_ui(ui, |ui| {
                for r in Resolution::ALL {
                    ui.selectable_value(&mut d.video.resolution, r, r.label());
                }
            });
        });
        let one = theme::fmt_bytes(d.recording_memory(&["(any game)"]));
        let desc = format!("Higher is sharper. While one game records, Replay uses about {one} of memory. See the Games page for every game");
        theme::setting(ui, t, icons, None, "Quality", &desc, |ui| {
            ui.label(RichText::new(theme::fmt_mbps(d.video.bitrate_kbps)).color(t.text2));
            ui.spacing_mut().slider_width = 220.0;
            let mut kbps = d.video.bitrate_kbps;
            if ui.add(egui::Slider::new(&mut kbps, 500..=150_000).logarithmic(true).show_value(false)).changed() {
                // Fine steps for low bitrates, whole Mbit/s above 10.
                let step = if kbps < 10_000 { 100 } else { 1000 };
                d.video.bitrate_kbps = ((kbps + step / 2) / step * step).max(500);
            }
        });
        theme::setting(ui, t, icons, None, "Rate control", "Variable gives better quality for the same size", |ui| {
            let label = if d.video.rate_control == RateControl::Vbr { "Variable" } else { "Constant" };
            egui::ComboBox::from_id_salt("rc").width(COMBO_W).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.video.rate_control, RateControl::Vbr, "Variable");
                ui.selectable_value(&mut d.video.rate_control, RateControl::Cbr, "Constant");
            });
        });
        theme::setting(ui, t, icons, None, "Show the mouse cursor", "Draw the pointer into clips", |ui| {
            theme::toggle(ui, t, &mut d.video.capture_cursor);
        });
        theme::setting(ui, t, icons, None, "HDR games", "When Windows HDR is on, convert to normal colors so clips look right on every screen", |ui| {
            theme::toggle(ui, t, &mut d.video.hdr_to_sdr);
        });
    }

    fn storage(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "Storage");
        let (n, bytes) = self.usage;
        let used = format!("The clips folder holds {n} {} ({:.1} GB). Favorites are never deleted.", if n == 1 { "clip" } else { "clips" }, bytes as f64 / 1e9);
        theme::setting(ui, t, icons, Some(icon::FOLDER), "Limit the clips folder size", &used, |ui| {
            let label = |gb: u32| if gb == 0 { "No limit".to_string() } else if gb >= 1000 { format!("{} TB", gb / 1000) } else { format!("{gb} GB") };
            egui::ComboBox::from_id_salt("max-gb").width(COMBO_W).selected_text(label(d.storage.max_gb)).show_ui(ui, |ui| {
                for gb in [0, 10, 25, 50, 100, 250, 500, 1000] {
                    ui.selectable_value(&mut d.storage.max_gb, gb, label(gb));
                }
            });
        });
        theme::setting(ui, t, icons, Some(icon::CLOCK), "Delete old clips", "Clips over the limits are deleted for good, oldest first", |ui| {
            let label = |days: u32| match days {
                0 => "Never".to_string(),
                365 => "After 1 year".to_string(),
                d => format!("After {d} days"),
            };
            egui::ComboBox::from_id_salt("max-age").width(COMBO_W).selected_text(label(d.storage.max_age_days)).show_ui(ui, |ui| {
                for days in [0, 7, 14, 30, 60, 90, 180, 365] {
                    ui.selectable_value(&mut d.storage.max_age_days, days, label(days));
                }
            });
        });
    }

    fn audio(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "Audio");
        theme::setting(ui, t, icons, Some(icon::GAME), "Game audio", "Record only the game's own sound, not chat or music", |ui| {
            theme::toggle(ui, t, &mut d.audio.game_audio);
        });
        theme::setting(ui, t, icons, Some(icon::MIC), "Microphone", "Record your voice on its own track", |ui| {
            theme::toggle(ui, t, &mut d.audio.microphone);
        });
        let mics = self.mics.clone();
        theme::setting(ui, t, icons, None, "Microphone device", "", |ui| {
            let current = mics.iter().find(|(id, _)| *id == d.audio.microphone_device).map(|(_, n)| n.clone()).unwrap_or_else(|| "System default".into());
            egui::ComboBox::from_id_salt("mic").width(300.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.audio.microphone_device, String::new(), "System default");
                for (id, name) in &mics {
                    ui.selectable_value(&mut d.audio.microphone_device, id.clone(), name);
                }
            });
        });
        theme::setting(ui, t, icons, Some(icon::VOLUME), "Mixed track", "Add game and mic together as the first track, so Discord and web players hear both", |ui| {
            theme::toggle(ui, t, &mut d.audio.mixed_track);
        });
        for (label, v) in [("Game volume in the mix", &mut d.audio.game_volume), ("Mic volume in the mix", &mut d.audio.microphone_volume)] {
            theme::setting(ui, t, icons, None, label, "", |ui| {
                ui.label(RichText::new(format!("{v}%")).color(t.text2));
                ui.spacing_mut().slider_width = 220.0;
                ui.add(egui::Slider::new(v, 0..=200).show_value(false));
            });
        }
        theme::setting(ui, t, icons, None, "Audio quality", "AAC bitrate", |ui| {
            egui::ComboBox::from_id_salt("abr").width(COMBO_W).selected_text(format!("{} kbit/s", d.audio.bitrate_kbps)).show_ui(ui, |ui| {
                for b in [96, 128, 160, 192] {
                    ui.selectable_value(&mut d.audio.bitrate_kbps, b, format!("{b} kbit/s"));
                }
            });
        });
    }

    fn webcam(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "Webcam");
        let desc = if self.cameras.is_empty() { "No camera was found" } else { "Show your camera in a corner of every recording. The camera is on only while a game records" };
        theme::setting(ui, t, icons, Some(icon::VIDEO), "Webcam in recordings", desc, |ui| {
            theme::toggle(ui, t, &mut d.webcam.enabled);
        });
        if !d.webcam.enabled {
            return;
        }
        let cams = self.cameras.clone();
        theme::setting(ui, t, icons, None, "Camera", "", |ui| {
            let current = cams.iter().find(|(id, _)| *id == d.webcam.device).map(|(_, n)| n.clone()).unwrap_or_else(|| "First camera".into());
            egui::ComboBox::from_id_salt("cam").width(300.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.webcam.device, String::new(), "First camera");
                for (id, name) in &cams {
                    ui.selectable_value(&mut d.webcam.device, id.clone(), name);
                }
            });
        });
        theme::setting(ui, t, icons, None, "Position", "", |ui| {
            egui::ComboBox::from_id_salt("cam-corner").width(COMBO_W).selected_text(d.webcam.corner.label()).show_ui(ui, |ui| {
                for c in OverlayCorner::ALL {
                    ui.selectable_value(&mut d.webcam.corner, c, c.label());
                }
            });
        });
        theme::setting(ui, t, icons, None, "Size", "Height of the camera picture", |ui| {
            ui.label(RichText::new(format!("{}%", d.webcam.size)).color(t.text2));
            ui.spacing_mut().slider_width = 220.0;
            ui.add(egui::Slider::new(&mut d.webcam.size, 10..=50).show_value(false));
        });
    }

    fn hotkeys(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "Hotkeys");
        for (label, desc, value) in [
            ("Save a replay", "Saves the game in front", &mut d.hotkeys.save_focused),
            ("Save all games", "Saves every game being recorded", &mut d.hotkeys.save_all),
            ("Start or stop recording", "Records the whole session of the game in front until you press it again", &mut d.hotkeys.record),
            ("Mark as game", "Starts or stops recording the app in front", &mut d.hotkeys.toggle_game),
        ] {
            theme::setting(ui, t, icons, Some(icon::KEYBOARD), label, desc, |ui| {
                let ok = crate::hotkey::parse(value);
                ui.add(egui::TextEdit::singleline(value).desired_width(180.0).margin(Margin::symmetric(10, 7)));
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

    fn detection(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "Game detection");
        theme::setting(ui, t, icons, Some(icon::GAME), "Find games automatically", "Games from launchers, game folders and the Windows Game Bar list", |ui| {
            theme::toggle(ui, t, &mut d.detection.enabled);
        });
        theme::setting(ui, t, icons, None, "Fullscreen apps", "Also record other 3D apps that fill a whole screen", |ui| {
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

    fn sharing(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, shared: &mut Shared) {
        let d = &mut self.draft;
        theme::section(ui, t, "Sharing");
        theme::setting(ui, t, icons, Some(icon::SHARE), "Upload to", d.share.target.describe(), |ui| {
            egui::ComboBox::from_id_salt("target").width(COMBO_W).selected_text(d.share.target.label()).show_ui(ui, |ui| {
                for u in UploadTarget::ALL {
                    ui.selectable_value(&mut d.share.target, u, u.label());
                }
            });
        });
        match d.share.target {
            UploadTarget::Streamable => {
                theme::setting(ui, t, icons, None, "Streamable email", "Used to upload clips to your Streamable account", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.share.streamable_email).desired_width(260.0).margin(Margin::symmetric(10, 7)));
                });
                let saved = !shared.cfg.share.streamable_password_protected.is_empty();
                let hint = if saved { "Saved. Type to replace it" } else { "" };
                theme::setting(ui, t, icons, None, "Streamable password", "Stored encrypted for your user account, and sent only to Streamable", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text(hint).desired_width(260.0).margin(Margin::symmetric(10, 7)));
                });
            }
            UploadTarget::Catbox => {}
            UploadTarget::Litterbox => {
                theme::setting(ui, t, icons, None, "Keep files for", "Litterbox deletes them after this time", |ui| {
                    let label = |h: u32| if h == 1 { "1 hour".to_string() } else { format!("{h} hours") };
                    egui::ComboBox::from_id_salt("litter").width(COMBO_W).selected_text(label(d.share.litterbox_hours)).show_ui(ui, |ui| {
                        for h in [1, 12, 24, 72] {
                            ui.selectable_value(&mut d.share.litterbox_hours, h, label(h));
                        }
                    });
                });
            }
            UploadTarget::Custom => {
                let mut import = false;
                theme::setting(ui, t, icons, None, "ShareX uploader", "Import a .sxcu file, or fill in the fields below", |ui| {
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
        theme::setting(ui, t, icons, Some(icon::UPLOAD), "Upload every replay", &format!("Send each saved clip to {target} automatically"), |ui| {
            theme::toggle(ui, t, &mut d.share.auto_upload);
        });
        theme::setting(ui, t, icons, Some(icon::LINK), "Copy the link after an upload", "", |ui| {
            theme::toggle(ui, t, &mut d.share.copy_link_after_upload);
        });
    }

    fn updates(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, shared: &mut Shared) {
        theme::section(ui, t, "Updates");
        let d = &mut self.draft;
        theme::setting(ui, t, icons, Some(icon::UPDATE), "Automatic updates", "Check once a day and download new versions in the background", |ui| {
            theme::toggle(ui, t, &mut d.updates.auto_check);
        });
        let version = format!("Replay {} for {}", crate::update::CURRENT, crate::sys::PLATFORM);
        let state = self.update.clone();
        let (title, desc): (String, String) = match &state {
            UpdateState::Checking => ("Checking for updates...".into(), version.clone()),
            UpdateState::UpToDate => ("You're up to date".into(), version.clone()),
            UpdateState::Ready(v) => (format!("Version {v} is ready to install"), "Replay restarts to finish the update".into()),
            UpdateState::Available { version: v, why, .. } => (format!("Version {v} is available"), why.clone()),
            UpdateState::Failed(e) => ("The update check failed".into(), e.clone()),
            UpdateState::Idle => (version.clone(), "Releases are signed; only verified updates are installed".into()),
        };
        let mut check = false;
        let mut restart = false;
        let mut open_page = None;
        theme::setting(ui, t, icons, None, &title, &desc, |ui| match &state {
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

    fn general(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let d = &mut self.draft;
        theme::section(ui, t, "General");
        theme::setting(ui, t, icons, Some(icon::INFO), "Notifications", "Show a Windows notification when a replay is saved", |ui| {
            theme::toggle(ui, t, &mut d.notify);
        });
        theme::setting(ui, t, icons, Some(icon::VIDEO), "In-game notice", "Show a small card over the game when a replay is saved. It never shows up in clips", |ui| {
            theme::toggle(ui, t, &mut d.overlay.enabled);
        });
        theme::setting(ui, t, icons, None, "Notice position", "", |ui| {
            egui::ComboBox::from_id_salt("corner").width(COMBO_W).selected_text(d.overlay.corner.label()).show_ui(ui, |ui| {
                for c in OverlayCorner::ALL {
                    ui.selectable_value(&mut d.overlay.corner, c, c.label());
                }
            });
        });
        theme::setting(ui, t, icons, Some(icon::VOLUME), "Sound", "Play a short sound when a replay is saved", |ui| {
            theme::toggle(ui, t, &mut d.sound);
        });
        theme::setting(ui, t, icons, Some(icon::POWER), "Start with your computer", "Run the recorder when you sign in", |ui| {
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

    fn dirty(&self, current: &Config) -> bool {
        let saved_headers = crate::sys::secret::unprotect(&current.share.custom.headers_protected).unwrap_or_default();
        !self.password.is_empty() || self.custom_headers != saved_headers || self.built(current) != *current
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
                shared.toast("Settings saved.", false);
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

