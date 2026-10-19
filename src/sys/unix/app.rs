//! The recorder for macOS and Linux: a tray (menu bar) icon, global hotkeys,
//! and one recording per detected game. Platform capture lives behind
//! [`Backend`]. The event loop sleeps between a 2-second housekeeping tick
//! and incoming events.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::clip::{AudioOptions, ClipSource};
use crate::config::Config;
use crate::ipc::RecorderMsg;
use crate::media::VideoState;
use crate::ring::{PacketRing, TICKS_PER_SEC};

const TICK: Duration = Duration::from_secs(2);
const GRACE: Duration = Duration::from_secs(90);
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub struct GameInfo {
    /// Stable key for the recording (a process id, or "screen" on Linux).
    pub key: String,
    pub pid: u32,
    /// Lower-case executable name, as used by the include/exclude lists.
    pub exe: String,
    pub name: String,
}

/// One running capture.
pub trait Recording: Send {
    fn game(&self) -> &GameInfo;
    fn video(&self) -> Arc<Mutex<PacketRing>>;
    fn video_state(&self) -> VideoState;
    fn game_audio(&self) -> Option<Arc<Mutex<PacketRing>>>;
    /// Set when the capture stopped on its own.
    fn failure(&self) -> Option<String>;
    fn stop(&mut self);
}

pub trait Stop: Send {
    fn stop(&mut self);
}

/// What each platform provides to the recorder.
pub trait Backend {
    /// Games running now. Called every 2 seconds; must be cheap.
    fn running_games(&mut self, cfg: &Config) -> Vec<GameInfo>;
    /// Key of the game in front, when known.
    fn focused_key(&self) -> Option<String>;
    fn start(&mut self, game: &GameInfo, cfg: &Config) -> Result<Box<dyn Recording>, String>;
    fn start_mic(&mut self, cfg: &Config, ring: Arc<Mutex<PacketRing>>) -> Result<Box<dyn Stop>, String>;
    /// Executable name of the app in front, for "mark as game".
    fn focused_exe(&self) -> Option<String> {
        None
    }
    /// The include/exclude lists changed; forget cached detection results.
    fn config_changed(&mut self) {}
}

pub fn ring_for(cfg: &Config, kbps: u32) -> PacketRing {
    let secs = i64::from(cfg.replay_seconds) + 2;
    let bytes = (u64::from(kbps) * 1000 / 8) * (secs as u64 + 3) * 5 / 4;
    PacketRing::new(secs * TICKS_PER_SEC, bytes as usize)
}

type UpdateResult = Result<Option<Result<String, (String, String, String)>>, String>;

enum UserEvent {
    Menu(String),
    Hotkey(u32),
    Ipc(RecorderMsg),
    Saved(String, Result<PathBuf, String>),
    Uploaded(Result<String, String>),
    Update(UpdateResult),
}

struct Session {
    rec: Box<dyn Recording>,
    ended: Option<Instant>,
}

impl Session {
    fn running(&self) -> bool {
        self.ended.is_none()
    }
    fn end(&mut self) {
        if self.ended.is_none() {
            self.rec.stop();
            self.ended = Some(Instant::now());
        }
    }
}

struct Mic {
    ring: Arc<Mutex<PacketRing>>,
    capture: Option<Box<dyn Stop>>,
}

#[derive(Clone, Copy, PartialEq)]
enum HotkeyAction {
    Save,
    SaveAll,
    Toggle,
}

struct App {
    cfg: Config,
    backend: Box<dyn Backend>,
    proxy: EventLoopProxy<UserEvent>,
    sessions: Vec<Session>,
    mic: Option<Mic>,
    tray: Option<TrayIcon>,
    icon_idle: Option<Icon>,
    icon_rec: Option<Icon>,
    menu_sig: String,
    hotkeys: Option<GlobalHotKeyManager>,
    registered: Vec<(u32, HotkeyAction, global_hotkey::hotkey::HotKey)>,
    paused: bool,
    failed_keys: Vec<(String, Instant)>,
    next_update_check: Instant,
    update_checking: bool,
    update_ready: Option<String>,
    update_notice: Option<(String, String)>,
    next_tick: Instant,
    /// Executables last published as recording in games.json.
    published: Vec<String>,
}

fn icon(rgba: &[u8]) -> Option<Icon> {
    Icon::from_rgba(rgba.to_vec(), 64, 64).ok()
}

pub fn notify(title: &str, body: &str) {
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body).appname("Replay");
    if let Err(e) = n.show() {
        log::debug!("notification failed: {e}");
    }
}

fn play_sound() {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("afplay").arg("/System/Library/Sounds/Glass.aiff").spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("canberra-gtk-play").args(["-i", "camera-shutter"]).spawn();
}

fn open_library() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--library").spawn();
    }
}

/// Runs the recorder until "Exit" is chosen.
pub fn run(backend: Box<dyn Backend>) {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();

    let p = proxy.clone();
    if let Err(e) = super::ipc::listen(move |m| {
        let _ = p.send_event(UserEvent::Ipc(m));
    }) {
        log::info!("{e}; asking it to show the library");
        super::ipc::send(RecorderMsg::Show);
        return;
    }
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p.send_event(UserEvent::Menu(e.id.0.clone()));
    }));
    let p = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            let _ = p.send_event(UserEvent::Hotkey(e.id));
        }
    }));

    let mut backend = Some(backend);
    let mut app: Option<App> = None;
    event_loop.run(move |event, _target, control_flow| {
        match event {
            Event::NewEvents(StartCause::Init) => {
                let mut a = App::new(backend.take().expect("backend"), proxy.clone());
                a.tick();
                app = Some(a);
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(a) = app.as_mut()
                    && Instant::now() >= a.next_tick
                {
                    a.tick();
                }
            }
            Event::UserEvent(e) => {
                if let Some(a) = app.as_mut()
                    && a.handle(e)
                {
                    a.shutdown();
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            Event::LoopDestroyed => {
                if let Some(a) = app.as_mut() {
                    a.shutdown();
                }
            }
            _ => {}
        }
        if let Some(a) = app.as_ref() {
            *control_flow = ControlFlow::WaitUntil(a.next_tick);
        }
    });
}

impl App {
    fn new(backend: Box<dyn Backend>, proxy: EventLoopProxy<UserEvent>) -> Self {
        let cfg = Config::load();
        if !Config::path().exists() {
            let _ = cfg.save();
        }
        let mut app = Self {
            cfg,
            backend,
            proxy,
            sessions: Vec::new(),
            mic: None,
            tray: None,
            icon_idle: icon(include_bytes!("../../../assets/generated/icon-64.rgba")),
            icon_rec: icon(include_bytes!("../../../assets/generated/icon-recording-64.rgba")),
            menu_sig: String::new(),
            hotkeys: GlobalHotKeyManager::new().map_err(|e| log::warn!("global hotkeys unavailable: {e}")).ok(),
            registered: Vec::new(),
            paused: false,
            failed_keys: Vec::new(),
            next_update_check: Instant::now() + FIRST_UPDATE_CHECK,
            update_checking: false,
            update_ready: crate::update::staged().map(|s| s.version),
            update_notice: None,
            next_tick: Instant::now() + TICK,
            published: Vec::new(),
        };
        app.register_hotkeys(true);
        let marker = crate::paths::data_dir().join("tray-hint-shown");
        if !marker.exists() {
            notify("Replay is running", "It records in the background from the menu bar. Open it there to see your clips.");
            let _ = std::fs::create_dir_all(crate::paths::data_dir());
            let _ = std::fs::write(&marker, b"");
        }
        app.rebuild_tray();
        app
    }

    fn register_hotkeys(&mut self, announce: bool) {
        let Some(mgr) = &self.hotkeys else {
            if announce {
                notify(
                    "Replay: no global hotkeys",
                    "This desktop does not allow global hotkeys. Bind \"replay --save\" to a keyboard shortcut in your system settings.",
                );
            }
            return;
        };
        for (_, _, hk) in self.registered.drain(..) {
            let _ = mgr.unregister(hk);
        }
        let mut problems = Vec::new();
        for (text, action) in [
            (self.cfg.hotkeys.save_focused.clone(), HotkeyAction::Save),
            (self.cfg.hotkeys.save_all.clone(), HotkeyAction::SaveAll),
            (self.cfg.hotkeys.toggle_game.clone(), HotkeyAction::Toggle),
        ] {
            if text.trim().is_empty() {
                continue;
            }
            let Some(hk) = crate::hotkey::parse(&text).ok().and_then(|h| super::hotkeys::to_global(&h)) else {
                problems.push(format!("{text} is not a valid hotkey"));
                continue;
            };
            match mgr.register(hk) {
                Ok(()) => self.registered.push((hk.id(), action, hk)),
                Err(e) => problems.push(format!("{text}: {e}")),
            }
        }
        if announce && !problems.is_empty() {
            notify("Replay: hotkey problem", &problems.join("; "));
        }
    }

    fn running(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter().filter(|s| s.running())
    }

    fn tick(&mut self) {
        self.next_tick = Instant::now() + TICK;
        // Captures that failed on their own.
        for s in self.sessions.iter_mut().filter(|s| s.running()) {
            if let Some(e) = s.rec.failure() {
                log::error!("{}: recording stopped: {e}", s.rec.game().name);
                notify("Replay: recording stopped", &format!("{}: {e}", s.rec.game().name));
                self.failed_keys.push((s.rec.game().key.clone(), Instant::now()));
                s.end();
            }
        }
        self.failed_keys.retain(|(_, t)| t.elapsed() < Duration::from_secs(30));

        let games = if self.paused { Vec::new() } else { self.backend.running_games(&self.cfg) };
        for s in self.sessions.iter_mut().filter(|s| s.running()) {
            if !games.iter().any(|g| g.key == s.rec.game().key) {
                log::info!("{} closed", s.rec.game().name);
                s.end();
            }
        }
        for g in games {
            if self.sessions.iter().any(|s| s.running() && s.rec.game().key == g.key)
                || self.failed_keys.iter().any(|(k, _)| *k == g.key)
            {
                continue;
            }
            log::info!("detected {} (pid {})", g.name, g.pid);
            crate::games::note_detected(&g.exe, &g.name, "", "Detected");
            match self.backend.start(&g, &self.cfg) {
                Ok(rec) => {
                    // A new session for the same key replaces any ended one.
                    self.sessions.retain(|s| s.rec.game().key != g.key);
                    self.sessions.push(Session { rec, ended: None });
                }
                Err(e) => {
                    log::error!("cannot record {}: {e}", g.name);
                    notify("Replay: cannot record", &format!("{}: {e}", g.name));
                    self.failed_keys.push((g.key.clone(), Instant::now()));
                }
            }
        }
        self.sessions.retain(|s| s.ended.is_none_or(|t| t.elapsed() < GRACE));
        self.ensure_mic();

        if self.cfg.updates.auto_check && !self.update_checking && Instant::now() >= self.next_update_check {
            self.next_update_check = Instant::now() + UPDATE_EVERY;
            self.start_update_check();
        }
        self.rebuild_tray();
    }

    fn ensure_mic(&mut self) {
        let want = self.cfg.audio.microphone && self.running().next().is_some();
        match (&mut self.mic, want) {
            (None, true) => {
                let ring = Arc::new(Mutex::new(ring_for(&self.cfg, self.cfg.audio.bitrate_kbps)));
                let capture = self.backend.start_mic(&self.cfg, ring.clone()).map_err(|e| log::warn!("microphone: {e}")).ok();
                self.mic = Some(Mic { ring, capture });
            }
            (Some(m), false) => {
                if let Some(mut c) = m.capture.take() {
                    c.stop();
                }
                if self.sessions.is_empty() {
                    self.mic = None;
                }
            }
            (Some(m), true) if m.capture.is_none() => {
                self.mic = None;
                self.ensure_mic();
            }
            _ => {}
        }
    }

    fn rebuild_tray(&mut self) {
        let mut exes: Vec<String> = self.running().map(|s| s.rec.game().exe.clone()).collect();
        exes.sort();
        if exes != self.published {
            crate::games::note_recording(&exes);
            self.published = exes;
        }
        let names: Vec<String> = self.running().map(|s| s.rec.game().name.clone()).collect();
        let sig = format!(
            "{names:?}|{}|{:?}|{:?}|{}|{}",
            self.paused,
            self.update_ready,
            self.update_notice.as_ref().map(|n| &n.0),
            self.sessions.len(),
            self.cfg.hotkeys.save_focused
        );
        if sig == self.menu_sig && self.tray.is_some() {
            return;
        }
        self.menu_sig = sig;
        let menu = Menu::new();
        let any = !self.sessions.is_empty();
        let item = |id: &str, text: &str, enabled: bool| MenuItem::with_id(id, text, enabled, None);
        let _ = menu.append(&item("save", &format!("Save replay ({})", self.cfg.hotkeys.save_focused), any));
        let _ = menu.append(&item("save-all", "Save all games", any));
        let _ = menu.append(&PredefinedMenuItem::separator());
        if self.sessions.is_empty() {
            let _ = menu.append(&item("none", "No game detected", false));
        }
        for s in &self.sessions {
            let g = s.rec.game();
            let state = if s.running() { "Recording" } else { "Closed" };
            let _ = menu.append(&item(&format!("info:{}", g.key), &format!("{state}: {}", g.name), false));
            if s.running() {
                let _ = menu.append(&item(&format!("stop:{}", g.key), &format!("    Stop recording {}", g.name), true));
            }
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&CheckMenuItem::with_id("pause", "Pause game detection", true, self.paused, None));
        let _ = menu.append(&item("folder", "Open clips folder", true));
        let _ = menu.append(&item("library", "Clips, editor and settings...", true));
        let _ = menu.append(&PredefinedMenuItem::separator());
        if let Some(v) = &self.update_ready {
            let _ = menu.append(&item("update", &format!("Restart to update to {v}"), true));
        } else if let Some((v, _)) = &self.update_notice {
            let _ = menu.append(&item("release", &format!("Replay {v} is available..."), true));
        }
        let _ = menu.append(&item("exit", "Quit Replay", true));

        let recording = !names.is_empty();
        let tip = if recording { format!("Replay: recording {}", names.join(", ")) } else { "Replay: waiting for a game".into() };
        let icon = if recording { self.icon_rec.clone() } else { self.icon_idle.clone() };
        match &self.tray {
            Some(t) => {
                t.set_menu(Some(Box::new(menu)));
                let _ = t.set_icon(icon);
                let _ = t.set_tooltip(Some(tip));
            }
            None => {
                let mut b = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(tip);
                if let Some(i) = icon {
                    b = b.with_icon(i);
                }
                match b.build() {
                    Ok(t) => self.tray = Some(t),
                    Err(e) => log::error!("cannot create the tray icon: {e}"),
                }
            }
        }
    }

    /// Returns true when the app must exit.
    fn handle(&mut self, e: UserEvent) -> bool {
        match e {
            UserEvent::Menu(id) => return self.on_menu(&id),
            UserEvent::Hotkey(id) => {
                match self.registered.iter().find(|(i, _, _)| *i == id).map(|(_, a, _)| *a) {
                    Some(HotkeyAction::Save) => self.save_focused(),
                    Some(HotkeyAction::SaveAll) => self.save_all(),
                    Some(HotkeyAction::Toggle) => self.toggle_focused(),
                    None => {}
                }
            }
            UserEvent::Ipc(m) => match m {
                RecorderMsg::Reload => self.reload(),
                RecorderMsg::Save(all) => {
                    if all {
                        self.save_all()
                    } else {
                        self.save_focused()
                    }
                }
                RecorderMsg::Show => open_library(),
                RecorderMsg::UpdateStaged => {
                    self.update_ready = crate::update::staged().map(|s| s.version);
                    self.rebuild_tray();
                }
                RecorderMsg::InstallUpdate => return self.install_update(),
            },
            UserEvent::Saved(game, r) => self.on_saved(game, r),
            UserEvent::Uploaded(r) => match r {
                Ok(url) => {
                    let copied = self.cfg.share.copy_link_after_upload && super::shell::copy_text(&url);
                    notify("Replay uploaded", &if copied { format!("{url}\nLink copied.") } else { url });
                }
                Err(e) => notify("Replay: upload failed", &e),
            },
            UserEvent::Update(r) => self.on_update_result(r),
        }
        false
    }

    fn on_menu(&mut self, id: &str) -> bool {
        match id {
            "save" => self.save_focused(),
            "save-all" => self.save_all(),
            "pause" => {
                self.paused = !self.paused;
                if self.paused {
                    for s in &mut self.sessions {
                        s.end();
                    }
                }
                self.ensure_mic();
                self.rebuild_tray();
            }
            "folder" => {
                let _ = std::fs::create_dir_all(&self.cfg.output_dir);
                super::shell::open_path(&self.cfg.output_dir);
            }
            "library" => open_library(),
            "update" => return self.install_update(),
            "release" => {
                if let Some((_, page)) = &self.update_notice {
                    super::shell::open_url(page);
                }
            }
            "exit" => return true,
            other => {
                if let Some(key) = other.strip_prefix("stop:") {
                    for s in self.sessions.iter_mut().filter(|s| s.rec.game().key == key) {
                        s.end();
                    }
                    self.failed_keys.push((key.to_string(), Instant::now() + Duration::from_secs(3600 * 24)));
                    self.ensure_mic();
                    self.rebuild_tray();
                }
            }
        }
        false
    }

    fn session_for_save(&self) -> Option<usize> {
        let by_key = |k: &str| self.sessions.iter().rposition(|s| s.rec.game().key == k);
        self.backend
            .focused_key()
            .and_then(|k| by_key(&k))
            .or_else(|| self.sessions.iter().rposition(Session::running))
            .or_else(|| (!self.sessions.is_empty()).then(|| self.sessions.len() - 1))
    }

    fn save_focused(&mut self) {
        match self.session_for_save() {
            Some(i) => self.save(i),
            None => notify("Replay", "No game is being recorded."),
        }
    }

    fn save_all(&mut self) {
        if self.sessions.is_empty() {
            notify("Replay", "No game is being recorded.");
        }
        for i in 0..self.sessions.len() {
            self.save(i);
        }
    }

    fn save(&mut self, i: usize) {
        let s = &self.sessions[i];
        let dur = i64::from(self.cfg.replay_seconds) * TICKS_PER_SEC;
        let video = s.rec.video().lock().map(|r| r.snapshot(dur)).unwrap_or_default();
        let Some(t0) = video.first().map(|p| p.pts) else {
            notify("Replay", &format!("{}: nothing recorded yet.", s.rec.game().name));
            return;
        };
        let state = s.rec.video_state();
        let game_audio = s.rec.game_audio().and_then(|r| r.lock().ok().map(|r| r.snapshot_from(t0)));
        let mic = self.mic.as_ref().and_then(|m| m.ring.lock().ok().map(|r| r.snapshot_from(t0)));
        let name = s.rec.game().name.clone();
        let src = ClipSource { game_name: name.clone(), video, state, game_audio, mic };
        let dir = self.cfg.output_dir.clone();
        let opts = AudioOptions::from_config(&self.cfg);
        let proxy = self.proxy.clone();
        if self.cfg.sound {
            play_sound();
        }
        std::thread::Builder::new()
            .name("save".into())
            .spawn(move || {
                let started = Instant::now();
                let r = crate::clip::write_clip(src, &dir, &opts);
                if let Ok(p) = &r {
                    log::info!("saved {} in {} ms", p.display(), started.elapsed().as_millis());
                }
                let _ = proxy.send_event(UserEvent::Saved(name, r));
            })
            .ok();
    }

    fn on_saved(&mut self, game: String, r: Result<PathBuf, String>) {
        match r {
            Ok(p) => {
                if self.cfg.notify {
                    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    notify(&format!("Replay saved: {game}"), &format!("{:.1} MB, in {}", size as f64 / 1e6, p.display()));
                }
                if self.cfg.share.auto_upload {
                    let proxy = self.proxy.clone();
                    crate::share::upload_in_background(p, self.cfg.share.clone(), move |r| {
                        let _ = proxy.send_event(UserEvent::Uploaded(r));
                    });
                }
            }
            Err(e) => {
                log::error!("save failed: {e}");
                notify("Replay: save failed", &e);
            }
        }
    }

    fn toggle_focused(&mut self) {
        let Some(exe) = self.backend.focused_exe() else {
            notify("Replay", "Marking apps as games is not available on this desktop. Use Settings > Game detection.");
            return;
        };
        let recording = self.backend.focused_key().is_some_and(|k| self.sessions.iter().any(|s| s.running() && s.rec.game().key == k));
        let mut cfg = self.cfg.clone();
        let msg = if recording {
            cfg.detection.include.retain(|e| e != &exe);
            cfg.detection.exclude.push(exe.clone());
            format!("{exe} will not be recorded.")
        } else {
            cfg.detection.exclude.retain(|e| e != &exe);
            cfg.detection.include.push(exe.clone());
            format!("{exe} is now marked as a game.")
        };
        self.cfg = cfg.sanitized();
        let _ = self.cfg.save();
        notify("Replay", &msg);
        self.tick();
    }

    fn reload(&mut self) {
        let old = std::mem::replace(&mut self.cfg, Config::load());
        log::info!("settings reloaded");
        if old.hotkeys != self.cfg.hotkeys {
            self.register_hotkeys(true);
        }
        if old.detection != self.cfg.detection {
            // Games turned off stop at the next tick; turned on start again.
            self.backend.config_changed();
            self.failed_keys.clear();
        }
        if old.video != self.cfg.video || old.replay_seconds != self.cfg.replay_seconds || old.audio != self.cfg.audio {
            for s in &mut self.sessions {
                s.end();
            }
            self.sessions.clear();
            if let Some(mut m) = self.mic.take()
                && let Some(mut c) = m.capture.take()
            {
                c.stop();
            }
        }
        self.tick();
    }

    fn start_update_check(&mut self) {
        if self.update_ready.is_some() {
            return;
        }
        self.update_checking = true;
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                let r: UpdateResult = (|| {
                    let Some(rel) = crate::update::check()? else { return Ok(None) };
                    match crate::update::can_self_update() {
                        Ok(()) => crate::update::download(&rel, &|_, _| {}).map(|s| Some(Ok(s.version))),
                        Err(why) => Ok(Some(Err((rel.version, rel.page, why)))),
                    }
                })();
                let _ = proxy.send_event(UserEvent::Update(r));
            })
            .ok();
    }

    fn on_update_result(&mut self, r: UpdateResult) {
        self.update_checking = false;
        match r {
            Ok(Some(Ok(v))) => {
                notify(&format!("Replay {v} is ready"), "Open the Replay menu and choose \"Restart to update\".");
                self.update_ready = Some(v);
            }
            Ok(Some(Err((v, page, why)))) => {
                if self.update_notice.as_ref().map(|n| &n.0) != Some(&v) {
                    notify(&format!("Replay {v} is available"), &why);
                }
                self.update_notice = Some((v, page));
            }
            Ok(None) => log::debug!("Replay is up to date"),
            Err(e) => log::warn!("update check failed: {e}"),
        }
        self.rebuild_tray();
    }

    /// Returns true when the app must exit to finish the update.
    fn install_update(&mut self) -> bool {
        let Some(staged) = crate::update::staged() else {
            notify("Replay", "No downloaded update was found.");
            return false;
        };
        for s in &mut self.sessions {
            s.end();
        }
        if let Some(mut m) = self.mic.take()
            && let Some(mut c) = m.capture.take()
        {
            c.stop();
        }
        super::ipc::cleanup();
        match crate::update::apply_and_restart(&staged, &[]) {
            Ok(()) => true,
            Err(e) => {
                log::error!("update failed: {e}");
                notify("Replay: update failed", &e);
                false
            }
        }
    }

    fn shutdown(&mut self) {
        for s in &mut self.sessions {
            s.end();
        }
        self.sessions.clear();
        if let Some(mut m) = self.mic.take()
            && let Some(mut c) = m.capture.take()
        {
            c.stop();
        }
        self.tray = None;
        super::ipc::cleanup();
        log::info!("exit");
    }
}
