//! Tray mode (`DiskPulse.exe --tray`): a notification-area icon whose color shows the worst disk status, Windows
//! notifications for warnings, a once-a-day update check and, unless the background service does it, the scheduled
//! snapshots. There is no window; "Open" starts the normal window as a separate process.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use diskpulse_core::alerts::{Throttle, message};
use diskpulse_core::assess::Level;
use diskpulse_core::collector::{self, AlertLog, Config};
use diskpulse_core::history::HistoryStore;
use diskpulse_core::i18n::{Lang, fmt, tr};
use diskpulse_core::service;
use diskpulse_core::units::now_unix;
use serde::{Deserialize, Serialize};

use crate::settings::Settings;
use crate::{diag, update};

/// How often the worker wakes up to look for due work.
const TICK: Duration = Duration::from_secs(30);

// ---- pure parts ---------------------------------------------------------------------------------------------------

fn level_rgb(l: Option<Level>) -> [u8; 3] {
    match l {
        Some(Level::Good) => [63, 185, 80],
        Some(Level::Warning) => [214, 158, 36],
        Some(Level::Bad) => [226, 74, 74],
        None => [120, 126, 138],
    }
}

/// The tray icon: the base picture (64x64 RGBA) downscaled to `size` with a status dot in the lower right corner.
/// Returned as premultiplied-free BGRA, the layout `CreateBitmap` wants.
pub fn render_icon_bgra(base_rgba64: &[u8], level: Option<Level>, size: usize) -> Vec<u8> {
    assert_eq!(base_rgba64.len(), 64 * 64 * 4);
    let mut out = vec![0u8; size * size * 4];
    let scale = 64 / size.max(1);
    for y in 0..size {
        for x in 0..size {
            // box filter over the source pixels that map to this one
            let (mut acc, mut n) = ([0u32; 4], 0u32);
            for sy in y * scale..(y + 1) * scale {
                for sx in x * scale..(x + 1) * scale {
                    let o = (sy * 64 + sx) * 4;
                    let a = u32::from(base_rgba64[o + 3]);
                    acc[0] += u32::from(base_rgba64[o]) * a;
                    acc[1] += u32::from(base_rgba64[o + 1]) * a;
                    acc[2] += u32::from(base_rgba64[o + 2]) * a;
                    acc[3] += a;
                    n += 1;
                }
            }
            let o = (y * size + x) * 4;
            if acc[3] > 0 {
                out[o] = (acc[2] / acc[3]) as u8;
                out[o + 1] = (acc[1] / acc[3]) as u8;
                out[o + 2] = (acc[0] / acc[3]) as u8;
                out[o + 3] = (acc[3] / n) as u8;
            }
        }
    }
    // status dot: filled circle with a dark ring so it reads on light and dark taskbars
    let (cx, cy, r) = (size as f32 * 0.70, size as f32 * 0.70, size as f32 * 0.30);
    let [rr, gg, bb] = level_rgb(level);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let o = (y * size + x) * 4;
            if d <= r {
                out[o..o + 4].copy_from_slice(&[bb, gg, rr, 255]);
            } else if d <= r + 1.2 {
                out[o..o + 4].copy_from_slice(&[24, 24, 28, 255]);
            }
        }
    }
    out
}

pub fn tip_text(lang: Lang, level: Option<Level>) -> String {
    let key = match level {
        Some(Level::Good) => "level.good",
        Some(Level::Warning) => "level.warning",
        Some(Level::Bad) => "level.bad",
        None => "level.unknown",
    };
    fmt(lang, "tray.tip", &[tr(lang, key).to_owned()])
}

/// Window of time between two scheduled snapshots, never shorter than a minute.
pub fn interval(minutes: u32) -> Duration {
    Duration::from_secs(u64::from(minutes.max(1)) * 60)
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct TrayState {
    /// Alerts with `t` up to this value were already shown.
    last_alert_t: u64,
    /// The latest version a notification was already shown for.
    notified_update: String,
}

fn state_path() -> Option<PathBuf> {
    crate::settings::dir().map(|d| d.join("tray_state.json"))
}

fn load_state() -> TrayState {
    state_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_state(s: &TrayState) {
    if let Some(p) = state_path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec(s) {
            let _ = std::fs::write(p, b);
        }
    }
}

/// Commands from the worker thread to the window thread.
enum Ui {
    Level(Option<Level>),
    Notify(String, String),
}

static QUEUE: Mutex<Vec<Ui>> = Mutex::new(Vec::new());
static COLLECT_NOW: AtomicBool = AtomicBool::new(false);
static HWND_RAW: AtomicIsize = AtomicIsize::new(0);

fn post(cmd: Ui) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(cmd);
    #[cfg(windows)]
    win::wake();
}

/// One pass of background work. Returns the worst level seen.
fn work(settings: &Settings, throttle: &mut Throttle, state: &mut TrayState, collect: bool) {
    let store = HistoryStore::new(service::history_dir());
    let lang = settings.lang;
    if collect {
        let cfg = Config {
            thresholds: settings.thresholds(),
            alerts: settings.alert_config(),
        };
        let results = collector::collect_all(&store, &cfg, now_unix());
        for o in &results {
            diag::register_serial(&o.info.serial);
        }
        diag::info(format!(
            "tray: collected {} disk(s), {} sample(s)",
            results.len(),
            results.iter().filter(|o| o.recorded.sample.is_some()).count()
        ));
    }
    // alerts raised by this process or by the service since the last pass
    let log = AlertLog::new(store.dir());
    let mut lines = Vec::new();
    let mut newest = state.last_alert_t;
    for r in log.since(state.last_alert_t) {
        newest = newest.max(r.t);
        if settings.notifications && throttle.allow(&r.disk_key, r.kind, now_unix()) {
            lines.push(message(lang, &r.disk, &r.alert()));
        }
    }
    if newest != state.last_alert_t {
        state.last_alert_t = newest;
        save_state(state);
        if let Some(d) = crate::settings::dir() {
            throttle.save(&d.join("notified.json"));
        }
    }
    if !lines.is_empty() {
        let more = lines.len().saturating_sub(4);
        lines.truncate(4);
        if more > 0 {
            lines.push(format!("+{more}"));
        }
        post(Ui::Notify(tr(lang, "alert.title").to_owned(), lines.join("\n")));
    }
    post(Ui::Level(collector::worst_level(&store)));
}

fn check_update(settings: &mut Settings, state: &mut TrayState) {
    let now = now_unix();
    if !settings.auto_update || now.saturating_sub(settings.last_update_check) < update::CHECK_INTERVAL_SECS {
        return;
    }
    settings.last_update_check = now;
    settings.save();
    let shared: update::Shared = std::sync::Arc::new(Mutex::new(update::State::Idle));
    if let update::State::Available { version, .. } = update::run_blocking(&shared, false, &|| {})
        && state.notified_update != version
    {
        state.notified_update = version.clone();
        save_state(state);
        post(Ui::Notify(
            tr(settings.lang, "update.notify_title").to_owned(),
            fmt(settings.lang, "update.notify", &[version]),
        ));
    }
}

fn worker() {
    let mut settings = Settings::load();
    let mut state = load_state();
    let mut throttle = crate::settings::dir()
        .map(|d| Throttle::load(&d.join("notified.json")))
        .unwrap_or_default();
    let mut last_collect = std::time::Instant::now() - interval(settings.collect_interval_min) - Duration::from_secs(1);
    let (_tx, rx) = mpsc::channel::<()>();
    loop {
        settings = Settings::load();
        let due = last_collect.elapsed() >= interval(settings.collect_interval_min)
            || COLLECT_NOW.swap(false, Ordering::Relaxed);
        // when the service runs it collects (with administrator rights); the tray only reads what it wrote
        let collect = due && !service::is_running();
        if due {
            last_collect = std::time::Instant::now();
        }
        work(&settings, &mut throttle, &mut state, collect);
        check_update(&mut settings, &mut state);
        if rx.recv_timeout(TICK).is_ok() {
            break;
        }
    }
}

#[cfg(windows)]
pub use win::run;

#[cfg(windows)]
mod win {
    use std::sync::atomic::Ordering;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
        Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, HICON, ICONINFO, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
        PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        TrackPopupMenu, TranslateMessage, WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
        WNDCLASSW,
    };

    use super::*;

    const WM_TRAY: u32 = WM_APP + 1;
    const WM_WAKE: u32 = WM_APP + 2;
    const ID_OPEN: usize = 1;
    const ID_COLLECT: usize = 2;
    const ID_EXIT: usize = 3;
    const ICON_PX: usize = 32;

    struct Gui {
        lang: Lang,
        level: Option<Level>,
        /// An `HICON` (a raw handle is not `Send`).
        icon: isize,
        taskbar_created: u32,
    }

    static GUI: Mutex<Option<Gui>> = Mutex::new(None);

    fn wide<const N: usize>(s: &str) -> [u16; N] {
        let mut a = [0u16; N];
        for (i, u) in s.encode_utf16().take(N - 1).enumerate() {
            a[i] = u;
        }
        a
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn make_icon(level: Option<Level>) -> HICON {
        let bgra = render_icon_bgra(include_bytes!("../assets/tray64.rgba"), level, ICON_PX);
        let mask = vec![0u8; ICON_PX * ICON_PX / 8];
        // SAFETY: `bgra` / `mask` are valid for the stated bitmap sizes; the bitmaps are deleted after the icon copies them.
        unsafe {
            let color = CreateBitmap(ICON_PX as i32, ICON_PX as i32, 1, 32, bgra.as_ptr().cast());
            let monochrome = CreateBitmap(ICON_PX as i32, ICON_PX as i32, 1, 1, mask.as_ptr().cast());
            let info = ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: monochrome,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&info);
            DeleteObject(color);
            DeleteObject(monochrome);
            icon
        }
    }

    fn data(hwnd: HWND, flags: u32) -> NOTIFYICONDATAW {
        // SAFETY: NOTIFYICONDATAW is plain data; all-zero is valid.
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = flags;
        nid
    }

    fn add_or_update(hwnd: HWND, add: bool) {
        let mut guard = GUI.lock().unwrap_or_else(|e| e.into_inner());
        let Some(g) = guard.as_mut() else { return };
        let old = g.icon as HICON;
        let new = make_icon(g.level);
        g.icon = new as isize;
        let mut nid = data(hwnd, NIF_MESSAGE | NIF_ICON | NIF_TIP);
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = new;
        nid.szTip = wide(&tip_text(g.lang, g.level));
        // SAFETY: `nid` is fully initialized for the flags set.
        unsafe {
            Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &nid);
            if !old.is_null() {
                DestroyIcon(old);
            }
        }
    }

    fn balloon(hwnd: HWND, title: &str, text: &str) {
        let mut nid = data(hwnd, NIF_INFO);
        nid.szInfoTitle = wide(title);
        nid.szInfo = wide(text);
        nid.dwInfoFlags = NIIF_WARNING;
        // SAFETY: `nid` is fully initialized for NIF_INFO.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    pub fn wake() {
        let h = HWND_RAW.load(Ordering::Relaxed);
        if h != 0 {
            // SAFETY: posting to a window handle owned by the UI thread; a stale handle only makes the call fail.
            unsafe { PostMessageW(h as HWND, WM_WAKE, 0, 0) };
        }
    }

    fn open_app() {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }

    fn show_menu(hwnd: HWND) {
        let lang = GUI
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(Lang::Ru, |g| g.lang);
        // SAFETY: a popup menu created, shown and destroyed on this thread; strings are NUL-terminated and outlive the calls.
        unsafe {
            let menu = CreatePopupMenu();
            AppendMenuW(menu, MF_STRING, ID_OPEN, wide_z(tr(lang, "tray.open")).as_ptr());
            AppendMenuW(menu, MF_STRING, ID_COLLECT, wide_z(tr(lang, "tray.collect")).as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(menu, MF_STRING, ID_EXIT, wide_z(tr(lang, "tray.exit")).as_ptr());
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            match cmd as usize {
                ID_OPEN => open_app(),
                ID_COLLECT => {
                    COLLECT_NOW.store(true, Ordering::Relaxed);
                }
                ID_EXIT => {
                    PostMessageW(hwnd, WM_COMMAND, ID_EXIT, 0);
                }
                _ => {}
            }
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let taskbar_created = GUI
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |g| g.taskbar_created);
        match msg {
            WM_TRAY => {
                match lp as u32 {
                    WM_LBUTTONUP => open_app(),
                    WM_RBUTTONUP => show_menu(hwnd),
                    _ => {}
                }
                0
            }
            WM_WAKE => {
                let cmds: Vec<Ui> = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
                for c in cmds {
                    match c {
                        Ui::Level(l) => {
                            let changed = {
                                let mut g = GUI.lock().unwrap_or_else(|e| e.into_inner());
                                g.as_mut().is_some_and(|g| std::mem::replace(&mut g.level, l) != l)
                            };
                            if changed {
                                add_or_update(hwnd, false);
                            }
                        }
                        Ui::Notify(title, text) => balloon(hwnd, &title, &text),
                    }
                }
                0
            }
            WM_COMMAND if wp == ID_EXIT => {
                // SAFETY: valid window handle owned by this thread.
                unsafe { Shell_NotifyIconW(NIM_DELETE, &data(hwnd, 0)) };
                unsafe { PostQuitMessage(0) };
                0
            }
            WM_DESTROY => {
                // SAFETY: trivial.
                unsafe { PostQuitMessage(0) };
                0
            }
            m if m == taskbar_created && m != 0 => {
                add_or_update(hwnd, true); // Explorer restarted: put the icon back
                0
            }
            // SAFETY: default handling of a message we do not process.
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }

    /// Runs the tray until "Exit" is chosen.
    pub fn run() -> Result<(), String> {
        let settings = Settings::load();
        let class = wide_z("DiskPulseTray");
        // SAFETY: standard window-class registration and hidden-window creation; the strings outlive the calls.
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err("cannot create the tray window".into());
            }
            HWND_RAW.store(hwnd as isize, Ordering::Relaxed);
            *GUI.lock().unwrap_or_else(|e| e.into_inner()) = Some(Gui {
                lang: settings.lang,
                level: None,
                icon: 0,
                taskbar_created: RegisterWindowMessageW(wide_z("TaskbarCreated").as_ptr()),
            });
            add_or_update(hwnd, true);
            std::thread::Builder::new()
                .name("tray-worker".into())
                .spawn(super::worker)
                .map_err(|e| e.to_string())?;
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<u8> {
        include_bytes!("../assets/tray64.rgba").to_vec()
    }

    #[test]
    fn icon_has_the_status_color_in_the_corner() {
        for (level, rgb) in [
            (Some(Level::Good), [63, 185, 80]),
            (Some(Level::Bad), [226, 74, 74]),
            (None, [120, 126, 138]),
        ] {
            let icon = render_icon_bgra(&base(), level, 32);
            assert_eq!(icon.len(), 32 * 32 * 4);
            let o = (22 * 32 + 22) * 4; // inside the dot
            assert_eq!(&icon[o..o + 4], &[rgb[2], rgb[1], rgb[0], 255]);
        }
    }

    #[test]
    fn icon_keeps_transparent_corners() {
        let icon = render_icon_bgra(&base(), Some(Level::Good), 32);
        assert_eq!(icon[3], 0, "top-left pixel must stay transparent");
    }

    #[test]
    fn interval_is_at_least_a_minute() {
        assert_eq!(interval(0), Duration::from_secs(60));
        assert_eq!(interval(30), Duration::from_secs(1800));
    }

    #[test]
    fn tip_mentions_the_level_in_both_languages() {
        assert!(tip_text(Lang::En, Some(Level::Bad)).contains("Bad"));
        assert!(tip_text(Lang::Ru, None).contains("Нет данных"));
    }
}
