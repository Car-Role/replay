//! Settings page. Saving writes the config file and tells the recorder to
//! reload it.

use eframe::egui::{self, Color32, RichText};

use super::Shared;
use crate::config::{Codec, Config, RateControl, Resolution};

/// What the Updates section shows.
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
    mics: Vec<(String, String)>,
    encoders: Vec<(Codec, bool)>,
}

const FPS: [u32; 5] = [30, 60, 120, 144, 240];

impl SettingsPage {
    pub fn new(cfg: &Config) -> Self {
        let encoders = crate::sys::available_codecs();
        Self {
            update: match crate::update::staged() {
                Some(s) => UpdateState::Ready(s.version),
                None => UpdateState::Idle,
            },
            draft: cfg.clone(),
            include_text: cfg.detection.include.join("\n"),
            exclude_text: cfg.detection.exclude.join("\n"),
            password: String::new(),
            mics: crate::sys::list_microphones(),
            encoders,
        }
    }

    fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
        ui.add_space(10.0);
        ui.label(RichText::new(title).strong().size(16.0));
        ui.separator();
        egui::Grid::new(title).num_columns(2).spacing([24.0, 8.0]).show(ui, body);
    }

    /// Returns true when the clips folder changed (the list must rescan).
    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &mut Shared) -> bool {
        let mut folder_changed = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            let d = &mut self.draft;

            Self::section(ui, "Recording", |ui| {
                ui.label("Replay length");
                ui.add(egui::Slider::new(&mut d.replay_seconds, 15..=1200).suffix(" s").logarithmic(true));
                ui.end_row();

                ui.label("Clips folder");
                ui.horizontal(|ui| {
                    let mut text = d.output_dir.to_string_lossy().into_owned();
                    if ui.add(egui::TextEdit::singleline(&mut text).desired_width(360.0)).changed() {
                        d.output_dir = text.into();
                    }
                    if ui.button("Browse...").clicked()
                        && let Some(p) = crate::sys::shell::pick_folder()
                    {
                        d.output_dir = p;
                    }
                });
                ui.end_row();

                ui.label("Video codec");
                egui::ComboBox::from_id_salt("codec").selected_text(d.video.codec.label()).show_ui(ui, |ui| {
                    for (c, ok) in &self.encoders {
                        let label = if *ok { c.label().to_string() } else { format!("{} (no hardware encoder)", c.label()) };
                        ui.add_enabled_ui(*ok, |ui| ui.selectable_value(&mut d.video.codec, *c, label));
                    }
                });
                ui.end_row();

                ui.label("Frame rate");
                egui::ComboBox::from_id_salt("fps").selected_text(format!("{} fps", d.video.fps)).show_ui(ui, |ui| {
                    for f in FPS {
                        ui.selectable_value(&mut d.video.fps, f, format!("{f} fps"));
                    }
                });
                ui.end_row();

                ui.label("Resolution");
                egui::ComboBox::from_id_salt("res").selected_text(d.video.resolution.label()).show_ui(ui, |ui| {
                    for r in Resolution::ALL {
                        ui.selectable_value(&mut d.video.resolution, r, r.label());
                    }
                });
                ui.end_row();

                ui.label("Bitrate");
                let mut mbps = d.video.bitrate_kbps / 1000;
                if ui.add(egui::Slider::new(&mut mbps, 5..=150).suffix(" Mbit/s")).changed() {
                    d.video.bitrate_kbps = mbps * 1000;
                }
                ui.end_row();

                ui.label("Rate control");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut d.video.rate_control, RateControl::Vbr, "Variable (better quality)");
                    ui.radio_value(&mut d.video.rate_control, RateControl::Cbr, "Constant");
                });
                ui.end_row();

                ui.label("Mouse cursor");
                ui.checkbox(&mut d.video.capture_cursor, "Show the cursor in clips");
                ui.end_row();

                let mb = (u64::from(d.video.peak_kbps() + 2 * d.audio.bitrate_kbps) * 1000 / 8) * u64::from(d.replay_seconds + 2) / 1_000_000;
                ui.label("");
                ui.label(RichText::new(format!("Uses up to about {mb} MB of RAM for each game being recorded.")).weak());
                ui.end_row();
            });

            Self::section(ui, "Audio", |ui| {
                ui.label("Game audio");
                ui.checkbox(&mut d.audio.game_audio, "Record only the game's own sound");
                ui.end_row();

                ui.label("Microphone");
                ui.checkbox(&mut d.audio.microphone, "Record the microphone on its own track");
                ui.end_row();

                ui.label("Microphone device");
                let current = self.mics.iter().find(|(id, _)| *id == d.audio.microphone_device).map(|(_, n)| n.clone()).unwrap_or_else(|| "Windows default".into());
                egui::ComboBox::from_id_salt("mic").selected_text(current).width(320.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.audio.microphone_device, String::new(), "Windows default");
                    for (id, name) in &self.mics {
                        ui.selectable_value(&mut d.audio.microphone_device, id.clone(), name);
                    }
                });
                ui.end_row();

                ui.label("Mixed track");
                ui.checkbox(&mut d.audio.mixed_track, "Add a first track with game and mic together (for Discord and web players)");
                ui.end_row();

                ui.label("Game volume in mix");
                ui.add(egui::Slider::new(&mut d.audio.game_volume, 0..=200).suffix("%"));
                ui.end_row();

                ui.label("Mic volume in mix");
                ui.add(egui::Slider::new(&mut d.audio.microphone_volume, 0..=200).suffix("%"));
                ui.end_row();

                ui.label("Audio bitrate");
                egui::ComboBox::from_id_salt("abr").selected_text(format!("{} kbit/s", d.audio.bitrate_kbps)).show_ui(ui, |ui| {
                    for b in [96, 128, 160, 192] {
                        ui.selectable_value(&mut d.audio.bitrate_kbps, b, format!("{b} kbit/s"));
                    }
                });
                ui.end_row();
            });

            Self::section(ui, "Hotkeys", |ui| {
                for (label, value) in [
                    ("Save replay of the game in front", &mut d.hotkeys.save_focused),
                    ("Save replays of all games", &mut d.hotkeys.save_all),
                    ("Mark or unmark the app in front as a game", &mut d.hotkeys.toggle_game),
                ] {
                    ui.label(label);
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(value).desired_width(180.0));
                        match crate::hotkey::parse(value) {
                            Ok(_) => ui.label(RichText::new("OK").color(Color32::from_rgb(0x4c, 0xaf, 0x50))),
                            Err(e) => ui.label(RichText::new(e).color(Color32::from_rgb(0xef, 0x53, 0x50))),
                        };
                    });
                    ui.end_row();
                }
                ui.label("");
                ui.label(
                    RichText::new("Examples: Alt+F10, Ctrl+Shift+S, F9. If the NVIDIA app uses the same keys, turn off its Instant Replay.")
                        .weak(),
                );
                ui.end_row();
            });

            Self::section(ui, "Game detection", |ui| {
                ui.label("Automatic");
                ui.checkbox(&mut d.detection.enabled, "Find games on their own (launchers, game folders, Windows Game Bar list)");
                ui.end_row();
                ui.label("Fullscreen apps");
                ui.checkbox(&mut d.detection.fullscreen_apps, "Also record other 3D apps that fill the screen");
                ui.end_row();
                ui.label("Always record");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::multiline(&mut self.include_text).desired_rows(3).desired_width(360.0).hint_text("game.exe (one per line)"));
                    if ui.button("Add a program...").clicked()
                        && let Some(p) = crate::sys::shell::pick_exe()
                        && let Some(name) = p.file_name()
                    {
                        if !self.include_text.is_empty() && !self.include_text.ends_with('\n') {
                            self.include_text.push('\n');
                        }
                        self.include_text.push_str(&name.to_string_lossy().to_ascii_lowercase());
                    }
                });
                ui.end_row();
                ui.label("Never record");
                ui.add(egui::TextEdit::multiline(&mut self.exclude_text).desired_rows(3).desired_width(360.0).hint_text("app.exe (one per line)"));
                ui.end_row();
            });

            Self::section(ui, "Sharing", |ui| {
                ui.label("Streamable email");
                ui.add(egui::TextEdit::singleline(&mut d.share.streamable_email).desired_width(280.0));
                ui.end_row();
                ui.label("Streamable password");
                let hint = if d.share.streamable_password_protected.is_empty() { "" } else { "saved (type to replace)" };
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(280.0).hint_text(hint));
                ui.end_row();
                ui.label("");
                ui.label(RichText::new("The password is encrypted for your Windows account (DPAPI) and only sent to Streamable.").weak());
                ui.end_row();
                ui.label("Auto-upload");
                ui.checkbox(&mut d.share.auto_upload, "Upload every saved replay to Streamable");
                ui.end_row();
                ui.label("Link");
                ui.checkbox(&mut d.share.copy_link_after_upload, "Copy the link after an upload");
                ui.end_row();
            });

            Self::section(ui, "Updates", |ui| {
                ui.label("Automatic");
                ui.checkbox(&mut d.updates.auto_check, "Check for a new version once a day and download it in the background");
                ui.end_row();
                ui.label("Version");
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{} ({})", crate::update::CURRENT, crate::sys::PLATFORM));
                    match self.update.clone() {
                        UpdateState::Checking => {
                            ui.spinner();
                            ui.label("Checking...");
                        }
                        UpdateState::Ready(v) => {
                            ui.label(RichText::new(format!("Version {v} is downloaded.")).color(Color32::from_rgb(0x4c, 0xaf, 0x50)));
                            if ui.button("Restart to update").clicked() {
                                restart_to_update(ui.ctx(), shared);
                            }
                        }
                        UpdateState::Available { version, page, why } => {
                            ui.label(format!("Version {version} is available. {why}"));
                            if ui.button("Open the download page").clicked() {
                                crate::sys::shell::open_url(&page);
                            }
                        }
                        state => {
                            if let UpdateState::UpToDate = state {
                                ui.label(RichText::new("You have the latest version.").weak());
                            }
                            if let UpdateState::Failed(e) = &state {
                                ui.label(RichText::new(e).color(Color32::from_rgb(0xef, 0x53, 0x50)));
                            }
                            if ui.button("Check for updates").clicked() {
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
                    }
                });
                ui.end_row();
            });

            Self::section(ui, "General", |ui| {
                ui.label("Notifications");
                ui.checkbox(&mut d.notify, "Show a message when a replay is saved");
                ui.end_row();
                ui.label("Sound");
                ui.checkbox(&mut d.sound, "Play a sound when a replay is saved");
                ui.end_row();
                ui.label("Startup");
                ui.checkbox(&mut d.start_with_windows, "Start Replay with Windows");
                ui.end_row();
            });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let dirty = self.dirty(&shared.cfg);
                if ui.add_enabled(dirty, egui::Button::new(RichText::new("Save settings").strong())).clicked() {
                    folder_changed = self.save(shared);
                }
                if ui.add_enabled(dirty, egui::Button::new("Undo changes")).clicked() {
                    *self = Self { mics: std::mem::take(&mut self.mics), ..Self::new(&shared.cfg) };
                }
                if dirty {
                    ui.label(RichText::new("Unsaved changes").color(Color32::from_rgb(0xff, 0x98, 0x00)));
                }
            });
            ui.label(RichText::new("Changes to video or audio settings restart the recording, so current buffers start again.").weak());
            ui.add_space(20.0);
        });
        folder_changed
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect()
    }

    fn built(&self, current: &Config) -> Config {
        let mut c = self.draft.clone();
        c.detection.include = Self::lines(&self.include_text);
        c.detection.exclude = Self::lines(&self.exclude_text);
        if !self.password.is_empty() {
            c.share.streamable_password_protected = crate::sys::secret::protect(&self.password);
        } else {
            c.share.streamable_password_protected = current.share.streamable_password_protected.clone();
        }
        c.sanitized()
    }

    fn dirty(&self, current: &Config) -> bool {
        !self.password.is_empty() || self.built(current) != *current
    }

    fn save(&mut self, shared: &mut Shared) -> bool {
        let new = self.built(&shared.cfg);
        if let Err(e) = [&new.hotkeys.save_focused, &new.hotkeys.save_all, &new.hotkeys.toggle_game]
            .iter()
            .filter(|h| !h.trim().is_empty())
            .try_for_each(|h| crate::hotkey::parse(h).map(|_| ()))
        {
            shared.toast(format!("Fix the hotkey first: {e}"), true);
            return false;
        }
        if new.start_with_windows != shared.cfg.start_with_windows && !crate::sys::shell::set_autostart(new.start_with_windows) {
            shared.toast("Could not change the Windows startup setting.", true);
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

/// Installs the downloaded update. The recorder restarts itself when it
/// runs; otherwise this window does it. Either way this window closes.
fn restart_to_update(ctx: &egui::Context, shared: &mut Shared) {
    if crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::InstallUpdate) {
        // The recorder installs the update and starts again; reopen from the tray.
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
