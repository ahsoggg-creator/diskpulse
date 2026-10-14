//! Self-update from GitHub Releases: the only network traffic DiskPulse produces.
//!
//! The latest release must carry `DiskPulse.exe` and `SHA256SUMS.txt`. The new exe is downloaded next to the running
//! one, its SHA-256 is checked against `SHA256SUMS.txt`, then the files are swapped: Windows lets a running executable
//! be renamed (not overwritten), so the old one becomes `DiskPulse.exe.old` and is deleted on the next start.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

const API: &str = "https://api.github.com/repos/ahsoggg-creator/diskpulse/releases/latest";
const EXE_ASSET: &str = "DiskPulse.exe";
const SUM_ASSET: &str = "SHA256SUMS.txt";
/// Ask GitHub at most once a day.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 3600;

#[derive(Clone, PartialEq, Debug)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    /// A newer version exists; nothing downloaded yet.
    Available {
        version: String,
        page: String,
    },
    Downloading {
        version: String,
        done: u64,
        total: u64,
    },
    /// Installed; takes effect after a restart.
    Ready {
        version: String,
    },
    Failed(String),
}

pub type Shared = Arc<Mutex<State>>;

struct Release {
    version: String,
    page: String,
    exe_url: String,
    exe_size: u64,
    sum_url: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .build()
        .into()
}

fn ua() -> String {
    format!("DiskPulse/{}", env!("CARGO_PKG_VERSION"))
}

/// `"v1.2.3"` / `"1.2.3"` -> (1, 2, 3).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    ))
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    matches!((parse_version(remote), parse_version(local)), (Some(r), Some(l)) if r > l)
}

/// The hash for `file` in a `SHA256SUMS.txt` (`<hex>  <name>` per line; a leading `*` marks binary mode).
pub fn expected_hash(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let (hash, name) = (it.next()?, it.next()?.trim_start_matches('*'));
        (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn latest() -> Result<Release, String> {
    let mut resp = agent()
        .get(API)
        .header("User-Agent", &ua())
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => "no releases on GitHub yet".to_owned(),
            e => format!("GitHub is unreachable: {e}"),
        })?;
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("the GitHub reply has no tag_name")?;
    let asset = |name: &str| {
        v["assets"].as_array().and_then(|a| {
            a.iter().find(|x| x["name"].as_str() == Some(name)).map(|x| {
                (
                    x["browser_download_url"].as_str().unwrap_or("").to_owned(),
                    x["size"].as_u64().unwrap_or(0),
                )
            })
        })
    };
    let (exe_url, exe_size) = asset(EXE_ASSET).ok_or("the release has no DiskPulse.exe")?;
    let (sum_url, _) = asset(SUM_ASSET).ok_or("the release has no SHA256SUMS.txt")?;
    Ok(Release {
        version: tag.trim_start_matches('v').to_owned(),
        page: v["html_url"].as_str().unwrap_or(crate::winint::REPO_URL).to_owned(),
        exe_url,
        exe_size,
        sum_url,
    })
}

fn target_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

/// Deletes the previous executable left over by an update.
pub fn cleanup_old() {
    if let Ok(exe) = target_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
        let _ = std::fs::remove_file(exe.with_extension("exe.download"));
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".diskpulse-write-test");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn download(rel: &Release, state: &Shared, repaint: &dyn Fn()) -> Result<(), String> {
    let exe = target_exe()?;
    let tmp = exe.with_extension("exe.download");

    let sums = agent()
        .get(&rel.sum_url)
        .header("User-Agent", &ua())
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let expected = expected_hash(&sums, EXE_ASSET).ok_or("SHA256SUMS.txt has no valid entry for DiskPulse.exe")?;

    let mut resp = agent()
        .get(&rel.exe_url)
        .header("User-Agent", &ua())
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        *state.lock().unwrap_or_else(|e| e.into_inner()) = State::Downloading {
            version: rel.version.clone(),
            done,
            total: rel.exe_size,
        };
        repaint();
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);

    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != expected {
        let _ = std::fs::remove_file(&tmp);
        return Err("checksum mismatch: update cancelled".into());
    }

    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("cannot replace the exe: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp, &exe) {
        let _ = std::fs::rename(&old, &exe); // roll back
        return Err(format!("cannot replace the exe: {e}"));
    }
    Ok(())
}

fn set(state: &Shared, s: State) {
    *state.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

/// Checks (and with `install`, installs) synchronously. Returns the final state.
pub fn run_blocking(state: &Shared, install: bool, repaint: &dyn Fn()) -> State {
    set(state, State::Checking);
    let result = (|| -> Result<State, String> {
        let rel = latest()?;
        if !is_newer(&rel.version, env!("CARGO_PKG_VERSION")) {
            return Ok(State::UpToDate);
        }
        let writable = target_exe()
            .ok()
            .and_then(|e| e.parent().map(dir_writable))
            .unwrap_or(false);
        if !install || !writable {
            return Ok(State::Available {
                version: rel.version,
                page: rel.page,
            });
        }
        download(&rel, state, repaint)?;
        crate::winint::refresh_installed_version();
        Ok(State::Ready { version: rel.version })
    })();
    let result = result.unwrap_or_else(State::Failed);
    set(state, result.clone());
    repaint();
    result
}

/// Background check; with `install` the new version is also downloaded and swapped in.
pub fn spawn(state: Shared, install: bool, repaint: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let r = run_blocking(&state, install, &repaint);
        crate::diag::info(format!("update check: {r:?}"));
    });
}

/// Starts the (new) executable with the same non-flag arguments; the caller then closes its window.
pub fn restart() -> Result<(), String> {
    let exe = target_exe()?;
    std::process::Command::new(exe)
        .args(std::env::args().skip(1).filter(|a| !a.starts_with("--")))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.10"), Some((0, 10, 0)));
        assert_eq!(parse_version("2.0.0-beta.1"), Some((2, 0, 0)));
        assert_eq!(parse_version("x"), None);
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn finds_the_hash_of_the_right_file() {
        let h1 = "a".repeat(64);
        let h2 = "B".repeat(64);
        let sums = format!("{h1}  DiskPulse-Setup.exe\n{h2} *DiskPulse.exe\n{h1}  diskpulse-cli.exe\n");
        assert_eq!(expected_hash(&sums, "DiskPulse.exe"), Some("b".repeat(64)));
        assert_eq!(expected_hash(&sums, "missing.exe"), None);
        assert_eq!(expected_hash("short  DiskPulse.exe", "DiskPulse.exe"), None);
        assert_eq!(
            expected_hash(&format!("{} DiskPulse.exe", "g".repeat(64)), "DiskPulse.exe"),
            None
        );
    }
}
