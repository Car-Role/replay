//! Player, timeline with in/out points, audio mix and share actions.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, Color32, Key, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Vec2};

use super::{JobDone, JobKind, Shared, fmt_time};
use crate::config::Config;
use crate::edit::{self, Mix, Segment};
use crate::media::{AudioTracks, Frame, WallClock};
use crate::sys::player::{AudioOut, VideoDecoder};

pub enum Action {
    Delete(PathBuf),
}

pub struct Editor {
    path: PathBuf,
    duration: f64,
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
    shown_pts: f64,
    in_point: f64,
    out_point: f64,
    game_gain: u32,
    mic_gain: u32,
    keep_separate: bool,
    dragging: Option<Handle>,
}

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    In,
    Out,
    Playhead,
}

impl Editor {
    pub fn open(ctx: &egui::Context, path: &Path, cfg: &Config) -> Self {
        let info = crate::mux::reader::read(path);
        let (duration, has_game, has_mic, error) = match &info {
            Ok(m) => {
                let (g, mi) = edit::audio_sources(m);
                (m.duration(), g.is_some(), mi.is_some(), None)
            }
            Err(e) => (0.0, false, false, Some(format!("Cannot read this file: {e}"))),
        };
        let wake_ctx = ctx.clone();
        let video = VideoDecoder::open(path, move || wake_ctx.request_repaint());
        video.seek(0.0, 0);

        // Decode the audio tracks once in the background.
        let audio_rx = info.ok().map(|m| {
            let (tx, rx) = mpsc::channel();
            let p = path.to_path_buf();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                }
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
            shown_pts: 0.0,
            in_point: 0.0,
            out_point: duration,
            game_gain: cfg.audio.game_volume,
            mic_gain: cfg.audio.microphone_volume,
            keep_separate: true,
            dragging: None,
        }
    }

    fn position(&self) -> f64 {
        match &self.audio {
            Some(a) if a.frames > 0 => a.position(),
            _ => self.wall.position(),
        }
    }

    fn set_playing(&mut self, on: bool) {
        if on && self.position() >= self.out_point - 0.05 {
            self.seek(self.in_point);
        }
        self.playing = on;
        self.wall.set_playing(on);
        if let Some(a) = &self.audio {
            a.set_playing(on);
        }
    }

    fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration.max(0.0));
        self.generation += 1;
        self.video.seek(t, self.generation);
        if let Some(a) = &self.audio {
            a.seek(t);
        }
        self.wall.seek(t);
        self.pending = None;
        self.show_next = true;
        self.shown_pts = t;
    }

    fn pump(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.audio_rx
            && let Ok(r) = rx.try_recv()
        {
            self.audio_rx = None;
            match r {
                Ok(tracks) if tracks.game.is_some() || tracks.mic.is_some() => {
                    let a = AudioOut::start(tracks);
                    a.set_gains(self.game_gain, self.mic_gain);
                    a.seek(self.position());
                    a.set_playing(self.playing);
                    self.audio = Some(a);
                }
                Ok(_) => {}
                Err(e) => log::warn!("preview audio: {e}"),
            }
        }
        let now = self.position();
        if self.playing && now >= self.out_point {
            self.set_playing(false);
            self.seek(self.out_point);
        }
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
                self.shown_pts = frame.pts;
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
        if self.playing {
            ctx.request_repaint();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &mut Shared) -> Option<Action> {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        let mut action = None;

        ui.horizontal(|ui| {
            ui.heading(self.path.file_stem().unwrap_or_default().to_string_lossy());
        });
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(0xef, 0x53, 0x50), e);
            return None;
        }

        // Video area: aspect-fit in the space above the controls.
        let controls_h = 210.0;
        let avail = ui.available_size() - Vec2::new(0.0, controls_h);
        let (vw, vh) = self.texture.as_ref().map(|t| (t.size()[0] as f32, t.size()[1] as f32)).unwrap_or((16.0, 9.0));
        let scale = (avail.x / vw).min(avail.y.max(120.0) / vh);
        let size = Vec2::new(vw * scale, vh * scale);
        ui.vertical_centered(|ui| {
            let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
            ui.painter().rect_filled(rect, 4.0, Color32::BLACK);
            if let Some(t) = &self.texture {
                ui.painter().image(t.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            if resp.clicked() {
                let p = !self.playing;
                self.set_playing(p);
            }
        });

        ui.add_space(6.0);
        self.timeline(ui);

        // Transport.
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "Pause" } else { "Play" }).clicked() {
                let p = !self.playing;
                self.set_playing(p);
            }
            let pos = if self.playing { self.position() } else { self.shown_pts };
            ui.monospace(format!("{} / {}", fmt_time(pos), fmt_time(self.duration)));
            ui.separator();
            if ui.button("Set start  [I]").clicked() {
                self.in_point = pos.min(self.out_point - 0.1);
            }
            if ui.button("Set end  [O]").clicked() {
                self.out_point = pos.max(self.in_point + 0.1);
            }
            if ui.button("Reset").clicked() {
                self.in_point = 0.0;
                self.out_point = self.duration;
            }
            ui.label(RichText::new(format!("Selection {}", fmt_time(self.out_point - self.in_point))).weak());
        });

        // Keyboard shortcuts (when no text field has focus).
        if !ctx.egui_wants_keyboard_input() {
            let (space, i, o, left, right) = ctx.input(|inp| {
                (
                    inp.key_pressed(Key::Space),
                    inp.key_pressed(Key::I),
                    inp.key_pressed(Key::O),
                    inp.key_pressed(Key::ArrowLeft),
                    inp.key_pressed(Key::ArrowRight),
                )
            });
            let pos = if self.playing { self.position() } else { self.shown_pts };
            if space {
                let p = !self.playing;
                self.set_playing(p);
            }
            if i {
                self.in_point = pos.min(self.out_point - 0.1);
            }
            if o {
                self.out_point = pos.max(self.in_point + 0.1);
            }
            if left {
                self.seek(pos - 5.0);
            }
            if right {
                self.seek(pos + 5.0);
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Audio").strong());
            let mut changed = false;
            if self.has_game {
                ui.add_space(8.0);
                ui.label("Game");
                changed |= ui.add(egui::Slider::new(&mut self.game_gain, 0..=200).suffix("%")).changed();
            }
            if self.has_mic {
                ui.add_space(8.0);
                ui.label("Mic");
                changed |= ui.add(egui::Slider::new(&mut self.mic_gain, 0..=200).suffix("%")).changed();
                ui.checkbox(&mut self.keep_separate, "Keep separate tracks");
            }
            if !self.has_game && !self.has_mic {
                ui.label(RichText::new("No audio").weak());
            }
            if changed && let Some(a) = &self.audio {
                a.set_gains(self.game_gain, self.mic_gain);
            }
        });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let busy = !shared.jobs.is_empty();
            if ui.add_enabled(!busy, egui::Button::new("Save edit")).on_hover_text("Saves the selection with the audio mix as a new file. Video is not re-encoded, so the start snaps to the nearest keyframe before it (within 0.5 s).").clicked() {
                let seg = vec![Segment { path: self.path.clone(), start: self.in_point, end: self.out_point }];
                let out = edit::edited_name(&self.path, "edit");
                let mix = Mix { game: self.game_gain, mic: self.mic_gain, keep_separate: self.keep_separate, bitrate_kbps: shared.cfg.audio.bitrate_kbps };
                shared.spawn_job(&ctx, JobKind::Export, "Saving edit".into(), move |p| match edit::export(&seg, &out, &mix, p) {
                    Ok(path) => JobDone::Exported(path),
                    Err(e) => JobDone::Failed(format!("Save failed: {e}")),
                });
            }
            if ui.button("Copy file").on_hover_text("Paste with Ctrl+V into Discord, Slack or a folder.").clicked() {
                if crate::sys::shell::copy_file(&self.path) {
                    shared.toast("File copied. Paste it with Ctrl+V.", false);
                } else {
                    shared.toast("Could not use the clipboard.", true);
                }
            }
            let has_login = !shared.cfg.share.streamable_email.is_empty() && !shared.cfg.share.streamable_password_protected.is_empty();
            let upload = ui.add_enabled(!busy, egui::Button::new("Upload to Streamable"));
            let upload = if has_login { upload } else { upload.on_hover_text("Add your Streamable login in Settings > Sharing first.") };
            if upload.clicked() {
                if has_login {
                    shared.start_upload(&ctx, self.path.clone());
                } else {
                    shared.toast("Add your Streamable login in Settings > Sharing first.", true);
                }
            }
            if ui.button("Show in folder").clicked() {
                crate::sys::shell::reveal(&self.path);
            }
            if ui.button("Delete").clicked() {
                action = Some(Action::Delete(self.path.clone()));
            }
        });
        if let Some(url) = shared.uploads.get(self.path.to_string_lossy().as_ref()).cloned() {
            ui.horizontal(|ui| {
                ui.label("Shared at");
                ui.hyperlink(&url);
                if ui.small_button("Copy link").clicked() {
                    crate::sys::shell::copy_text(&url);
                    shared.toast("Link copied.", false);
                }
            });
        }
        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(8));
        }
        action
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        let dur = self.duration.max(0.001);
        let x_of = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
        let t_of = |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64) * dur;
        let bar = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 10.0), Pos2::new(rect.right(), rect.bottom() - 6.0));
        p.rect_filled(bar, 4.0, ui.visuals().extreme_bg_color);
        let sel = Rect::from_min_max(Pos2::new(x_of(self.in_point), bar.top()), Pos2::new(x_of(self.out_point), bar.bottom()));
        p.rect_filled(sel, 4.0, Color32::from_rgba_unmultiplied(0xe5, 0x39, 0x35, 70));
        let pos = if self.playing { self.position() } else { self.shown_pts };
        let accent = Color32::from_rgb(0xe5, 0x39, 0x35);
        for (t, _) in [(self.in_point, Handle::In), (self.out_point, Handle::Out)] {
            let x = x_of(t);
            p.line_segment([Pos2::new(x, rect.top() + 6.0), Pos2::new(x, rect.bottom() - 2.0)], Stroke::new(3.0, accent));
        }
        let px = x_of(pos);
        p.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(2.0, ui.visuals().strong_text_color()));
        p.circle_filled(Pos2::new(px, rect.top() + 4.0), 4.0, ui.visuals().strong_text_color());

        if let Some(m) = resp.interact_pointer_pos() {
            if resp.drag_started() || resp.clicked() {
                let near = |t: f64| (x_of(t) - m.x).abs() < 8.0;
                self.dragging = Some(if near(self.in_point) {
                    Handle::In
                } else if near(self.out_point) {
                    Handle::Out
                } else {
                    Handle::Playhead
                });
            }
            let t = t_of(m.x);
            match self.dragging {
                Some(Handle::In) => self.in_point = t.min(self.out_point - 0.1).max(0.0),
                Some(Handle::Out) => self.out_point = t.max(self.in_point + 0.1).min(dur),
                Some(Handle::Playhead) if (t - self.shown_pts).abs() > 0.05 || resp.clicked() => self.seek(t),
                Some(Handle::Playhead) | None => {}
            }
        }
        if resp.drag_stopped() || resp.clicked() {
            self.dragging = None;
        }
        if let Some(h) = resp.hover_pos() {
            resp.on_hover_text_at_pointer(fmt_time(t_of(h.x)));
        }
    }
}
