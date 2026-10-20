//! `replay --snapshot <folder>`: opens the library off-screen, steps through
//! every page and state by itself, and saves a PNG of each. No mouse or
//! keyboard is used, so it can run while someone works on the same PC. Used
//! for design reviews and to check the UI after changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;

use super::{ClipDialog, LibraryApp, Page};

/// One state to capture, and how long to let it settle first.
#[derive(Clone, Copy, Debug)]
enum Step {
    Clips,
    TileHover,
    ClipsByGame,
    Selecting,
    Editor,
    EditorVertical,
    EditorSmall,
    EditorFullscreen,
    Games,
    GameDetail,
    SettingsGeneral,
    Focus,
    SettingsRecording,
    SettingsAudio,
    SettingsSharing,
    SettingsAbout,
    TagsDialog,
    Montage,
    Toast,
    Compact,
    Empty,
}

const STEPS: [(Step, &str); 21] = [
    (Step::Clips, "01-clips"),
    (Step::TileHover, "01b-tile-hover-on-star"),
    (Step::ClipsByGame, "02-clips-by-game"),
    (Step::Selecting, "03-clips-selecting"),
    (Step::Editor, "04-editor"),
    (Step::EditorVertical, "05-editor-vertical"),
    (Step::EditorSmall, "05b-editor-10mb-warning"),
    (Step::EditorFullscreen, "05c-editor-fullscreen"),
    (Step::Games, "06-games"),
    (Step::GameDetail, "07-game"),
    (Step::SettingsGeneral, "08-settings-general"),
    (Step::Focus, "08b-keyboard-focus"),
    (Step::SettingsRecording, "09-settings-recording"),
    (Step::SettingsAudio, "09b-settings-audio"),
    (Step::SettingsSharing, "10-settings-sharing"),
    (Step::SettingsAbout, "11-settings-about"),
    (Step::TagsDialog, "12-dialog-tags"),
    (Step::Montage, "13-dialog-montage"),
    (Step::Toast, "14-toast-undo"),
    (Step::Compact, "15-compact-window"),
    (Step::Empty, "16-no-results"),
];

pub struct Snapshot {
    dir: PathBuf,
    index: usize,
    entered: Option<Instant>,
    requested: bool,
    saved: Arc<AtomicBool>,
    /// A pointer position fed to the app as input (hover states, no real mouse).
    pub(super) hover: Option<egui::Pos2>,
    /// Frames counted while hovering, and how many showed the hover buttons.
    frames: u32,
    lit: u32,
}

impl Snapshot {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, index: 0, entered: None, requested: false, saved: Arc::new(AtomicBool::new(false)), hover: None, frames: 0, lit: 0 }
    }
}

impl LibraryApp {
    /// Drives the snapshot run; call once per frame before drawing.
    pub(super) fn snapshot_tick(&mut self, ctx: &egui::Context) {
        let Some(snap) = self.snapshot.as_mut() else { return };
        ctx.request_repaint_after(Duration::from_millis(30));
        if snap.requested {
            if snap.saved.load(Ordering::Acquire) {
                snap.index += 1;
                snap.entered = None;
                snap.requested = false;
                snap.saved.store(false, Ordering::Release);
            }
            return;
        }
        let Some(&(step, name)) = STEPS.get(snap.index) else {
            println!("snapshots saved to {}", snap.dir.display());
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        let entered = match snap.entered {
            Some(t) => t,
            None => {
                snap.entered = Some(Instant::now());
                snap.hover = None;
                self.enter(ctx, step);
                return;
            }
        };
        // Hold the pointer on the first tile's star, the button that flickered.
        if matches!(step, Step::TileHover) {
            let at = self.gallery.probe_star.map(|(_, r)| r.center());
            let lit = self.gallery.probe_lit;
            if let Some(s) = self.snapshot.as_mut() {
                // Count from the second half of the step, once the pointer has settled.
                if s.hover.is_some() && entered.elapsed() > Duration::from_millis(500) {
                    s.frames += 1;
                    s.lit += u32::from(lit);
                }
                s.hover = at;
            }
        }
        // The first page waits for thumbnails; the editor for its first frame.
        let settle = match step {
            Step::Clips => 3500,
            Step::TileHover => 1500,
            Step::Editor => 6000,
            Step::EditorVertical => 1500,
            Step::Compact => 1200,
            _ => 700,
        };
        if entered.elapsed() < Duration::from_millis(settle) {
            return;
        }
        // The editor waits for its whole filmstrip (at most 30 s).
        if matches!(step, Step::Editor) && self.editor.as_ref().is_some_and(|e| e.loading()) && entered.elapsed() < Duration::from_secs(30) {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let snap = self.snapshot.as_mut().expect("set");
        if matches!(step, Step::TileHover) {
            println!("tile hover: buttons shown in {} of {} frames", snap.lit, snap.frames);
        }
        let path = snap.dir.join(format!("{name}.png"));
        let saved = snap.saved.clone();
        snap.requested = true;
        ctx.request_screenshot(move |img| {
            if let Err(e) = write_png(&path, img.size[0], img.size[1], img.as_raw()) {
                eprintln!("cannot write {}: {e}", path.display());
            }
            saved.store(true, Ordering::Release);
        });
    }

    /// Puts the window in the state for `step`.
    fn enter(&mut self, ctx: &egui::Context, step: Step) {
        self.dialog = None;
        self.montage = None;
        self.shared.toast = None;
        self.gallery.checked.clear();
        self.gallery.filter.clear();
        self.gallery.group = super::clips::Group::Day;
        if let Some(ed) = &mut self.editor {
            ed.set_fullscreen(false);
        }
        let first_clip = self.clips.first().map(|c| c.path.clone());
        match step {
            Step::Clips | Step::TileHover => self.page = Page::Clips,
            Step::ClipsByGame => self.gallery.group = super::clips::Group::Game,
            Step::Selecting => {
                self.page = Page::Clips;
                for c in self.clips.iter().take(2) {
                    self.gallery.checked.insert(c.path.clone());
                }
            }
            Step::Editor => {
                if let Some(p) = first_clip {
                    self.open_editor(ctx, p);
                }
            }
            Step::EditorVertical => {
                if let Some(ed) = &mut self.editor {
                    ed.snapshot_vertical();
                }
                self.page = Page::Editor;
            }
            Step::EditorFullscreen => {
                if let Some(ed) = &mut self.editor {
                    ed.snapshot_fullscreen();
                }
                self.page = Page::Editor;
            }
            Step::EditorSmall => {
                if let Some(ed) = &mut self.editor {
                    ed.snapshot_small();
                }
                self.page = Page::Editor;
            }
            Step::Games => {
                self.page = Page::Games;
                self.game_open = None;
            }
            Step::GameDetail => {
                self.page = Page::Games;
                self.game_open = self.games.games.first().map(|g| g.exe.clone());
            }
            Step::SettingsGeneral => {
                self.page = Page::Settings;
                self.settings.show(super::settings::Category::General);
            }
            Step::Focus => {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("snapshot-focus"), true));
            }
            Step::SettingsRecording => self.settings.show(super::settings::Category::Recording),
            Step::SettingsAudio => self.settings.show(super::settings::Category::Audio),
            Step::SettingsSharing => self.settings.show(super::settings::Category::Sharing),
            Step::SettingsAbout => self.settings.show(super::settings::Category::About),
            Step::TagsDialog => {
                self.page = Page::Clips;
                if let Some(p) = first_clip {
                    self.dialog = Some(ClipDialog::Tags(vec![p], "clutch, funny".into()));
                }
            }
            Step::Montage => {
                for c in self.clips.iter().take(3) {
                    self.gallery.checked.insert(c.path.clone());
                }
                self.open_montage();
                self.gallery.checked.clear();
            }
            Step::Toast => {
                self.shared.toast_action("2 clips deleted", super::ToastAction::Undo);
            }
            Step::Compact => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(900.0, 680.0)));
            }
            Step::Empty => {
                self.gallery.filter = "no such clip".into();
            }
        }
    }
}

/// Writes RGBA pixels as a PNG (stored deflate blocks: no compression
/// library needed).
pub fn write_png(path: &Path, w: usize, h: usize, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for row in rgba.chunks(w * 4).take(h) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_png_spec_example() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }
}
