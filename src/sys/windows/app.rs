//! The resident tray process: one hidden window on the main thread receives
//! hotkeys, tray clicks, foreground-window events and worker notifications.
//! Nothing polls fast: the only timer runs every 2 seconds and does a few
//! cheap window checks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{NIN_BALLOONUSERCLICK, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::clip::{AudioOptions, ClipSource};
use crate::config::Config;
use crate::detect::Detector;
use crate::gpu::Gpu;
use crate::ring::{PacketRing, TICKS_PER_SEC};
use crate::session::{Session, SessionEnd};
use crate::tray::{self, Icons};

pub const WINDOW_CLASS: PCWSTR = w!("ReplayRecorderWindow");
pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_SESSION_END: u32 = WM_APP + 2;
pub const WM_APP_CLIP_DONE: u32 = WM_APP + 3;
/// Sent by the library window after it saved the config.
pub const WM_APP_RELOAD: u32 = WM_APP + 4;
pub const WM_APP_UPLOAD_DONE: u32 = WM_APP + 5;
/// Sent by a second instance: open the library window.
pub const WM_APP_SHOW: u32 = WM_APP + 6;
/// Sent by `replay --save` (wParam 0) or `replay --save-all` (wParam 1).
pub const WM_APP_SAVE: u32 = WM_APP + 7;
/// Result of a background update check (lParam: Box<UpdateResult>).
pub const WM_APP_UPDATE_RESULT: u32 = WM_APP + 8;
/// The library window downloaded an update.
pub const WM_APP_UPDATE_STAGED: u32 = WM_APP + 9;
/// The library window asks to install the staged update now.
pub const WM_APP_INSTALL_UPDATE: u32 = WM_APP + 10;
/// A session recording finished (lParam: Box<(String, Result<PathBuf, String>)>).
pub const WM_APP_RECORD_DONE: u32 = WM_APP + 11;

/// Ok(None): up to date. Ok(Some(Ok(v))): v is downloaded and verified.
/// Ok(Some(Err((v, page, why)))): v exists but cannot be installed here.
type UpdateResult = Result<Option<Result<String, (String, String, String)>>, String>;

const UPDATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const FIRST_UPDATE_CHECK: Duration = Duration::from_secs(60);

const TIMER_TICK: usize = 1;
const TICK_MS: u32 = 2000;
const HK_SAVE: i32 = 1;
const HK_SAVE_ALL: i32 = 2;
const HK_TOGGLE: i32 = 3;
const HK_RECORD: i32 = 4;
const MAX_SESSIONS: usize = 6;
/// Buffers of games that just closed stay saveable this long.
const GRACE: Duration = Duration::from_secs(90);

const CMD_SAVE: usize = 100;
const CMD_SAVE_ALL: usize = 101;
const CMD_PAUSE: usize = 102;
const CMD_FOLDER: usize = 103;
const CMD_LIBRARY: usize = 104;
const CMD_EXIT: usize = 105;
const CMD_UPDATE: usize = 106;
const CMD_RELEASE_PAGE: usize = 107;
const CMD_RECORD: usize = 108;
const CMD_STOP_BASE: usize = 200;

/// Starts or stops one shared capture. Its ring outlives the capture while
/// sessions remain (`idle` false), so saves in the grace time still have it.
fn ensure_shared(slot: &mut Option<Mic>, want: bool, idle: bool, name: &str, src: crate::audio::Source, max_secs: u32, kbps: u32) {
    match (slot.as_mut(), want) {
        (None, true) => {
            let ring = Arc::new(Mutex::new(crate::session::ring_for(max_secs, kbps)));
            let stop = Arc::new(AtomicBool::new(false));
            let (r, s) = (ring.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name(name.into())
                .spawn(move || crate::audio::run(src, kbps, r, s))
                .ok();
            *slot = Some(Mic { ring, stop, thread });
        }
        (Some(m), false) => {
            if m.thread.is_some() {
                m.stop();
            }
            if idle {
                *slot = None;
            }
        }
        (Some(m), true) if m.thread.is_none() => {
            *slot = None;
            ensure_shared(slot, want, idle, name, src, max_secs, kbps);
        }
        _ => {}
    }
}

struct Mic {
    ring: Arc<Mutex<PacketRing>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mic {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct App {
    hwnd: HWND,
    cfg: Config,
    detector: Detector,
    gpu: Option<Arc<Gpu>>,
    sessions: Vec<Session>,
    mic: Option<Mic>,
    /// Voice chat (Discord), shared by all games like the microphone.
    voice: Option<Mic>,
    last_focused: Option<u32>,
    retry: HashMap<u32, (Instant, u32)>,
    stopped_by_user: Vec<u32>,
    paused: bool,
    icons: Icons,
    taskbar_created: u32,
    saving: usize,
    last_clip: Option<PathBuf>,
    hook: HWINEVENTHOOK,
    /// Foreground window and rectangle at the last check, to skip repeat work.
    last_checked: Option<(isize, windows::Win32::Foundation::RECT)>,
    /// Executables last published as "recording" in games.json.
    published: RefCell<Vec<String>>,
    /// Hotkeys another program held at the last attempt; retried on the timer.
    hotkeys_missing: bool,
    next_update_check: Instant,
    update_checking: bool,
    /// Version downloaded and verified, ready to install.
    update_ready: Option<String>,
    /// A newer version this copy cannot install itself: (version, page).
    update_notice: Option<(String, String)>,
    /// Session recordings, by game process id.
    recordings: Vec<(u32, crate::record::Recording)>,
    /// Stopped recordings still writing their file.
    finishing: Vec<crate::record::Recording>,
    /// Executables last published as having a session recording.
    published_capturing: RefCell<Vec<String>>,
    /// The camera while any game records with the webcam on.
    webcam: Option<Arc<crate::webcam::Webcam>>,
    webcam_failed: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

pub fn run() -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: instance.into(), lpszClassName: WINDOW_CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            WINDOW_CLASS,
            w!("Replay"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let cfg = Config::load();
        if !Config::path().exists() {
            let _ = cfg.save();
        }
        let icons = Icons::create();
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        tray::add(hwnd, WM_APP_TRAY, icons.idle, "Replay: waiting for a game");
        // First run: tell the user where Replay lives once its window is closed.
        let marker = crate::paths::data_dir().join("tray-hint-shown");
        if !marker.exists() {
            tray::balloon(
                hwnd,
                "Replay is running",
                "It records in the background from the notification area. Double-click the icon to open your clips.",
                false,
                icons.idle,
            );
            let _ = std::fs::create_dir_all(crate::paths::data_dir());
            let _ = std::fs::write(&marker, b"");
        }

        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );

        let app = App {
            hwnd,
            detector: Detector::new(cfg.detection.clone()),
            cfg,
            gpu: None,
            sessions: Vec::new(),
            mic: None,
            voice: None,
            last_focused: None,
            retry: HashMap::new(),
            stopped_by_user: Vec::new(),
            paused: false,
            icons,
            taskbar_created,
            saving: 0,
            last_clip: None,
            hook,
            last_checked: None,
            published: RefCell::new(Vec::new()),
            hotkeys_missing: false,
            next_update_check: Instant::now() + FIRST_UPDATE_CHECK,
            update_checking: false,
            update_ready: crate::update::staged().map(|s| s.version),
            update_notice: None,
            recordings: Vec::new(),
            finishing: Vec::new(),
            published_capturing: RefCell::new(Vec::new()),
            webcam: None,
            webcam_failed: false,
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        with_app(|a| {
            a.register_hotkeys(true);
            a.scan_all_windows();
            a.check_foreground();
            a.enforce_storage();
        });
        SetTimer(Some(hwnd), TIMER_TICK, TICK_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if let Some(mut app) = APP.with(|a| a.borrow_mut().take()) {
            app.shutdown();
        }
    }
    Ok(())
}

unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object == OBJID_WINDOW.0 {
        with_app(|a| a.on_foreground(hwnd.0 as isize));
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            with_app(|a| match wparam.0 as i32 {
                HK_SAVE => a.save_focused(),
                HK_SAVE_ALL => a.save_all(),
                HK_TOGGLE => a.toggle_foreground(),
                HK_RECORD => a.toggle_record(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_TIMER => {
            with_app(App::tick);
            LRESULT(0)
        }
        WM_APP_TRAY => {
            // NOTIFYICON_VERSION_4: the event is in the low word of lParam.
            match (lparam.0 & 0xffff) as u32 {
                WM_CONTEXTMENU | WM_RBUTTONUP => show_menu(hwnd),
                WM_LBUTTONDBLCLK => open_library(),
                NIN_BALLOONUSERCLICK => {
                    if let Some(Some(p)) = with_app(|a| a.last_clip.clone()) {
                        open_library_at(Some(&p));
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_SESSION_END => {
            let end = unsafe { *Box::from_raw(lparam.0 as *mut SessionEnd) };
            with_app(|a| a.on_session_end(wparam.0 as u32, end));
            LRESULT(0)
        }
        WM_APP_CLIP_DONE => {
            let result = unsafe { *Box::from_raw(lparam.0 as *mut (String, Result<PathBuf, String>)) };
            with_app(|a| a.on_clip_done(result.0, result.1));
            LRESULT(0)
        }
        WM_APP_RECORD_DONE => {
            let result = unsafe { *Box::from_raw(lparam.0 as *mut (String, Result<PathBuf, String>)) };
            with_app(|a| a.on_record_done(result.0, result.1));
            LRESULT(0)
        }
        WM_APP_UPLOAD_DONE => {
            let result = unsafe { *Box::from_raw(lparam.0 as *mut Result<String, String>) };
            with_app(|a| a.on_upload_done(result));
            LRESULT(0)
        }
        WM_APP_RELOAD => {
            with_app(App::reload);
            LRESULT(0)
        }
        WM_APP_SHOW => {
            open_library();
            LRESULT(0)
        }
        WM_APP_SAVE => {
            with_app(|a| match wparam.0 {
                1 => a.save_all(),
                2 => a.toggle_record(),
                _ => a.save_focused(),
            });
            LRESULT(0)
        }
        WM_APP_UPDATE_RESULT => {
            let r = unsafe { *Box::from_raw(lparam.0 as *mut UpdateResult) };
            with_app(|a| a.on_update_result(r));
            LRESULT(0)
        }
        WM_APP_UPDATE_STAGED => {
            with_app(|a| {
                a.update_ready = crate::update::staged().map(|s| s.version);
                a.update_tray();
            });
            LRESULT(0)
        }
        WM_APP_INSTALL_UPDATE => {
            if with_app(App::install_update) == Some(true) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            if let Some(mut app) = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.take())) {
                app.shutdown();
            }
            LRESULT(0)
        }
        _ => {
            let tc = with_app(|a| a.taskbar_created).unwrap_or(0);
            if msg != 0 && msg == tc {
                with_app(|a| {
                    tray::add(a.hwnd, WM_APP_TRAY, a.icons.idle, "Replay");
                    a.update_tray();
                });
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn post<T>(hwnd: isize, msg: u32, wparam: usize, value: T) {
    let p = Box::into_raw(Box::new(value));
    unsafe {
        if PostMessageW(Some(HWND(hwnd as _)), msg, WPARAM(wparam), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
        }
    }
}

fn open_library() {
    open_library_at(None);
}

fn open_library_at(clip: Option<&std::path::Path>) {
    if let Ok(exe) = std::env::current_exe() {
        let mut c = std::process::Command::new(exe);
        c.arg("--library");
        if let Some(p) = clip {
            c.arg(p);
        }
        let _ = c.spawn();
    }
}

fn show_menu(hwnd: HWND) {
    // Build while borrowed; run the modal menu loop without holding the borrow.
    let Some(menu) = with_app(App::build_menu) else { return };
    let cmd = unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let c = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        c.0 as usize
    };
    match cmd {
        0 => {}
        CMD_LIBRARY => open_library(),
        CMD_UPDATE => {
            if with_app(App::install_update) == Some(true) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
        CMD_EXIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {
            with_app(|a| a.on_command(cmd));
        }
    }
}

fn fmt_dur(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl App {
    fn register_hotkeys(&mut self, announce: bool) {
        unsafe {
            for id in [HK_SAVE, HK_SAVE_ALL, HK_TOGGLE, HK_RECORD] {
                let _ = UnregisterHotKey(Some(self.hwnd), id);
            }
        }
        let keys = [
            (HK_SAVE, self.cfg.hotkeys.save_focused.clone()),
            (HK_SAVE_ALL, self.cfg.hotkeys.save_all.clone()),
            (HK_TOGGLE, self.cfg.hotkeys.toggle_game.clone()),
            (HK_RECORD, self.cfg.hotkeys.record.clone()),
        ];
        let mut problems = Vec::new();
        for (id, text) in keys {
            if text.trim().is_empty() {
                continue;
            }
            match crate::hotkey::parse(&text) {
                Ok(h) => unsafe {
                    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN};
                    let mut mods = MOD_NOREPEAT;
                    for (on, m) in [(h.ctrl, MOD_CONTROL), (h.alt, MOD_ALT), (h.shift, MOD_SHIFT), (h.win, MOD_WIN)] {
                        if on {
                            mods |= m;
                        }
                    }
                    if RegisterHotKey(Some(self.hwnd), id, mods, h.vk).is_err() {
                        if announce {
                            log::warn!("hotkey {text} is taken by another program");
                        }
                        problems.push(format!("{text} is used by another program"));
                    }
                },
                Err(e) => problems.push(e),
            }
        }
        let was_missing = self.hotkeys_missing;
        self.hotkeys_missing = !problems.is_empty();
        if was_missing && !self.hotkeys_missing {
            log::info!("all hotkeys registered");
        }
        if announce && !problems.is_empty() {
            tray::balloon(
                self.hwnd,
                "Replay: hotkey problem",
                &format!("{}. Change it in Settings. If the NVIDIA app uses it, turn off Instant Replay there.", problems.join("; ")),
                true,
                self.icons.idle,
            );
        }
    }

    fn gpu(&mut self) -> Option<Arc<Gpu>> {
        if let Some(g) = &self.gpu
            && !g.is_lost()
        {
            return Some(g.clone());
        }
        match Gpu::create() {
            Ok(g) => {
                self.gpu = Some(g.clone());
                Some(g)
            }
            Err(e) => {
                log::error!("no usable GPU: {e}");
                None
            }
        }
    }

    fn running(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter().filter(|s| s.is_running())
    }

    fn on_foreground(&mut self, hwnd: isize) {
        if let Some(s) = self.sessions.iter().find(|s| s.is_running() && s.game.hwnd == hwnd) {
            self.last_focused = Some(s.game.pid);
            return;
        }
        if self.paused {
            return;
        }
        let Some((game, reason)) = self.detector.classify(hwnd) else { return };
        self.last_focused = Some(game.pid);
        if self.stopped_by_user.contains(&game.pid) {
            return;
        }
        if let Some(i) = self.sessions.iter().position(|s| s.is_running() && s.game.pid == game.pid) {
            // Same game, new main window (for example after a launcher splash).
            if unsafe { IsWindow(Some(HWND(self.sessions[i].game.hwnd as _))) }.as_bool() {
                return;
            }
            log::info!("{}: main window changed", game.name);
            self.sessions[i].stop();
        }
        if let Some((next, _)) = self.retry.get(&game.pid)
            && Instant::now() < *next
        {
            return;
        }
        if self.running().count() >= MAX_SESSIONS {
            log::warn!("too many games at once; not recording {}", game.name);
            return;
        }
        let Some(gpu) = self.gpu() else { return };
        let (mw, mh) = crate::detect::monitor_size(game.hwnd);
        let cfg = self.cfg.for_game(&game.exe);
        let out = cfg.video.resolution.output_size(mw, mh);
        log::info!("detected {} ({}) by {reason:?}", game.name, game.exe);
        crate::games::note_detected(&game.exe, &game.name, &crate::detect::process_path(game.pid).unwrap_or_default(), reason.describe());
        let hwnd_app = self.hwnd.0 as isize;
        let target = crate::capture::Target::Window(game.hwnd);
        let cam = self.ensure_webcam(&gpu);
        let session = Session::start(gpu, game, target, &cfg, out, cam, move |pid, end| post(hwnd_app, WM_APP_SESSION_END, pid as usize, end));
        self.sessions.push(session);
        self.ensure_mic();
        self.update_tray();
    }

    /// Starts sessions for games that already run, in front or not.
    fn scan_all_windows(&mut self) {
        unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
            let list = unsafe { &mut *(l.0 as *mut Vec<isize>) };
            if unsafe { IsWindowVisible(h) }.as_bool() && unsafe { GetWindow(h, GW_OWNER) }.is_err() {
                list.push(h.0 as isize);
            }
            true.into()
        }
        let mut list: Vec<isize> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM((&raw mut list) as isize));
        }
        let focused = self.last_focused;
        for h in list {
            self.on_foreground(h);
        }
        // Scanning is not focusing; keep the real focus for "save focused".
        self.last_focused = focused;
    }

    /// Timer path: re-check the foreground window only when it changed or
    /// moved (a game may switch to fullscreen after it starts).
    fn check_foreground(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        let mut rect = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = GetWindowRect(fg, &mut rect);
        }
        let key = (fg.0 as isize, rect);
        if self.last_checked == Some(key) {
            return;
        }
        self.last_checked = Some(key);
        self.on_foreground(fg.0 as isize);
    }

    fn on_session_end(&mut self, pid: u32, end: SessionEnd) {
        let Some(s) = self.sessions.iter_mut().find(|s| s.game.pid == pid && s.is_running()) else { return };
        s.stop();
        self.last_checked = None;
        if let SessionEnd::Failed(e) = end {
            let entry = self.retry.entry(pid).or_insert((Instant::now(), 0));
            entry.1 += 1;
            let wait = Duration::from_secs(5 * u64::from(entry.1.min(12)));
            entry.0 = Instant::now() + wait;
            if entry.1 == 1 {
                tray::balloon(self.hwnd, "Replay: recording stopped", &format!("{}: {e}", s.game.name), true, self.icons.idle);
            }
            if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {
                log::warn!("GPU device lost; it will be recreated");
                self.gpu = None;
            }
        }
        self.sync_recordings();
        self.ensure_mic();
        self.update_tray();
    }

    fn tick(&mut self) {
        if self.hotkeys_missing {
            self.register_hotkeys(false);
        }
        if self.cfg.updates.auto_check && !self.update_checking && Instant::now() >= self.next_update_check {
            self.next_update_check = Instant::now() + UPDATE_EVERY;
            self.start_update_check();
        }
        // Stop sessions whose window is gone (the capture item may not report it).
        for s in self.sessions.iter_mut().filter(|s| s.is_running()) {
            if !unsafe { IsWindow(Some(HWND(s.game.hwnd as _))) }.as_bool() {
                log::info!("{} closed", s.game.name);
                s.stop();
            }
        }
        let before = self.sessions.len();
        self.sessions.retain(|s| s.ended.is_none_or(|t| t.elapsed() < GRACE));
        if !self.stopped_by_user.is_empty() {
            self.stopped_by_user.retain(|pid| crate::detect::process_exe(*pid).is_some());
        }
        if !self.retry.is_empty() {
            self.retry.retain(|pid, _| crate::detect::process_exe(*pid).is_some());
        }
        if self.running().next().is_none() && self.webcam.take().is_some() {
            log::info!("no games; webcam closed");
        }
        if self.sessions.is_empty() && self.gpu.take().is_some() {
            log::info!("no games; GPU device released");
        }
        self.sync_recordings();
        if self.sessions.len() != before || self.running().count() == 0 {
            self.ensure_mic();
        }
        self.check_foreground();
        self.update_tray();
    }

    /// Starts or stops the shared captures (microphone, voice chat) as games run.
    fn ensure_mic(&mut self) {
        let running = self.running().next().is_some();
        let (a, max, idle) = (&self.cfg.audio, self.cfg.max_replay_seconds(), self.sessions.is_empty());
        let mic = crate::audio::Source::Microphone(a.microphone_device.clone());
        ensure_shared(&mut self.mic, a.microphone && running, idle, "mic", mic, max, a.bitrate_kbps);
        ensure_shared(&mut self.voice, a.voice_chat && running, idle, "voice", crate::audio::Source::VoiceChat, max, a.bitrate_kbps);
    }

    fn stop_shared(&mut self) {
        for m in [self.mic.take(), self.voice.take()].into_iter().flatten() {
            let mut m = m;
            m.stop();
        }
    }

    fn update_tray(&self) {
        let mut exes: Vec<String> = self.running().map(|s| s.game.exe.clone()).collect();
        exes.sort();
        let mut capturing: Vec<String> = self
            .recordings
            .iter()
            .filter_map(|(pid, _)| self.sessions.iter().find(|s| s.game.pid == *pid).map(|s| s.game.exe.clone()))
            .collect();
        capturing.sort();
        if *self.published.borrow() != exes || *self.published_capturing.borrow() != capturing {
            crate::games::note_recording(&exes, &capturing);
            *self.published.borrow_mut() = exes;
            *self.published_capturing.borrow_mut() = capturing;
        }
        let names: Vec<&str> = self.running().map(|s| s.game.name.as_str()).collect();
        let (icon, tip) = if names.is_empty() {
            (self.icons.idle, if self.paused { "Replay: paused".to_string() } else { "Replay: waiting for a game".to_string() })
        } else if !self.recordings.is_empty() {
            let rec: Vec<&str> = self.recordings.iter().map(|(_, r)| r.game.as_str()).collect();
            (self.icons.recording, format!("Replay: recording the session of {}", rec.join(", ")))
        } else {
            (self.icons.recording, format!("Replay: recording {}", names.join(", ")))
        };
        tray::update(self.hwnd, icon, &tip);
    }

    fn session_for_save(&self) -> Option<usize> {
        let fg = unsafe { GetForegroundWindow() };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
        let by_pid = |p: u32| self.sessions.iter().rposition(|s| s.game.pid == p);
        by_pid(pid)
            .or_else(|| self.last_focused.and_then(by_pid))
            .or_else(|| self.sessions.iter().rposition(Session::is_running))
            .or_else(|| (!self.sessions.is_empty()).then(|| self.sessions.len() - 1))
    }

    fn save_focused(&mut self) {
        match self.session_for_save() {
            Some(i) => self.save(i),
            None => tray::balloon(self.hwnd, "Replay", "No game is being recorded.", true, self.icons.idle),
        }
    }

    fn save_all(&mut self) {
        if self.sessions.is_empty() {
            tray::balloon(self.hwnd, "Replay", "No game is being recorded.", true, self.icons.idle);
        }
        for i in 0..self.sessions.len() {
            self.save(i);
        }
    }

    fn save(&mut self, i: usize) {
        let s = &self.sessions[i];
        let dur = i64::from(s.replay_seconds) * TICKS_PER_SEC;
        // Snapshots share packet memory with the rings; this takes microseconds.
        let video = s.video.lock().map(|r| r.snapshot(dur)).unwrap_or_default();
        let Some(t0) = video.first().map(|p| p.pts) else {
            tray::balloon(self.hwnd, "Replay", &format!("{}: nothing recorded yet.", s.game.name), true, self.icons.idle);
            return;
        };
        let state = s.video_state.lock().map(|v| v.clone());
        let Ok(state) = state else { return };
        let game_audio = s.game_audio.as_ref().and_then(|r| r.lock().ok().map(|r| r.snapshot_from(t0)));
        let mic = self.mic.as_ref().and_then(|m| m.ring.lock().ok().map(|r| r.snapshot_from(t0)));
        let voice = self.voice.as_ref().and_then(|m| m.ring.lock().ok().map(|r| r.snapshot_from(t0)));
        let src = ClipSource { game_name: s.game.name.clone(), video, state, game_audio, mic, voice };
        let dir = self.cfg.output_dir.clone();
        let opts = AudioOptions::from_config(&self.cfg);
        let hwnd = self.hwnd.0 as isize;
        let name = s.game.name.clone();
        self.notice(Some(s.game.hwnd), crate::overlay::Kind::Busy, "Saving replay", &s.game.name, Some(4000));
        self.saving += 1;
        if self.cfg.sound {
            unsafe {
                let _ = PlaySoundW(w!("DeviceConnect"), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
            }
        }
        std::thread::Builder::new()
            .name("save".into())
            .spawn(move || {
                crate::util::set_thread("replay save", windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL);
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                }
                let started = Instant::now();
                let r = crate::clip::write_clip(src, &dir, &opts);
                if let Ok(p) = &r {
                    log::info!("saved {} in {} ms", p.display(), started.elapsed().as_millis());
                }
                post(hwnd, WM_APP_CLIP_DONE, 0, (name, r));
            })
            .ok();
    }

    fn on_clip_done(&mut self, game: String, r: Result<PathBuf, String>) {
        self.saving = self.saving.saturating_sub(1);
        match r {
            Ok(p) => {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                let secs = crate::share::duration_hint(&p).unwrap_or(u64::from(self.cfg.replay_seconds));
                if self.cfg.notify {
                    tray::balloon(
                        self.hwnd,
                        &format!("Replay saved: {game}"),
                        &format!("{} - {:.1} MB. Click to edit or share.", fmt_dur(secs), size as f64 / 1e6),
                        false,
                        self.icons.recording,
                    );
                }
                self.notice(
                    None,
                    crate::overlay::Kind::Done,
                    "Replay saved",
                    &format!("{game}  \u{00b7}  {}  \u{00b7}  {:.1} MB", fmt_dur(secs), size as f64 / 1e6),
                    Some(2500),
                );
                self.last_clip = Some(p.clone());
                self.enforce_storage();
                if self.cfg.share.auto_upload {
                    let hwnd = self.hwnd.0 as isize;
                    crate::share::upload_in_background(p, self.cfg.share.clone(), move |r| post(hwnd, WM_APP_UPLOAD_DONE, 0, r));
                }
            }
            Err(e) => {
                self.notice(None, crate::overlay::Kind::Error, "Replay not saved", &e, Some(5000));
                log::error!("save failed: {e}");
                tray::balloon(self.hwnd, "Replay: save failed", &e, true, self.icons.idle);
            }
        }
    }

    /// Starts a session recording of the game in front, or stops it.
    fn toggle_record(&mut self) {
        let Some(i) = self.session_for_save() else {
            tray::balloon(self.hwnd, "Replay", "No game is being recorded.", true, self.icons.idle);
            return;
        };
        let pid = self.sessions[i].game.pid;
        if let Some(k) = self.recordings.iter().position(|(p, _)| *p == pid) {
            self.stop_recording(k);
            return;
        }
        let s = &self.sessions[i];
        if !s.is_running() {
            tray::balloon(self.hwnd, "Replay", &format!("{} is closed.", s.game.name), true, self.icons.idle);
            return;
        }
        let src = crate::record::Sources {
            video: s.video.clone(),
            state: s.video_state.clone(),
            game: s.game_audio.clone(),
            mic: self.mic.as_ref().map(|m| m.ring.clone()),
            voice: self.voice.as_ref().map(|m| m.ring.clone()),
        };
        let (name, hwnd_game) = (s.game.name.clone(), s.game.hwnd);
        let hwnd = self.hwnd.0 as isize;
        let n2 = name.clone();
        match crate::record::start(&self.cfg.output_dir, &name, src, AudioOptions::from_config(&self.cfg), move |r| {
            post(hwnd, WM_APP_RECORD_DONE, 0, (n2, r))
        }) {
            Ok(rec) => {
                self.recordings.push((pid, rec));
                let key = &self.cfg.hotkeys.record;
                let detail = if key.is_empty() { name.clone() } else { format!("{name}  \u{00b7}  {key} stops it") };
                self.notice(Some(hwnd_game), crate::overlay::Kind::Recording, "Recording started", &detail, Some(3000));
                if self.cfg.sound {
                    unsafe {
                        let _ = PlaySoundW(w!("DeviceConnect"), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
                    }
                }
            }
            Err(e) => {
                self.notice(Some(hwnd_game), crate::overlay::Kind::Error, "Recording did not start", &e, Some(5000));
                tray::balloon(self.hwnd, "Replay: recording did not start", &e, true, self.icons.idle);
            }
        }
        self.update_tray();
    }

    fn stop_recording(&mut self, k: usize) {
        let (pid, mut rec) = self.recordings.remove(k);
        rec.stop();
        let near = self.sessions.iter().find(|s| s.game.pid == pid).map(|s| s.game.hwnd);
        let secs = rec.started.elapsed().as_secs();
        self.notice(near, crate::overlay::Kind::Busy, "Saving recording", &format!("{}  \u{00b7}  {}", rec.game, fmt_dur(secs)), Some(4000));
        self.finishing.push(rec);
        self.update_tray();
    }

    /// Stops recordings whose game stopped recording.
    fn sync_recordings(&mut self) {
        while let Some(k) = self.recordings.iter().position(|(pid, _)| !self.sessions.iter().any(|s| s.game.pid == *pid && s.is_running())) {
            self.stop_recording(k);
        }
    }

    fn on_record_done(&mut self, game: String, r: Result<PathBuf, String>) {
        if let Some(i) = self.finishing.iter().position(|f| f.game == game) {
            let mut f = self.finishing.remove(i);
            f.stop_and_wait();
        }
        match r {
            Ok(p) => {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                let secs = crate::share::duration_hint(&p).unwrap_or(0);
                let detail = format!("{game}  \u{00b7}  {}  \u{00b7}  {:.0} MB", fmt_dur(secs), size as f64 / 1e6);
                self.notice(None, crate::overlay::Kind::Done, "Recording saved", &detail, Some(3000));
                if self.cfg.notify {
                    tray::balloon(self.hwnd, &format!("Recording saved: {game}"), &format!("{} - {:.0} MB. Click to open it.", fmt_dur(secs), size as f64 / 1e6), false, self.icons.recording);
                }
                self.last_clip = Some(p);
                self.enforce_storage();
            }
            Err(e) => {
                self.notice(None, crate::overlay::Kind::Error, "Recording not saved", &e, Some(5000));
                tray::balloon(self.hwnd, "Replay: recording not saved", &e, true, self.icons.idle);
            }
        }
    }

    /// The webcam for a new session, started on first use.
    fn ensure_webcam(&mut self, gpu: &Arc<Gpu>) -> Option<Arc<crate::webcam::Webcam>> {
        if !self.cfg.webcam.enabled || self.webcam_failed {
            return None;
        }
        if let Some(w) = &self.webcam {
            return Some(w.clone());
        }
        match crate::webcam::Webcam::start(gpu.clone(), &self.cfg.webcam.device) {
            Ok(w) => {
                self.webcam = Some(w.clone());
                Some(w)
            }
            Err(e) => {
                log::warn!("webcam: {e}");
                self.webcam_failed = true;
                tray::balloon(self.hwnd, "Replay: webcam not shown", &format!("{e}. Recording continues without it."), true, self.icons.idle);
                None
            }
        }
    }

    /// Shows the in-game notice when the user wants it.
    fn notice(&self, near: Option<isize>, kind: crate::overlay::Kind, title: &str, detail: &str, hold_ms: Option<u32>) {
        if self.cfg.overlay.enabled {
            crate::overlay::show(near, self.cfg.overlay.corner, kind, title, detail, hold_ms);
        }
    }

    /// Deletes clips over the storage limits, on a background thread.
    fn enforce_storage(&self) {
        let s = &self.cfg.storage;
        if s.max_gb == 0 && s.max_age_days == 0 {
            return;
        }
        let cfg = self.cfg.clone();
        std::thread::Builder::new()
            .name("storage".into())
            .spawn(move || {
                crate::util::set_thread("replay storage", windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL);
                let (n, bytes) = crate::library::enforce_storage(&cfg);
                if n > 0 {
                    log::info!("storage limits: deleted {n} clips ({:.1} GB)", bytes as f64 / 1e9);
                }
            })
            .ok();
    }

    /// Checks GitHub, and downloads and verifies a newer release in the background.
    fn start_update_check(&mut self) {
        if self.update_ready.is_some() {
            return;
        }
        self.update_checking = true;
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("update".into())
            .spawn(move || {
                crate::util::set_thread("replay update", windows::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL);
                let r: UpdateResult = (|| {
                    let Some(rel) = crate::update::check()? else { return Ok(None) };
                    match crate::update::can_self_update() {
                        Ok(()) => crate::update::download(&rel, &|_, _| {}).map(|s| Some(Ok(s.version))),
                        Err(why) => Ok(Some(Err((rel.version, rel.page, why)))),
                    }
                })();
                post(hwnd, WM_APP_UPDATE_RESULT, 0, r);
            })
            .ok();
    }

    fn on_update_result(&mut self, r: UpdateResult) {
        self.update_checking = false;
        match r {
            Ok(Some(Ok(v))) => {
                tray::balloon(
                    self.hwnd,
                    &format!("Replay {v} is ready"),
                    "Right-click the Replay icon and choose \"Restart to update\".",
                    false,
                    self.icons.idle,
                );
                self.update_ready = Some(v);
            }
            Ok(Some(Err((v, page, why)))) => {
                if self.update_notice.as_ref().map(|n| &n.0) != Some(&v) {
                    tray::balloon(self.hwnd, &format!("Replay {v} is available"), &why, false, self.icons.idle);
                }
                self.update_notice = Some((v, page));
            }
            Ok(None) => log::debug!("Replay is up to date"),
            Err(e) => log::warn!("update check failed: {e}"),
        }
        self.update_tray();
    }

    /// Installs the staged update and starts the new version.
    /// Returns true when this process must now exit.
    fn install_update(&mut self) -> bool {
        let Some(staged) = crate::update::staged() else {
            tray::balloon(self.hwnd, "Replay", "No downloaded update was found.", true, self.icons.idle);
            return false;
        };
        for s in &mut self.sessions {
            s.stop();
        }
        self.finish_all_recordings();
        self.stop_shared();
        match crate::update::apply_and_restart(&staged, &[]) {
            Ok(()) => true,
            Err(e) => {
                log::error!("update failed: {e}");
                tray::balloon(self.hwnd, "Replay: update failed", &e, true, self.icons.idle);
                false
            }
        }
    }

    fn on_upload_done(&mut self, r: Result<String, String>) {
        match r {
            Ok(url) => {
                if self.cfg.share.copy_link_after_upload {
                    crate::sys::shell::copy_text(&url);
                }
                let msg = if self.cfg.share.copy_link_after_upload { format!("{url}\nLink copied.") } else { url };
                tray::balloon(self.hwnd, "Replay uploaded", &msg, false, self.icons.recording);
            }
            Err(e) => tray::balloon(self.hwnd, "Replay: upload failed", &e, true, self.icons.idle),
        }
    }

    fn toggle_foreground(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
        let Some(exe) = crate::detect::process_exe(pid) else { return };
        let recording = self.sessions.iter().any(|s| s.is_running() && s.game.pid == pid);
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
        if let Err(e) = self.cfg.save() {
            log::error!("cannot save config: {e}");
        }
        self.detector.set_config(self.cfg.detection.clone());
        self.stopped_by_user.retain(|p| *p != pid);
        if recording {
            for s in self.sessions.iter_mut().filter(|s| s.game.pid == pid) {
                s.stop();
            }
        } else {
            self.retry.remove(&pid);
            self.on_foreground(fg.0 as isize);
        }
        tray::balloon(self.hwnd, "Replay", &msg, false, self.icons.idle);
        self.ensure_mic();
        self.update_tray();
    }

    fn reload(&mut self) {
        let old = std::mem::replace(&mut self.cfg, Config::load());
        log::info!("settings reloaded");
        self.detector.set_config(self.cfg.detection.clone());
        if old.hotkeys != self.cfg.hotkeys {
            self.register_hotkeys(true);
        }
        if old.storage != self.cfg.storage {
            self.enforce_storage();
        }
        if old.detection != self.cfg.detection {
            // Stop games the user turned off; look again for games turned on.
            for s in self.sessions.iter_mut().filter(|s| s.is_running()) {
                if !crate::games::is_enabled(&self.cfg.detection, &s.game.exe) {
                    log::info!("{} turned off; recording stopped", s.game.name);
                    s.stop();
                }
            }
            self.stopped_by_user.clear();
            self.last_checked = None;
            self.scan_all_windows();
        }
        let restart = old.video != self.cfg.video
            || old.replay_seconds != self.cfg.replay_seconds
            || old.audio != self.cfg.audio
            || old.games != self.cfg.games
            || old.webcam != self.cfg.webcam;
        if old.webcam != self.cfg.webcam {
            self.webcam = None;
            self.webcam_failed = false;
        }
        if restart {
            // New encoder settings need new sessions; buffers start again.
            for s in &mut self.sessions {
                s.stop();
            }
            self.sync_recordings();
            self.sessions.clear();
            self.stop_shared();
            self.last_checked = None;
            self.scan_all_windows();
            self.check_foreground();
        }
        self.ensure_mic();
        self.update_tray();
    }

    fn build_menu(&mut self) -> HMENU {
        unsafe {
            let m = CreatePopupMenu().unwrap_or_default();
            let add = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
                let t = crate::util::wide(text);
                let _ = AppendMenuW(m, flags, id, PCWSTR(t.as_ptr()));
            };
            let any = !self.sessions.is_empty();
            let dis = if any { MF_STRING } else { MF_STRING | MF_GRAYED };
            add(dis, CMD_SAVE, &format!("Save replay\t{}", self.cfg.hotkeys.save_focused));
            add(dis, CMD_SAVE_ALL, &format!("Save all games\t{}", self.cfg.hotkeys.save_all));
            let rec_label = match self.recordings.first() {
                Some((_, r)) => format!("Stop recording {} ({})\t{}", r.game, fmt_dur(r.started.elapsed().as_secs()), self.cfg.hotkeys.record),
                None => format!("Start recording\t{}", self.cfg.hotkeys.record),
            };
            add(if any || !self.recordings.is_empty() { MF_STRING } else { MF_STRING | MF_GRAYED }, CMD_RECORD, &rec_label);
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            if self.sessions.is_empty() {
                add(MF_STRING | MF_GRAYED, 0, "No game detected");
            }
            for (i, s) in self.sessions.iter().enumerate() {
                let buffered = s.video.lock().map(|r| (r.span() / TICKS_PER_SEC) as u64).unwrap_or(0);
                let mb = s.memory_bytes() as f64 / 1e6;
                let state = if s.is_running() { "Recording" } else { "Closed" };
                add(
                    MF_STRING | MF_GRAYED,
                    0,
                    &format!("{state}: {} ({} buffered, {mb:.0} MB)", s.game.name, fmt_dur(buffered)),
                );
                if s.is_running() {
                    add(MF_STRING, CMD_STOP_BASE + i, &format!("    Stop recording {}", s.game.name));
                }
            }
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            add(if self.paused { MF_STRING | MF_CHECKED } else { MF_STRING }, CMD_PAUSE, "Pause game detection");
            add(MF_STRING, CMD_FOLDER, "Open clips folder");
            add(MF_STRING | MF_DEFAULT, CMD_LIBRARY, "Clips, editor and settings...");
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            if let Some(v) = &self.update_ready {
                add(MF_STRING, CMD_UPDATE, &format!("Restart to update to {v}"));
            } else if let Some((v, _)) = &self.update_notice {
                add(MF_STRING, CMD_RELEASE_PAGE, &format!("Replay {v} is available..."));
            }
            add(MF_STRING, CMD_EXIT, "Exit");
            m
        }
    }

    fn on_command(&mut self, cmd: usize) {
        match cmd {
            CMD_SAVE => self.save_focused(),
            CMD_SAVE_ALL => self.save_all(),
            CMD_RECORD => {
                if self.recordings.is_empty() {
                    self.toggle_record();
                } else {
                    self.stop_recording(0);
                }
            }
            CMD_PAUSE => {
                self.paused = !self.paused;
                self.last_checked = None;
                if self.paused {
                    for s in &mut self.sessions {
                        s.stop();
                    }
                }
                self.ensure_mic();
                self.update_tray();
            }
            CMD_FOLDER => {
                let _ = std::fs::create_dir_all(&self.cfg.output_dir);
                let p = crate::util::wide(&self.cfg.output_dir.to_string_lossy());
                unsafe {
                    ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
                }
            }
            CMD_RELEASE_PAGE => {
                if let Some((_, page)) = &self.update_notice {
                    crate::sys::shell::open_url(page);
                }
            }
            c if c >= CMD_STOP_BASE => {
                if let Some(s) = self.sessions.get_mut(c - CMD_STOP_BASE) {
                    self.stopped_by_user.push(s.game.pid);
                    s.stop();
                }
                self.ensure_mic();
                self.update_tray();
            }
            _ => {}
        }
    }

    /// Stops every recording and waits until its file is complete.
    fn finish_all_recordings(&mut self) {
        for (_, mut r) in self.recordings.drain(..) {
            r.stop_and_wait();
        }
        for mut r in self.finishing.drain(..) {
            r.stop_and_wait();
        }
    }

    fn shutdown(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.hook);
            let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
        }
        for s in &mut self.sessions {
            s.stop();
        }
        self.finish_all_recordings();
        self.sessions.clear();
        self.stop_shared();
        tray::remove(self.hwnd);
        log::info!("exit");
    }
}
