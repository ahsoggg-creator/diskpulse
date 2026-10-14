//! Windows integration: per-user installation (no administrator rights needed), Start menu and desktop shortcuts,
//! the "Apps & features" entry, elevation checks and the "open as administrator" relaunch.
//!
//! Everything is written under HKCU / the user's profile, so installing never needs UAC. Administrator rights are
//! requested only by [`relaunch_elevated`] (one UAC prompt, a separate window); the main copy is never elevated.

use std::path::{Path, PathBuf};

pub const APP: &str = "DiskPulse";
pub const REPO_URL: &str = "https://github.com/ahsoggg-creator/diskpulse";

#[cfg(windows)]
pub use imp::*;

/// Starts this executable again as the same (unelevated) user with `args`.
pub fn relaunch_normal(args: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .args(args.split_whitespace())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[derive(Clone, Copy)]
pub struct InstallOptions {
    pub desktop_shortcut: bool,
}

/// `%LOCALAPPDATA%\Programs\DiskPulse`.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Programs").join(APP))
}

pub fn installed_exe() -> Option<PathBuf> {
    install_dir().map(|d| d.join(format!("{APP}.exe")))
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    norm(a) == norm(b)
}

/// The running executable is the installed copy.
pub fn running_installed() -> bool {
    match (std::env::current_exe(), installed_exe()) {
        (Ok(cur), Some(inst)) => same_file(&cur, &inst),
        _ => false,
    }
}

/// The executable was started as the installer (`DiskPulse-Setup.exe` or `--install`).
pub fn is_setup_launch() -> bool {
    std::env::args().any(|a| a == "--install")
        || std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
            .is_some_and(|s| s.contains("setup"))
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{APP, InstallOptions, REPO_URL, install_dir, installed_exe, same_file};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\DiskPulse";
    const APP_PATHS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths\DiskPulse.exe";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Lets a GUI-subsystem exe print to the console it was started from (`DiskPulse.exe --update` in a terminal).
    pub fn attach_parent_console() {
        // SAFETY: trivial call; fails harmlessly when there is no parent console.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    /// The process runs with an elevated (administrator) token.
    pub fn is_elevated() -> bool {
        // SAFETY: the token handle is opened for the current process, queried into a properly sized local and closed.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut ret = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                (&mut e as *mut TOKEN_ELEVATION).cast(),
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            );
            CloseHandle(token);
            ok != 0 && e.TokenIsElevated != 0
        }
    }

    /// Starts this executable again through the UAC prompt (a separate, elevated window). The caller stays unelevated.
    pub fn relaunch_elevated(args: &str) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let (verb, file, params) = (wide("runas"), wide(&exe.to_string_lossy()), wide(args));
        // SAFETY: all strings are NUL-terminated and outlive the call; no window handle or directory.
        let h = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if h as isize > 32 { Ok(()) } else { Err("UAC".into()) }
    }

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the key was opened by RegCreateKeyExW / RegOpenKeyExW and is closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn create_key(path: &str) -> Result<Key, String> {
        let p = wide(path);
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: `p` is NUL-terminated; the out-pointer references a live local; no class / security attributes.
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                p.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut h,
                std::ptr::null_mut(),
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(Key(h))
        } else {
            Err(format!("registry: cannot create {path} (code {rc})"))
        }
    }

    fn set_str(key: &Key, name: Option<&str>, value: &str) -> Result<(), String> {
        let n = name.map(wide);
        let v = wide(value);
        // SAFETY: `v` is a NUL-terminated UTF-16 buffer whose byte length is passed; name is NUL-terminated or null.
        let rc = unsafe {
            RegSetValueExW(
                key.0,
                n.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                0,
                REG_SZ,
                v.as_ptr().cast(),
                (v.len() * 2) as u32,
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("registry: writing a value failed (code {rc})"))
        }
    }

    fn set_dword(key: &Key, name: &str, value: u32) -> Result<(), String> {
        let n = wide(name);
        // SAFETY: the data pointer references a live u32 of the stated size.
        let rc = unsafe { RegSetValueExW(key.0, n.as_ptr(), 0, REG_DWORD, (&value as *const u32).cast(), 4) };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("registry: writing a value failed (code {rc})"))
        }
    }

    fn key_exists(path: &str) -> bool {
        let p = wide(path);
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: `p` is NUL-terminated; the out-pointer references a live local.
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, KEY_READ, &mut h) };
        if rc == ERROR_SUCCESS {
            drop(Key(h));
            true
        } else {
            false
        }
    }

    fn delete_tree(path: &str) {
        let p = wide(path);
        // SAFETY: `p` is NUL-terminated; deleting a missing key just returns an error code.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, p.as_ptr()) };
    }

    /// Start with Windows (current user, hidden in the tray): the `Run` value `DiskPulse`.
    pub fn set_autostart(enable: bool, exe: &Path) -> Result<(), String> {
        if enable {
            let k = create_key(RUN_KEY)?;
            set_str(&k, Some(APP), &format!("\"{}\" --tray", exe.display()))
        } else {
            let p = wide(RUN_KEY);
            let mut h: HKEY = std::ptr::null_mut();
            // SAFETY: `p` is NUL-terminated; the key handle is closed by `Key` below.
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, KEY_WRITE, &mut h) } == ERROR_SUCCESS {
                let k = Key(h);
                let n = wide(APP);
                // SAFETY: valid open key, NUL-terminated name; a missing value only returns an error code.
                unsafe { RegDeleteValueW(k.0, n.as_ptr()) };
            }
            Ok(())
        }
    }

    /// Runs a PowerShell snippet without a console window. The script travels base64-encoded, so paths with quotes or
    /// non-ASCII characters need no escaping beyond PowerShell's own single quotes.
    fn powershell(script: &str) -> Result<(), String> {
        let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let out = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &base64(&utf16),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("cannot start PowerShell: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("PowerShell: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    fn base64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
        for c in data.chunks(3) {
            let n =
                u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= c.len() {
                    s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    }

    fn ps_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    /// Start menu (searchable as "DiskPulse") and, optionally, desktop shortcuts.
    fn create_shortcuts(exe: &Path, desktop: bool) -> Result<(), String> {
        let exe = ps_quote(&exe.to_string_lossy());
        let mut script = format!(
            "$ws = New-Object -ComObject WScript.Shell\n\
             function Make($dir) {{ $l = $ws.CreateShortcut((Join-Path $dir 'DiskPulse.lnk')); \
             $l.TargetPath = {exe}; $l.IconLocation = {exe} + ',0'; \
             $l.Description = 'DiskPulse - S.M.A.R.T., history, read-only surface scan'; $l.Save() }}\n\
             Make ([Environment]::GetFolderPath('Programs'))\n"
        );
        if desktop {
            script.push_str("Make ([Environment]::GetFolderPath('Desktop'))\n");
        }
        powershell(&script)
    }

    fn remove_shortcuts() {
        let _ = powershell(
            "foreach ($f in 'Programs','Desktop') { \
             $p = Join-Path ([Environment]::GetFolderPath($f)) 'DiskPulse.lnk'; \
             if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Force } }",
        );
    }

    /// Copies the running executable into `%LOCALAPPDATA%\Programs\DiskPulse` and registers it. Returns the installed exe.
    pub fn install(opts: InstallOptions) -> Result<PathBuf, String> {
        let dir = install_dir().ok_or("LOCALAPPDATA is not set")?;
        let exe = installed_exe().ok_or("LOCALAPPDATA is not set")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let cur = std::env::current_exe().map_err(|e| e.to_string())?;
        if !same_file(&cur, &exe) {
            // A running older copy cannot be overwritten, but it can be renamed out of the way.
            if exe.exists() && std::fs::remove_file(&exe).is_err() {
                let old = exe.with_extension("exe.old");
                let _ = std::fs::remove_file(&old);
                std::fs::rename(&exe, &old).map_err(|e| format!("{}: {e}", exe.display()))?;
            }
            std::fs::copy(&cur, &exe).map_err(|e| format!("copy to {}: {e}", exe.display()))?;
        }

        create_shortcuts(&exe, opts.desktop_shortcut)?;

        let ap = create_key(APP_PATHS_KEY)?; // Win+R "diskpulse"
        set_str(&ap, None, &exe.to_string_lossy())?;

        let u = create_key(UNINSTALL_KEY)?;
        let exe_s = exe.to_string_lossy();
        set_str(&u, Some("DisplayName"), APP)?;
        set_str(&u, Some("DisplayVersion"), env!("CARGO_PKG_VERSION"))?;
        set_str(&u, Some("Publisher"), APP)?;
        set_str(&u, Some("DisplayIcon"), &format!("{exe_s},0"))?;
        set_str(&u, Some("InstallLocation"), &dir.to_string_lossy())?;
        set_str(&u, Some("UninstallString"), &format!("\"{exe_s}\" --uninstall"))?;
        set_str(&u, Some("URLInfoAbout"), REPO_URL)?;
        set_dword(&u, "NoModify", 1)?;
        set_dword(&u, "NoRepair", 1)?;
        let kb = std::fs::metadata(&exe).map_or(0, |m| m.len() / 1024) as u32;
        set_dword(&u, "EstimatedSize", kb)?;
        Ok(exe)
    }

    /// Updates the version shown in "Apps & features" after a self-update.
    pub fn refresh_installed_version() {
        if key_exists(UNINSTALL_KEY)
            && let Ok(k) = create_key(UNINSTALL_KEY)
        {
            let _ = set_str(&k, Some("DisplayVersion"), env!("CARGO_PKG_VERSION"));
        }
    }

    /// Removes shortcuts, registry entries, autostart and settings, then deletes the install folder once this
    /// process has exited.
    pub fn uninstall() -> Result<(), String> {
        remove_shortcuts();
        let _ = set_autostart(false, Path::new(""));
        delete_tree(APP_PATHS_KEY);
        delete_tree(UNINSTALL_KEY);
        if let Some(d) = crate::settings::dir() {
            let _ = std::fs::remove_dir_all(d);
        }
        if let Some(dir) = install_dir() {
            // The running exe cannot delete itself: a hidden PowerShell retries for ~20 s until this process has exited.
            let d = ps_quote(&dir.to_string_lossy());
            let script = format!(
                "for ($i = 0; $i -lt 20; $i++) {{ Remove-Item -LiteralPath {d} -Recurse -Force -ErrorAction SilentlyContinue;                  if (-not (Test-Path -LiteralPath {d})) {{ break }}; Start-Sleep -Seconds 1 }}"
            );
            let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let _ = std::process::Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-EncodedCommand",
                    &base64(&utf16),
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}
