//! Clip editor: preview, filmstrip trim bar that scrubs the preview while
//! you drag, audio mix, save and share.

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
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    In,
    Out,
    Playhead,
}

const THUMBS: usize = 24;
const THUMB_H: usize = 64;
const STRIP_H: f32 = 64.0;
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
    last_scrub: Instant,
    film_rx: Option<Receiver<Thumb>>,
    film: Vec<Option<TextureHandle>>,
}

impl Editor {
    pub fn open(ctx: &egui::Context, path: &Path, cfg: &Config) -> Self {
        let info = crate::mux::reader::read(path);
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
            last_scrub: Instant::now(),
            film_rx,
            film: (0..THUMBS).map(|_| None).collect(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
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

    pub fn ui(&mut self, ui: &mut egui::Ui, shared: &mut Shared) -> Option<Action> {
        let ctx = ui.ctx().clone();
        let t = theme::tokens(&ctx);
        let icons = shared.icons;
        self.pump(&ctx);
        self.keyboard(&ctx);
        let mut action = None;

        // Header: back, title, details.
        ui.horizontal(|ui| {
            if theme::subtle(ui, theme::label_with_icon(icons, icon::BACK, if icons { "" } else { "Back" }))
                .on_hover_text("Back to clips")
                .clicked()
            {
                action = Some(Action::Back);
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                let name = self.path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                ui.add(egui::Label::new(theme::subtitle(name).color(t.text)).truncate());
                let game = self.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.label(
                    theme::caption(format!("{game}  ·  {}  ·  {:.1} MB", theme::fmt_short(self.duration), self.size_bytes as f64 / 1e6))
                        .color(t.text2),
                );
            });
        });
        ui.add_space(12.0);
        if let Some(e) = &self.error {
            theme::card(&t).show(ui, |ui| {
                ui.label(RichText::new(e).color(t.critical));
            });
            return action;
        }

        let side_w = 320.0;
        if ui.available_width() >= 980.0 {
            ui.horizontal_top(|ui| {
                let main_w = ui.available_width() - side_w - 16.0;
                let h = ui.available_height();
                ui.allocate_ui_with_layout(Vec2::new(main_w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    self.main_column(ui, &t, icons);
                });
                ui.add_space(16.0);
                ui.allocate_ui_with_layout(Vec2::new(side_w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    egui::ScrollArea::vertical().id_salt("side").show(ui, |ui| {
                        if let Some(a) = self.side_panel(ui, &t, icons, shared) {
                            action = Some(a);
                        }
                    });
                });
            });
        } else {
            egui::ScrollArea::vertical().id_salt("narrow").show(ui, |ui| {
                self.main_column(ui, &t, icons);
                ui.add_space(16.0);
                if let Some(a) = self.side_panel(ui, &t, icons, shared) {
                    action = Some(a);
                }
            });
        }
        action
    }

    fn main_column(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        // Video: aspect-fit, leaving room for the transport and the trim bar.
        let reserve = 230.0;
        let avail = Vec2::new(ui.available_width(), (ui.available_height() - reserve).max(180.0));
        let (vw, vh) = self.texture.as_ref().map(|t| (t.size()[0] as f32, t.size()[1] as f32)).unwrap_or((16.0, 9.0));
        let scale = (avail.x / vw).min(avail.y / vh);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail.x, vh * scale), Sense::click());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(theme::R_CARD), Color32::BLACK);
        if let Some(tex) = &self.texture {
            let img = Rect::from_center_size(rect.center(), Vec2::new(vw * scale, vh * scale));
            p.image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        if resp.clicked() {
            let on = !self.playing;
            self.set_playing(on);
        }
        if !self.playing && self.drag.is_none() && resp.hovered() {
            let c = rect.center();
            p.circle_filled(c, 32.0, Color32::from_black_alpha(140));
            if icons {
                p.text(c, Align2::CENTER_CENTER, icon::PLAY, FontId::new(26.0, FontFamily::Name(theme::ICONS.into())), Color32::WHITE);
            } else {
                draw_play(&p, c, 14.0, Color32::WHITE);
            }
        }

        ui.add_space(12.0);
        self.transport(ui, t, icons);
        ui.add_space(4.0);
        self.trim_bar(ui, t);
    }

    fn transport(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool) {
        // One full-width row: time on the left, controls centred, selection on the right.
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
        ui.painter().text(
            Pos2::new(r.left(), r.center().y),
            Align2::LEFT_CENTER,
            format!("{}  /  {}", theme::fmt_time(self.cursor), theme::fmt_time(self.duration)),
            FontId::proportional(14.0),
            t.text,
        );
        let total = 32.0 * 4.0 + 44.0 + 8.0 * 4.0;
        let controls = Rect::from_center_size(r.center(), Vec2::new(total, 44.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(controls).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui2 = &mut child;
        if theme::subtle(ui2, theme::label_with_icon(icons, icon::SKIP_BACK, if icons { "" } else { "|<" })).on_hover_text("Go to start (Home)").clicked() {
            if self.playing {
                self.set_playing(false);
            }
            let i = self.in_point;
            self.seek(i);
        }
        if theme::subtle(ui2, RichText::new("[").size(18.0)).on_hover_text("Set start at the playhead (I)").clicked() {
            self.in_point = self.cursor.min(self.out_point - 0.1).max(0.0);
        }
        let (b, resp) = ui2.allocate_exact_size(Vec2::splat(44.0), Sense::click());
        let fill = if resp.hovered() { t.accent_hover } else { t.accent };
        ui2.painter().circle_filled(b.center(), 22.0, fill);
        if icons {
            let g = if self.playing { icon::PAUSE } else { icon::PLAY };
            ui2.painter().text(b.center(), Align2::CENTER_CENTER, g, FontId::new(18.0, FontFamily::Name(theme::ICONS.into())), t.on_accent);
        } else if self.playing {
            let c = b.center();
            ui2.painter().rect_filled(Rect::from_center_size(c - Vec2::new(4.0, 0.0), Vec2::new(4.0, 14.0)), 1.0, t.on_accent);
            ui2.painter().rect_filled(Rect::from_center_size(c + Vec2::new(4.0, 0.0), Vec2::new(4.0, 14.0)), 1.0, t.on_accent);
        } else {
            draw_play(ui2.painter(), b.center(), 8.0, t.on_accent);
        }
        if resp.on_hover_text("Play the selection (Space)").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            let on = !self.playing;
            self.set_playing(on);
        }
        if theme::subtle(ui2, RichText::new("]").size(18.0)).on_hover_text("Set end at the playhead (O)").clicked() {
            self.out_point = self.cursor.max(self.in_point + 0.1).min(self.duration);
        }
        if theme::subtle(ui2, theme::label_with_icon(icons, icon::SKIP_FWD, if icons { "" } else { ">|" })).on_hover_text("Go to end (End)").clicked() {
            if self.playing {
                self.set_playing(false);
            }
            let o = self.out_point;
            self.seek(o);
        }
        // Selection length on the right.
        ui.painter().text(
            Pos2::new(r.right(), r.center().y),
            Align2::RIGHT_CENTER,
            format!("Selection {}", theme::fmt_time(self.out_point - self.in_point)),
            FontId::proportional(12.0),
            t.text2,
        );
    }

    /// Filmstrip with trim handles. Dragging a handle or the playhead shows
    /// that frame in the preview.
    fn trim_bar(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let dur = self.duration.max(0.001);
        let (outer, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), STRIP_H + 56.0), Sense::click_and_drag());
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
            let Some(tex) = self.film.get(src.min(THUMBS - 1)).and_then(|t| t.as_ref()) else { continue };
            let r = Rect::from_min_size(Pos2::new(strip.left() + slot * i as f32, strip.top()), Vec2::new(slot, STRIP_H));
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
        p.rect_stroke(sel, CornerRadius::ZERO, Stroke::new(3.0, t.accent), StrokeKind::Inside);
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
        let head = if t.dark { Color32::WHITE } else { Color32::from_gray(24) };
        let xp = x_of(self.cursor);
        p.line_segment([Pos2::new(xp, strip.top() - 8.0), Pos2::new(xp, strip.bottom() + 4.0)], Stroke::new(2.0, head));
        p.circle_filled(Pos2::new(xp, strip.top() - 9.0), 5.0, head);

        // Ruler under the strip.
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0].into_iter().find(|s| dur / s <= 10.0).unwrap_or(600.0);
        let mut s = 0.0;
        while s <= dur + 1e-6 {
            let x = x_of(s);
            p.line_segment([Pos2::new(x, strip.bottom() + 6.0), Pos2::new(x, strip.bottom() + 10.0)], Stroke::new(1.0, t.text3));
            p.text(Pos2::new(x, strip.bottom() + 12.0), Align2::CENTER_TOP, theme::fmt_short(s), FontId::proportional(12.0), t.text3);
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

    fn side_panel(&mut self, ui: &mut egui::Ui, t: &Tokens, icons: bool, shared: &mut Shared) -> Option<Action> {
        let mut action = None;
        let ctx = ui.ctx().clone();
        let full = |ui: &egui::Ui| Vec2::new(ui.available_width(), theme::CONTROL_H);

        theme::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::body_strong("Audio").color(t.text));
            ui.add_space(4.0);
            let mut changed = false;
            let mut row = |ui: &mut egui::Ui, c: char, label: &str, v: &mut u32| {
                ui.horizontal(|ui| {
                    if icons {
                        ui.label(theme::glyph(c, 16.0).color(t.text2));
                    }
                    ui.add_sized(Vec2::new(40.0, 20.0), egui::Label::new(RichText::new(label).color(t.text)));
                    ui.spacing_mut().slider_width = (ui.available_width() - 48.0).max(80.0);
                    changed |= ui.add(egui::Slider::new(v, 0..=200).show_value(false)).changed();
                    ui.label(theme::caption(format!("{v}%")).color(t.text2));
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
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Keep separate tracks").color(t.text));
                        ui.label(theme::caption("Game and mic stay editable").color(t.text2));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme::toggle(ui, t, &mut self.keep_separate);
                    });
                });
            }
        });
        ui.add_space(12.0);

        let busy = !shared.jobs.is_empty();
        theme::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::body_strong("Save").color(t.text));
            ui.label(
                theme::caption(format!(
                    "Saves the {} selection as a new clip with this audio mix. Video is copied, not re-encoded, so the start moves to the keyframe just before it (at most 0.5 s).",
                    theme::fmt_time(self.out_point - self.in_point)
                ))
                .color(t.text2),
            );
            ui.add_space(8.0);
            let text = theme::label_with_icon(icons, icon::CUT, "Save trimmed clip").color(t.on_accent);
            let b = egui::Button::new(text).fill(t.accent).stroke(Stroke::NONE).min_size(full(ui));
            if ui.add_enabled(!busy, b).clicked() {
                let seg = vec![Segment { path: self.path.clone(), start: self.in_point, end: self.out_point }];
                let out = edit::edited_name(&self.path, "edit");
                let mix = Mix { game: self.game_gain, mic: self.mic_gain, keep_separate: self.keep_separate, bitrate_kbps: shared.cfg.audio.bitrate_kbps };
                shared.spawn_job(&ctx, JobKind::Export, "Saving clip".into(), move |p| match edit::export(&seg, &out, &mix, p) {
                    Ok(path) => JobDone::Exported(path),
                    Err(e) => JobDone::Failed(format!("Save failed: {e}")),
                });
            }
        });
        ui.add_space(12.0);

        theme::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(theme::body_strong("Share").color(t.text));
            ui.add_space(4.0);
            if ui
                .add(egui::Button::new(theme::label_with_icon(icons, icon::COPY, "Copy file")).min_size(full(ui)))
                .on_hover_text("Then paste it into Discord, Slack or a folder")
                .clicked()
            {
                if crate::sys::shell::copy_file(&self.path) {
                    shared.toast("File copied. Paste it with Ctrl+V.", false);
                } else {
                    shared.toast("Could not use the clipboard.", true);
                }
            }
            let has_login = !shared.cfg.share.streamable_email.is_empty() && !shared.cfg.share.streamable_password_protected.is_empty();
            let up = ui.add_enabled(!busy, egui::Button::new(theme::label_with_icon(icons, icon::UPLOAD, "Upload to Streamable")).min_size(full(ui)));
            let up = if has_login { up } else { up.on_hover_text("Add your Streamable login in Settings first") };
            if up.clicked() {
                if has_login {
                    shared.start_upload(&ctx, self.path.clone());
                } else {
                    shared.toast("Add your Streamable login in Settings > Sharing first.", true);
                }
            }
            if let Some(url) = shared.uploads.get(self.path.to_string_lossy().as_ref()).cloned() {
                ui.horizontal(|ui| {
                    ui.hyperlink_to(theme::caption(url.trim_start_matches("https://")), &url);
                    if theme::subtle(ui, theme::label_with_icon(icons, icon::COPY, if icons { "" } else { "Copy" })).on_hover_text("Copy link").clicked() {
                        crate::sys::shell::copy_text(&url);
                        shared.toast("Link copied.", false);
                    }
                });
            }
            if ui.add(egui::Button::new(theme::label_with_icon(icons, icon::FOLDER, "Show in folder")).min_size(full(ui))).clicked() {
                crate::sys::shell::reveal(&self.path);
            }
            ui.add_space(4.0);
            let del = egui::Button::new(theme::label_with_icon(icons, icon::DELETE, "Delete clip").color(t.critical))
                .frame_when_inactive(false)
                .min_size(full(ui));
            if ui.add(del).clicked() {
                action = Some(Action::Delete(self.path.clone()));
            }
        });
        ui.add_space(12.0);
        ui.label(theme::caption("Space play  ·  I / O set start and end  ·  ← → step  ·  Shift for 1 s  ·  Home / End").color(t.text3));
        action
    }
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
