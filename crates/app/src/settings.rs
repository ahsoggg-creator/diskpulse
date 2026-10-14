//! Persistent user settings: `%APPDATA%\DiskPulse\settings.json`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use diskpulse_core::i18n::Lang;
use serde::{Deserialize, Serialize};

/// Demo mode (`--demo`): invented data; nothing real is read and nothing is written to disk.
static DEMO: AtomicBool = AtomicBool::new(false);

pub fn set_demo() {
    DEMO.store(true, Ordering::Relaxed);
}

pub fn is_demo() -> bool {
    DEMO.load(Ordering::Relaxed)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub lang: Lang,
    /// Check GitHub once a day and offer / install new releases.
    pub auto_update: bool,
    /// Unix seconds of the last update check.
    pub last_update_check: u64,
    /// Temperature (C) from which a disk is reported as "warning" (and "bad" 10 degrees above).
    pub overheat_c: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            lang: Lang::Ru,
            auto_update: true,
            last_update_check: 0,
            overheat_c: 70,
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("DiskPulse"))
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        if is_demo() {
            return Settings::default();
        }
        file()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Best effort: failing to save a preference is not worth an error dialog.
    pub fn save(&self) {
        let Some(f) = file().filter(|_| !is_demo()) else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = f.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &f);
            }
        }
    }

    pub fn thresholds(&self) -> diskpulse_core::assess::Thresholds {
        diskpulse_core::assess::Thresholds {
            overheat_c: self.overheat_c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let s: Settings = serde_json::from_str(r#"{"lang":"en"}"#).unwrap();
        assert_eq!((s.lang, s.auto_update, s.overheat_c), (Lang::En, true, 70));
    }
}
