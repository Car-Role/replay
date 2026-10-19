//! The library window: a navigation rail (base layer) and pages on the
//! content layer: the clip gallery, the editor, and settings. It runs as its
//! own process (`replay --library`) and exits when closed, so the resident
//! recorder never carries UI code or memory.

mod editor;
mod filmstrip;
mod settings;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, Vec2,
};

use crate::config::Config;
use editor::Editor;
use theme::{Tokens, icon};

pub fn run(open: Option<PathBuf>, settings: bool) {
    let icon = egui::IconData { rgba: include_bytes!("../../assets/generated/icon-64.rgba").to_vec(), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Replay")
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Replay", options, Box::new(move |cc| Ok(Box::new(LibraryApp::new(cc, open, settings))))) {
        log::error!("library window failed: {e}");
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Clips,
    Editor,
    Settings,
}

#[derive(Clone)]
pub struct ClipEntry {
    pub path: PathBuf,
    pub game: String,
    pub name: String,
    pub modified: SystemTime,
    pub size: u64,
    pub duration: f64,
}

pub enum JobKind {
    Export,
    Upload,
    Update,
}

pub struct Job {
    pub id: usize,
    pub kind: JobKind,
    pub label: String,
    pub progress: Arc<Mutex<f32>>,
}

pub enum JobDone {
    Exported(PathBuf),
    Uploaded(PathBuf, String),
    Update(settings::UpdateState),
    Failed(String),
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
    link: Option<String>,
}

pub struct Shared {
    pub cfg: Config,
    pub jobs: Vec<Job>,
    pub job_tx: Sender<(usize, JobDone)>,
    pub next_job: usize,
    pub uploads: HashMap<String, String>,
    /// True when the Segoe Fluent Icons font is available.
    pub icons: bool,
    toast: Option<Toast>,
}

impl Shared {
    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast { text: text.into(), error, at: Instant::now(), link: None });
    }

    pub fn toast_link(&mut self, text: impl Into<String>, link: String) {
        self.toast = Some(Toast { text: text.into(), error: false, at: Instant::now(), link: Some(link) });
    }

    /// Runs `work` on a thread with a progress value the UI shows.
    pub fn spawn_job(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        label: String,
        work: impl FnOnce(&(dyn Fn(f32) + Sync)) -> JobDone + Send + 'static,
    ) {
        let progress = Arc::new(Mutex::new(0.0f32));
        let id = self.next_job;
        self.next_job += 1;
        self.jobs.push(Job { id, kind, label, progress: progress.clone() });
        let tx = self.job_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            crate::sys::init_tool();
            let p2 = progress.clone();
            let c2 = ctx.clone();
            let report = move |f: f32| {
                if let Ok(mut p) = p2.lock() {
                    *p = f;
                }
                c2.request_repaint_after(Duration::from_millis(100));
            };
            let result = work(&report);
            let _ = tx.send((id, result));
            ctx.request_repaint();
        });
    }

    pub fn start_upload(&mut self, ctx: &egui::Context, path: PathBuf) {
        let share = self.cfg.share.clone();
        let label = format!("Uploading {}", path.file_name().unwrap_or_default().to_string_lossy());
        self.spawn_job(ctx, JobKind::Upload, label, move |progress| {
            let password = crate::sys::secret::unprotect(&share.streamable_password_protected).unwrap_or_default();
            let r = crate::share::upload_streamable(&path, &share.streamable_email, &password, &|sent, total| {
                progress(sent as f32 / total.max(1) as f32)
            });
            match r {
                Ok(url) => JobDone::Uploaded(path, url),
                Err(e) => JobDone::Failed(format!("Upload failed: {e}")),
            }
        });
    }
}

fn uploads_path() -> PathBuf {
    crate::paths::data_dir().join("uploads.json")
}

pub struct LibraryApp {
    page: Page,
    shared: Shared,
    job_rx: Receiver<(usize, JobDone)>,
    clips: Vec<ClipEntry>,
    scan_rx: Option<Receiver<Vec<ClipEntry>>>,
    last_scan: Instant,
    filter: String,
    checked: HashSet<PathBuf>,
    thumbs: HashMap<PathBuf, TextureHandle>,
    thumb_tx: Sender<PathBuf>,
    thumb_rx: Receiver<(PathBuf, usize, usize, Vec<u8>)>,
    thumb_requested: HashSet<PathBuf>,
    editor: Option<Editor>,
    settings: settings::SettingsPage,
    confirm_delete: Option<Vec<PathBuf>>,
    logo: TextureHandle,
    recorder_running: bool,
    recorder_checked: Instant,
}

impl LibraryApp {
    fn new(cc: &eframe::CreationContext, open: Option<PathBuf>, settings: bool) -> Self {
        let ctx = cc.egui_ctx.clone();
        let icons = theme::install_fonts(&ctx);
        theme::apply(&ctx);

        let (thumb_tx, thumb_req_rx) = mpsc::channel::<PathBuf>();
        let (thumb_done_tx, thumb_rx) = mpsc::channel();
        let tctx = ctx.clone();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || {
                crate::sys::init_ui();
                while let Ok(p) = thumb_req_rx.recv() {
                    if let Some((w, h, px)) = thumbnail(&p) {
                        let _ = thumb_done_tx.send((p, w, h, px));
                        tctx.request_repaint();
                    }
                }
            })
            .ok();

        let (job_tx, job_rx) = mpsc::channel();
        let cfg = Config::load();
        let uploads = std::fs::read_to_string(uploads_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let logo_img = egui::ColorImage::from_rgba_unmultiplied([64, 64], include_bytes!("../../assets/generated/icon-64.rgba"));
        let logo = ctx.load_texture("logo", logo_img, egui::TextureOptions::LINEAR);
        let mut app = Self {
            page: if settings { Page::Settings } else { Page::Clips },
            settings: settings::SettingsPage::new(&cfg),
            shared: Shared { cfg, jobs: Vec::new(), job_tx, next_job: 0, uploads, icons, toast: None },
            job_rx,
            clips: Vec::new(),
            scan_rx: None,
            last_scan: Instant::now(),
            filter: String::new(),
            checked: HashSet::new(),
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            thumb_requested: HashSet::new(),
            editor: None,
            confirm_delete: None,
            logo,
            recorder_running: crate::sys::shell::recorder_running(),
            recorder_checked: Instant::now(),
        };
        app.rescan(&ctx);
        if let Some(p) = open.filter(|p| p.exists()) {
            app.open_editor(&ctx, p);
        }
        app
    }

    fn rescan(&mut self, ctx: &egui::Context) {
        let dir = self.shared.cfg.output_dir.clone();
        let known: HashMap<PathBuf, (SystemTime, u64, f64)> =
            self.clips.iter().map(|c| (c.path.clone(), (c.modified, c.size, c.duration))).collect();
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(scan(&dir, &known));
            ctx.request_repaint();
        });
        self.scan_rx = Some(rx);
        self.last_scan = Instant::now();
    }

    fn open_editor(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.editor.as_ref().map(|e| e.path() != path).unwrap_or(true) {
            self.editor = Some(Editor::open(ctx, &path, &self.shared.cfg));
        }
        self.page = Page::Editor;
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.scan_rx
            && let Ok(list) = rx.try_recv()
        {
            self.clips = list;
            self.scan_rx = None;
            let existing: HashSet<&PathBuf> = self.clips.iter().map(|c| &c.path).collect();
            self.checked.retain(|p| existing.contains(p));
        }
        if self.scan_rx.is_none() && self.last_scan.elapsed() > Duration::from_secs(4) && self.page == Page::Clips {
            self.rescan(ctx);
        }
        if self.recorder_checked.elapsed() > Duration::from_secs(2) {
            self.recorder_running = crate::sys::shell::recorder_running();
            self.recorder_checked = Instant::now();
        }
        while let Ok((p, w, h, px)) = self.thumb_rx.try_recv() {
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &px);
            let tex = ctx.load_texture(p.to_string_lossy(), img, egui::TextureOptions::LINEAR);
            self.thumbs.insert(p, tex);
        }
        while let Ok((id, done)) = self.job_rx.try_recv() {
            self.shared.jobs.retain(|j| j.id != id);
            match done {
                JobDone::Exported(p) => {
                    self.shared.toast(format!("Saved {}", p.file_name().unwrap_or_default().to_string_lossy()), false);
                    self.rescan(ctx);
                    self.editor = None;
                    self.open_editor(ctx, p);
                }
                JobDone::Uploaded(p, url) => {
                    self.shared.uploads.insert(p.to_string_lossy().into_owned(), url.clone());
                    if let Ok(t) = serde_json::to_string_pretty(&self.shared.uploads) {
                        let _ = std::fs::write(uploads_path(), t);
                    }
                    let copied = self.shared.cfg.share.copy_link_after_upload && crate::sys::shell::copy_text(&url);
                    self.shared.toast_link(if copied { "Uploaded. The link is copied." } else { "Uploaded." }, url);
                }
                JobDone::Update(state) => {
                    if let settings::UpdateState::Ready(v) = &state {
                        self.shared.toast(format!("Replay {v} is downloaded. Restart to update in Settings."), false);
                    }
                    self.settings.update = state;
                }
                JobDone::Failed(e) => self.shared.toast(e, true),
            }
        }
        if !self.shared.jobs.is_empty() || self.shared.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    // ------------------------------------------------------------ nav rail

    fn nav(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let icons = self.shared.icons;
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(self.logo.id(), Vec2::splat(20.0))));
            ui.add_space(6.0);
            ui.label(theme::body_strong("Replay").color(t.text));
        });
        ui.add_space(14.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        if theme::nav_item(ui, t, icons, icon::LIBRARY, "Clips", self.page == Page::Clips).clicked() {
            self.page = Page::Clips;
        }
        if self.editor.is_some() && theme::nav_item(ui, t, icons, icon::CUT, "Editor", self.page == Page::Editor).clicked() {
            self.page = Page::Editor;
        }
        // Bottom of the rail: settings and recorder status, pinned to the window's bottom edge.
        let bottom = ui.ctx().content_rect().bottom();
        let r = ui.max_rect();
        let area = egui::Rect::from_min_max(egui::pos2(r.left(), bottom - 140.0), egui::pos2(r.right(), bottom));
        let mut bottom_ui = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::bottom_up(egui::Align::Min)));
        bottom_ui.spacing_mut().item_spacing.y = 4.0;
        bottom_ui.scope(|ui| {
            ui.add_space(10.0);
            egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        let c = if self.recorder_running { t.success } else { t.text3 };
                        ui.painter().circle_filled(r.center(), 4.0, c);
                        ui.label(theme::caption(if self.recorder_running { "Recorder on" } else { "Recorder off" }).color(t.text2));
                    });
                    if !self.recorder_running
                        && ui.add(egui::Button::new(theme::caption("Start recorder")).min_size(Vec2::new(0.0, 28.0))).clicked()
                        && let Ok(exe) = std::env::current_exe()
                    {
                        let _ = std::process::Command::new(exe).spawn();
                        self.recorder_checked = Instant::now() - Duration::from_secs(1);
                    }
                });
            });
            if theme::nav_item(ui, t, icons, icon::SETTINGS, "Settings", self.page == Page::Settings).clicked() {
                self.page = Page::Settings;
            }
        });
    }

    // ---------------------------------------------------------- clips page

    fn clips_page(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let ctx = ui.ctx().clone();
        let icons = self.shared.icons;
        ui.horizontal(|ui| {
            ui.label(theme::title("Clips").color(t.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::subtle(ui, theme::label_with_icon(icons, icon::REFRESH, if icons { "" } else { "Refresh" })).on_hover_text("Refresh").clicked() {
                    self.rescan(&ctx);
                }
                if theme::button(ui, theme::label_with_icon(icons, icon::FOLDER, "Open folder"), true).clicked() {
                    let _ = std::fs::create_dir_all(&self.shared.cfg.output_dir);
                    crate::sys::shell::open_path(&self.shared.cfg.output_dir);
                }
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search clips").desired_width(240.0).margin(Margin::symmetric(10, 7)));
            });
        });
        ui.add_space(16.0);

        let filter = self.filter.to_lowercase();
        let mut by_game: Vec<(String, Vec<ClipEntry>)> = Vec::new();
        for c in self.clips.iter().filter(|c| filter.is_empty() || c.name.to_lowercase().contains(&filter) || c.game.to_lowercase().contains(&filter)) {
            match by_game.iter_mut().find(|(g, _)| *g == c.game) {
                Some((_, v)) => v.push(c.clone()),
                None => by_game.push((c.game.clone(), vec![c.clone()])),
            }
        }

        if self.clips.is_empty() && self.scan_rx.is_none() {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                if icons {
                    ui.label(theme::glyph(icon::VIDEO, 40.0).color(t.text3));
                }
                ui.add_space(8.0);
                ui.label(theme::subtitle("No clips yet").color(t.text));
                ui.label(
                    RichText::new(format!(
                        "Play a game and press {} to save the last {} seconds.",
                        self.shared.cfg.hotkeys.save_focused, self.shared.cfg.replay_seconds
                    ))
                    .color(t.text2),
                );
            });
            return;
        }

        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let card_w = 264.0;
            let gap = 12.0;
            for (game, clips) in &by_game {
                ui.horizontal(|ui| {
                    ui.label(theme::subtitle(game).color(t.text));
                    ui.label(theme::caption(format!("{} {}", clips.len(), if clips.len() == 1 { "clip" } else { "clips" })).color(t.text3));
                });
                ui.add_space(4.0);
                let cols = (((ui.available_width() + gap) / (card_w + gap)).floor() as usize).max(1);
                let w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
                for row in clips.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for c in row {
                            if self.clip_card(ui, t, c, w) {
                                open = Some(c.path.clone());
                            }
                        }
                    });
                    ui.add_space(gap - 8.0);
                }
                ui.add_space(20.0);
            }
            ui.add_space(64.0);
        });
        if let Some(p) = open {
            self.open_editor(&ctx, p);
        }
    }

    /// One gallery card. Returns true when it was clicked (open in editor).
    fn clip_card(&mut self, ui: &mut egui::Ui, t: &Tokens, c: &ClipEntry, w: f32) -> bool {
        let thumb_h = (w * 9.0 / 16.0).round();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, thumb_h + 62.0), Sense::click());
        let checked = self.checked.contains(&c.path);
        let hovered = resp.hovered();
        let p = ui.painter_at(rect);
        p.rect(
            rect,
            CornerRadius::same(theme::R_CARD),
            if hovered { t.control_hover } else { t.card },
            Stroke::new(if checked { 2.0 } else { 1.0 }, if checked { t.accent } else { t.card_stroke }),
            StrokeKind::Inside,
        );

        let img = Rect::from_min_size(rect.min + Vec2::splat(1.0), Vec2::new(w - 2.0, thumb_h));
        let cr = CornerRadius { nw: theme::R_CARD - 1, ne: theme::R_CARD - 1, sw: 0, se: 0 };
        p.rect_filled(img, cr, Color32::from_gray(16));
        match self.thumbs.get(&c.path) {
            Some(tex) => {
                let [tw, th] = tex.size();
                let have = tw as f32 / th.max(1) as f32;
                let want = img.width() / img.height();
                let uv = if have > want {
                    let u = want / have;
                    Rect::from_min_max(Pos2::new((1.0 - u) / 2.0, 0.0), Pos2::new((1.0 + u) / 2.0, 1.0))
                } else {
                    let v = have / want;
                    Rect::from_min_max(Pos2::new(0.0, (1.0 - v) / 2.0), Pos2::new(1.0, (1.0 + v) / 2.0))
                };
                p.add(egui::epaint::RectShape::filled(img, cr, Color32::WHITE).with_texture(tex.id(), uv));
            }
            None => {
                if self.thumb_requested.insert(c.path.clone()) {
                    let _ = self.thumb_tx.send(c.path.clone());
                }
            }
        }
        // Duration badge on the thumbnail.
        let galley = p.layout_no_wrap(theme::fmt_short(c.duration), FontId::proportional(12.0), Color32::WHITE);
        let size = galley.size() + Vec2::new(10.0, 4.0);
        let br = Rect::from_min_size(img.right_bottom() - size - Vec2::new(8.0, 8.0), size);
        p.rect_filled(br, CornerRadius::same(theme::R_CONTROL), Color32::from_black_alpha(190));
        p.galley(br.min + Vec2::new(5.0, 2.0), galley, Color32::WHITE);

        // Title (truncated) and details.
        let tx = rect.left() + 12.0;
        let mut job = egui::text::LayoutJob::single_section(
            c.name.clone(),
            egui::TextFormat::simple(FontId::new(14.0, egui::FontFamily::Name(theme::SEMIBOLD.into())), t.text),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(w - 24.0);
        p.galley(Pos2::new(tx, img.bottom() + 10.0), p.layout_job(job), t.text);
        let uploaded = self.shared.uploads.contains_key(c.path.to_string_lossy().as_ref());
        let meta = format!("{:.1} MB{}", c.size as f64 / 1e6, if uploaded { "  ·  Uploaded" } else { "" });
        p.text(Pos2::new(tx, img.bottom() + 32.0), Align2::LEFT_TOP, meta, FontId::proportional(12.0), t.text2);

        // Selection check box on the thumbnail (on hover, or while selecting).
        let cb = Rect::from_min_size(img.min + Vec2::splat(8.0), Vec2::splat(22.0));
        let mut toggled = false;
        if hovered || checked || !self.checked.is_empty() {
            let cb_resp = ui.interact(cb, resp.id.with("check"), Sense::click());
            p.rect(
                cb,
                CornerRadius::same(theme::R_CONTROL),
                if checked { t.accent } else { Color32::from_black_alpha(150) },
                Stroke::new(1.0, if checked { t.accent } else { Color32::from_white_alpha(210) }),
                StrokeKind::Inside,
            );
            if checked {
                let c0 = cb.center();
                p.line_segment([c0 + Vec2::new(-5.0, 0.0), c0 + Vec2::new(-1.5, 4.0)], Stroke::new(2.0, t.on_accent));
                p.line_segment([c0 + Vec2::new(-1.5, 4.0), c0 + Vec2::new(5.5, -4.5)], Stroke::new(2.0, t.on_accent));
            }
            if cb_resp.on_hover_text("Select").clicked() {
                toggled = true;
                if checked {
                    self.checked.remove(&c.path);
                } else {
                    self.checked.insert(c.path.clone());
                }
            }
        }
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        resp.clicked() && !toggled
    }

    /// Floating command bar for selected clips.
    fn selection_bar(&mut self, ctx: &egui::Context, t: &Tokens, area: Rect) {
        if self.checked.is_empty() || self.page != Page::Clips {
            return;
        }
        let icons = self.shared.icons;
        let n = self.checked.len();
        egui::Area::new(egui::Id::new("selection-bar"))
            .fixed_pos(Pos2::new(area.center().x - 240.0, area.bottom() - 76.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(if t.dark { Color32::from_gray(44) } else { Color32::from_gray(252) })
                    .stroke(Stroke::new(1.0, t.control_stroke))
                    .corner_radius(CornerRadius::same(theme::R_CARD))
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_width(448.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{n} selected")).color(t.text));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if theme::subtle(ui, "Clear").clicked() {
                                    self.checked.clear();
                                }
                                if theme::button(ui, theme::label_with_icon(icons, icon::DELETE, "Delete"), true).clicked() {
                                    self.confirm_delete = Some(self.checked.iter().cloned().collect());
                                }
                                let join = theme::label_with_icon(icons, icon::JOIN, &format!("Join {n} clips"));
                                if theme::primary(ui, t, join, n >= 2).on_disabled_hover_text("Select two or more clips to join them").clicked() {
                                    self.join_checked(ctx);
                                }
                            });
                        });
                    });
            });
    }

    fn join_checked(&mut self, ctx: &egui::Context) {
        let mut list: Vec<&ClipEntry> = self.clips.iter().filter(|c| self.checked.contains(&c.path)).collect();
        list.sort_by_key(|c| c.modified);
        let segments: Vec<crate::edit::Segment> =
            list.iter().map(|c| crate::edit::Segment { path: c.path.clone(), start: 0.0, end: f64::MAX }).collect();
        let out = crate::edit::edited_name(&segments[0].path, "joined");
        let a = &self.shared.cfg.audio;
        let mix = crate::edit::Mix { game: a.game_volume, mic: a.microphone_volume, keep_separate: true, bitrate_kbps: a.bitrate_kbps };
        let label = format!("Joining {} clips", segments.len());
        self.shared.spawn_job(ctx, JobKind::Export, label, move |p| match crate::edit::export(&segments, &out, &mix, p) {
            Ok(path) => JobDone::Exported(path),
            Err(e) => JobDone::Failed(format!("Join failed: {e}")),
        });
        self.checked.clear();
    }

    // ------------------------------------------------------------- overlays

    /// Progress and messages, as Fluent InfoBars in the bottom-right corner.
    fn info_bars(&mut self, ctx: &egui::Context, t: &Tokens) {
        let icons = self.shared.icons;
        let expired = self.shared.toast.as_ref().is_some_and(|x| x.at.elapsed() > Duration::from_secs(if x.link.is_some() { 30 } else { 6 }));
        if expired {
            self.shared.toast = None;
        }
        if self.shared.jobs.is_empty() && self.shared.toast.is_none() {
            return;
        }
        let fill = if t.dark { Color32::from_gray(44) } else { Color32::from_gray(252) };
        let frame = egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0, t.control_stroke))
            .corner_radius(CornerRadius::same(theme::R_CARD))
            .shadow(ctx.style_of(ctx.theme()).visuals.popup_shadow)
            .inner_margin(Margin::same(12));
        let jobs: Vec<(String, f32)> = self.shared.jobs.iter().map(|j| (j.label.clone(), j.progress.lock().map(|p| *p).unwrap_or(0.0))).collect();
        let toast = self.shared.toast.as_ref().map(|x| (x.text.clone(), x.error, x.link.clone()));
        let mut close = false;
        egui::Area::new(egui::Id::new("info-bars"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-24.0, -24.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (label, p) in &jobs {
                    frame.show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.label(RichText::new(label).color(t.text));
                        ui.add(egui::ProgressBar::new(*p).desired_height(4.0).fill(t.accent));
                    });
                    ui.add_space(8.0);
                }
                if let Some((text, error, link)) = &toast {
                    frame.show(ui, |ui| {
                        ui.set_width(340.0);
                        ui.horizontal(|ui| {
                            if icons {
                                let (g, c) = if *error { (icon::ERROR, t.critical) } else { (icon::CHECK, t.success) };
                                ui.label(theme::glyph(g, 16.0).color(c));
                            }
                            ui.add(egui::Label::new(RichText::new(text).color(t.text)).wrap());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                if theme::subtle(ui, RichText::new("✕").size(12.0)).clicked() {
                                    close = true;
                                }
                            });
                        });
                        if let Some(link) = link {
                            ui.horizontal(|ui| {
                                ui.hyperlink_to(theme::caption(link.trim_start_matches("https://")), link);
                                if theme::subtle(ui, theme::caption("Copy link")).clicked() {
                                    crate::sys::shell::copy_text(link);
                                }
                            });
                        }
                    });
                }
            });
        if close {
            self.shared.toast = None;
        }
    }

    /// Fluent ContentDialog for deleting clips.
    fn delete_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(paths) = self.confirm_delete.clone() else { return };
        let screen = ctx.content_rect();
        let mut done = false;
        egui::Area::new(egui::Id::new("smoke")).fixed_pos(screen.min).order(egui::Order::Middle).show(ctx, |ui| {
            let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(r, CornerRadius::ZERO, t.smoke);
            if resp.clicked() {
                done = true;
            }
        });
        egui::Area::new(egui::Id::new("delete-dialog")).anchor(Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new()
                .fill(if t.dark { Color32::from_gray(43) } else { Color32::WHITE })
                .stroke(Stroke::new(1.0, t.control_stroke))
                .corner_radius(CornerRadius::same(theme::R_CARD))
                .shadow(ui.visuals().window_shadow)
                .show(ui, |ui| {
                    ui.set_width(420.0);
                    egui::Frame::new().inner_margin(Margin::same(24)).show(ui, |ui| {
                        let n = paths.len();
                        ui.label(theme::subtitle(if n == 1 { "Delete this clip?".to_string() } else { format!("Delete {n} clips?") }).color(t.text));
                        ui.add_space(8.0);
                        ui.label(RichText::new("They move to the Recycle Bin, so you can restore them.").color(t.text2));
                    });
                    egui::Frame::new()
                        .fill(if t.dark { Color32::from_gray(32) } else { Color32::from_gray(243) })
                        .corner_radius(CornerRadius { nw: 0, ne: 0, sw: theme::R_CARD, se: theme::R_CARD })
                        .inner_margin(Margin::same(24))
                        .show(ui, |ui| {
                            ui.columns(2, |cols| {
                                let w = cols[0].available_width();
                                let del = egui::Button::new(RichText::new("Delete").color(t.on_accent)).fill(t.accent).stroke(Stroke::NONE);
                                if cols[0].add_sized(Vec2::new(w, theme::CONTROL_H), del).clicked() {
                                    if self.editor.as_ref().is_some_and(|e| paths.iter().any(|p| p == e.path())) {
                                        self.editor = None;
                                        self.page = Page::Clips;
                                    }
                                    if crate::sys::shell::recycle(&paths) {
                                        self.shared.toast(format!("Moved {} clip(s) to the Recycle Bin.", paths.len()), false);
                                    } else {
                                        self.shared.toast("The clip could not be deleted. Is it open in another program?", true);
                                    }
                                    self.checked.clear();
                                    done = true;
                                }
                                if cols[1].add_sized(Vec2::new(w, theme::CONTROL_H), egui::Button::new("Cancel")).clicked() {
                                    done = true;
                                }
                            });
                        });
                });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            done = true;
        }
        if done {
            self.confirm_delete = None;
            self.rescan(ctx);
        }
    }
}

impl eframe::App for LibraryApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = theme::tokens(&ctx);
        self.poll_background(&ctx);

        egui::Panel::left("nav")
            .exact_size(220.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(t.base))
            .show(ui, |ui| self.nav(ui, &t));

        let mut content_rect = Rect::NOTHING;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.base)).show(ui, |ui| {
            // Content layer, rounded where it meets the navigation rail.
            let r = ui.max_rect();
            content_rect = r;
            ui.painter().rect(r, CornerRadius { nw: theme::R_CARD, ne: 0, sw: 0, se: 0 }, t.layer, Stroke::new(1.0, t.layer_stroke), StrokeKind::Inside);
            let inner = r.shrink(theme::GUTTER);
            let mut page_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
            match self.page {
                Page::Clips => self.clips_page(&mut page_ui, &t),
                Page::Editor => match &mut self.editor {
                    Some(ed) => match ed.ui(&mut page_ui, &mut self.shared) {
                        Some(editor::Action::Back) => self.page = Page::Clips,
                        Some(editor::Action::Delete(p)) => self.confirm_delete = Some(vec![p]),
                        None => {}
                    },
                    None => self.page = Page::Clips,
                },
                Page::Settings => {
                    if self.settings.ui(&mut page_ui, &mut self.shared) {
                        self.rescan(&ctx);
                    }
                }
            }
        });
        self.selection_bar(&ctx, &t, content_rect);
        self.info_bars(&ctx, &t);
        self.delete_dialog(&ctx, &t);
    }

    /// Opaque window background, so nothing behind the window shows through.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        let c = if visuals.dark_mode { theme::DARK.base } else { theme::LIGHT.base };
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}

/// System thumbnail, or a frame decoded from the clip when the system has none.
fn thumbnail(p: &Path) -> Option<(usize, usize, Vec<u8>)> {
    if let Some(t) = crate::sys::shell::thumbnail(p, 320, 180) {
        return Some(t);
    }
    let dec = crate::sys::player::VideoDecoder::open(p, || {});
    dec.seek(1.0, 1);
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if let Ok(f) = dec.frames.recv_timeout(Duration::from_millis(500))
            && f.generation == 1
        {
            return Some(filmstrip::shrink(&f.rgba, f.width, f.height, 180));
        }
    }
    None
}

fn scan(dir: &Path, known: &HashMap<PathBuf, (SystemTime, u64, f64)>) -> Vec<ClipEntry> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                if depth < 2 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4")) {
                continue;
            }
            let game = if d == dir { "Other".to_string() } else { d.file_name().unwrap_or_default().to_string_lossy().into_owned() };
            let modified = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let duration = match known.get(&p) {
                Some((m, sz, d)) if *m == modified && *sz == md.len() => *d,
                _ => crate::mux::reader::read(&p).map(|m| m.duration()).unwrap_or(0.0),
            };
            out.push(ClipEntry {
                name: p.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                path: p,
                game,
                modified,
                size: md.len(),
                duration,
            });
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.modified));
    out
}
