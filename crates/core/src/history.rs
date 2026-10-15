//! Snapshot history. Storage choice: one JSON-lines file per disk (`<key>.jsonl`) plus a small `<key>.meta.json`.
//!
//! Why not SQLite: it needs a native C library, the data is a few hundred bytes per sample appended in time order,
//! and thinned history stays tiny (about 1 500 lines for a year). Append-only text is trivially crash-safe (a torn
//! last line is skipped on load), diffable and needs no dependency. Rewrites (thinning) go through a temp file + rename.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::assess::Level;
use crate::smart::{SmartData, SmartReport};

pub const DAY: u64 = 86_400;

/// One snapshot of a disk. Every field is optional: a SATA drive has no Percentage Used, an NVMe drive no attribute 197.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Sample {
    /// Unix seconds.
    pub t: u64,
    pub temp_c: Option<i32>,
    pub pct_used: Option<u8>,
    pub spare: Option<u8>,
    /// Total bytes written by the host. NVMe: exact (Data Units x 512 000). SATA: attribute 241 x 512 (an estimate).
    pub written: Option<u64>,
    pub read: Option<u64>,
    pub poh: Option<u64>,
    pub media_errors: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub uncorrectable: Option<u64>,
    pub crit: Option<u8>,
    pub level: Option<Level>,
}

fn clamp64(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

impl Sample {
    /// `None` when the report has no data (an unreadable disk leaves no gap-filling zeros in the history).
    pub fn from_report(t: u64, report: &SmartReport, level: Option<Level>) -> Option<Sample> {
        let mut s = Sample {
            t,
            level,
            ..Sample::default()
        };
        match report.data()? {
            SmartData::Nvme(h) => {
                s.temp_c = h.temperature_c;
                s.pct_used = Some(h.percentage_used);
                s.spare = Some(h.available_spare);
                s.written = Some(clamp64(h.bytes_written()));
                s.read = Some(clamp64(h.bytes_read()));
                s.poh = Some(clamp64(h.power_on_hours));
                s.media_errors = Some(clamp64(h.media_errors));
                s.unsafe_shutdowns = Some(clamp64(h.unsafe_shutdowns));
                s.crit = Some(h.critical_warning);
            }
            SmartData::Ata(a) => {
                s.temp_c = a.temperature_c();
                s.written = a.bytes_written_estimate().map(clamp64);
                s.read = a.bytes_read_estimate().map(clamp64);
                s.poh = a.power_on_hours();
                s.reallocated = a.raw(5);
                s.pending = a.raw(197);
                s.uncorrectable = a.raw(198);
            }
        }
        Some(s)
    }
}

// ---- counters ---------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta {
    /// Bytes (or units) added since the previous sample.
    Added(u64),
    /// The counter went backwards and a wrap-around does not explain it: a firmware reset or a replaced drive.
    /// The step contributes nothing; the new value is the baseline from here on.
    Reset,
}

/// Difference between two readings of a monotonic counter that is `wrap_bits` wide (`None` = never wraps).
///
/// A decrease counts as a wrap only if the previous value was in the top quarter of the counter's range; anything
/// else is a reset (a counter does not "almost" wrap from the middle of its range).
pub fn counter_delta(prev: u64, cur: u64, wrap_bits: Option<u32>) -> Delta {
    if cur >= prev {
        return Delta::Added(cur - prev);
    }
    match wrap_bits {
        Some(bits) if bits < 64 => {
            let modulus = 1u64 << bits;
            if prev >= modulus - modulus / 4 && prev < modulus {
                Delta::Added(modulus - prev + cur)
            } else {
                Delta::Reset
            }
        }
        _ => Delta::Reset,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WrittenStats {
    pub bytes: u64,
    /// Seconds of the requested window that the samples actually cover.
    pub covered_secs: u64,
    pub resets: u32,
}

/// Bytes written during the last `window` seconds before `now`, from consecutive samples. `wrap_bits` is the width of
/// the underlying counter (48 for ATA attribute 241; `None` for NVMe's 128-bit counter).
pub fn written_in_window(samples: &[Sample], now: u64, window: u64, wrap_bits: Option<u32>) -> WrittenStats {
    let start = now.saturating_sub(window);
    let pts: Vec<(u64, u64)> = samples
        .iter()
        .filter_map(|s| s.written.map(|w| (s.t, w)))
        .filter(|(t, _)| *t <= now)
        .collect();
    // baseline: the last sample at or before the window start, else the first sample inside the window
    let first = pts
        .iter()
        .rposition(|(t, _)| *t <= start)
        .or_else(|| pts.iter().position(|(t, _)| *t > start));
    let Some(first) = first else {
        return WrittenStats::default();
    };
    let mut st = WrittenStats::default();
    let from = pts[first].0.max(start);
    st.covered_secs = pts.last().map_or(0, |(t, _)| t.saturating_sub(from));
    for w in pts[first..].windows(2) {
        match counter_delta(w[0].1, w[1].1, wrap_bits) {
            Delta::Added(d) => st.bytes = st.bytes.saturating_add(d),
            Delta::Reset => st.resets += 1,
        }
    }
    st
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Forecast {
    pub remaining_bytes: u64,
    /// `None` when the write rate is zero (no end in sight).
    pub days_left: Option<f64>,
}

/// TBW forecast. Only produced when the manufacturer's rated TBW is known **and** the history covers at least
/// `min_covered_days` of writes; it is an estimate (it assumes the recent write rate continues).
pub fn tbw_forecast(
    total_written: u64,
    rated_tbw_bytes: Option<u64>,
    recent: WrittenStats,
    min_covered_days: u64,
) -> Option<Forecast> {
    let rated = rated_tbw_bytes?;
    if recent.covered_secs < min_covered_days * DAY {
        return None;
    }
    let remaining = rated.saturating_sub(total_written);
    let per_day = recent.bytes as f64 / (recent.covered_secs as f64 / DAY as f64);
    Some(Forecast {
        remaining_bytes: remaining,
        days_left: (per_day > 0.0).then(|| remaining as f64 / per_day),
    })
}

// ---- summary ----------------------------------------------------------------------------------------------------

/// Width of the "written" counter by bus: NVMe's is 128-bit (never wraps in practice); a SATA drive's attribute 241 is
/// a 48-bit LBA count, i.e. 2^57 bytes at 512 bytes per LBA.
pub fn wrap_bits_for_bus(bus: &str) -> Option<u32> {
    if bus.eq_ignore_ascii_case("nvme") {
        None
    } else {
        Some(57)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub first_t: u64,
    pub last_t: u64,
    pub temp_min: Option<i32>,
    pub temp_max: Option<i32>,
    pub temp_avg: Option<f64>,
    pub pct_used_first: Option<u8>,
    pub pct_used_last: Option<u8>,
    pub written_total: Option<u64>,
    pub week: WrittenStats,
    pub month: WrittenStats,
    /// Only when the rated TBW is known and the last 30 days cover enough data. An estimate.
    pub forecast: Option<Forecast>,
}

pub fn summarize(samples: &[Sample], now: u64, bus: &str, rated_tbw_bytes: Option<u64>) -> Option<Summary> {
    let (first, last) = (samples.first()?, samples.last()?);
    let temps: Vec<i32> = samples.iter().filter_map(|s| s.temp_c).collect();
    let wrap = wrap_bits_for_bus(bus);
    let month = written_in_window(samples, now, 30 * DAY, wrap);
    let written_total = samples.iter().rev().find_map(|s| s.written);
    Some(Summary {
        count: samples.len(),
        first_t: first.t,
        last_t: last.t,
        temp_min: temps.iter().copied().min(),
        temp_max: temps.iter().copied().max(),
        temp_avg: (!temps.is_empty()).then(|| temps.iter().map(|t| f64::from(*t)).sum::<f64>() / temps.len() as f64),
        pct_used_first: samples.iter().find_map(|s| s.pct_used),
        pct_used_last: samples.iter().rev().find_map(|s| s.pct_used),
        written_total,
        week: written_in_window(samples, now, 7 * DAY, wrap),
        month,
        forecast: written_total.and_then(|w| tbw_forecast(w, rated_tbw_bytes, month, 3)),
    })
}

// ---- thinning ---------------------------------------------------------------------------------------------------

/// Keeps every sample from the last 2 days, one per hour up to 30 days, one per day up to 5 years, nothing older.
/// The newest sample of each bucket survives (counters only grow, so the last one carries the most information).
pub fn thin(samples: &[Sample], now: u64) -> Vec<Sample> {
    let mut buckets: BTreeMap<(u8, u64), &Sample> = BTreeMap::new();
    for s in samples {
        let age = now.saturating_sub(s.t);
        let key = if age < 2 * DAY {
            (0, s.t)
        } else if age < 30 * DAY {
            (1, s.t / 3600)
        } else if age < 5 * 365 * DAY {
            (2, s.t / DAY)
        } else {
            continue;
        };
        let e = buckets.entry(key).or_insert(s);
        if s.t >= e.t {
            *e = s;
        }
    }
    let mut out: Vec<Sample> = buckets.into_values().cloned().collect();
    out.sort_by_key(|s| s.t);
    out
}

// ---- store ------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskMeta {
    pub key: String,
    pub model: String,
    /// Masked, e.g. `****1234`.
    pub serial_masked: String,
    pub bus: String,
    pub size_bytes: u64,
    /// The manufacturer's rated endurance in terabytes written, if the user entered it.
    pub tbw_rated_tb: Option<f64>,
    pub first_seen: u64,
    pub last_seen: u64,
}

impl DiskMeta {
    pub fn tbw_rated_bytes(&self) -> Option<u64> {
        self.tbw_rated_tb.filter(|t| *t > 0.0).map(|t| (t * 1e12) as u64)
    }
}

/// Rewrite the file (thinning) once it has this many lines.
const THIN_AT_LINES: usize = 3000;

pub struct HistoryStore {
    dir: PathBuf,
}

impl HistoryStore {
    pub fn new(dir: impl Into<PathBuf>) -> HistoryStore {
        HistoryStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.jsonl"))
    }

    fn meta_file(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.meta.json"))
    }

    /// Appends one sample. Thins the file when it has grown large.
    pub fn append(&self, key: &str, sample: &Sample) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.file(key);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let line = serde_json::to_string(sample).map_err(std::io::Error::other)?;
        writeln!(f, "{line}")?;
        drop(f);
        let lines = std::fs::metadata(&path).map(|m| m.len() / 160).unwrap_or(0) as usize;
        if lines >= THIN_AT_LINES {
            self.thin_file(key, sample.t)?;
        }
        Ok(())
    }

    /// All samples, oldest first. Unparseable lines (a torn write) are skipped.
    pub fn load(&self, key: &str) -> Vec<Sample> {
        let text = std::fs::read_to_string(self.file(key)).unwrap_or_default();
        let mut v: Vec<Sample> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        v.sort_by_key(|s| s.t);
        v
    }

    pub fn thin_file(&self, key: &str, now: u64) -> std::io::Result<()> {
        let kept = thin(&self.load(key), now);
        let tmp = self.dir.join(format!("{key}.jsonl.tmp"));
        let mut f = std::fs::File::create(&tmp)?;
        for s in &kept {
            writeln!(f, "{}", serde_json::to_string(s).map_err(std::io::Error::other)?)?;
        }
        f.sync_all()?;
        drop(f);
        std::fs::rename(tmp, self.file(key))
    }

    pub fn save_meta(&self, meta: &DiskMeta) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{}.meta.json.tmp", meta.key));
        std::fs::write(&tmp, serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, self.meta_file(&meta.key))
    }

    pub fn load_meta(&self, key: &str) -> Option<DiskMeta> {
        serde_json::from_slice(&std::fs::read(self.meta_file(key)).ok()?).ok()
    }

    /// Every disk that has history here.
    pub fn disks(&self) -> Vec<DiskMeta> {
        let mut v: Vec<DiskMeta> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let key = name.strip_suffix(".meta.json")?.to_owned();
                self.load_meta(&key)
            })
            .collect();
        v.sort_by(|a, b| a.model.cmp(&b.model).then(a.key.cmp(&b.key)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t: u64, written: u64) -> Sample {
        Sample {
            t,
            written: Some(written),
            ..Sample::default()
        }
    }

    #[test]
    fn delta_plain_growth() {
        assert_eq!(counter_delta(100, 250, None), Delta::Added(150));
        assert_eq!(counter_delta(7, 7, Some(32)), Delta::Added(0));
    }

    #[test]
    fn delta_detects_wrap_of_a_narrow_counter() {
        // a 32-bit counter close to the top wraps: 2^32 - 10 -> 5 means 15 more
        assert_eq!(counter_delta((1 << 32) - 10, 5, Some(32)), Delta::Added(15));
        // 48-bit counter near the top of its range
        assert_eq!(counter_delta((1 << 48) - 1000, 500, Some(48)), Delta::Added(1500));
    }

    #[test]
    fn delta_treats_other_decreases_as_reset() {
        // from the middle of the range this cannot be a wrap
        assert_eq!(counter_delta(1 << 31, 5, Some(32)), Delta::Reset);
        // a counter that cannot wrap can only have been reset
        assert_eq!(counter_delta(1_000_000, 10, None), Delta::Reset);
        // 128-bit NVMe counters stored as u64: no wrap assumed
        assert_eq!(counter_delta(u64::MAX - 5, 3, None), Delta::Reset);
    }

    #[test]
    fn written_window_sums_deltas_and_survives_reset() {
        let now = 10 * DAY;
        // 100 -> 300 (+200), reset to 50 (baseline), 50 -> 90 (+40)
        let s = vec![at(2 * DAY, 100), at(4 * DAY, 300), at(6 * DAY, 50), at(9 * DAY, 90)];
        let w = written_in_window(&s, now, 10 * DAY, None);
        assert_eq!((w.bytes, w.resets), (240, 1));
        assert_eq!(w.covered_secs, 7 * DAY);
    }

    #[test]
    fn written_window_uses_a_baseline_before_the_window() {
        let now = 20 * DAY;
        let s = vec![at(5 * DAY, 1000), at(14 * DAY, 4000), at(19 * DAY, 9000)];
        // last 7 days start at day 13: the baseline is the day-5 sample, so the whole 1000 -> 9000 growth counts
        let w = written_in_window(&s, now, 7 * DAY, None);
        assert_eq!(w.bytes, 8000);
        // coverage is only what lies inside the window
        assert_eq!(w.covered_secs, 6 * DAY);
    }

    #[test]
    fn written_window_with_wrapping_attribute() {
        let top = (1u64 << 32) - 100;
        let s = vec![at(DAY, top), at(2 * DAY, 400)];
        assert_eq!(written_in_window(&s, 3 * DAY, 3 * DAY, Some(32)).bytes, 500);
    }

    #[test]
    fn written_window_without_data_is_empty() {
        assert_eq!(written_in_window(&[], 100, 50, None), WrittenStats::default());
        let only_before = vec![at(1, 5)];
        assert_eq!(written_in_window(&only_before, 10 * DAY, DAY, None).bytes, 0);
    }

    #[test]
    fn forecast_needs_a_rated_tbw_and_enough_history() {
        let recent = WrittenStats {
            bytes: 30_000,
            covered_secs: 30 * DAY,
            resets: 0,
        };
        assert!(tbw_forecast(1_000, None, recent, 3).is_none());
        assert!(
            tbw_forecast(
                1_000,
                Some(100_000),
                WrittenStats {
                    covered_secs: DAY,
                    ..recent
                },
                3
            )
            .is_none()
        );
        let f = tbw_forecast(1_000, Some(100_000), recent, 3).unwrap();
        assert_eq!(f.remaining_bytes, 99_000);
        // 1000 bytes per day -> 99 days
        assert!((f.days_left.unwrap() - 99.0).abs() < 1e-9);
        let idle = tbw_forecast(1_000, Some(100_000), WrittenStats { bytes: 0, ..recent }, 3).unwrap();
        assert_eq!(idle.days_left, None);
    }

    #[test]
    fn thinning_keeps_recent_and_drops_ancient_samples() {
        let now = 3000 * DAY;
        let mut v = Vec::new();
        for i in 0..48u64 {
            v.push(at(now - i * 1800, i)); // every 30 min for a day
        }
        for d in 3..20u64 {
            for h in 0..24u64 {
                v.push(at(now - d * DAY - h * 3600, 0)); // hourly
                v.push(at(now - d * DAY - h * 3600 - 600, 0)); // plus an extra in the same hour
            }
        }
        v.push(at(now - 6 * 365 * DAY, 1)); // too old
        let t = thin(&v, now);
        assert!(t.iter().all(|s| s.t > now - 5 * 365 * DAY));
        // the last day is untouched
        assert_eq!(t.iter().filter(|s| now - s.t < 2 * DAY).count(), 48);
        // days 3..20 (two samples per hour in) collapse to about one sample per hour (+-1 at the range edges)
        let mid = t.iter().filter(|s| (3 * DAY..21 * DAY).contains(&(now - s.t))).count();
        assert!((17 * 24..=17 * 24 + 2).contains(&mid), "{mid}");
        assert!(t.windows(2).all(|w| w[0].t < w[1].t));
    }

    #[test]
    fn thinning_old_data_to_one_per_day_keeps_the_newest_of_the_day() {
        let now = 400 * DAY;
        let day = 100 * DAY;
        let v = vec![at(day + 10, 1), at(day + 5000, 2), at(day + 80_000, 3)];
        let t = thin(&v, now);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].written, Some(3));
    }

    #[test]
    fn store_roundtrip_skips_torn_lines_and_lists_disks() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        store.append("abc", &at(100, 1)).unwrap();
        store.append("abc", &at(200, 2)).unwrap();
        // simulate a crash in the middle of a write
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("abc.jsonl"))
            .unwrap()
            .write_all(b"{\"t\":3")
            .unwrap();
        let v = store.load("abc");
        assert_eq!(v.iter().map(|s| s.t).collect::<Vec<_>>(), vec![100, 200]);
        let meta = DiskMeta {
            key: "abc".into(),
            model: "M".into(),
            serial_masked: "****1".into(),
            bus: "NVMe".into(),
            size_bytes: 1,
            tbw_rated_tb: Some(150.0),
            first_seen: 100,
            last_seen: 200,
        };
        store.save_meta(&meta).unwrap();
        assert_eq!(store.disks(), vec![meta.clone()]);
        assert_eq!(meta.tbw_rated_bytes(), Some(150_000_000_000_000));
        assert!(store.load("missing").is_empty());
    }

    #[test]
    fn summary_combines_windows_and_forecast() {
        let now = 40 * DAY;
        let mut v = Vec::new();
        for d in 0..40u64 {
            v.push(Sample {
                t: d * DAY,
                written: Some(d * 1000),
                temp_c: Some(30 + (d % 5) as i32),
                pct_used: Some((d / 10) as u8),
                ..Sample::default()
            });
        }
        let s = summarize(&v, now, "NVMe", Some(100_000)).unwrap();
        assert_eq!((s.count, s.temp_min, s.temp_max), (40, Some(30), Some(34)));
        assert_eq!((s.pct_used_first, s.pct_used_last), (Some(0), Some(3)));
        assert_eq!(s.written_total, Some(39_000));
        assert_eq!(s.week.bytes, 6000);
        assert!(s.forecast.is_some());
        assert!(summarize(&v, now, "NVMe", None).unwrap().forecast.is_none());
        assert!(summarize(&[], now, "NVMe", None).is_none());
    }

    #[test]
    fn sata_counters_wrap_at_the_48_bit_lba_boundary() {
        assert_eq!(wrap_bits_for_bus("SATA"), Some(57));
        assert_eq!(wrap_bits_for_bus("nvme"), None);
        let top = (1u64 << 57) - 512 * 10;
        let v = vec![at(DAY, top), at(2 * DAY, 512 * 5)];
        assert_eq!(summarize(&v, 3 * DAY, "SATA", None).unwrap().month.bytes, 512 * 15);
    }

    #[test]
    fn sample_from_report_has_no_gap_filling_zeros_for_unavailable_disks() {
        let r = SmartReport::unavailable(crate::smart::UnavailableReason::UsbBridge, "x");
        assert!(Sample::from_report(1, &r, None).is_none());
        let h = crate::nvme::parse_health_log(&crate::nvme::tests::sample_page()).unwrap();
        let ok = SmartReport::Ok {
            method: "m".into(),
            data: SmartData::Nvme(h),
        };
        let s = Sample::from_report(5, &ok, Some(Level::Good)).unwrap();
        assert_eq!(
            (s.temp_c, s.pct_used, s.written),
            (Some(36), Some(3), Some(1_234_567 * 512_000))
        );
        assert!(s.reallocated.is_none());
    }
}
