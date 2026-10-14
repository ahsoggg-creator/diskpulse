//! Collects one snapshot per disk into the history store and records the warnings it raises.
//! Used by the tray process, by the optional background service and by `diskpulse-cli collect`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::alerts::{Alert, AlertConfig, AlertKind, evaluate};
use crate::assess::{Level, Thresholds, assess_report};
use crate::disks::DiskInfo;
use crate::history::{DiskMeta, HistoryStore, Sample};
use crate::smart::SmartReport;

/// A warning that was raised during collection (the tray shows it; the service only writes it down).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertRecord {
    pub t: u64,
    pub disk_key: String,
    /// Model name (no serial number).
    pub disk: String,
    pub kind: AlertKind,
    pub args: Vec<String>,
}

impl AlertRecord {
    pub fn alert(&self) -> Alert {
        Alert {
            kind: self.kind,
            args: self.args.clone(),
        }
    }
}

/// Append-only log of raised warnings (`alerts.jsonl` next to the history files).
pub struct AlertLog {
    path: PathBuf,
}

impl AlertLog {
    pub fn new(dir: &Path) -> AlertLog {
        AlertLog {
            path: dir.join("alerts.jsonl"),
        }
    }

    pub fn append(&self, r: &AlertRecord) -> std::io::Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(r).map_err(std::io::Error::other)?)
    }

    /// Records with `t > after`, oldest first.
    pub fn since(&self, after: u64) -> Vec<AlertRecord> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut v: Vec<AlertRecord> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|r: &AlertRecord| r.t > after)
            .collect();
        v.sort_by_key(|r| r.t);
        v
    }

    /// Keeps the last 500 records (the file only needs to bridge the gap between the service and the tray).
    pub fn trim(&self) {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > 500 {
            let _ = std::fs::write(&self.path, lines[lines.len() - 500..].join("\n") + "\n");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub thresholds: Thresholds,
    pub alerts: AlertConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            thresholds: Thresholds::default(),
            alerts: AlertConfig::default(),
        }
    }
}

pub struct Recorded {
    pub sample: Option<Sample>,
    pub level: Option<Level>,
    pub alerts: Vec<Alert>,
}

/// Records one reading of `info`: appends a sample, updates the disk's metadata, evaluates warnings against the previous
/// sample and writes them to the alert log. A disk whose SMART could not be read leaves no sample (and no warnings).
pub fn record_disk(store: &HistoryStore, info: &DiskInfo, report: &SmartReport, cfg: &Config, now: u64) -> Recorded {
    let level = assess_report(report, cfg.thresholds).map(|a| a.level);
    let Some(sample) = Sample::from_report(now, report, level) else {
        return Recorded {
            sample: None,
            level: None,
            alerts: Vec::new(),
        };
    };
    let key = info.key();
    let prev = store.load(&key).pop();
    let alerts = evaluate(prev.as_ref(), &sample, &cfg.alerts);
    let _ = store.append(&key, &sample);
    let old = store.load_meta(&key);
    let _ = store.save_meta(&DiskMeta {
        key: key.clone(),
        model: info.model.clone(),
        serial_masked: info.serial_masked(),
        bus: info.bus.label().to_owned(),
        size_bytes: info.size_bytes,
        tbw_rated_tb: old.as_ref().and_then(|m| m.tbw_rated_tb),
        first_seen: old.as_ref().map_or(now, |m| m.first_seen),
        last_seen: now,
    });
    let log = AlertLog::new(store.dir());
    for a in &alerts {
        let _ = log.append(&AlertRecord {
            t: now,
            disk_key: key.clone(),
            disk: info.model.clone(),
            kind: a.kind,
            args: a.args.clone(),
        });
    }
    Recorded {
        sample: Some(sample),
        level,
        alerts,
    }
}

pub struct DiskOutcome {
    pub info: DiskInfo,
    pub report: SmartReport,
    pub recorded: Recorded,
}

/// Reads every disk and records it.
#[cfg(windows)]
pub fn collect_all(store: &HistoryStore, cfg: &Config, now: u64) -> Vec<DiskOutcome> {
    crate::disks::list_disks()
        .into_iter()
        .map(|info| {
            let report = crate::smart::read_smart(&info);
            let recorded = record_disk(store, &info, &report, cfg, now);
            DiskOutcome { info, report, recorded }
        })
        .collect()
}

/// The worst level among the latest sample of every known disk (for the tray icon).
pub fn worst_level(store: &HistoryStore) -> Option<Level> {
    store
        .disks()
        .iter()
        .filter_map(|m| store.load(&m.key).pop())
        .filter_map(|s| s.level)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disks::BusType;
    use crate::smart::{SmartData, UnavailableReason};

    fn disk() -> DiskInfo {
        DiskInfo {
            number: 0,
            model: "Test NVMe".into(),
            serial: "SECRET-1234".into(),
            firmware: String::new(),
            bus: BusType::Nvme,
            size_bytes: 1,
            removable: false,
            rotational: Some(false),
            volumes: vec![],
        }
    }

    fn report(media_errors: u8) -> SmartReport {
        let mut p = crate::nvme::tests::sample_page();
        p[160..176].fill(0);
        p[160] = media_errors;
        p[196..200].fill(0);
        SmartReport::Ok {
            method: "m".into(),
            data: SmartData::Nvme(crate::nvme::parse_health_log(&p).unwrap()),
        }
    }

    #[test]
    fn records_samples_meta_and_growth_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let cfg = Config::default();
        let first = record_disk(&store, &disk(), &report(0), &cfg, 1000);
        assert!(first.sample.is_some() && first.alerts.is_empty());
        let second = record_disk(&store, &disk(), &report(3), &cfg, 2000);
        assert_eq!(
            second.alerts.iter().map(|a| a.kind).collect::<Vec<_>>(),
            vec![AlertKind::MediaErrors]
        );
        let key = disk().key();
        assert_eq!(store.load(&key).len(), 2);
        let meta = store.load_meta(&key).unwrap();
        assert_eq!((meta.first_seen, meta.last_seen), (1000, 2000));
        assert_eq!(meta.serial_masked, "****1234");
        let log = AlertLog::new(dir.path()).since(0);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].disk, "Test NVMe");
        assert!(AlertLog::new(dir.path()).since(2000).is_empty());
    }

    #[test]
    fn no_file_contains_the_full_serial() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        record_disk(&store, &disk(), &report(0), &Config::default(), 1);
        record_disk(&store, &disk(), &report(2), &Config::default(), 2);
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!name.contains("SECRET"), "{name}");
            assert!(!std::fs::read_to_string(&p).unwrap().contains("SECRET"), "{name}");
        }
    }

    #[test]
    fn unreadable_disk_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let r = SmartReport::unavailable(UnavailableReason::NeedsAdmin, "denied");
        let rec = record_disk(&store, &disk(), &r, &Config::default(), 1);
        assert!(rec.sample.is_none() && rec.level.is_none());
        assert!(store.disks().is_empty() && store.load(&disk().key()).is_empty());
    }

    #[test]
    fn worst_level_is_the_maximum_of_the_latest_samples() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        record_disk(&store, &disk(), &report(0), &Config::default(), 1);
        assert_eq!(worst_level(&store), Some(Level::Good));
        record_disk(&store, &disk(), &report(5), &Config::default(), 2);
        assert_eq!(worst_level(&store), Some(Level::Warning));
    }
}
