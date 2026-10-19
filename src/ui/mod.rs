//! The library window: clip browser, editor and settings. It runs as its
//! own process (`replay --library`) and exits when closed, so the resident
//! recorder never carries UI code or memory.

mod editor;
mod settings;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32, RichText, TextureHandle, Vec2};

use crate::config::Config;
use editor::Editor;

pub fn run(open: Option<PathBuf>, settings: bool) {
    let icon = {
        let size = 64u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        let c = (size as f32 - 1.0) / 2.0;
        for y in 0..size {
            for x in 0..size {
                let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                let a = ((size as f32 * 0.45 - d) + 0.5).clamp(0.0, 1.0);
                let i = ((y * size + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[0xe5, 0x39, 0x35, (a * 255.0) as u8]);
            }
        }
        egui::IconData { rgba, width: size, height: size }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Replay")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Replay", options, Box::new(move |cc| Ok(Box::new(LibraryApp::new(cc, open, settings))))) {
        log::error!("library window failed: {e}");
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Clips,
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
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
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
            let password = crate::share::unprotect(&share.streamable_password_protected).unwrap_or_default();
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
    tab: Tab,
    shared: Shared,
    job_rx: Receiver<(usize, JobDone)>,
    clips: Vec<ClipEntry>,
    scan_rx: Option<Receiver<Vec<ClipEntry>>>,
    last_scan: Instant,
    filter: String,
    selected: Option<PathBuf>,
    checked: HashSet<PathBuf>,
    thumbs: HashMap<PathBuf, TextureHandle>,
    thumb_tx: Sender<PathBuf>,
    thumb_rx: Receiver<(PathBuf, usize, usize, Vec<u8>)>,
    thumb_requested: HashSet<PathBuf>,
    editor: Option<Editor>,
    settings: settings::SettingsPage,
    confirm_delete: Option<Vec<PathBuf>>,
}

impl LibraryApp {
    fn new(cc: &eframe::CreationContext, open: Option<PathBuf>, settings: bool) -> Self {
        let ctx = cc.egui_ctx.clone();
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let mut style = (*ctx.style_of(theme)).clone();
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(10.0, 4.0);
            ctx.set_style_of(theme, style);
        }

        let (thumb_tx, thumb_req_rx) = mpsc::channel::<PathBuf>();
        let (thumb_done_tx, thumb_rx) = mpsc::channel();
        let tctx = ctx.clone();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || {
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
                }
                while let Ok(p) = thumb_req_rx.recv() {
                    if let Some((w, h, px)) = crate::win::thumbnail(&p, 192, 108) {
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
            tab: Tab::Clips,
            settings: settings::SettingsPage::new(&cfg),
            shared: Shared { cfg, jobs: Vec::new(), job_tx, next_job: 0, uploads, toast: None },
            job_rx,
            clips: Vec::new(),
            scan_rx: None,
            last_scan: Instant::now(),
            filter: String::new(),
            selected: None,
            checked: HashSet::new(),
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            thumb_requested: HashSet::new(),
            editor: None,
            confirm_delete: None,
        };
        app.rescan(&ctx);
        if settings {
            app.tab = Tab::Settings;
        }
        if let Some(p) = open.filter(|p| p.exists()) {
            app.select(&ctx, p);
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

    fn select(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.selected.as_ref() == Some(&path) {
            return;
        }
        self.editor = Some(Editor::open(ctx, &path, &self.shared.cfg));
        self.selected = Some(path);
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
        // New clips arrive while the window is open; look every few seconds.
        if self.scan_rx.is_none() && self.last_scan.elapsed() > Duration::from_secs(4) {
            self.rescan(ctx);
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
                    self.selected = None;
                    self.select(ctx, p);
                }
                JobDone::Uploaded(p, url) => {
                    self.shared.uploads.insert(p.to_string_lossy().into_owned(), url.clone());
                    if let Ok(t) = serde_json::to_string_pretty(&self.shared.uploads) {
                        let _ = std::fs::write(uploads_path(), t);
                    }
                    let copied = self.shared.cfg.share.copy_link_after_upload && crate::share::copy_text(&url);
                    let msg = if copied { "Uploaded. Link copied." } else { "Uploaded." };
                    self.shared.toast_link(msg, url);
                }
                JobDone::Failed(e) => self.shared.toast(e, true),
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 7.0, Color32::from_rgb(0xe5, 0x39, 0x35));
            ui.label(RichText::new("Replay").strong().size(18.0));
            ui.add_space(16.0);
            ui.selectable_value(&mut self.tab, Tab::Clips, RichText::new("Clips").size(15.0));
            ui.selectable_value(&mut self.tab, Tab::Settings, RichText::new("Settings").size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::win::recorder_running() {
                    ui.label(RichText::new("Recorder running").color(Color32::from_rgb(0x4c, 0xaf, 0x50)));
                } else {
                    if ui.button("Start recorder").clicked()
                        && let Ok(exe) = std::env::current_exe()
                    {
                        let _ = std::process::Command::new(exe).spawn();
                    }
                    ui.label(RichText::new("Recorder not running").color(Color32::from_rgb(0xff, 0x98, 0x00)));
                }
            });
        });
    }

    fn clip_list(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search clips").desired_width(f32::INFINITY));
        });
        ui.horizontal(|ui| {
            if ui.button("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&self.shared.cfg.output_dir);
                crate::win::open_path(&self.shared.cfg.output_dir);
            }
            if ui.button("Refresh").clicked() {
                self.rescan(&ctx);
            }
            ui.label(RichText::new(format!("{} clips", self.clips.len())).weak());
        });
        if self.checked.len() >= 2 {
            ui.horizontal(|ui| {
                if ui.button(format!("Join {} clips", self.checked.len())).clicked() {
                    self.join_checked(&ctx);
                }
                if ui.button("Delete selected").clicked() {
                    self.confirm_delete = Some(self.checked.iter().cloned().collect());
                }
                if ui.button("Clear").clicked() {
                    self.checked.clear();
                }
            });
        }
        ui.separator();

        let filter = self.filter.to_lowercase();
        let mut by_game: Vec<(String, Vec<ClipEntry>)> = Vec::new();
        for c in self.clips.iter().filter(|c| filter.is_empty() || c.name.to_lowercase().contains(&filter) || c.game.to_lowercase().contains(&filter)) {
            match by_game.iter_mut().find(|(g, _)| *g == c.game) {
                Some((_, v)) => v.push(c.clone()),
                None => by_game.push((c.game.clone(), vec![c.clone()])),
            }
        }
        if self.clips.is_empty() && self.scan_rx.is_none() {
            ui.add_space(20.0);
            ui.label(format!(
                "No clips yet.\n\nPlay a game and press {} to save the last {} seconds.",
                self.shared.cfg.hotkeys.save_focused, self.shared.cfg.replay_seconds
            ));
        }
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (game, clips) in &by_game {
                egui::CollapsingHeader::new(RichText::new(format!("{game}  ({})", clips.len())).strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        for c in clips {
                            if self.clip_row(ui, c) {
                                clicked = Some(c.path.clone());
                            }
                        }
                    });
            }
        });
        if let Some(p) = clicked {
            self.select(&ctx, p);
        }
    }

    /// One row; returns true when clicked.
    fn clip_row(&mut self, ui: &mut egui::Ui, c: &ClipEntry) -> bool {
        let selected = self.selected.as_ref() == Some(&c.path);
        let mut clicked = false;
        let frame = egui::Frame::NONE
            .inner_margin(4.0)
            .corner_radius(4.0)
            .fill(if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.5) } else { Color32::TRANSPARENT });
        let resp = frame
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut checked = self.checked.contains(&c.path);
                    if ui.checkbox(&mut checked, "").changed() {
                        if checked {
                            self.checked.insert(c.path.clone());
                        } else {
                            self.checked.remove(&c.path);
                        }
                    }
                    let size = Vec2::new(96.0, 54.0);
                    match self.thumbs.get(&c.path) {
                        Some(t) => {
                            ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), size)).corner_radius(3.0));
                        }
                        None => {
                            if self.thumb_requested.insert(c.path.clone()) {
                                let _ = self.thumb_tx.send(c.path.clone());
                            }
                            let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                            ui.painter().rect_filled(r, 3.0, ui.visuals().extreme_bg_color);
                        }
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&c.name).strong());
                        ui.label(
                            RichText::new(format!("{}  ·  {:.1} MB", fmt_time(c.duration), c.size as f64 / 1e6)).weak().small(),
                        );
                        if self.shared.uploads.contains_key(c.path.to_string_lossy().as_ref()) {
                            ui.label(RichText::new("Uploaded").small().color(Color32::from_rgb(0x42, 0xa5, 0xf5)));
                        }
                    });
                });
            })
            .response;
        if resp.interact(egui::Sense::click()).clicked() {
            clicked = true;
        }
        clicked
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

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for j in &self.shared.jobs {
                let p = j.progress.lock().map(|p| *p).unwrap_or(0.0);
                let _ = matches!(j.kind, JobKind::Export | JobKind::Upload);
                ui.label(&j.label);
                ui.add(egui::ProgressBar::new(p).desired_width(160.0).show_percentage());
            }
            if let Some(t) = &self.shared.toast {
                if t.at.elapsed() > Duration::from_secs(if t.link.is_some() { 30 } else { 6 }) {
                    self.shared.toast = None;
                } else {
                    let color = if t.error { Color32::from_rgb(0xef, 0x53, 0x50) } else { ui.visuals().text_color() };
                    ui.label(RichText::new(&t.text).color(color));
                    if let Some(link) = t.link.clone() {
                        ui.hyperlink(&link);
                        if ui.small_button("Copy link").clicked() {
                            crate::share::copy_text(&link);
                        }
                    }
                    ui.ctx().request_repaint_after(Duration::from_secs(1));
                }
            }
        });
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(paths) = self.confirm_delete.clone() else { return };
        let mut open = true;
        let mut done = false;
        egui::Window::new("Delete clips")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Move {} clip(s) to the Recycle Bin?", paths.len()));
                ui.horizontal(|ui| {
                    if ui.button("Move to Recycle Bin").clicked() {
                        if self.selected.as_ref().is_some_and(|s| paths.contains(s)) {
                            self.editor = None;
                            self.selected = None;
                        }
                        if crate::win::recycle(&paths) {
                            self.shared.toast(format!("Moved {} clip(s) to the Recycle Bin", paths.len()), false);
                        } else {
                            self.shared.toast("Could not delete the clip (is it open in another program?)", true);
                        }
                        self.checked.clear();
                        done = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                });
            });
        if done || !open {
            self.confirm_delete = None;
            self.rescan(ctx);
        }
    }
}

impl eframe::App for LibraryApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_background(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        match self.tab {
            Tab::Clips => {
                egui::Panel::left("clips").resizable(true).default_size(380.0).size_range(280.0..=600.0).show(ui, |ui| {
                    self.clip_list(ui);
                });
                egui::CentralPanel::default().show(ui, |ui| match &mut self.editor {
                    Some(ed) => {
                        if let Some(action) = ed.ui(ui, &mut self.shared) {
                            match action {
                                editor::Action::Delete(p) => self.confirm_delete = Some(vec![p]),
                            }
                        }
                    }
                    None => {
                        ui.centered_and_justified(|ui| {
                            ui.label(RichText::new("Select a clip to play, trim, mix or share.").weak().size(16.0));
                        });
                    }
                });
            }
            Tab::Settings => {
                egui::CentralPanel::default().show(ui, |ui| {
                    if self.settings.ui(ui, &mut self.shared) {
                        self.rescan(&ctx);
                    }
                });
            }
        }
        self.delete_dialog(&ctx);
    }
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0) as u64;
    format!("{m}:{:04.1}", s - m as f64 * 60.0)
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
    // Newest first, games ordered by their newest clip.
    out.sort_by_key(|c| std::cmp::Reverse(c.modified));
    out
}
