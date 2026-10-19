//! Decides whether a window belongs to a game.
//!
//! Signals, strongest first:
//! 1. the user's include / exclude lists,
//! 2. the executable lives in a known game library folder,
//! 3. Windows Game Bar lists it as a game (`HKCU\System\GameConfigStore`),
//! 4. it covers a whole monitor and loads a 3D API (with a list of
//!    browsers, players and tools that are never games).
//!
//! Work happens only on foreground changes and a slow timer; results per
//! process are cached.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, MAX_PATH, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::{PCWSTR, w};

use crate::config::DetectionConfig;
use crate::session::GameInfo;

/// Never treated as games, even when fullscreen.
const NEVER: &[&str] = &[
    "explorer.exe", "chrome.exe", "msedge.exe", "msedgewebview2.exe", "firefox.exe", "opera.exe", "brave.exe",
    "vivaldi.exe", "arc.exe", "iexplore.exe", "vlc.exe", "mpc-hc64.exe", "mpc-hc.exe", "mpc-be64.exe",
    "potplayermini64.exe", "potplayer64.exe", "mpv.exe", "wmplayer.exe", "video.ui.exe", "microsoft.media.player.exe",
    "applicationframehost.exe", "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe", "battle.net.exe",
    "eadesktop.exe", "origin.exe", "galaxyclient.exe", "upc.exe", "ubisoftconnect.exe", "riotclientux.exe",
    "riotclientservices.exe", "riot client.exe", "rockstarservice.exe", "playnite.desktopapp.exe",
    "playnite.fullscreenapp.exe", "discord.exe", "slack.exe", "teams.exe", "ms-teams.exe", "zoom.exe", "obs64.exe",
    "obs32.exe", "nvidia app.exe", "nvidia overlay.exe", "nvcontainer.exe", "devenv.exe", "code.exe", "powerpnt.exe",
    "winword.exe", "excel.exe", "outlook.exe", "onenote.exe", "acrobat.exe", "acrord32.exe", "photoshop.exe",
    "blender.exe", "unity.exe", "unity hub.exe", "unrealeditor.exe", "ue4editor.exe", "taskmgr.exe", "searchhost.exe",
    "startmenuexperiencehost.exe", "shellexperiencehost.exe", "lockapp.exe", "textinputhost.exe",
    "screenclippinghost.exe", "snippingtool.exe", "mstsc.exe", "vmware.exe", "vmconnect.exe", "virtualboxvm.exe",
    "parsecd.exe", "moonlight.exe", "spotify.exe", "unitycrashhandler64.exe", "unitycrashhandler32.exe",
    "crashreportclient.exe", "easyanticheat_eos_setup.exe", "easyanticheat_setup.exe", "beservice.exe",
    "steamerrorreporter.exe", "steamerrorreporter64.exe", "unins000.exe", "uninstall.exe", "dxsetup.exe",
    "vc_redist.x64.exe", "vc_redist.x86.exe", "replay.exe", "powershell.exe", "pwsh.exe", "windowsterminal.exe",
    "cmd.exe", "conhost.exe", "notepad.exe", "mspaint.exe", "photos.exe", "systemsettings.exe", "msiexec.exe",
];

const SHELL_CLASSES: &[&str] = &[
    "Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow", "TopLevelWindowForOverflowXamlIsland", "ForegroundStaging",
    "MultitaskingViewFrame", "TaskManagerWindow", "NotifyIconOverflowWindow", "#32768",
];

/// Folders that only hold games. The next path component is the game's folder.
const LIBRARIES: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\gog galaxy\\games\\",
    "\\gog games\\",
    "\\xboxgames\\",
    "\\riot games\\",
    "\\ea games\\",
    "\\electronic arts\\",
    "\\ubisoft game launcher\\games\\",
    "\\rockstar games\\",
    "\\amazon games\\library\\",
    "\\itch\\apps\\",
    "\\battlestate games\\",
    "\\games\\",
];

/// Game launchers. A process started by one of these (directly or a few
/// levels down) is a game, even in a window.
const LAUNCHERS: &[&str] = &[
    "steam.exe", "epicgameslauncher.exe", "jagexlauncher.exe", "battle.net.exe", "riotclientservices.exe",
    "eadesktop.exe", "origin.exe", "galaxyclient.exe", "upc.exe", "ubisoftconnect.exe", "launcher.exe",
    "rockstarlauncher.exe", "playnite.desktopapp.exe", "playnite.fullscreenapp.exe", "amazon games.exe", "itch.exe",
    "heroic.exe", "prismlauncher.exe", "multimc.exe", "minecraftlauncher.exe", "minecraft.exe", "xboxpcapp.exe",
    "gamelaunchhelper.exe", "bethesda.net_launcher.exe", "wargaminggamecenter.exe", "arenanet.exe", "trovelauncher.exe",
];

/// Launcher helper processes that must not count as games themselves.
const LAUNCHER_HELPERS: &[&str] = &[
    "steamwebhelper.exe", "steamservice.exe", "gameoverlayui.exe", "steamerrorreporter.exe", "steamerrorreporter64.exe",
    "epicwebhelper.exe", "epiconlineservicesuserhelper.exe", "unrealcefsubprocess.exe", "jagexlauncher.exe",
    "agent.exe", "battle.net.exe", "riotclientux.exe", "riotclientuxrender.exe", "riotclientcrashhandler.exe",
    "eabackgroundservice.exe", "eadesktop.exe", "galaxyclient helper.exe", "galaxycommunication.exe",
    "upcwebhelper.exe", "uplaywebcore.exe", "qtwebengineprocess.exe", "cefsharp.browsersubprocess.exe",
    "crashpad_handler.exe", "werfault.exe", "conhost.exe",
];

/// Games that often run without a launcher, in a window.
const KNOWN_GAMES: &[&str] = &[
    "runelite.exe", "osclient.exe", "rs2client.exe", "javaw.exe#minecraft", "minecraft.windows.exe",
    "league of legends.exe", "valorant-win64-shipping.exe", "fortniteclient-win64-shipping.exe", "r5apex.exe",
    "r5apex_dx12.exe", "cs2.exe", "dota2.exe", "overwatch.exe", "wow.exe", "wowclassic.exe", "hearthstone.exe",
    "ffxiv_dx11.exe", "gw2-64.exe", "pathofexile.exe", "pathofexile_x64steam.exe", "pathofexilesteam.exe",
    "rocketleague.exe", "robloxplayerbeta.exe", "eldenring.exe", "gta5.exe", "gta5_enhanced.exe", "rdr2.exe",
    "cyberpunk2077.exe", "starfield.exe", "tslgame.exe", "destiny2.exe", "warframe.x64.exe", "osu!.exe",
    "terraria.exe", "stardew valley.exe", "factorio.exe", "rimworldwin64.exe", "eft.exe", "escapefromtarkov.exe",
    "deadlock.exe", "marvel-win64-shipping.exe", "bf2042.exe", "cod.exe", "modernwarfare.exe", "blackops6.exe",
    "rainbowsix.exe", "rainbowsix_be.exe", "fallguys_client_game.exe", "sotgame.exe", "palworld-win64-shipping.exe",
    "helldivers2.exe", "baldur's gate 3.exe", "bg3.exe", "bg3_dx11.exe", "witcher3.exe", "sc2_x64.exe",
    "diablo iv.exe", "lostark.exe", "newworld.exe", "starcitizen.exe", "vrchat.exe", "beatsaber.exe",
];

const API_MODULES: &[&str] =
    &["d3d9.dll", "d3d10.dll", "d3d10_1.dll", "d3d11.dll", "d3d12.dll", "vulkan-1.dll", "opengl32.dll", "d3d8.dll"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    UserList,
    KnownGame,
    Launcher,
    Library,
    GameBar,
    Fullscreen,
}

impl Reason {
    /// How the game was found, in words for the Games page.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::UserList => "Added by you",
            Reason::KnownGame => "Known game",
            Reason::Launcher => "Started by a game launcher",
            Reason::Library => "In a game library folder",
            Reason::GameBar => "On the Windows Game Bar list",
            Reason::Fullscreen => "Fullscreen 3D app",
        }
    }
}

#[derive(Clone)]
struct ProcInfo {
    path: String,
    exe: String,
    /// Strong signal (list, library, Game Bar), independent of window state.
    strong: Option<Reason>,
    uses_3d: Option<bool>,
}

pub struct Detector {
    cfg: DetectionConfig,
    game_bar: HashSet<String>,
    game_bar_loaded: Option<Instant>,
    cache: HashMap<(u32, u64), ProcInfo>,
    own_pid: u32,
}

impl Detector {
    pub fn new(cfg: DetectionConfig) -> Self {
        Self { cfg, game_bar: HashSet::new(), game_bar_loaded: None, cache: HashMap::new(), own_pid: unsafe { GetCurrentProcessId() } }
    }

    pub fn set_config(&mut self, cfg: DetectionConfig) {
        self.cfg = cfg;
        self.cache.clear();
    }

    fn refresh_game_bar(&mut self, force: bool) {
        let stale = self.game_bar_loaded.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
        if force || stale {
            self.game_bar = read_game_config_store();
            self.game_bar_loaded = Some(Instant::now());
        }
    }

    fn proc_info(&mut self, pid: u32) -> Option<ProcInfo> {
        let key = (pid, process_start(pid)?);
        if let Some(p) = self.cache.get(&key) {
            return Some(p.clone());
        }
        let path = process_path(pid)?;
        let lower = path.to_ascii_lowercase();
        let exe = Path::new(&lower).file_name()?.to_string_lossy().into_owned();
        let strong = if self.cfg.include.contains(&exe) {
            Some(Reason::UserList)
        } else if self.cfg.exclude.contains(&exe) || NEVER.contains(&exe.as_str()) {
            None
        } else if is_known_game(&exe, pid) {
            Some(Reason::KnownGame)
        } else if LIBRARIES.iter().any(|l| lower.contains(l)) {
            Some(Reason::Library)
        } else if !LAUNCHER_HELPERS.contains(&exe.as_str()) && launched_by_launcher(pid) {
            Some(Reason::Launcher)
        } else {
            self.refresh_game_bar(false);
            if !self.game_bar.contains(&lower) && self.game_bar_loaded.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
                // Game Bar may have just learned about this game.
                self.refresh_game_bar(true);
            }
            self.game_bar.contains(&lower).then_some(Reason::GameBar)
        };
        let info = ProcInfo { path, exe, strong, uses_3d: None };
        if self.cache.len() > 512 {
            self.cache.clear();
        }
        self.cache.insert(key, info.clone());
        Some(info)
    }

    fn is_excluded(&self, exe: &str) -> bool {
        !self.cfg.include.iter().any(|e| e == exe) && (self.cfg.exclude.iter().any(|e| e == exe) || NEVER.contains(&exe))
    }

    /// Returns game info when `hwnd` is a game window worth recording.
    pub fn classify(&mut self, hwnd: isize) -> Option<(GameInfo, Reason)> {
        let h = HWND(hwnd as _);
        if hwnd == 0 || !is_candidate_window(h) {
            return None;
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == 0 || pid == self.own_pid {
            return None;
        }
        let mut info = self.proc_info(pid)?;
        if self.is_excluded(&info.exe) {
            return None;
        }
        let reason = match info.strong {
            Some(Reason::UserList) => Reason::UserList,
            _ if !self.cfg.enabled => return None,
            Some(r) => {
                // Skip small helper windows (launchers, splash screens) of real games.
                let (w, hgt) = window_size(h);
                if w < 640 || hgt < 360 {
                    return None;
                }
                r
            }
            None => {
                if !self.cfg.fullscreen_apps || fullscreen_monitor(hwnd).is_none() {
                    return None;
                }
                let uses = match info.uses_3d {
                    Some(u) => u,
                    None => {
                        let u = uses_3d_api(pid);
                        info.uses_3d = Some(u);
                        if let Some(start) = process_start(pid) {
                            self.cache.insert((pid, start), info.clone());
                        }
                        u
                    }
                };
                if !uses {
                    return None;
                }
                Reason::Fullscreen
            }
        };
        let name = display_name(&info.path, h);
        Some((GameInfo { pid, hwnd, exe: info.exe, name }, reason))
    }
}

fn is_known_game(exe: &str, pid: u32) -> bool {
    KNOWN_GAMES.iter().any(|k| match k.split_once('#') {
        // "exe#word": the exe only counts when a window title contains the word.
        Some((e, word)) => e == exe && window_titles_contain(pid, word),
        None => *k == exe,
    })
}

fn window_titles_contain(pid: u32, word: &str) -> bool {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    use windows::core::BOOL;
    struct Ctx<'a> {
        pid: u32,
        word: &'a str,
        found: bool,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(l.0 as *mut Ctx) };
        let mut p = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        if p == ctx.pid {
            let mut t = [0u16; 256];
            let n = unsafe { GetWindowTextW(h, &mut t) } as usize;
            if String::from_utf16_lossy(&t[..n]).to_ascii_lowercase().contains(ctx.word) {
                ctx.found = true;
                return false.into();
            }
        }
        true.into()
    }
    let mut ctx = Ctx { pid, word, found: false };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM((&raw mut ctx) as isize));
    }
    ctx.found
}

/// True when a known launcher started this process (up to 3 levels up).
/// Parents must have started before their child, which guards against
/// reused process ids.
fn launched_by_launcher(pid: u32) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    let mut parents: HashMap<u32, (u32, String)> = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let name = crate::util::from_wide(&e.szExeFile).to_ascii_lowercase();
            parents.insert(e.th32ProcessID, (e.th32ParentProcessID, name));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let mut child = pid;
    let mut child_start = process_start(pid).unwrap_or(u64::MAX);
    for _ in 0..3 {
        let Some((parent, _)) = parents.get(&child) else { return false };
        let Some((_, parent_name)) = parents.get(parent) else { return false };
        let parent_start = process_start(*parent).unwrap_or(u64::MAX);
        if parent_start > child_start {
            return false;
        }
        if LAUNCHERS.contains(&parent_name.as_str()) {
            return true;
        }
        child = *parent;
        child_start = parent_start;
    }
    false
}

fn is_candidate_window(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return false;
        }
        if GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(h, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4).is_ok() && cloaked != 0 {
            return false;
        }
        let mut class = [0u16; 128];
        let n = GetClassNameW(h, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        !SHELL_CLASSES.contains(&class.as_str())
    }
}

fn window_size(h: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

/// The monitor a window fully covers (borderless or exclusive fullscreen).
pub fn fullscreen_monitor(hwnd: isize) -> Option<isize> {
    unsafe {
        let h = HWND(hwnd as _);
        let mut r = RECT::default();
        GetWindowRect(h, &mut r).ok()?;
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let m = mi.rcMonitor;
        (r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom).then_some(mon.0 as isize)
    }
}

/// The client area of a window inside its captured image, as (x, y, w, h).
/// Window capture covers the visible frame (title bar and borders); games
/// in a window only want the client area. None for fullscreen windows.
pub fn client_crop(hwnd: isize) -> Option<(u32, u32, u32, u32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    unsafe {
        let h = HWND(hwnd as _);
        let mut frame = RECT::default();
        DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut frame).cast(), size_of::<RECT>() as u32).ok()?;
        let mut client = RECT::default();
        GetClientRect(h, &mut client).ok()?;
        let mut origin = POINT::default();
        if !ClientToScreen(h, &mut origin).as_bool() {
            return None;
        }
        let x = (origin.x - frame.left).max(0) as u32;
        let y = (origin.y - frame.top).max(0) as u32;
        let w = (client.right - client.left).max(0) as u32;
        let hgt = (client.bottom - client.top).max(0) as u32;
        (w >= 2 && hgt >= 2).then_some((x, y, w, hgt))
    }
}

/// Pixel size of the monitor that shows the window.
pub fn monitor_size(hwnd: isize) -> (u32, u32) {
    unsafe {
        let mon = MonitorFromWindow(HWND(hwnd as _), MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let m = mi.rcMonitor;
            return ((m.right - m.left).max(2) as u32, (m.bottom - m.top).max(2) as u32);
        }
        (1920, 1080)
    }
}

pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize * 2];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn process_exe(pid: u32) -> Option<String> {
    let p = process_path(pid)?;
    Some(Path::new(&p).file_name()?.to_string_lossy().to_ascii_lowercase())
}

fn process_start(pid: u32) -> Option<u64> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let r = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        let _ = CloseHandle(h);
        r.ok()?;
        Some((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
    }
}

fn uses_3d_api(pid: u32) -> bool {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) else {
            // Protected processes (often games with anti-cheat) refuse this.
            return true;
        };
        let mut e = MODULEENTRY32W { dwSize: size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Module32FirstW(snap, &mut e).is_ok();
        while ok {
            let name = crate::util::from_wide(&e.szModule).to_ascii_lowercase();
            if API_MODULES.contains(&name.as_str()) {
                found = true;
                // d3d11 is also used by many desktop apps; keep looking for a stronger hint.
                if name != "d3d11.dll" && name != "opengl32.dll" {
                    break;
                }
            }
            ok = Module32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

fn read_game_config_store() -> HashSet<String> {
    let mut out = HashSet::new();
    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("System\\GameConfigStore\\Children"), None, KEY_READ, &mut root).is_err() {
            return out;
        }
        let mut i = 0;
        loop {
            let mut name = [0u16; 128];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(root, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            i += 1;
            let mut child = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(name.as_ptr()), None, KEY_READ, &mut child).is_err() {
                continue;
            }
            let mut buf = [0u16; 1024];
            let mut size = (buf.len() * 2) as u32;
            let mut kind = REG_VALUE_TYPE::default();
            if RegQueryValueExW(child, w!("MatchedExeFullPath"), None, Some(&mut kind), Some(buf.as_mut_ptr().cast()), Some(&mut size)).is_ok() {
                let s = crate::util::from_wide(&buf[..(size as usize / 2).min(buf.len())]);
                if !s.is_empty() {
                    out.insert(s.to_ascii_lowercase());
                }
            }
            let _ = RegCloseKey(child);
        }
        let _ = RegCloseKey(root);
    }
    log::debug!("Game Bar knows {} games", out.len());
    out
}

fn version_string(path: &str, field: &str) -> Option<String> {
    unsafe {
        let wpath = crate::util::wide(path);
        let size = GetFileVersionInfoSizeW(PCWSTR(wpath.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wpath.as_ptr()), None, size, data.as_mut_ptr().cast()).ok()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let q = crate::util::wide("\\VarFileInfo\\Translation");
        let mut langs = vec![(0x0409u16, 0x04b0u16)];
        if VerQueryValueW(data.as_ptr().cast(), PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len >= 4 {
            let p = ptr as *const u16;
            langs.insert(0, (*p, *p.add(1)));
        }
        for (lang, cp) in langs {
            let q = crate::util::wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{field}"));
            if VerQueryValueW(data.as_ptr().cast(), PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len > 0 {
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr as *const u16, len as usize));
                let s = s.trim_end_matches('\0').trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }
}

const GENERIC_NAMES: &[&str] = &["game", "unreal engine", "unity", "bootstrappackagedgame", "launcher", "application", "client"];

fn is_generic(s: &str, exe_stem: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.len() < 2
        || l == exe_stem.to_ascii_lowercase()
        || GENERIC_NAMES.contains(&l.as_str())
        || l.contains("shipping")
        || l.contains("win64")
}

fn strip_launcher(s: &str) -> String {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    for suffix in [" launcher", " client", " game"] {
        if lower.ends_with(suffix) && t.len() > suffix.len() + 1 {
            return t[..t.len() - suffix.len()].trim().to_string();
        }
    }
    t.to_string()
}

/// A human name for the game, used for folders and file names.
pub fn display_name(path: &str, hwnd: HWND) -> String {
    let lower = path.to_ascii_lowercase();
    let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // 1. The library folder name, e.g. steamapps\common\<Cyberpunk 2077>\...
    for lib in LIBRARIES {
        if let Some(i) = lower.find(lib) {
            let rest = &path[i + lib.len()..];
            if let Some(folder) = rest.split('\\').next()
                && rest.contains('\\')
                && !is_generic(folder, &stem)
            {
                return folder.to_string();
            }
        }
    }
    // 2. Version resource.
    for field in ["FileDescription", "ProductName"] {
        if let Some(s) = version_string(path, field) {
            let s = strip_launcher(&s);
            if !is_generic(&s, &stem) {
                return s;
            }
        }
    }
    // 3. Window title, if short.
    let mut title = [0u16; 128];
    let n = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
    let title = String::from_utf16_lossy(&title[..n]);
    if !title.trim().is_empty() && title.chars().count() <= 40 {
        return title.trim().to_string();
    }
    stem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_names() {
        assert!(is_generic("Game-Win64-Shipping", "x"));
        assert!(is_generic("eldenring", "eldenring"));
        assert!(!is_generic("ELDEN RING", "eldenring"));
        assert_eq!(strip_launcher("RuneLite Launcher"), "RuneLite");
        assert_eq!(strip_launcher("Launcher"), "Launcher");
    }
}
