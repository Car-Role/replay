//! Clip editor, a full page (see docs/design.md): a command bar on top
//! (back, name, favorite, more, share), the video on black in the middle, an
//! inspector on the right (Trim, Audio, Export, Details), and a full-width
//! timeline at the bottom (transport, filmstrip with trim handles, waveform).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Key, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle,
    Vec2,
};

use super::filmstrip::{self, Thumb};
use super::theme::{self, Tokens, icon};
use super::{JobDone, JobKind, Shared};
use crate::config::Config;
use crate::edit::{self, Mix, Segment};
use crate::media::{AudioTracks, Frame, WallClock};
use crate::sys::player::{AudioOut, VideoDecoder};

pub enum Action {
    Back,
    Delete(PathBuf),
    Favorite(PathBuf, bool),
    Rename(PathBuf, String),
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    In,
    Out,
    Playhead,
}

/// Export targets in the Export card.
#[derive(Clone, Copy, PartialEq)]
enum Preset {
    Discord10,
    Discord50,
    Discord500,
    Custom,
    Original,
}

impl Preset {
    const ALL: [Preset; 5] = [Preset::Original, Preset::Discord10, Preset::Discord50, Preset::Discord500, Preset::Custom];

    fn label(self) -> &'static str {
        match self {
            Preset::Discord10 => "Discord (10 MB)",
            Preset::Discord50 => "Discord Nitro Basic (50 MB)",
            Preset::Discord500 => "Discord Nitro (500 MB)",
            Preset::Custom => "Custom size",
            Preset::Original => "Original size",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Preset::Discord10 => "Fits Discord without Nitro",
            Preset::Discord50 => "Fits Discord with Nitro Basic",
            Preset::Discord500 => "Fits Discord with Nitro",
            Preset::Custom => "Fits the size you choose",
            Preset::Original => "Same quality as the recording",
        }
    }

    fn size_mb(self, custom: f64) -> Option<f64> {
        match self {
            Preset::Discord10 => Some(10.0),
            Preset::Discord50 => Some(50.0),
            Preset::Discord500 => Some(500.0),
            Preset::Custom => Some(custom),
            Preset::Original => None,
        }
    }
}

const THUMBS: usize = 24;
const THUMB_H: usize = 64;
const STRIP_H: f32 = 56.0;
/// Waveform strip under the filmstrip.
const WAVE_H: f32 = 28.0;
const WAVE_BUCKETS: usize = 800;
const INSPECTOR_W: f32 = 320.0;
const HANDLE_W: f32 = 12.0;

pub struct Editor {
    path: PathBuf,
    duration: f64,
    size_bytes: u64,
    fps: f64,
    has_game: bool,
    has_mic: bool,
    error: Option<String>,
    video: VideoDecoder,
    audio: Option<AudioOut>,
    audio_rx: Option<Receiver<Result<AudioTracks, String>>>,
    wall: WallClock,
    playing: bool,
    generation: u64,
    texture: Option<TextureHandle>,
    pending: Option<Frame>,
    show_next: bool,
    /// Playhead position: the last seek while paused, the clock while playing.
    cursor: f64,
    in_point: f64,
    out_point: f64,
    game_gain: u32,
    mic_gain: u32,
    keep_separate: bool,
    drag: Option<Drag>,
    /// Re-encode saves so they start on the exact frame.
    exact: bool,
    preset: Preset,
    custom_mb: f64,
    /// Vertical 9:16 export, and the crop's horizontal centre (0..1).
    vertical: bool,
    crop_center: f32,
    /// Source size and average video bitrate, for estimates.
    video_size: (u32, u32),
    video_kbps: u32,
    last_scrub: Instant,
    film_rx: Option<Receiver<Thumb>>,
    film: Vec<Option<TextureHandle>>,
    /// Peak level (0..1) per slice of the clip, for the waveform strip.
    waveform: Vec<f32>,
    /// The name while it is being edited in the command bar.
    name_edit: Option<String>,
    codec: String,
}

impl Editor {
    pub fn open(ctx: &egui::Context, path: &Path, cfg: &Config) -> Self {
        let info = crate::mux::reader::read(path);
        let codec = match info.as_ref().ok().and_then(|m| m.video()).map(|v| &v.kind) {
            Some(crate::mux::reader::TrackKindIn::Video { codec, .. }) => codec.label().to_string(),
            _ => String::new(),
        };
        let (video_size, video_kbps) = match &info {
            Ok(m) => (
                match m.video().map(|v| &v.kind) {
                    Some(crate::mux::reader::TrackKindIn::Video { width, height, .. }) => (*width, *height),
                    _ => (1920, 1080),
                },
                edit::video_kbps(m),
            ),
            Err(_) => ((1920, 1080), 0),
        };
        let (duration, has_game, has_mic, fps, error) = match &info {
            Ok(m) => {
                let (g, mi) = edit::audio_sources(m);
                let fps = m
                    .video()
                    .filter(|v| v.samples.len() > 1)
                    .map(|v| (v.samples.len() - 1) as f64 / (v.seconds(v.samples.len() - 1) - v.seconds(0)).max(0.001))
                    .unwrap_or(60.0);
                (m.duration(), g.is_some(), mi.is_some(), fps, None)
            }
            Err(e) => (0.0, false, false, 60.0, Some(format!("This file cannot be opened: {e}"))),
        };
        let wake = ctx.clone();
        let video = VideoDecoder::open(path, move || wake.request_repaint());
        video.seek(0.0, 0);
        let wake = ctx.clone();
        let film_rx = (error.is_none() && duration > 0.0)
            .then(|| filmstrip::build(path, duration, THUMBS, THUMB_H, move || wake.request_repaint()));

        let audio_rx = info.ok().map(|m| {
            let (tx, rx) = mpsc::channel();
            let p = path.to_path_buf();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                crate::sys::init_tool();
                let r = (|| -> Result<AudioTracks, String> {
                    let mut f = File::open(&p).map_err(|e| e.to_string())?;
                    let (g, mi) = edit::audio_sources(&m);
                    let dur = m.duration();
                    let game = g.map(|t| edit::decode_range(&mut f, t, 0.0, dur)).transpose()?;
                    let mic = mi.map(|t| edit::decode_range(&mut f, t, 0.0, dur)).transpose()?;
                    Ok(AudioTracks { game, mic })
                })();
                let _ = tx.send(r);
                ctx.request_repaint();
            });
            rx
        });

        Self {
            path: path.to_path_buf(),
            duration,
            size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            fps,
            has_game,
            has_mic,
            error,
            video,
            audio: None,
            audio_rx,
            wall: WallClock::new(),
            playing: false,
            generation: 0,
            texture: None,
            pending: None,
            show_next: true,
            cursor: 0.0,
            in_point: 0.0,
            out_point: duration,
            game_gain: cfg.audio.game_volume,
            mic_gain: cfg.audio.microphone_volume,
            keep_separate: true,
            drag: None,
            exact: false,
            preset: Preset::Original,
            custom_mb: 25.0,
            vertical: false,
            crop_center: 0.5,
            video_size,
            video_kbps,
            last_scrub: Instant::now(),
            film_rx,
            film: (0..THUMBS).map(|_| None).collect(),
            waveform: Vec::new(),
            name_edit: None,
            codec,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// For `--snapshot`: show the vertical export with its crop frame.
    pub fn snapshot_vertical(&mut self) {
        self.vertical = true;
    }

    fn clock(&self) -> f64 {
        match &self.audio {
            Some(a) if a.frames > 0 => a.position(),
            _ => self.wall.position(),
        }
    }

    fn set_playing(&mut self, on: bool) {
        if on && (self.cursor >= self.out_point - 0.05 || self.cursor < self.in_point - 0.01) {
            self.seek(self.in_point);
        }
        self.playing = on;
        self.wall.seek(self.cursor);
        self.wall.set_playing(on);
        if let Some(a) = &self.audio {
            a.seek(self.cursor);
            a.set_playing(on);
        }
    }

    fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration.max(0.0));
        self.cursor = t;
        self.generation += 1;
        self.video.seek(t, self.generation);
        if let Some(a) = &self.audio {
            a.seek(t);
        }
        self.wall.seek(t);
        self.pending = None;
        self.show_next = true;
    }

    /// Moves the playhead while dragging; asks the decoder for a frame at
    /// most ~25 times a second so it can keep up.
    fn scrub(&mut self, t: f64) {
        self.cursor = t.clamp(0.0, self.duration);
        if self.last_scrub.elapsed() >= Duration::from_millis(40) {
            self.last_scrub = Instant::now();
            let c = self.cursor;
            self.seek(c);
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.audio_rx
            && let Ok(r) = rx.try_recv()
        {
            self.audio_rx = None;
            match r {
                Ok(tracks) if tracks.game.is_some() || tracks.mic.is_some() => {
                    self.waveform = waveform(&tracks);
                    let a = AudioOut::start(tracks);
                    a.set_gains(self.game_gain, self.mic_gain);
                    a.seek(self.cursor);
                    a.set_playing(self.playing);
                    self.audio = Some(a);
                }
                Ok(_) => {}
                Err(e) => log::warn!("preview audio: {e}"),
            }
        }
        if let Some(rx) = &self.film_rx {
            loop {
                match rx.try_recv() {
                    Ok(t) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied([t.width, t.height], &t.rgba);
                        self.film[t.index] = Some(ctx.load_texture(format!("film{}", t.index), img, egui::TextureOptions::LINEAR));
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.film_rx = None;
                        break;
                    }
                }
            }
        }
        if self.playing {
            let now = self.clock();
            self.cursor = now;
            if now >= self.out_point {
                self.set_playing(false);
                let o = self.out_point;
                self.seek(o);
            }
        }
        let now = self.cursor;
        loop {
            let frame = match self.pending.take() {
                Some(f) => f,
                None => match self.video.frames.try_recv() {
                    Ok(f) => f,
                    Err(_) => break,
                },
            };
            if frame.generation != self.generation {
                continue;
            }
            if self.show_next || frame.pts <= now + 0.008 {
                self.show_next = false;
                let img = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR)),
                }
                if !self.playing {
                    break;
                }
            } else {
                self.pending = Some(frame);
                break;
            }
        }
        if self.playing || self.film_rx.is_some() || self.drag.is_some() {
            ctx.request_repaint_after(Duration::from_millis(8));
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (space, i, o, left, right, shift, home, end) = ctx.input(|inp| {
            (
                inp.key_pressed(Key::Space),
                inp.key_pressed(Key::I),
                inp.key_pressed(Key::O),
                inp.key_pressed(Key::ArrowLeft),
                inp.key_pressed(Key::ArrowRight),
                inp.modifiers.shift,
                inp.key_pressed(Key::Home),
                inp.key_pressed(Key::End),
            )
        });
        if space {
            let p = !self.playing;
            self.set_playing(p);
        }
        if i {
            self.in_point = self.cursor.min(self.out_point - 0.1).max(0.0);
        }
        if o {
            self.out_point = self.cursor.max(self.in_point + 0.1).min(self.duration);
        }
        let jump = if shift { 1.0 } else { (1.0 / self.fps.max(1.0)).max(0.1) };
        if left || right || home || end {
            if self.playing {
                self.set_playing(false);
            }
            let target = if home {
                self.in_point
            } else if end {
                self.out_point
            } else if left {
                self.cursor - jump
            } else {
                self.cursor + jump
            };
            self.seek(target);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &mut Shared, favorite: bool) -> Option<Action> {
        let ctx = ui.ctx().clone();
        let t = theme::tokens(&ctx);
        let icons = shared.icons;
        self.pump(&ctx);
        self.keyboard(&ctx);
        let mut action = self.command_bar(ui, &t, icons, shared, favorite);
        ui.add_space(8.0);
        if let Some(e) = &self.error {
            ui.add_space(24.0);
            ui.label(RichText::new(e).color(t.critical));
            return action;
        }

        // Timeline height, then the preview and inspector above it.
        let dock_h = 44.0 + STRIP_H + WAVE_H + 60.0 + 12.0;
        let full = ui.available_rect_before_wrap();
        let top = Rect::from_min_max(full.min, Pos2::new(full.right(), (full.bottom() - dock_h - 8.0).max(full.top() + 160.0)));
        let dock = Rect::from_min_max(Pos2::new(full.left(), top.bottom() + 8.0), full.max - Vec2::new(0.0, 12.0));
        let side = full.width() >= 860.0;
        let preview = if side { Rect::from_min_max(top.min, Pos2::new(top.right() - INSPECTOR_W - 12.0, top.bottom())) } else { top };
        let mut pv = ui.new_child(egui::UiBuilder::new().max_rect(preview));
        self.preview(&mut pv, &t, icons, preview);
        if side {
            let insp = Rect::from_min_max(Pos2::new(preview.right() + 12.0, top.top()), top.max);
            let mut iu = ui.new_child(egui::UiBuilder::new().max_rect(insp).layout(egui::Layout::top_down(egui::Align::Min)));
            if let Some(a) = self.inspector(&mut iu, &t, icons, shared) {
                action = Some(a);
            }
        }
        let mut du = ui.new_child(egui::UiBuilder::new().max_rect(dock).layout(egui::Layout::top_down(egui::Align::Min)));
        self.transport(&mut du, &t, icons);
        self.trim_bar(&mut du, &t);
        if !side {
            // Narrow windows: the inspector goes in a window of its own.
            egui::Window::new("Clip").default_pos(Pos2::new(full.right() - INSPECTOR_W, full.top() + 60.0)).default_width(INSPECTOR_W).show(&ctx, |ui| {
                if let Some(a) = self.inspector(ui, &t, icons, shared) {
                    action = Some(a);
                }
            });
        }
        ui.allocate_rect(full, Sense::hover());
        action
    }

    /// Back, the clip name (click to rename), then favorite, more and share.
    fn command_bar(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, shared: &mut Shared, favorite: bool) -> Option<Action> {
        let ctx = ui.ctx().clone();
        let mut action = None;
        let busy = !shared.jobs.is_empty();
        ui.horizontal(|ui| {
            ui.set_min_height(44.0);
            if theme::subtle(ui, theme::label_with_icon(icons, icon::BACK, if icons { "" } else { "Back" })).on_hover_text("Back to clips (Esc)").clicked() {
                action = Some(Action::Back);
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                let name = self.path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                match &mut self.name_edit {
                    Some(text) => {
                        let r = ui.add(egui::TextEdit::singleline(text).font(FontId::proportional(18.0)).desired_width(420.0).margin(egui::Margin::symmetric(6, 2)));
                        r.request_focus();
                        if ui.input(|i| i.key_pressed(Key::Enter)) {
                            let new = text.trim().to_string();
                            if !new.is_empty() && new != name {
                                action = Some(Action::Rename(self.path.clone(), new));
                            }
                            self.name_edit = None;
                        } else if ui.input(|i| i.key_pressed(Key::Escape)) || r.lost_focus() {
                            self.name_edit = None;
                        }
                    }
                    None => {
                        let game = self.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let shown = super::clips::friendly_title(&self.path, &game);
                        let r = ui.add(egui::Label::new(RichText::new(&shown).size(18.0).color(t.text)).truncate().sense(Sense::click()));
                        // A default name invites a real one.
                        let default_name = shown != name;
                        let hint = if default_name { "  \u{00b7}  Add a name" } else { "" };
                        if !hint.is_empty() {
                            let at = Pos2::new(r.rect.right() + 2.0, r.rect.center().y);
                            let hr = ui.painter().text(at, Align2::LEFT_CENTER, hint, FontId::proportional(14.0), t.text3);
                            let hresp = ui.interact(hr, r.id.with("add-name"), Sense::click());
                            if hresp.on_hover_cursor(egui::CursorIcon::Text).clicked() {
                                self.name_edit = Some(String::new());
                            }
                        } else if r.hovered() && icons {
                            let at = Pos2::new(r.rect.right() + 10.0, r.rect.center().y);
                            ui.painter().text(at, Align2::LEFT_CENTER, icon::EDIT, FontId::new(14.0, FontFamily::Name(theme::ICONS.into())), t.text2);
                        }
                        if r.on_hover_text("Click to rename").on_hover_cursor(egui::CursorIcon::Text).clicked() {
                            self.name_edit = Some(name.clone());
                        }
                    }
                }
                let game = self.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(theme::caption(format!("{game}  \u{00b7}  {}  \u{00b7}  {:.1} MB", theme::fmt_short(self.duration), self.size_bytes as f64 / 1e6)).color(t.text2));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Share: the one accent button on this page.
                ui.scope(|ui| {
                    let w = &mut ui.visuals_mut().widgets;
                    for s in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
                        s.weak_bg_fill = t.accent;
                        s.bg_stroke = Stroke::NONE;
                    }
                    w.hovered.weak_bg_fill = t.accent_hover;
                    let text = theme::icon_label(icons, icon::SHARE, "Share", t.on_accent);
                    ui.menu_button(text, |ui| {
                        ui.reset_style();
                        ui.set_min_width(220.0);
                        if ui.button(theme::label_with_icon(icons, icon::COPY, "Copy file")).on_hover_text("Then paste it into Discord or a chat").clicked() {
                            if crate::sys::shell::copy_file(&self.path) {
                                shared.toast("File copied. Paste it with Ctrl+V.", false);
                            } else {
                                shared.toast("Could not use the clipboard.", true);
                            }
                            ui.close();
                        }
                        let ready = crate::share::ready(&shared.cfg.share);
                        let label = format!("Upload to {}", shared.cfg.share.target.label());
                        let r = ui.add_enabled(!busy, egui::Button::new(theme::label_with_icon(icons, icon::UPLOAD, &label)));
                        let r = match ready {
                            Ok(()) => r.on_hover_text(shared.cfg.share.target.describe()),
                            Err(why) => r.on_hover_text(why),
                        };
                        if r.clicked() {
                            match ready {
                                Ok(()) => shared.start_upload(&ctx, self.path.clone()),
                                Err(why) => shared.toast(why, true),
                            }
                            ui.close();
                        }
                        let mb = shared.cfg.share.discord_mb;
                        let r = ui.add_enabled(!busy, egui::Button::new(theme::label_with_icon(icons, icon::SHARE, &format!("Copy for Discord ({mb} MB)"))));
                        if r.on_hover_text(format!("Makes a copy that fits your {mb} MB Discord limit and puts it on the clipboard. Change the limit in Settings > Sharing")).clicked() {
                            self.copy_for_discord(&ctx, shared);
                            ui.close();
                        }
                        if let Some(url) = shared.uploads.get(self.path.to_string_lossy().as_ref()).cloned() {
                            ui.separator();
                            if ui.button(theme::label_with_icon(icons, icon::LINK, "Copy link")).on_hover_text(&url).clicked() {
                                crate::sys::shell::copy_text(&url);
                                shared.toast("Link copied.", false);
                                ui.close();
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                ui.scope(|ui| {
                    let w = &mut ui.visuals_mut().widgets;
                    for s in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
                        s.bg_stroke = Stroke::NONE;
                    }
                    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    w.hovered.weak_bg_fill = t.subtle_hover;
                    w.active.weak_bg_fill = t.subtle_pressed;
                    w.open.weak_bg_fill = t.subtle_hover;
                    ui.spacing_mut().button_padding = Vec2::new(8.0, 8.0);
                ui.menu_button(theme::label_with_icon(icons, icon::MORE, if icons { "" } else { "More" }), |ui| {
                    ui.reset_style();
                    ui.set_min_width(180.0);
                    if ui.button("Rename").clicked() {
                        self.name_edit = Some(self.path.file_stem().unwrap_or_default().to_string_lossy().into_owned());
                        ui.close();
                    }
                    if ui.button("Show in folder").clicked() {
                        crate::sys::shell::reveal(&self.path);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(RichText::new("Delete").color(t.critical)).clicked() {
                        action = Some(Action::Delete(self.path.clone()));
                        ui.close();
                    }
                }).response.on_hover_text("More");
                });
                ui.add_space(4.0);
                let star = if icons { theme::glyph(if favorite { icon::STAR_FILLED } else { icon::STAR }, 16.0) } else { RichText::new(if favorite { "\u{2605}" } else { "\u{2606}" }) };
                let star = if favorite { star.color(Color32::from_rgb(255, 200, 61)) } else { star };
                let name = if favorite { "Remove from favorites" } else { "Add to favorites" };
                if theme::icon_button(ui, t, star, name).on_hover_text(name).clicked() {
                    action = Some(Action::Favorite(self.path.clone(), !favorite));
                }
            });
        });
        if self.name_edit.is_none() && action.is_none() && !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            action = Some(Action::Back);
        }
        action
    }

    /// The video, aspect-fit on black. Click to play or pause; in vertical
    /// mode, drag to move the 9:16 crop.
    fn preview(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, rect: Rect) {
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(theme::R_CARD), t.well);
        let (vw, vh) = self.texture.as_ref().map(|t| (t.size()[0] as f32, t.size()[1] as f32)).unwrap_or((16.0, 9.0));
        let scale = (rect.width() / vw).min(rect.height() / vh);
        let img = Rect::from_center_size(rect.center(), Vec2::new(vw * scale, vh * scale));
        if let Some(tex) = &self.texture {
            p.image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        if self.vertical {
            let spec = edit::EncodeSpec { codec: crate::config::Codec::H264, bitrate_kbps: 0, cbr: false, max_height: None, vertical: Some(self.crop_center) };
            let (sw, sh) = self.video_size;
            let ((cx, _, cw, _), _) = edit::plan_frame(sw, sh, &spec);
            let x0 = img.left() + img.width() * cx as f32 / sw.max(1) as f32;
            let w = img.width() * cw as f32 / sw.max(1) as f32;
            let keep = Rect::from_min_max(Pos2::new(x0, img.top()), Pos2::new(x0 + w, img.bottom())).intersect(img);
            let shade = Color32::from_black_alpha(200);
            p.rect_filled(Rect::from_min_max(img.min, Pos2::new(keep.left(), img.bottom())), CornerRadius::ZERO, shade);
            p.rect_filled(Rect::from_min_max(Pos2::new(keep.right(), img.top()), img.max), CornerRadius::ZERO, shade);
            p.rect_stroke(keep, CornerRadius::ZERO, Stroke::new(2.0, t.accent), StrokeKind::Inside);
            for x in [keep.left(), keep.right()] {
                p.rect_filled(Rect::from_center_size(Pos2::new(x, keep.center().y), Vec2::new(8.0, 24.0)), CornerRadius::same(2), t.accent);
            }
            let tag = p.layout_no_wrap("Drag to frame".to_string(), FontId::proportional(12.0), Color32::WHITE);
            let tr = Rect::from_center_size(Pos2::new(keep.center().x, keep.top() + 8.0 + (tag.size().y + 4.0) / 2.0), tag.size() + Vec2::new(12.0, 4.0));
            p.rect_filled(tr, CornerRadius::same(theme::R_CONTROL), Color32::from_black_alpha(170));
            p.galley(tr.min + Vec2::new(6.0, 2.0), tag, Color32::WHITE);
            if resp.dragged()
                && let Some(m) = resp.interact_pointer_pos()
            {
                self.crop_center = ((m.x - img.left()) / img.width().max(1.0)).clamp(0.0, 1.0);
            }
            if resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        if resp.clicked() {
            let on = !self.playing;
            self.set_playing(on);
        }
        if !self.playing && self.drag.is_none() && resp.hovered() && !self.vertical {
            let c = rect.center();
            p.circle_filled(c, 32.0, Color32::from_black_alpha(140));
            if icons {
                p.text(c, Align2::CENTER_CENTER, icon::PLAY, FontId::new(26.0, FontFamily::Name(theme::ICONS.into())), Color32::WHITE);
            } else {
                draw_play(&p, c, 14.0, Color32::WHITE);
            }
        }
    }

    /// Timecode on the left, playback controls in the middle, selection on the right.
    fn transport(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
        let mono = FontId::proportional(14.0);
        ui.painter().text(
            Pos2::new(r.left() + 4.0, r.center().y),
            Align2::LEFT_CENTER,
            format!("{}  /  {}", theme::fmt_time(self.cursor), theme::fmt_time(self.duration)),
            mono.clone(),
            t.text,
        );
        let total = 32.0 * 6.0 + 40.0 + 4.0 * 7.0;
        let controls = Rect::from_center_size(r.center(), Vec2::new(total, 44.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let u = &mut child;
        u.spacing_mut().item_spacing.x = 4.0;
        let step = 1.0 / self.fps.max(1.0);
        let go = |ed: &mut Self, target: f64| {
            if ed.playing {
                ed.set_playing(false);
            }
            ed.seek(target);
        };
        if theme::subtle(u, theme::label_with_icon(icons, icon::SKIP_BACK, if icons { "" } else { "|<" })).on_hover_text("Go to start (Home)").clicked() {
            let i = self.in_point;
            go(self, i);
        }
        if glyph_button(u, t, Glyph::SetIn).on_hover_text("Set start here (I)").clicked() {
            self.in_point = self.cursor.min(self.out_point - 0.1).max(0.0);
        }
        if glyph_button(u, t, Glyph::Prev).on_hover_text("Previous frame (\u{2190})").clicked() {
            let c = self.cursor - step;
            go(self, c);
        }
        let (b, resp) = u.allocate_exact_size(Vec2::splat(40.0), Sense::click());
        let fill = if resp.hovered() { t.control_hover } else { t.control };
        u.painter().circle(b.center(), 20.0, fill, Stroke::new(1.0, t.control_stroke));
        if icons {
            let g = if self.playing { icon::PAUSE } else { icon::PLAY };
            u.painter().text(b.center(), Align2::CENTER_CENTER, g, FontId::new(16.0, FontFamily::Name(theme::ICONS.into())), t.text);
        } else {
            draw_play(u.painter(), b.center(), 7.0, t.text);
        }
        theme::focus_ring(u, t, &resp, b, 20.0);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, if self.playing { "Pause" } else { "Play" }));
        if resp.on_hover_text("Play the selection (Space)").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            let on = !self.playing;
            self.set_playing(on);
        }
        if glyph_button(u, t, Glyph::Next).on_hover_text("Next frame (\u{2192})").clicked() {
            let c = self.cursor + step;
            go(self, c);
        }
        if glyph_button(u, t, Glyph::SetOut).on_hover_text("Set end here (O)").clicked() {
            self.out_point = self.cursor.max(self.in_point + 0.1).min(self.duration);
        }
        if theme::subtle(u, theme::label_with_icon(icons, icon::SKIP_FWD, if icons { "" } else { ">|" })).on_hover_text("Go to end (End)").clicked() {
            let o = self.out_point;
            go(self, o);
        }
        let _ = mono;
    }

    /// Filmstrip with trim handles. Dragging a handle or the playhead shows
    /// that frame in the preview.
    fn trim_bar(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let dur = self.duration.max(0.001);
        let (outer, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), STRIP_H + WAVE_H + 60.0), Sense::click_and_drag());
        let strip = Rect::from_min_size(
            Pos2::new(outer.left() + HANDLE_W, outer.top() + 28.0),
            Vec2::new(outer.width() - HANDLE_W * 2.0, STRIP_H),
        );
        let x_of = |s: f64| strip.left() + (s / dur) as f32 * strip.width();
        let t_of = |x: f32| ((x - strip.left()) / strip.width()).clamp(0.0, 1.0) as f64 * dur;
        let p = ui.painter_at(outer);

        // Filmstrip: 16:9 frames at strip height, each taken from the decoded
        // frame nearest its slot's time and centre-cropped to fit.
        p.rect_filled(strip, CornerRadius::same(theme::R_CONTROL), t.control_pressed);
        let slots = ((strip.width() / (STRIP_H * 16.0 / 9.0)).round() as usize).clamp(1, THUMBS);
        let slot = strip.width() / slots as f32;
        for i in 0..slots {
            let src = (((i as f32 + 0.5) / slots as f32) * THUMBS as f32) as usize;
            let r = Rect::from_min_size(Pos2::new(strip.left() + slot * i as f32, strip.top()), Vec2::new(slot, STRIP_H));
            let Some(tex) = self.film.get(src.min(THUMBS - 1)).and_then(|t| t.as_ref()) else {
                // Loading: a soft light band moves across the empty tiles.
                let phase = (ui.input(|i| i.time) * 0.8).fract() as f32;
                let x = strip.left() + phase * strip.width();
                let d = ((r.center().x - x).abs() / (strip.width() * 0.25)).min(1.0);
                let base = t.card_hover;
                p.rect_filled(r.shrink(1.0), CornerRadius::ZERO, base.lerp_to_gamma(t.control_hover, 1.0 - d));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
                continue;
            };
            let [tw, th] = tex.size();
            let want = slot / STRIP_H;
            let have = tw as f32 / th.max(1) as f32;
            let uv = if have > want {
                let u = want / have;
                Rect::from_min_max(Pos2::new((1.0 - u) / 2.0, 0.0), Pos2::new((1.0 + u) / 2.0, 1.0))
            } else {
                let v = have / want;
                Rect::from_min_max(Pos2::new(0.0, (1.0 - v) / 2.0), Pos2::new(1.0, (1.0 + v) / 2.0))
            };
            p.image(tex.id(), r, uv, Color32::WHITE);
        }

        // Dim what will be cut away.
        let (xi, xo) = (x_of(self.in_point), x_of(self.out_point));
        let smoke = Color32::from_black_alpha(if t.dark { 175 } else { 140 });
        p.rect_filled(Rect::from_min_max(strip.min, Pos2::new(xi, strip.bottom())), CornerRadius::ZERO, smoke);
        p.rect_filled(Rect::from_min_max(Pos2::new(xo, strip.top()), strip.max), CornerRadius::ZERO, smoke);

        // Selection frame and grab handles.
        let sel = Rect::from_min_max(Pos2::new(xi, strip.top() - 3.0), Pos2::new(xo, strip.bottom() + 3.0));
        let trimmed = self.in_point > 0.001 || self.out_point < dur - 0.001;
        if trimmed {
            p.rect_stroke(sel, CornerRadius::ZERO, Stroke::new(3.0, t.accent), StrokeKind::Inside);
        } else {
            p.rect_stroke(sel, CornerRadius::ZERO, Stroke::new(1.0, t.control_stroke), StrokeKind::Inside);
        }
        let hovered_handle = resp.hover_pos().and_then(|m| self.handle_at(m.x, xi, xo));
        for (x, which) in [(xi, Drag::In), (xo, Drag::Out)] {
            let h = if which == Drag::In {
                Rect::from_min_max(Pos2::new(x - HANDLE_W, strip.top() - 3.0), Pos2::new(x, strip.bottom() + 3.0))
            } else {
                Rect::from_min_max(Pos2::new(x, strip.top() - 3.0), Pos2::new(x + HANDLE_W, strip.bottom() + 3.0))
            };
            let cr = if which == Drag::In { CornerRadius { nw: 4, sw: 4, ne: 0, se: 0 } } else { CornerRadius { nw: 0, sw: 0, ne: 4, se: 4 } };
            let active = self.drag == Some(which) || (self.drag.is_none() && hovered_handle == Some(which));
            p.rect_filled(h, cr, if active { t.accent_hover } else { t.accent });
            let c = h.center();
            for dx in [-1.5f32, 1.5] {
                p.line_segment([Pos2::new(c.x + dx, c.y - 8.0), Pos2::new(c.x + dx, c.y + 8.0)], Stroke::new(1.0, t.on_accent));
            }
        }


        // Playhead.
        let head = t.accent;
        let xp = x_of(self.cursor);
        p.line_segment([Pos2::new(xp, strip.top() - 8.0), Pos2::new(xp, strip.bottom() + 4.0)], Stroke::new(2.0, head));
        p.circle_filled(Pos2::new(xp, strip.top() - 9.0), 5.0, head);

        // Waveform, dimmed outside the selection like the filmstrip.
        let wave = Rect::from_min_size(Pos2::new(strip.left(), strip.bottom() + 6.0), Vec2::new(strip.width(), WAVE_H));
        if !self.waveform.is_empty() {
            let n = self.waveform.len();
            let cols = (wave.width() / 2.0).max(1.0) as usize;
            for k in 0..cols {
                let a = k * n / cols;
                let b = ((k + 1) * n / cols).max(a + 1).min(n);
                let peak = self.waveform[a..b].iter().cloned().fold(0.0f32, f32::max);
                let h = (peak * (WAVE_H - 2.0)).max(1.0);
                let x = wave.left() + k as f32 * 2.0 + 1.0;
                let inside = x >= xi && x <= xo;
                let c = if inside { t.text2 } else { t.text3.gamma_multiply(0.5) };
                p.line_segment([Pos2::new(x, wave.center().y - h / 2.0), Pos2::new(x, wave.center().y + h / 2.0)], Stroke::new(1.0, c));
            }
        } else if self.has_game || self.has_mic {
            p.text(wave.left_center(), Align2::LEFT_CENTER, "Loading audio\u{2026}", FontId::proportional(12.0), t.text3);
        }
        p.line_segment([Pos2::new(xp, wave.top()), Pos2::new(xp, wave.bottom())], Stroke::new(2.0, head));

        // Ruler under the waveform.
        let ruler = wave.bottom() + 4.0;
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0].into_iter().find(|s| dur / s <= 10.0).unwrap_or(600.0);
        let mut s = 0.0;
        while s <= dur + 1e-6 {
            let x = x_of(s);
            p.line_segment([Pos2::new(x, ruler), Pos2::new(x, ruler + 4.0)], Stroke::new(1.0, t.text3));
            p.text(Pos2::new(x, ruler + 6.0), Align2::CENTER_TOP, theme::fmt_short(s), FontId::proportional(12.0), t.text3);
            s += step;
        }

        // Interaction: grab a handle, or scrub the playhead anywhere else.
        if (resp.drag_started() || resp.is_pointer_button_down_on())
            && self.drag.is_none()
            && let Some(m) = resp.interact_pointer_pos()
        {
            self.drag = Some(self.handle_at(m.x, xi, xo).unwrap_or(Drag::Playhead));
            if self.playing {
                self.set_playing(false);
            }
        }
        if let Some(d) = self.drag {
            if let Some(m) = resp.interact_pointer_pos() {
                let v = match d {
                    Drag::In => {
                        self.in_point = t_of(m.x + HANDLE_W / 2.0).min(self.out_point - 0.1).max(0.0);
                        self.in_point
                    }
                    Drag::Out => {
                        self.out_point = t_of(m.x - HANDLE_W / 2.0).max(self.in_point + 0.1).min(dur);
                        self.out_point
                    }
                    Drag::Playhead => t_of(m.x),
                };
                self.scrub(v);
                bubble(&p, t, Pos2::new(x_of(v), outer.top() + 11.0), &theme::fmt_time(v));
                ui.ctx().set_cursor_icon(if d == Drag::Playhead { egui::CursorIcon::Grabbing } else { egui::CursorIcon::ResizeHorizontal });
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                // Released: land exactly on the final position.
                let c = self.cursor;
                self.seek(c);
                self.drag = None;
            }
        } else if let Some(m) = resp.hover_pos() {
            if hovered_handle.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            } else if strip.x_range().contains(m.x) {
                p.line_segment([Pos2::new(m.x, strip.top()), Pos2::new(m.x, strip.bottom())], Stroke::new(1.0, head.gamma_multiply(0.45)));
                bubble(&p, t, Pos2::new(m.x, outer.top() + 11.0), &theme::fmt_time(t_of(m.x)));
            }
        }
    }

    fn handle_at(&self, x: f32, xi: f32, xo: f32) -> Option<Drag> {
        let di = (x - (xi - HANDLE_W / 2.0)).abs();
        let do_ = (x - (xo + HANDLE_W / 2.0)).abs();
        match (di <= HANDLE_W, do_ <= HANDLE_W) {
            (true, true) => Some(if di <= do_ { Drag::In } else { Drag::Out }),
            (true, false) => Some(Drag::In),
            (false, true) => Some(Drag::Out),
            _ => None,
        }
    }

    /// The inspector: Trim, Audio, Export and Details, one level deep.
    fn inspector(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, shared: &mut Shared) -> Option<Action> {
        let _ctx = ui.ctx().clone();
        let busy = !shared.jobs.is_empty();
        egui::Frame::new()
            .fill(t.card)
            .stroke(Stroke::new(1.0, t.card_stroke))
            .corner_radius(CornerRadius::same(theme::R_CARD))
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height());
                let footer_h = 112.0;
                let out = egui::ScrollArea::vertical()
                    .id_salt("inspector")
                    .auto_shrink([false, false])
                    .max_height((ui.available_height() - footer_h).max(80.0))
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;

                    section(ui, t, "Trim", true, |ui| {
                        ui.columns(3, |cols| {
                            for (col, (k, v)) in cols.iter_mut().zip([("Start", self.in_point), ("End", self.out_point), ("Length", self.out_point - self.in_point)]) {
                                col.label(theme::caption(k).color(t.text3));
                                col.label(RichText::new(theme::fmt_time(v)).color(t.text2));
                            }
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Exact cut").color(t.text));
                                ui.label(theme::caption(if self.exact { "Slower. Cuts on the exact frame." } else { "Fast. Starts on the nearest keyframe." }).color(t.text2));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                theme::toggle(ui, t, &mut self.exact);
                            });
                        });
                    });

                    section(ui, t, "Audio", false, |ui| {
                        let mut changed = false;
                        let mut row = |ui: &mut egui::Ui, c: char, label: &str, v: &mut u32| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let (ir, _) = ui.allocate_exact_size(Vec2::new(20.0, 24.0), Sense::hover());
                                if icons {
                                    ui.painter().text(ir.center(), Align2::CENTER_CENTER, c, FontId::new(16.0, FontFamily::Name(theme::ICONS.into())), t.text2);
                                }
                                let (lr, _) = ui.allocate_exact_size(Vec2::new(44.0, 24.0), Sense::hover());
                                ui.painter().text(lr.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(14.0), t.text);
                                let w = (ui.available_width() - 56.0).max(60.0);
                                changed |= theme::slider(ui, t, v, 0..=200, false, w).changed();
                                let (vr, _) = ui.allocate_exact_size(Vec2::new(40.0, 24.0), Sense::hover());
                                ui.painter().text(vr.right_center(), Align2::RIGHT_CENTER, format!("{v}%"), FontId::proportional(12.0), t.text2);
                            });
                        };
                        if self.has_game {
                            row(ui, icon::GAME, "Game", &mut self.game_gain);
                        }
                        if self.has_mic {
                            row(ui, icon::MIC, "Mic", &mut self.mic_gain);
                        }
                        if !self.has_game && !self.has_mic {
                            ui.label(theme::caption("This clip has no audio.").color(t.text2));
                        }
                        if changed && let Some(a) = &self.audio {
                            a.set_gains(self.game_gain, self.mic_gain);
                        }
                        if self.has_mic {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Keep separate tracks").color(t.text));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    theme::toggle(ui, t, &mut self.keep_separate);
                                });
                            });
                        }
                    });

                    section(ui, t, "Save as", true, |ui| {
                        self.save_options(ui, t);
                    });

                    section(ui, t, "Details", false, |ui| {
                        egui::Grid::new("details").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                            let mut row = |k: &str, v: String| {
                                ui.label(theme::caption(k).color(t.text2));
                                ui.label(theme::caption(v).color(t.text));
                                ui.end_row();
                            };
                            row("Video", format!("{} x {}  \u{00b7}  {:.0} fps", self.video_size.0, self.video_size.1, self.fps));
                            row("Codec", format!("{}  \u{00b7}  {}", self.codec, theme::fmt_mbps(self.video_kbps)));
                            let tracks = match (self.has_game, self.has_mic) {
                                (true, true) => "Game and mic",
                                (true, false) => "Game",
                                (false, true) => "Mic",
                                _ => "None",
                            };
                            row("Audio", tracks.to_string());
                            row("Size", format!("{:.1} MB", self.size_bytes as f64 / 1e6));
                        });
                        if ui.add(egui::Button::new(theme::label_with_icon(icons, icon::FOLDER, "Show in folder")).frame_when_inactive(false)).clicked() {
                            crate::sys::shell::reveal(&self.path);
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(theme::caption("Space play  \u{00b7}  I / O start and end  \u{00b7}  \u{2190} \u{2192} frame  \u{00b7}  Shift 1 s").color(t.text3));
                });
                // More below: fade the last 12 px into the panel.
                if out.state.offset().y + out.inner_rect.height() < out.content_size.y - 1.0 {
                    let r = out.inner_rect;
                    for i in 0..12 {
                        let y = r.bottom() - 12.0 + i as f32;
                        let a = (i as f32 / 12.0).powf(0.7);
                        ui.painter().line_segment([Pos2::new(r.left(), y), Pos2::new(r.right() - 8.0, y)], Stroke::new(1.0, t.card.gamma_multiply(a)));
                    }
                }
                // Pinned footer: what comes out, and the button that saves it.
                let (d, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
                ui.painter().line_segment([Pos2::new(d.left() - 16.0, d.center().y), Pos2::new(d.right() + 16.0, d.center().y)], Stroke::new(1.0, t.divider));
                self.save_footer(ui, t, icons, shared, busy);
            });
        None
    }

    /// Export for Discord, phones or a custom size: the selection is encoded
    /// again with H.264, which every app and site plays.
    fn save_options(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let ctx = ui.ctx().clone();
        let _ = &ctx;
        ui.label(theme::caption("Size").color(t.text2));
        let w = ui.available_width();
        theme::combo("preset").width(w).selected_text(self.preset.label()).show_ui(ui, |ui| {
            for p in Preset::ALL {
                ui.selectable_value(&mut self.preset, p, p.label()).on_hover_text(p.hint());
            }
        });
        if self.preset == Preset::Custom {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Largest size").color(t.text));
                ui.add(egui::DragValue::new(&mut self.custom_mb).range(1.0..=4000.0).speed(1.0).suffix(" MB"));
            });
        }
        ui.add_space(2.0);
        ui.label(theme::caption("Shape").color(t.text2));
        let w = ui.available_width();
        let shapes = [("Landscape", "As recorded"), ("Vertical 9:16", "For Shorts, TikTok and Reels")];
        if let Some(i) = theme::segmented(ui, t, &shapes, usize::from(self.vertical), w) {
            self.vertical = i == 1;
        }

    }

    /// Share > Copy for Discord: encodes a 10 MB copy and puts it on the
    /// clipboard when it is done.
    fn copy_for_discord(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let secs = (self.out_point - self.in_point).max(0.1);
        let mb = f64::from(shared.cfg.share.discord_mb.max(1));
        let (k, h) = edit::size_target(mb, secs, 128);
        let soft = k < 800;
        let spec = edit::EncodeSpec { codec: crate::config::Codec::H264, bitrate_kbps: k, cbr: true, max_height: h, vertical: self.vertical.then_some(self.crop_center) };
        let seg = vec![Segment { path: self.path.clone(), start: self.in_point, end: self.out_point }];
        let out = edit::edited_name(&self.path, "discord");
        let mix = Mix { game: self.game_gain, mic: self.mic_gain, keep_separate: false, bitrate_kbps: 128, music: None };
        shared.spawn_job(ctx, JobKind::Export, "Making a copy for Discord".into(), move |p| match edit::export(&seg, &out, &mix, &edit::VideoMode::Encode(spec), p) {
            Ok(path) => JobDone::ExportedCopied(path, soft),
            Err(e) => JobDone::Failed(format!("Copy for Discord failed: {e}")),
        });
    }

    /// The pinned footer: estimate, notes and "Save a copy".
    fn save_footer(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, shared: &mut Shared, busy: bool) {
        let ctx = ui.ctx().clone();
        // What comes out.
        let secs = (self.out_point - self.in_point).max(0.1);
        let size_mb = self.preset.size_mb(self.custom_mb);
        let small = size_mb.is_some();
        let audio_kbps = if small { 128 } else { shared.cfg.audio.bitrate_kbps * if self.keep_separate && self.has_mic { 3 } else { 1 } };
        // Original + landscape copies the video (or encodes it again for an exact cut).
        let copy = !small && !self.vertical;
        let spec = match size_mb {
            Some(mb) => {
                let (k, h) = edit::size_target(mb, secs, audio_kbps);
                edit::EncodeSpec { codec: crate::config::Codec::H264, bitrate_kbps: k, cbr: true, max_height: h, vertical: self.vertical.then_some(self.crop_center) }
            }
            None => {
                // Same quality per pixel as the source; a crop keeps fewer pixels.
                let mut spec = edit::EncodeSpec { codec: crate::config::Codec::H264, bitrate_kbps: 0, cbr: false, max_height: None, vertical: self.vertical.then_some(self.crop_center) };
                let (_, (ow, oh)) = edit::plan_frame(self.video_size.0, self.video_size.1, &spec);
                let area = f64::from(ow * oh) / f64::from((self.video_size.0 * self.video_size.1).max(1));
                let h264 = if self.codec.starts_with("H.264") { 1.0 } else { 1.4 };
                spec.bitrate_kbps = ((f64::from(self.video_kbps.max(2000)) * area * h264) as u32).clamp(1_500, 150_000);
                spec
            }
        };
        let (_, (ow, oh)) = edit::plan_frame(self.video_size.0, self.video_size.1, &spec);
        let line = if copy {
            let mb = self.size_bytes as f64 / 1e6 * secs / self.duration.max(0.1);
            format!("About {mb:.0} MB  \u{00b7}  {} \u{00d7} {}", self.video_size.0, self.video_size.1)
        } else {
            let est_mb = (f64::from(spec.bitrate_kbps) + f64::from(audio_kbps)) * secs / 8000.0;
            format!("About {est_mb:.0} MB  \u{00b7}  {ow} \u{00d7} {oh}")
        };
        ui.label(theme::caption(line).color(t.text2));
        // Always one note line, so the button never moves.
        let (note, color) = if !copy && spec.bitrate_kbps < 800 {
            ("Long for this size: the picture will be soft. Trim it shorter.".to_string(), t.caution)
        } else if !copy {
            ("Encoded as H.264, so it plays on every phone and site.".to_string(), t.text3)
        } else if self.exact {
            (format!("Encoded again as {} to cut on the exact frame.", self.codec), t.text3)
        } else {
            (format!("Copied as {}, so nothing is lost.", self.codec), t.text3)
        };
        ui.add(egui::Label::new(theme::caption(note).color(color)).truncate());
        let b = egui::Button::new(theme::label_with_icon(icons, icon::SAVE, "Save a copy")).min_size(Vec2::new(ui.available_width(), theme::CONTROL_H));
        if ui.add_enabled(!busy, b).clicked() {
            let seg = vec![Segment { path: self.path.clone(), start: self.in_point, end: self.out_point }];
            let suffix = match (small, self.vertical) {
                (_, true) => "vertical",
                (true, false) => "small",
                (false, false) => "edit",
            };
            let out = edit::edited_name(&self.path, suffix);
            let mix = Mix {
                game: self.game_gain,
                mic: self.mic_gain,
                // Size targets keep only the mixed track; every bit goes to the picture.
                keep_separate: self.keep_separate && !small,
                bitrate_kbps: if small { 128 } else { shared.cfg.audio.bitrate_kbps },
                music: None,
            };
            let exact = self.exact;
            let (src, label) = (self.path.clone(), format!("Saving {}", theme::fmt_short(secs)));
            shared.spawn_job(&ctx, JobKind::Export, label, move |p| {
                let video = if !copy {
                    edit::VideoMode::Encode(spec)
                } else if exact {
                    match edit::encode_spec(&src, secs, mix.bitrate_kbps * 3, &edit::ExportChoice::default()) {
                        Ok(s) => edit::VideoMode::Encode(s),
                        Err(e) => return JobDone::Failed(format!("Save failed: {e}")),
                    }
                } else {
                    edit::VideoMode::Copy
                };
                match edit::export(&seg, &out, &mix, &video, p) {
                    Ok(path) => JobDone::Exported(path),
                    Err(e) => JobDone::Failed(format!("Save failed: {e}")),
                }
            });
        }
    }
}

/// One collapsible inspector section (one level only): a full-width
/// header row with a chevron.
fn section(ui: &mut egui::Ui, t: &Tokens, title: &str, open_default: bool, body: impl FnOnce(&mut egui::Ui)) {
    let id = ui.id().with(("section", title));
    let mut open = ui.data_mut(|d| *d.get_temp_mut_or(id, open_default));
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r.expand2(Vec2::new(8.0, 0.0)), CornerRadius::same(theme::R_CONTROL), t.subtle_hover);
    }
    theme::focus_ring(ui, t, &resp, r, f32::from(theme::R_CONTROL));
    ui.painter().text(Pos2::new(r.left(), r.center().y), Align2::LEFT_CENTER, title, FontId::new(14.0, FontFamily::Name(theme::SEMIBOLD.into())), t.text);
    let chevron = if open { icon::CHEVRON_UP } else { icon::CHEVRON_DOWN };
    ui.painter().text(Pos2::new(r.right() - 4.0, r.center().y), Align2::RIGHT_CENTER, chevron, FontId::new(12.0, FontFamily::Name(theme::ICONS.into())), t.text2);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    if open {
        ui.add_space(2.0);
        body(ui);
        ui.add_space(8.0);
    }
    ui.add(egui::Separator::default().spacing(4.0));
}

/// Peak level per slice of the clip (game and mic together), 0..1.
fn waveform(tracks: &AudioTracks) -> Vec<f32> {
    let len = tracks.game.as_ref().map_or(0, Vec::len).max(tracks.mic.as_ref().map_or(0, Vec::len));
    if len == 0 {
        return Vec::new();
    }
    let per = len.div_ceil(WAVE_BUCKETS).max(2);
    let mut out: Vec<f32> = (0..len.div_ceil(per))
        .map(|b| {
            let range = b * per..((b + 1) * per).min(len);
            let peak = |v: &Option<Vec<i16>>| v.as_ref().map_or(0, |v| v.get(range.clone()).map_or(0, |s| s.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0)));
            f32::from(peak(&tracks.game).max(peak(&tracks.mic))) / 32768.0
        })
        .collect();
    // Scale so the loudest part fills the strip; quiet clips stay readable.
    let max = out.iter().cloned().fold(0.0f32, f32::max).max(0.02);
    for v in &mut out {
        *v = (*v / max).sqrt();
    }
    out
}

#[derive(Clone, Copy)]
enum Glyph {
    SetIn,
    SetOut,
    Prev,
    Next,
}

/// A 32 px subtle button with a drawn transport glyph, so every control in
/// the row shares one size and baseline.
fn glyph_button(ui: &mut egui::Ui, t: &Tokens, g: Glyph) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(theme::CONTROL_H), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(theme::R_CONTROL), t.subtle_hover);
    }
    theme::focus_ring(ui, t, &resp, r, f32::from(theme::R_CONTROL));
    let c = r.center();
    let s = Stroke::new(1.5, t.text);
    match g {
        Glyph::SetIn | Glyph::SetOut => {
            // A bracket: vertical bar with short arms toward the inside.
            let dir = if matches!(g, Glyph::SetIn) { 1.0 } else { -1.0 };
            let x = c.x - 3.0 * dir;
            p.line_segment([Pos2::new(x, c.y - 7.0), Pos2::new(x, c.y + 7.0)], s);
            p.line_segment([Pos2::new(x, c.y - 7.0), Pos2::new(x + 5.0 * dir, c.y - 7.0)], s);
            p.line_segment([Pos2::new(x, c.y + 7.0), Pos2::new(x + 5.0 * dir, c.y + 7.0)], s);
        }
        Glyph::Prev | Glyph::Next => {
            let dir = if matches!(g, Glyph::Next) { 1.0 } else { -1.0 };
            p.line_segment([Pos2::new(c.x - 2.5 * dir, c.y - 5.0), Pos2::new(c.x + 2.5 * dir, c.y)], s);
            p.line_segment([Pos2::new(c.x + 2.5 * dir, c.y), Pos2::new(c.x - 2.5 * dir, c.y + 5.0)], s);
        }
    }
    let name = match g {
        Glyph::SetIn => "Set start",
        Glyph::SetOut => "Set end",
        Glyph::Prev => "Previous frame",
        Glyph::Next => "Next frame",
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn draw_play(p: &egui::Painter, c: Pos2, s: f32, color: Color32) {
    let pts = vec![Pos2::new(c.x - s * 0.6, c.y - s), Pos2::new(c.x - s * 0.6, c.y + s), Pos2::new(c.x + s, c.y)];
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A small rounded time label (like a ToolTip) centred on `at`.
fn bubble(p: &egui::Painter, t: &Tokens, at: Pos2, text: &str) {
    let galley = p.layout_no_wrap(text.to_string(), FontId::proportional(12.0), t.text);
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let r = Rect::from_center_size(at, size);
    let fill = if t.dark { Color32::from_gray(44) } else { Color32::from_gray(250) };
    p.rect(r, CornerRadius::same(theme::R_CONTROL), fill, Stroke::new(1.0, t.control_stroke), StrokeKind::Inside);
    p.galley(r.min + Vec2::new(6.0, 3.0), galley, t.text);
}
