//! When to warn the user. Pure logic: compares a new snapshot with the previous one and applies an anti-spam throttle
//! (the same warning for the same disk is shown at most once per day).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::history::{DAY, Delta, Sample, counter_delta};
use crate::i18n::{Lang, fmt, tr};
use crate::nvme::critical_warning_flags;
use crate::units::human_bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Overheat,
    CriticalWarning,
    MediaErrors,
    Reallocated,
    Pending,
    SpareDropped,
    WriteSpike,
}

impl AlertKind {
    pub fn id(self) -> &'static str {
        match self {
            AlertKind::Overheat => "overheat",
            AlertKind::CriticalWarning => "critical_warning",
            AlertKind::MediaErrors => "media_errors",
            AlertKind::Reallocated => "reallocated",
            AlertKind::Pending => "pending",
            AlertKind::SpareDropped => "spare",
            AlertKind::WriteSpike => "write_spike",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertConfig {
    pub overheat_c: i32,
    /// Bytes written between two consecutive snapshots that count as a "sharp jump".
    pub write_spike_bytes: u64,
}

impl Default for AlertConfig {
    fn default() -> Self {
        AlertConfig {
            overheat_c: 70,
            write_spike_bytes: 200_000_000_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub kind: AlertKind,
    /// Values for the message text (after the disk name).
    pub args: Vec<String>,
}

/// Which warnings the new snapshot `cur` raises, given the previous snapshot of the same disk.
/// Without a previous snapshot only the state-based warnings (overheat, critical warning) can fire:
/// growth cannot be judged from a single reading.
pub fn evaluate(prev: Option<&Sample>, cur: &Sample, cfg: &AlertConfig) -> Vec<Alert> {
    let mut v = Vec::new();
    if let Some(t) = cur.temp_c.filter(|t| *t >= cfg.overheat_c) {
        v.push(Alert {
            kind: AlertKind::Overheat,
            args: vec![t.to_string(), cfg.overheat_c.to_string()],
        });
    }
    if let Some(cw) = cur.crit.filter(|c| *c != 0) {
        v.push(Alert {
            kind: AlertKind::CriticalWarning,
            args: vec![critical_warning_flags(cw).join(",")],
        });
    }
    let Some(p) = prev else { return v };
    let grew = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) if b > a => Some((a, b)),
        _ => None,
    };
    if let Some((a, b)) = grew(p.media_errors, cur.media_errors) {
        v.push(Alert {
            kind: AlertKind::MediaErrors,
            args: vec![a.to_string(), b.to_string()],
        });
    }
    if let Some((a, b)) = grew(p.reallocated, cur.reallocated) {
        v.push(Alert {
            kind: AlertKind::Reallocated,
            args: vec![a.to_string(), b.to_string()],
        });
    }
    let pend = |s: &Sample| {
        s.pending
            .zip(s.uncorrectable)
            .map(|(a, b)| a.saturating_add(b))
            .or(s.pending)
            .or(s.uncorrectable)
    };
    if let Some((a, b)) = grew(pend(p), pend(cur)) {
        v.push(Alert {
            kind: AlertKind::Pending,
            args: vec![a.to_string(), b.to_string()],
        });
    }
    if let (Some(a), Some(b)) = (p.spare, cur.spare)
        && b < a
    {
        v.push(Alert {
            kind: AlertKind::SpareDropped,
            args: vec![a.to_string(), b.to_string()],
        });
    }
    if let (Some(a), Some(b)) = (p.written, cur.written)
        && let Delta::Added(d) = counter_delta(a, b, None)
        && d >= cfg.write_spike_bytes
    {
        v.push(Alert {
            kind: AlertKind::WriteSpike,
            args: vec![d.to_string()],
        });
    }
    v
}

/// The notification text: `disk` is a label such as the model name.
pub fn message(lang: Lang, disk: &str, a: &Alert) -> String {
    let mut args = vec![disk.to_owned()];
    match a.kind {
        AlertKind::CriticalWarning => {
            let text = a
                .args
                .first()
                .map(String::as_str)
                .unwrap_or("")
                .split(',')
                .filter(|f| !f.is_empty())
                .map(|f| tr(lang, &format!("cw.{f}")))
                .collect::<Vec<_>>()
                .join(", ");
            args.push(text);
        }
        AlertKind::WriteSpike => args.push(human_bytes(
            lang,
            a.args.first().and_then(|s| s.parse().ok()).unwrap_or(0),
        )),
        _ => args.extend(a.args.iter().cloned()),
    }
    fmt(lang, &format!("alert.{}", a.kind.id()), &args)
}

/// Anti-spam: remembers when each (disk, warning) was last shown.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Throttle {
    last: BTreeMap<String, u64>,
}

impl Throttle {
    pub fn load(path: &Path) -> Throttle {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    /// `true` if this warning may be shown now; records it. The same warning for the same disk is allowed again
    /// only after a full day.
    pub fn allow(&mut self, disk_key: &str, kind: AlertKind, now: u64) -> bool {
        let key = format!("{disk_key}|{}", kind.id());
        match self.last.get(&key) {
            Some(&t) if now.saturating_sub(t) < DAY => false,
            _ => {
                self.last.insert(key, now);
                self.last.retain(|_, t| now.saturating_sub(*t) < 30 * DAY);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(f: impl FnOnce(&mut Sample)) -> Sample {
        let mut x = Sample {
            t: 1000,
            ..Sample::default()
        };
        f(&mut x);
        x
    }

    fn kinds(v: &[Alert]) -> Vec<AlertKind> {
        v.iter().map(|a| a.kind).collect()
    }

    #[test]
    fn overheat_and_critical_warning_fire_without_history() {
        let cfg = AlertConfig::default();
        assert!(evaluate(None, &s(|x| x.temp_c = Some(69)), &cfg).is_empty());
        let v = evaluate(
            None,
            &s(|x| {
                x.temp_c = Some(70);
                x.crit = Some(0b1);
            }),
            &cfg,
        );
        assert_eq!(kinds(&v), vec![AlertKind::Overheat, AlertKind::CriticalWarning]);
        assert_eq!(v[0].args, vec!["70", "70"]);
    }

    #[test]
    fn growth_needs_a_previous_snapshot() {
        let cfg = AlertConfig::default();
        let cur = s(|x| x.media_errors = Some(5));
        assert!(evaluate(None, &cur, &cfg).is_empty());
        let v = evaluate(Some(&s(|x| x.media_errors = Some(3))), &cur, &cfg);
        assert_eq!(kinds(&v), vec![AlertKind::MediaErrors]);
        assert_eq!(v[0].args, vec!["3", "5"]);
        assert!(evaluate(Some(&cur), &cur, &cfg).is_empty());
    }

    #[test]
    fn sata_counters_pending_includes_uncorrectable() {
        let cfg = AlertConfig::default();
        let prev = s(|x| {
            x.reallocated = Some(0);
            x.pending = Some(1);
            x.uncorrectable = Some(0);
        });
        let cur = s(|x| {
            x.reallocated = Some(2);
            x.pending = Some(1);
            x.uncorrectable = Some(3);
        });
        assert_eq!(
            kinds(&evaluate(Some(&prev), &cur, &cfg)),
            vec![AlertKind::Reallocated, AlertKind::Pending]
        );
        // a decrease (sectors were remapped) is not a warning
        let better = s(|x| {
            x.reallocated = Some(2);
            x.pending = Some(0);
            x.uncorrectable = Some(0);
        });
        assert!(evaluate(Some(&cur), &better, &cfg).is_empty());
    }

    #[test]
    fn spare_drop_and_write_spike() {
        let cfg = AlertConfig {
            write_spike_bytes: 1000,
            ..AlertConfig::default()
        };
        let prev = s(|x| {
            x.spare = Some(100);
            x.written = Some(10_000);
        });
        let cur = s(|x| {
            x.spare = Some(97);
            x.written = Some(11_500);
        });
        assert_eq!(
            kinds(&evaluate(Some(&prev), &cur, &cfg)),
            vec![AlertKind::SpareDropped, AlertKind::WriteSpike]
        );
        // exactly below the spike, and a counter reset, do not fire
        assert!(evaluate(Some(&prev), &s(|x| x.written = Some(10_999)), &cfg).is_empty());
        assert!(evaluate(Some(&prev), &s(|x| x.written = Some(5)), &cfg).is_empty());
    }

    #[test]
    fn same_warning_is_throttled_for_a_day_per_disk() {
        let mut t = Throttle::default();
        assert!(t.allow("d1", AlertKind::Overheat, 1000));
        assert!(!t.allow("d1", AlertKind::Overheat, 1000 + DAY - 1));
        // another disk and another kind are independent
        assert!(t.allow("d2", AlertKind::Overheat, 1001));
        assert!(t.allow("d1", AlertKind::MediaErrors, 1002));
        // after a full day it may fire again
        assert!(t.allow("d1", AlertKind::Overheat, 1000 + DAY));
        assert!(!t.allow("d1", AlertKind::Overheat, 1000 + DAY + 5));
    }

    #[test]
    fn throttle_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notified.json");
        let mut t = Throttle::default();
        assert!(t.allow("d", AlertKind::Overheat, 5000));
        t.save(&path);
        let mut again = Throttle::load(&path);
        assert!(!again.allow("d", AlertKind::Overheat, 5100));
    }

    #[test]
    fn messages_are_localized_for_every_kind() {
        let alerts = [
            Alert {
                kind: AlertKind::Overheat,
                args: vec!["80".into(), "70".into()],
            },
            Alert {
                kind: AlertKind::CriticalWarning,
                args: vec!["spare_low,read_only".into()],
            },
            Alert {
                kind: AlertKind::MediaErrors,
                args: vec!["1".into(), "2".into()],
            },
            Alert {
                kind: AlertKind::Reallocated,
                args: vec!["1".into(), "2".into()],
            },
            Alert {
                kind: AlertKind::Pending,
                args: vec!["1".into(), "2".into()],
            },
            Alert {
                kind: AlertKind::SpareDropped,
                args: vec!["100".into(), "90".into()],
            },
            Alert {
                kind: AlertKind::WriteSpike,
                args: vec!["250000000000".into()],
            },
        ];
        for a in &alerts {
            for lang in [Lang::Ru, Lang::En] {
                let m = message(lang, "Disk X", a);
                assert!(
                    m.contains("Disk X") && !m.contains('{') && !m.contains(crate::i18n::MISSING),
                    "{m}"
                );
            }
        }
        assert_eq!(
            message(Lang::En, "D", &alerts[1]),
            "D: controller critical warning \u{2014} spare below threshold, media is read-only"
        );
        assert!(message(Lang::En, "D", &alerts[6]).contains("250 GB"));
    }
}
