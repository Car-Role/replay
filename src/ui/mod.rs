//! The library window. Layout (see docs/design.md): a navigation rail on the
//! base layer (Clips, Games; Settings and the capture status at the bottom)
//! and pages on the content layer. The editor is a full-page drill-in from
//! Clips. It runs as its own process (`replay --library`) and exits when
//! closed, so the resident recorder never carries UI code or memory.

mod clips;
mod editor;
mod filmstrip;
mod games_page;
mod settings;
mod snapshot;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align2, CornerRadius, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, Vec2};

use crate::config::Config;
use crate::library::uploads_path;
use editor::Editor;
use theme::{Tokens, icon};

/// `replay --snapshot <folder>`: renders every page off-screen to PNG files,
/// without touching the mouse or keyboard.
pub fn snapshot(dir: PathBuf) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Replay snapshot")
            .with_inner_size([1320.0, 840.0])
            .with_position([-30_000.0, -30_000.0])
            .with_active(false)
            .with_taskbar(false),
        ..Default::default()
    };
    let r = eframe::run_native(
        "Replay snapshot",
        options,
        Box::new(move |cc| {
            let mut app = LibraryApp::new(cc, None, "clips");
            app.snapshot = Some(snapshot::Snapshot::new(dir));
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = r {
        eprintln!("snapshot failed: {e}");
    }
}

/// `replay --frame`: decodes the frame at `at` seconds and saves it as PNG.
pub fn save_frame(clip: &Path, at: f64, out: &Path) -> bool {
    let dec = crate::sys::player::VideoDecoder::open(clip, || {});
    dec.seek(at, 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(f) = dec.frames.recv_timeout(Duration::from_millis(500))
            && f.generation == 1
        {
            println!("frame at {:.3} s, {}x{}", f.pts, f.width, f.height);
            return snapshot::write_png(out, f.width, f.height, &f.rgba).is_ok();
        }
    }
    println!("no frame decoded");
    false
}

pub fn run(open: Option<PathBuf>, start: &str) {
    let start = start.to_string();
    let icon = egui::IconData { rgba: include_bytes!("../../assets/generated/icon-64.rgba").to_vec(), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Replay")
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([800.0, 560.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Replay", options, Box::new(move |cc| Ok(Box::new(LibraryApp::new(cc, open, &start))))) {
        log::error!("library window failed: {e}");
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Clips,
    Editor,
    Games,
    Settings,
}

/// The montage dialog: clips in order, and optional music.
struct Montage {
    clips: Vec<PathBuf>,
    music: Option<PathBuf>,
    music_volume: u32,
    game_volume: u32,
    fade_out: bool,
    /// Largest size in MB; None keeps the original size.
    size_mb: Option<u32>,
    vertical: bool,
}

/// A dialog that edits clips: rename one, or set tags on several.
enum ClipDialog {
    Rename(PathBuf, String),
    Tags(Vec<PathBuf>, String),
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
    /// Exported, then put on the clipboard (Copy for Discord). The flag says
    /// the copy will look soft (long clip for the size).
    ExportedCopied(PathBuf, bool),
    Uploaded(PathBuf, String),
    Update(settings::UpdateState),
    Failed(String),
}

/// The one action a toast offers.
#[derive(Clone)]
enum ToastAction {
    CopyLink(String),
    Undo,
    Open(PathBuf),
}

struct Toast {
    text: String,
    error: bool,
    at: Instant,
    action: Option<ToastAction>,
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
        self.toast = Some(Toast { text: text.into(), error, at: Instant::now(), action: None });
    }

    pub fn toast_link(&mut self, text: impl Into<String>, link: String) {
        self.toast = Some(Toast { text: text.into(), error: false, at: Instant::now(), action: Some(ToastAction::CopyLink(link)) });
    }

    fn toast_action(&mut self, text: impl Into<String>, action: ToastAction) {
        self.toast = Some(Toast { text: text.into(), error: false, at: Instant::now(), action: Some(action) });
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
        let label = format!("Uploading to {}", share.target.label());
        self.spawn_job(ctx, JobKind::Upload, label, move |progress| {
            let r = crate::share::upload(&path, &share, &|sent, total| progress(sent as f32 / total.max(1) as f32));
            match r {
                Ok(url) => JobDone::Uploaded(path, url),
                Err(e) => JobDone::Failed(format!("Upload failed: {e}")),
            }
        });
    }
}

/// Clips deleted a moment ago: hidden now, sent to the Recycle Bin when the
/// Undo toast goes away.
struct PendingDelete {
    paths: Vec<PathBuf>,
    until: Instant,
}

const UNDO_TIME: Duration = Duration::from_secs(6);

pub struct LibraryApp {
    page: Page,
    shared: Shared,
    job_rx: Receiver<(usize, JobDone)>,
    clips: Vec<ClipEntry>,
    scan_rx: Option<Receiver<Vec<ClipEntry>>>,
    last_scan: Instant,
    thumbs: HashMap<PathBuf, TextureHandle>,
    thumb_tx: Sender<PathBuf>,
    thumb_rx: Receiver<(PathBuf, usize, usize, Vec<u8>)>,
    thumb_requested: HashSet<PathBuf>,
    editor: Option<Editor>,
    settings: settings::SettingsPage,
    recorder_running: bool,
    recorder_checked: Instant,
    games: crate::games::GameDb,
    lib: crate::library::Library,
    gallery: clips::Gallery,
    dialog: Option<ClipDialog>,
    montage: Option<Montage>,
    pending_delete: Option<PendingDelete>,
    /// The game whose page is open on the Games tab.
    game_open: Option<String>,
    snapshot: Option<snapshot::Snapshot>,
    /// When the library was opened before this time; newer clips are "New".
    seen_before: Option<SystemTime>,
}

impl LibraryApp {
    fn new(cc: &eframe::CreationContext, open: Option<PathBuf>, start: &str) -> Self {
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
        let mut app = Self {
            page: match start {
                "settings" => Page::Settings,
                "games" => Page::Games,
                _ => Page::Clips,
            },
            settings: settings::SettingsPage::new(&cfg),
            shared: Shared { cfg, jobs: Vec::new(), job_tx, next_job: 0, uploads, icons, toast: None },
            job_rx,
            clips: Vec::new(),
            scan_rx: None,
            last_scan: Instant::now(),
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            thumb_requested: HashSet::new(),
            editor: None,
            recorder_running: crate::sys::shell::recorder_running(),
            // Check at once, so stale "recording" flags never show.
            recorder_checked: Instant::now() - Duration::from_secs(3),
            games: crate::games::GameDb::load(),
            lib: crate::library::Library::load(),
            gallery: clips::Gallery::default(),
            dialog: None,
            montage: None,
            pending_delete: None,
            game_open: None,
            snapshot: None,
            seen_before: seen_marker(),
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
            let mut ed = Editor::open(ctx, &path, &self.shared.cfg);
            ed.set_crop(self.lib.get(&path).crop);
            self.editor = Some(ed);
        }
        self.page = Page::Editor;
    }

    /// Requests a thumbnail (clip frame or program icon) once.
    fn thumb(&mut self, path: &Path) -> Option<&TextureHandle> {
        if !self.thumbs.contains_key(path) && self.thumb_requested.insert(path.to_path_buf()) {
            let _ = self.thumb_tx.send(path.to_path_buf());
        }
        self.thumbs.get(path)
    }

    fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.scan_rx
            && let Ok(list) = rx.try_recv()
        {
            let newest = self.clips.iter().map(|c| c.modified).max();
            if let Some(newest) = newest
                && self.snapshot.is_none()
                && let Some(c) = list.iter().filter(|c| c.modified > newest).max_by_key(|c| c.modified)
                && c.modified.elapsed().is_ok_and(|e| e < Duration::from_secs(120))
            {
                let title = clips::friendly_title(&c.path, &c.game);
                self.shared.toast_action(format!("New clip: {title}"), ToastAction::Open(c.path.clone()));
            }
            self.clips = list;
            self.scan_rx = None;
            let existing: HashSet<&PathBuf> = self.clips.iter().map(|c| &c.path).collect();
            self.gallery.checked.retain(|p| existing.contains(p));
        }
        if self.scan_rx.is_none() && self.last_scan.elapsed() > Duration::from_secs(4) && matches!(self.page, Page::Clips | Page::Games) {
            self.rescan(ctx);
        }
        if self.recorder_checked.elapsed() > Duration::from_secs(1) {
            self.recorder_running = crate::sys::shell::recorder_running();
            self.recorder_checked = Instant::now();
            self.games = crate::games::GameDb::load();
            if !self.recorder_running {
                // A recorder that crashed or was ended cannot clear its flags.
                for g in &mut self.games.games {
                    g.recording = false;
                    g.capturing = false;
                }
            }
            // Keep the capture status and its timer current.
            ctx.request_repaint_after(Duration::from_secs(1));
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
                    self.commit_delete();
                    let name = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    self.shared.toast_action(format!("Saved \u{201c}{name}\u{201d}"), ToastAction::Open(p));
                    self.rescan(ctx);
                }
                JobDone::ExportedCopied(p, soft) => {
                    let mb = std::fs::metadata(&p).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0);
                    if crate::sys::shell::copy_file(&p) {
                        let tip = if soft { " It is long for this size, so it looks soft; trim it for a sharper copy." } else { "" };
                        self.shared.toast(format!("Ready for Discord ({mb:.1} MB) and copied. Paste it with Ctrl+V.{tip}"), false);
                    } else {
                        self.shared.toast("The copy is saved, but the clipboard was busy.", true);
                    }
                    self.rescan(ctx);
                }
                JobDone::Uploaded(p, url) => {
                    self.commit_delete();
                    self.shared.uploads.insert(p.to_string_lossy().into_owned(), url.clone());
                    if let Ok(t) = serde_json::to_string_pretty(&self.shared.uploads) {
                        let _ = std::fs::write(uploads_path(), t);
                    }
                    let copied = self.shared.cfg.share.copy_link_after_upload && crate::sys::shell::copy_text(&url);
                    self.shared.toast_link(if copied { "Uploaded. Link copied." } else { "Uploaded." }, url);
                }
                JobDone::Update(state) => {
                    if let settings::UpdateState::Ready(v) = &state {
                        self.shared.toast(format!("Replay {v} is ready. Restart from Settings > About."), false);
                    }
                    self.settings.update = state;
                }
                JobDone::Failed(e) => self.shared.toast(e, true),
            }
        }
        if self.pending_delete.as_ref().is_some_and(|d| Instant::now() >= d.until) {
            self.commit_delete();
        }
        if !self.shared.jobs.is_empty() || self.shared.toast.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    // ------------------------------------------------------------- deleting

    /// Hides clips at once and offers Undo; they go to the Recycle Bin when
    /// the toast goes away.
    fn delete_clips(&mut self, paths: Vec<PathBuf>) {
        self.commit_delete();
        if paths.is_empty() {
            return;
        }
        if self.editor.as_ref().is_some_and(|e| paths.iter().any(|p| p == e.path())) {
            self.editor = None;
            self.page = Page::Clips;
        }
        self.gallery.checked.clear();
        let n = paths.len();
        self.pending_delete = Some(PendingDelete { paths, until: Instant::now() + UNDO_TIME });
        let text = if n == 1 { "Clip deleted".to_string() } else { format!("{n} clips deleted") };
        self.shared.toast_action(text, ToastAction::Undo);
    }

    fn commit_delete(&mut self) {
        let Some(d) = self.pending_delete.take() else { return };
        let paths: Vec<PathBuf> = d.paths.into_iter().filter(|p| p.exists()).collect();
        if paths.is_empty() {
            return;
        }
        if crate::sys::shell::recycle(&paths) {
            self.lib = crate::library::Library::load();
            for p in &paths {
                self.lib.forget(p);
            }
            let _ = self.lib.save();
        } else {
            self.shared.toast("A clip could not be deleted. Is it open in another program?", true);
        }
        self.clips.retain(|c| !paths.contains(&c.path));
    }

    fn is_pending_delete(&self, p: &Path) -> bool {
        self.pending_delete.as_ref().is_some_and(|d| d.paths.iter().any(|x| x == p))
    }

    pub(crate) fn set_favorite(&mut self, path: &Path, on: bool) {
        self.lib = crate::library::Library::load();
        self.lib.update(path, |m| m.favorite = on);
        if let Err(e) = self.lib.save() {
            self.shared.toast(format!("Could not save: {e}"), true);
        }
    }

    // ------------------------------------------------------------ nav rail

    fn nav(&mut self, ui: &mut egui::Ui, t: &Tokens, compact: bool) {
        let icons = self.shared.icons;
        // The window's title bar already shows the icon and "Replay", so the
        // pane starts with its first item (as in Windows Settings).
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        let on_clips = matches!(self.page, Page::Clips | Page::Editor);
        if theme::nav_item(ui, t, icons, icon::LIBRARY, "Clips", on_clips, compact).clicked() {
            self.page = Page::Clips;
        }
        if theme::nav_item(ui, t, icons, icon::GAME, "Games", self.page == Page::Games, compact).clicked() {
            self.page = Page::Games;
            self.game_open = None;
        }

        // Bottom: capture status, then Settings at the very bottom.
        let r = ui.max_rect();
        let bottom = r.bottom() - 8.0;
        let status_h = if compact { 136.0 } else { 144.0 };
        let area = Rect::from_min_max(Pos2::new(r.left(), bottom - 48.0 - status_h), Pos2::new(r.right(), bottom));
        let mut b = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::Min)));
        b.spacing_mut().item_spacing.y = 4.0;
        self.capture_status(&mut b, t, compact, status_h);
        b.add_space(8.0);
        if theme::nav_item(&mut b, t, icons, icon::SETTINGS, "Settings", self.page == Page::Settings, compact).clicked() {
            self.page = Page::Settings;
        }
    }

    /// Recorder state in the rail footer: idle, replay ready, or recording a
    /// session (red, with a timer). Holds the matching action.
    fn capture_status(&mut self, ui: &mut egui::Ui, t: &Tokens, compact: bool, height: f32) {
        let icons = self.shared.icons;
        let buffering: Vec<&crate::games::GameRecord> = self.games.games.iter().filter(|g| g.recording).collect();
        let capturing: Vec<&crate::games::GameRecord> = self.games.games.iter().filter(|g| g.capturing).collect();
        let now = crate::games::now_secs();
        let (dot, line, sub) = if !self.recorder_running {
            (t.text3, "Recorder off".to_string(), "Clips are not being kept".to_string())
        } else if let Some(c) = capturing.first() {
            let secs = now.saturating_sub(c.capture_started);
            (t.recording, format!("Recording {}", theme::fmt_clock(secs)), c.name.clone())
        } else if let Some(g) = buffering.first() {
            let secs = self.shared.cfg.for_game(&g.exe).replay_seconds;
            let names: Vec<&str> = buffering.iter().map(|g| g.name.as_str()).collect();
            (t.success, format!("Replay ready \u{00b7} {}", theme::fmt_short(f64::from(secs))), names.join(", "))
        } else {
            (t.text3, "Waiting for a game".to_string(), "Start a game to begin".to_string())
        };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        // A divider sets the status apart from the pages above.
        ui.painter().line_segment([Pos2::new(rect.left(), rect.top()), Pos2::new(rect.right(), rect.top())], Stroke::new(1.0, t.divider));
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(0.0, 8.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut inner;
        ui.spacing_mut().item_spacing.y = 4.0;
        let pulse = if capturing.is_empty() { 1.0 } else { 0.55 + 0.45 * ((ui.input(|i| i.time) * 3.0).sin() as f32 * 0.5 + 0.5) };
        if !capturing.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        // Status lines use the rail's icon and label columns (22 and 44 px).
        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), if compact { 20.0 } else { 40.0 }), Sense::hover());
        if compact {
            ui.painter().circle_filled(r.center(), 5.0, dot.gamma_multiply(pulse));
            // Under the dot: the timer while recording, "Ready" when a replay can be saved.
            let short = if !capturing.is_empty() { line.rsplit(' ').next().unwrap_or("").to_string() } else if !buffering.is_empty() { "Ready".to_string() } else { String::new() };
            let (sr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
            if !short.is_empty() {
                ui.painter().text(sr.center(), Align2::CENTER_CENTER, short, egui::FontId::proportional(12.0), t.text2);
            }
        } else {
            ui.painter().circle_filled(Pos2::new(r.left() + 22.0, r.top() + 10.0), 5.0, dot.gamma_multiply(pulse));
            ui.painter().text(Pos2::new(r.left() + 44.0, r.top() + 10.0), Align2::LEFT_CENTER, &line, egui::FontId::new(14.0, egui::FontFamily::Name(theme::SEMIBOLD.into())), t.text);
            let mut job = egui::text::LayoutJob::single_section(sub.clone(), egui::TextFormat::simple(egui::FontId::proportional(12.0), t.text2));
            job.wrap = egui::text::TextWrapping::truncate_at_width(r.width() - 52.0);
            let g = ui.painter().layout_job(job);
            ui.painter().galley(Pos2::new(r.left() + 44.0, r.top() + 22.0), g, t.text2);
        }
        resp.on_hover_text(format!("{line}\n{sub}"));
        ui.add_space(if compact { 12.0 } else { 8.0 });
        let save_key = self.shared.cfg.hotkeys.save_focused.clone();
        let rec_key = self.shared.cfg.hotkeys.record.clone();
        if !self.recorder_running {
            if theme::rail_button(ui, t, icons, icon::POWER, "Start recorder", "", t.text, true, compact).on_hover_text("Start recording games").clicked()
                && let Ok(exe) = std::env::current_exe()
            {
                let _ = std::process::Command::new(exe).spawn();
                self.recorder_checked = Instant::now() - Duration::from_secs(1);
            }
        } else if !capturing.is_empty() {
            if theme::rail_button(ui, t, icons, icon::STOP, "Stop recording", &rec_key, t.recording, true, compact).on_hover_text("Finish the recording and save it").clicked() {
                crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::Record);
                self.recorder_checked = Instant::now() - Duration::from_millis(800);
            }
        } else {
            let ready = !buffering.is_empty();
            let r = theme::rail_button(ui, t, icons, icon::SAVE, "Save replay", &save_key, t.accent, ready, compact);
            if r.on_hover_text(format!("Save the replay ({save_key})")).clicked() {
                if crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::Save(false)) {
                    self.shared.toast("Saving the replay\u{2026}", false);
                } else {
                    self.shared.toast("The recorder is not running.", true);
                }
            }
            let r = theme::rail_button(ui, t, icons, icon::RECORD, "Record session", &rec_key, t.text2, ready, compact);
            if r.on_hover_text(format!("Record the whole session ({rec_key})")).clicked() {
                crate::sys::shell::send_to_recorder(crate::ipc::RecorderMsg::Record);
                self.recorder_checked = Instant::now() - Duration::from_millis(800);
            }
        }
    }

    // ------------------------------------------------------------- overlays

    /// Job progress and messages: one stack, bottom centre, above the page.
    fn toasts(&mut self, ctx: &egui::Context, t: &Tokens, area: Rect) {
        let lasts = |x: &Toast| if x.error { Duration::from_secs(3600) } else if matches!(x.action, Some(ToastAction::CopyLink(_))) { Duration::from_secs(20) } else if matches!(x.action, Some(ToastAction::Undo)) { UNDO_TIME } else { Duration::from_secs(5) };
        if self.shared.toast.as_ref().is_some_and(|x| x.at.elapsed() > lasts(x)) {
            self.shared.toast = None;
        }
        if self.shared.jobs.is_empty() && self.shared.toast.is_none() {
            return;
        }
        let icons = self.shared.icons;
        let jobs: Vec<(String, f32)> = self.shared.jobs.iter().map(|j| (j.label.clone(), j.progress.lock().map(|p| *p).unwrap_or(0.0))).collect();
        let toast = self.shared.toast.as_ref().map(|x| (x.text.clone(), x.error, x.action.clone()));
        let mut close = false;
        let mut act = None;
        let frame = egui::Frame::new()
            .fill(t.flyout)
            .stroke(Stroke::new(1.0, t.card_stroke))
            .corner_radius(CornerRadius::same(theme::R_CARD))
            .shadow(ctx.style_of(ctx.theme()).visuals.popup_shadow)
            .inner_margin(Margin::symmetric(16, 10));
        let width = 420.0;
        let lift = if self.gallery.checked.is_empty() || self.page != Page::Clips { 24.0 } else { 88.0 };
        egui::Area::new(egui::Id::new("toasts"))
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(Pos2::new(area.center().x, area.bottom() - lift))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (label, p) in &jobs {
                    frame.show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(label).color(t.text)).truncate());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(theme::caption(format!("{:.0}%", p * 100.0)).color(t.text2));
                            });
                        });
                        ui.add(egui::ProgressBar::new(*p).desired_height(3.0).fill(t.accent));
                    });
                    ui.add_space(8.0);
                }
                if let Some((text, error, action)) = &toast {
                    frame.show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            if icons {
                                let (g, c) = if *error { (icon::ERROR, t.critical) } else { (icon::CHECK, t.success) };
                                ui.label(theme::glyph(g, 16.0).color(c));
                            }
                            ui.add(egui::Label::new(RichText::new(text).color(t.text)).wrap());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let x = if icons { theme::glyph(icon::CANCEL, 10.0) } else { RichText::new("x").size(12.0) };
                                if theme::subtle(ui, x).on_hover_text("Close").clicked() {
                                    close = true;
                                }
                                let label = match action {
                                    Some(ToastAction::CopyLink(_)) => Some("Copy link"),
                                    Some(ToastAction::Undo) => Some("Undo"),
                                    Some(ToastAction::Open(_)) => Some("Open"),
                                    None => None,
                                };
                                if let Some(l) = label
                                    && ui.add(egui::Button::new(RichText::new(l).color(t.accent)).frame_when_inactive(false)).clicked()
                                {
                                    act = action.clone();
                                }
                            });
                        });
                    });
                }
            });
        match act {
            Some(ToastAction::CopyLink(url)) => {
                crate::sys::shell::copy_text(&url);
                self.shared.toast("Link copied.", false);
            }
            Some(ToastAction::Undo) => {
                self.pending_delete = None;
                self.shared.toast = None;
            }
            Some(ToastAction::Open(p)) => {
                self.shared.toast = None;
                self.editor = None;
                self.open_editor(ctx, p);
            }
            None => {}
        }
        if close {
            if matches!(self.shared.toast.as_ref().and_then(|x| x.action.as_ref()), Some(ToastAction::Undo)) {
                self.commit_delete();
            }
            self.shared.toast = None;
        }
    }

    /// Fluent ContentDialog: smoke, a title, a body, and a footer with the
    /// primary button and Cancel on the right. Returns Some(true) for the
    /// primary button (or Enter), Some(false) to close.
    fn content_dialog(ctx: &egui::Context, t: &Tokens, width: f32, title: &str, primary: &str, body: impl FnOnce(&mut egui::Ui)) -> Option<bool> {
        let enter_adds = title == "Add tags";
        let screen = ctx.content_rect();
        let mut result = None;
        egui::Area::new(egui::Id::new("smoke")).fixed_pos(screen.min).order(egui::Order::Middle).show(ctx, |ui| {
            let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(r, CornerRadius::ZERO, t.smoke);
            if resp.clicked() {
                result = Some(false);
            }
        });
        egui::Area::new(egui::Id::new("content-dialog")).anchor(Align2::CENTER_CENTER, Vec2::ZERO).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::new()
                .fill(t.flyout)
                .stroke(Stroke::new(1.0, t.card_stroke))
                .corner_radius(CornerRadius::same(theme::R_CARD))
                .shadow(ui.visuals().window_shadow)
                .show(ui, |ui| {
                    ui.set_width(width);
                    egui::Frame::new().inner_margin(Margin::same(24)).show(ui, |ui| {
                        ui.label(theme::subtitle(title).color(t.text));
                        ui.add_space(12.0);
                        body(ui);
                    });
                    egui::Frame::new()
                        .fill(t.card_alt)
                        .corner_radius(CornerRadius { nw: 0, ne: 0, sw: theme::R_CARD, se: theme::R_CARD })
                        .inner_margin(Margin::same(24))
                        .show(ui, |ui| {
                            ui.columns(2, |cols| {
                                let w = cols[0].available_width();
                                if theme::button_centered(&mut cols[0], t, primary, w, true).clicked() {
                                    result = Some(true);
                                }
                                if theme::button_centered(&mut cols[1], t, "Cancel", w, false).clicked() {
                                    result = Some(false);
                                }
                            });
                        });
                });
        });
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                result = Some(false);
            } else if i.key_pressed(egui::Key::Enter) && result.is_none() && !i.modifiers.any() && !enter_adds {
                result = Some(true);
            }
        });
        result
    }

    /// Rename and tag dialogs.
    fn clip_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let known_tags = self.lib.all_tags();
        let Some(dialog) = &mut self.dialog else { return };
        let (title, primary) = match dialog {
            ClipDialog::Rename(..) => ("Rename clip", "Rename"),
            ClipDialog::Tags(..) => ("Add tags", "Save"),
        };
        let result = Self::content_dialog(ctx, t, 420.0, title, primary, |ui| match dialog {
            ClipDialog::Rename(_, name) => {
                ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY).margin(Margin::symmetric(10, 7))).request_focus();
            }
            ClipDialog::Tags(paths, text) => {
                let what = if paths.len() == 1 { "this clip".to_string() } else { format!("these {} clips", paths.len()) };
                ui.label(RichText::new(format!("Tags help you find {what} later.")).color(t.text2));
                ui.add_space(8.0);
                // Current tags as chips with a remove button.
                let mut have = crate::library::parse_tags(text);
                let mut drop = None;
                if !have.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for (i, tag) in have.iter().enumerate() {
                            if theme::tag_chip(ui, t, tag).on_hover_text("Remove").clicked() {
                                drop = Some(i);
                            }
                        }
                    });
                    ui.add_space(4.0);
                }
                if let Some(i) = drop {
                    have.remove(i);
                    *text = have.join(", ");
                }
                let input_id = egui::Id::new("tag-input");
                let mut input: String = ui.data_mut(|d| d.get_temp::<String>(input_id).unwrap_or_default());
                let r = ui.add(egui::TextEdit::singleline(&mut input).desired_width(f32::INFINITY).hint_text("Type a tag and press Enter").margin(Margin::symmetric(10, 7)));
                r.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Comma)) && !input.trim().is_empty() {
                    for tag in crate::library::parse_tags(&input) {
                        if !have.iter().any(|h| h.eq_ignore_ascii_case(&tag)) {
                            have.push(tag);
                        }
                    }
                    *text = have.join(", ");
                    input.clear();
                }
                ui.data_mut(|d| d.insert_temp(input_id, input));
                let pool: Vec<String> = if known_tags.is_empty() { ["clutch", "funny", "fail", "ace", "win"].iter().map(|s| s.to_string()).collect() } else { known_tags.clone() };
                let unused: Vec<String> = pool.iter().filter(|k| !have.iter().any(|h| h.eq_ignore_ascii_case(k))).cloned().collect();
                if !unused.is_empty() {
                    ui.add_space(8.0);
                    ui.label(theme::caption(if known_tags.is_empty() { "Ideas" } else { "Tags you use" }).color(t.text2));
                    ui.horizontal_wrapped(|ui| {
                        for k in unused.iter().take(12) {
                            if theme::idea_chip(ui, t, k).on_hover_text("Add this tag").clicked() {
                                if !text.trim().is_empty() {
                                    text.push_str(", ");
                                }
                                text.push_str(k);
                            }
                        }
                    });
                }
            }
        });
        let Some(ok) = result else { return };
        let dialog = self.dialog.take().expect("open");
        if !ok {
            return;
        }
        match dialog {
            ClipDialog::Rename(path, name) => self.rename(ctx, &path, &name),
            ClipDialog::Tags(paths, text) => {
                self.lib = crate::library::Library::load();
                let tags = crate::library::parse_tags(&text);
                for p in &paths {
                    self.lib.update(p, |m| m.tags = tags.clone());
                }
                if let Err(e) = self.lib.save() {
                    self.shared.toast(format!("Could not save: {e}"), true);
                }
            }
        }
    }

    pub(crate) fn rename(&mut self, ctx: &egui::Context, path: &Path, name: &str) {
        match crate::library::rename_clip(path, name) {
            Ok(new) => {
                self.lib = crate::library::Library::load();
                if let Some(url) = self.shared.uploads.remove(path.to_string_lossy().as_ref()) {
                    self.shared.uploads.insert(new.to_string_lossy().into_owned(), url);
                }
                if self.editor.as_ref().is_some_and(|e| e.path() == path) {
                    self.editor = None;
                    if self.page == Page::Editor {
                        self.open_editor(ctx, new.clone());
                    }
                }
                self.thumbs.remove(path);
                self.thumb_requested.remove(path);
                if self.gallery.checked.remove(path) {
                    self.gallery.checked.insert(new);
                }
                self.rescan(ctx);
            }
            Err(e) => self.shared.toast(format!("Could not rename: {e}"), true),
        }
    }

    /// The montage dialog.
    fn montage_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(paths) = self.montage.as_ref().map(|m| m.clips.clone()) else { return };
        // Per clip: thumbnail, short title and length.
        let rows: HashMap<PathBuf, (Option<egui::TextureId>, String, f64)> = paths
            .iter()
            .map(|p| {
                let entry = self.clips.iter().find(|c| &c.path == p).cloned();
                let tex = self.thumb(p).map(|t| t.id());
                let (game, dur) = entry.map(|c| (c.game, c.duration)).unwrap_or_default();
                (p.clone(), (tex, clips::friendly_title(p, &game), dur))
            })
            .collect();
        let discord_mb = self.shared.cfg.share.discord_mb;
        let Some(m) = &mut self.montage else { return };
        let icons = self.shared.icons;
        let result = Self::content_dialog(ctx, t, 560.0, "Make a montage", "Create montage", |ui| {
            ui.label(RichText::new("The clips play in this order. Music plays under all of them.").color(t.text2));
            ui.add_space(8.0);
            let mut swap: Option<(usize, usize)> = None;
            let mut remove = None;
            let n = m.clips.len();
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                for (i, p) in m.clips.iter().enumerate() {
                    let (tex, title, dur) = rows.get(p).cloned().unwrap_or((None, String::new(), 0.0));
                    ui.horizontal(|ui| {
                        ui.add_sized(Vec2::new(20.0, 36.0), egui::Label::new(theme::caption(format!("{}", i + 1)).color(t.text2)));
                        let (tr, _) = ui.allocate_exact_size(Vec2::new(64.0, 36.0), Sense::hover());
                        ui.painter().rect_filled(tr, CornerRadius::same(theme::R_CONTROL), t.card);
                        if let Some(id) = tex {
                            ui.painter().image(id, tr, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
                        }
                        let name = format!("{title}  \u{00b7}  {}", theme::fmt_short(dur));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn = |ui: &mut egui::Ui, c: char, alt: &str, enabled: bool| {
                                ui.add_enabled(enabled, egui::Button::new(theme::label_with_icon(icons, c, if icons { "" } else { alt })).frame_when_inactive(false).min_size(Vec2::splat(theme::CONTROL_H)))
                            };
                            if btn(ui, icon::CANCEL, "x", n > 1).on_hover_text("Leave out").clicked() {
                                remove = Some(i);
                            }
                            let down = ui.add_enabled(i + 1 < n, egui::Button::new(theme::label_with_icon(icons, icon::DOWN, if icons { "" } else { "v" })).frame_when_inactive(false).min_size(Vec2::splat(theme::CONTROL_H)));
                            if down.on_hover_text("Later").clicked() {
                                swap = Some((i, i + 1));
                            }
                            let up = ui.add_enabled(i > 0, egui::Button::new(theme::label_with_icon(icons, icon::UP, if icons { "" } else { "^" })).frame_when_inactive(false).min_size(Vec2::splat(theme::CONTROL_H)));
                            if up.on_hover_text("Earlier").clicked() {
                                swap = Some((i, i - 1));
                            }
                            let _ = &btn;
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(name).color(t.text)).truncate());
                            });
                        });
                    });
                }
            });
            let total: f64 = m.clips.iter().filter_map(|p| rows.get(p)).map(|r| r.2).sum();
            let source_mb: f64 = m.clips.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|md| md.len() as f64 / 1e6).sum();
            // With a size limit the result lands just under it.
            let mb = match m.size_mb {
                Some(limit) => (f64::from(limit) * 0.92).min(source_mb),
                None if m.vertical => source_mb * 0.45,
                None => source_mb,
            };
            let mb = mb.max(0.0);
            ui.add_space(4.0);
            ui.label(theme::caption(format!("{} clips  \u{00b7}  {} in total  \u{00b7}  about {mb:.0} MB", m.clips.len(), theme::fmt_short(total))).color(t.text2));
            if let Some((a, b)) = swap {
                m.clips.swap(a, b);
            }
            if let Some(i) = remove
                && m.clips.len() > 1
            {
                m.clips.remove(i);
            }
            ui.add_space(12.0);
            // One form: a 140 px label column and controls from one edge,
            // every row 32 px tall on a 40 px pitch.
            ui.spacing_mut().item_spacing.y = 8.0;
            fn form_row(ui: &mut egui::Ui, t: &Tokens, label: &str, control: impl FnOnce(&mut egui::Ui)) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (lr, _) = ui.allocate_exact_size(Vec2::new(140.0, theme::CONTROL_H), Sense::hover());
                    ui.painter().text(lr.left_center(), Align2::LEFT_CENTER, label, egui::FontId::proportional(14.0), t.text);
                    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), theme::CONTROL_H), egui::Layout::left_to_right(egui::Align::Center), control);
                });
            }
            form_row(ui, t, "Music", |ui| {
                if theme::button(ui, "Choose music\u{2026}", true).clicked()
                    && let Some(p) = crate::sys::shell::pick_audio()
                {
                    m.music = Some(p);
                }
                let label = m.music.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "No music".into());
                ui.add(egui::Label::new(theme::caption(label).color(t.text2)).truncate());
                if m.music.is_some() && theme::subtle(ui, "Remove").clicked() {
                    m.music = None;
                }
            });
            let slider = |ui: &mut egui::Ui, v: &mut u32, max: u32| {
                let w = ui.available_width() - 60.0 - 8.0;
                theme::slider(ui, t, v, 0..=max, false, w);
                let (vr, _) = ui.allocate_exact_size(Vec2::new(60.0, theme::CONTROL_H), Sense::hover());
                ui.painter().text(vr.right_center(), Align2::RIGHT_CENTER, format!("{v}%"), egui::FontId::proportional(14.0), t.text2);
            };
            form_row(ui, t, "Game sound", |ui| slider(ui, &mut m.game_volume, 200));
            if m.music.is_some() {
                form_row(ui, t, "Music volume", |ui| slider(ui, &mut m.music_volume, 100));
                form_row(ui, t, "Fade out at the end", |ui| {
                    theme::toggle(ui, t, &mut m.fade_out);
                });
            }
            form_row(ui, t, "Size", |ui| {
                let label = |s: Option<u32>| match s {
                    None => "Original size".to_string(),
                    Some(10) => "Discord (10 MB)".to_string(),
                    Some(50) => "Discord Nitro Basic (50 MB)".to_string(),
                    Some(mb) => format!("Discord Nitro ({mb} MB)"),
                };
                let w = ui.available_width();
                theme::combo("montage-size").width(w).selected_text(label(m.size_mb)).show_ui(ui, |ui| {
                    for s in [None, Some(10), Some(50), Some(500)] {
                        ui.selectable_value(&mut m.size_mb, s, label(s));
                    }
                });
            });
            form_row(ui, t, "Shape", |ui| {
                let w = ui.available_width();
                let shapes = [("Landscape", "As recorded"), ("Vertical 9:16", "Uses each clip's framing from the editor, or the centre")];
                if let Some(i) = theme::segmented(ui, t, &shapes, usize::from(m.vertical), w) {
                    m.vertical = i == 1;
                }
            });
            ui.add_space(4.0);
            let limit = discord_mb;
            let soft = m.size_mb.is_some_and(|s| crate::edit::size_target(f64::from(s), total, 128).0 < 800);
            if soft {
                let size = m.size_mb.unwrap_or(limit);
                let h = crate::edit::size_target(f64::from(size), total, 128).1.unwrap_or(1080);
                let text = format!("{} is long for {size} MB, so it is saved at {h}p and may look soft.", theme::fmt_short(total));
                let fix = (size < 50).then_some("Use 50 MB");
                let text = if fix.is_some() { text } else { format!("{text} Pick fewer clips.") };
                if theme::caution(ui, t, icons, &text, fix) {
                    m.size_mb = Some(50);
                }
            } else if m.size_mb.is_none() && mb > f64::from(limit) {
                theme::caution(ui, t, icons, &format!("Too big for Discord ({limit} MB). Pick a Discord size to share it there."), None);
            } else {
                let note = if m.vertical {
                    "Vertical uses each clip's framing from the editor, or the centre. Encoding again takes longer."
                } else {
                    "Clips with different settings or a size limit are encoded again, which takes longer."
                };
                ui.label(theme::caption(note).color(t.text3));
            }
        });
        let Some(ok) = result else { return };
        let m = self.montage.take().expect("open");
        if !ok || m.clips.is_empty() {
            return;
        }
        let segments: Vec<crate::edit::Segment> = m.clips.iter().map(|p| crate::edit::Segment { path: p.clone(), start: 0.0, end: f64::MAX }).collect();
        let out = crate::edit::edited_name(&m.clips[0], "montage");
        let a = &self.shared.cfg.audio;
        let mix = crate::edit::Mix {
            game: m.game_volume,
            mic: a.microphone_volume,
            keep_separate: true,
            bitrate_kbps: a.bitrate_kbps,
            music: m.music.map(|path| crate::edit::Music { path, volume: m.music_volume, fade_out: m.fade_out }),
        };
        let total: f64 = self.clips.iter().filter(|c| m.clips.contains(&c.path)).map(|c| c.duration).sum();
        let label = format!("Making a montage of {} clips", segments.len());
        // A size limit or a vertical crop means encoding again with H.264.
        let fixed = (m.size_mb.is_some() || m.vertical).then(|| {
            let (k, h) = match m.size_mb {
                Some(mb) => crate::edit::size_target(f64::from(mb), total, 128),
                None => (12_000, None),
            };
            crate::edit::EncodeSpec { codec: crate::config::Codec::H264, bitrate_kbps: k, cbr: m.size_mb.is_some(), max_height: h, vertical: m.vertical.then_some(0.5) }
        });
        let mut mix = mix;
        if m.size_mb.is_some() {
            mix.keep_separate = false;
            mix.bitrate_kbps = 128;
        }
        self.shared.spawn_job(ctx, JobKind::Export, label, move |p| match fixed {
            Some(spec) => match crate::edit::export(&segments, &out, &mix, &crate::edit::VideoMode::Encode(spec), p) {
                Ok(path) => JobDone::Exported(path),
                Err(e) => JobDone::Failed(format!("Montage failed: {e}")),
            },
            None => export_joined(&segments, &out, &mix, total, p, "Montage"),
        });
        self.gallery.checked.clear();
    }

    /// Joins the selected clips, oldest first.
    fn join_checked(&mut self, ctx: &egui::Context) {
        let mut list: Vec<&ClipEntry> = self.clips.iter().filter(|c| self.gallery.checked.contains(&c.path)).collect();
        list.sort_by_key(|c| c.modified);
        let segments: Vec<crate::edit::Segment> = list.iter().map(|c| crate::edit::Segment { path: c.path.clone(), start: 0.0, end: f64::MAX }).collect();
        let Some(first) = segments.first() else { return };
        let out = crate::edit::edited_name(&first.path, "joined");
        let a = &self.shared.cfg.audio;
        let mix = crate::edit::Mix { game: a.game_volume, mic: a.microphone_volume, keep_separate: true, bitrate_kbps: a.bitrate_kbps, music: None };
        let label = format!("Joining {} clips", segments.len());
        let total: f64 = list.iter().map(|c| c.duration).sum();
        self.shared.spawn_job(ctx, JobKind::Export, label, move |p| export_joined(&segments, &out, &mix, total, p, "Join"));
        self.gallery.checked.clear();
    }

    fn open_montage(&mut self) {
        let mut list: Vec<&ClipEntry> = self.clips.iter().filter(|c| self.gallery.checked.contains(&c.path)).collect();
        list.sort_by_key(|c| c.modified);
        self.montage = Some(Montage { clips: list.iter().map(|c| c.path.clone()).collect(), music: None, music_volume: 60, game_volume: 100, fade_out: true, size_mb: Some(self.shared.cfg.share.discord_mb), vertical: false });
    }
}

/// Joins segments; clips with different video settings are encoded again.
fn export_joined(segments: &[crate::edit::Segment], out: &Path, mix: &crate::edit::Mix, total: f64, p: &(dyn Fn(f32) + Sync), what: &str) -> JobDone {
    let paths: Vec<PathBuf> = segments.iter().map(|s| s.path.clone()).collect();
    let video = if crate::edit::same_video_settings(&paths) {
        crate::edit::VideoMode::Copy
    } else {
        let tracks = if mix.music.is_some() { 4 } else { 3 };
        match crate::edit::encode_spec(&paths[0], total, mix.bitrate_kbps * tracks, &crate::edit::ExportChoice::default()) {
            Ok(s) => crate::edit::VideoMode::Encode(s),
            Err(e) => return JobDone::Failed(format!("{what} failed: {e}")),
        }
    };
    match crate::edit::export(segments, out, mix, &video, p) {
        Ok(path) => JobDone::Exported(path),
        Err(e) => JobDone::Failed(format!("{what} failed: {e}")),
    }
}

impl eframe::App for LibraryApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        // `--snapshot` hover steps: a synthetic pointer, never the real mouse.
        if let Some(p) = self.snapshot.as_ref().and_then(|s| s.hover) {
            raw.events.push(egui::Event::PointerMoved(p));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = theme::tokens(&ctx);
        self.poll_background(&ctx);
        self.snapshot_tick(&ctx);
        // Closing the window leaves Replay in the tray: the recorder keeps
        // running (it starts now if it was off), and this process exits so
        // its memory is freed. The tray icon opens the window again.
        if ctx.input(|i| i.viewport().close_requested()) && self.snapshot.is_none() {
            self.commit_delete();
            if !crate::sys::shell::recorder_running()
                && let Ok(exe) = std::env::current_exe()
            {
                let _ = std::process::Command::new(exe).spawn();
            }
        }

        // NavigationView: labels from 1008 px wide, icons only below.
        let compact = ctx.content_rect().width() < 1008.0;
        egui::Panel::left("nav")
            .exact_size(if compact { 56.0 } else { 232.0 })
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(t.base).inner_margin(Margin::symmetric(4, 0)))
            .show(ui, |ui| self.nav(ui, &t, compact));

        let mut content_rect = Rect::NOTHING;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.base)).show(ui, |ui| {
            // Content layer, rounded where it meets the navigation rail.
            let r = ui.max_rect();
            content_rect = r;
            ui.painter().rect(r, CornerRadius { nw: theme::R_CARD, ne: 0, sw: 0, se: 0 }, t.layer, Stroke::new(1.0, t.layer_stroke), StrokeKind::Inside);
            let gutter = if compact { 16.0 } else { theme::GUTTER };
            // The editor uses the whole layer; other pages have margins.
            let inner = if self.page == Page::Editor { r.shrink2(Vec2::new(16.0, 12.0)) } else { r.shrink2(Vec2::new(gutter, 20.0)) };
            let mut page_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
            match self.page {
                Page::Clips => self.clips_page(&mut page_ui, &t),
                Page::Editor => self.editor_page(&mut page_ui),
                Page::Games => self.games_page(&mut page_ui, &t),
                Page::Settings => {
                    if self.settings.ui(&mut page_ui, &mut self.shared) {
                        self.rescan(&ctx);
                    }
                }
            }
        });
        self.selection_bar(&ctx, &t, content_rect);
        self.toasts(&ctx, &t, content_rect);
        self.clip_dialog(&ctx, &t);
        self.montage_dialog(&ctx, &t);
    }

    /// Opaque window background, so nothing behind the window shows through.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        let c = if visuals.dark_mode { theme::DARK.base } else { theme::LIGHT.base };
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}

impl Drop for LibraryApp {
    fn drop(&mut self) {
        // Deletes still waiting for Undo happen now.
        self.commit_delete();
    }
}

impl LibraryApp {
    fn editor_page(&mut self, ui: &mut egui::Ui) {
        let Some(ed) = &mut self.editor else {
            self.page = Page::Clips;
            return;
        };
        let fav = self.lib.is_favorite(ed.path());
        match ed.ui(ui, &mut self.shared, fav) {
            Some(editor::Action::Back) => self.page = Page::Clips,
            Some(editor::Action::Delete(p)) => self.delete_clips(vec![p]),
            Some(editor::Action::Favorite(p, on)) => self.set_favorite(&p, on),
            Some(editor::Action::SaveCrop(p, c)) => {
                self.lib = crate::library::Library::load();
                self.lib.update(&p, |m| m.crop = Some(c));
                let _ = self.lib.save();
            }
            Some(editor::Action::Rename(p, name)) => {
                let ctx = ui.ctx().clone();
                self.rename(&ctx, &p, &name);
            }
            None => {}
        }
    }
}

/// Reads when the library was last opened, and records now.
fn seen_marker() -> Option<SystemTime> {
    let p = crate::paths::data_dir().join("library-seen");
    let before = std::fs::read_to_string(&p).ok().and_then(|s| s.trim().parse::<u64>().ok()).map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s));
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = std::fs::write(&p, now.to_string());
    before
}

/// System thumbnail (a clip frame, or a program's icon), or a frame decoded
/// from the clip when the system has none.
fn thumbnail(p: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let exe = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"));
    if let Some(t) = crate::sys::shell::thumbnail(p, if exe { 64 } else { 320 }, if exe { 64 } else { 180 }) {
        return Some(t);
    }
    if exe {
        return None;
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
