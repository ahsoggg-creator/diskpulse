//! `history` and `collect` commands.

use std::path::PathBuf;
use std::process::ExitCode;

use diskpulse_core::collector::{self, Config};
use diskpulse_core::disks::list_disks;
use diskpulse_core::history::{DAY, DiskMeta, HistoryStore, Sample, Summary, WrittenStats, summarize};
use diskpulse_core::i18n::{Lang, fmt, tr};
use diskpulse_core::service;
use diskpulse_core::smart::SmartReport;
use diskpulse_core::units::{format_utc, human_bytes, human_span, now_unix};
use serde::Serialize;

fn store(dir: Option<PathBuf>) -> HistoryStore {
    HistoryStore::new(dir.unwrap_or_else(service::history_dir))
}

/// `diskpulse-cli collect`: one snapshot of every readable disk into the history.
pub fn collect(lang: Lang, dir: Option<PathBuf>, overheat: i32) -> ExitCode {
    let store = store(dir);
    let mut cfg = Config::default();
    cfg.thresholds.overheat_c = overheat;
    cfg.alerts.overheat_c = overheat;
    let mut any = false;
    for o in collector::collect_all(&store, &cfg, now_unix()) {
        match (&o.report, &o.recorded.sample) {
            (SmartReport::Ok { .. }, Some(_)) => {
                any = true;
                println!(
                    "#{} {}: recorded{}",
                    o.info.number,
                    o.info.model,
                    if o.recorded.alerts.is_empty() {
                        ""
                    } else {
                        " (warnings raised)"
                    }
                );
                for a in &o.recorded.alerts {
                    println!("    ! {}", diskpulse_core::alerts::message(lang, &o.info.model, a));
                }
            }
            (SmartReport::Unavailable { reason, .. }, _) => println!(
                "#{} {}: skipped, SMART unavailable ({reason:?})",
                o.info.number, o.info.model
            ),
            _ => println!("#{} {}: skipped", o.info.number, o.info.model),
        }
    }
    println!("{}", store.dir().display());
    if any { ExitCode::SUCCESS } else { ExitCode::from(2) }
}

#[derive(Serialize)]
struct HistoryOutput<'a> {
    meta: &'a DiskMeta,
    samples: &'a [Sample],
    written_7d: Written,
    written_30d: Written,
    forecast_days: Option<f64>,
    forecast_is_estimate: bool,
}

#[derive(Serialize)]
struct Written {
    bytes: u64,
    covered_days: f64,
    resets: u32,
}

fn written(w: WrittenStats) -> Written {
    Written {
        bytes: w.bytes,
        covered_days: w.covered_secs as f64 / DAY as f64,
        resets: w.resets,
    }
}

/// `diskpulse-cli history [disk]`.
pub fn history(
    lang: Lang,
    disk: Option<u32>,
    json: bool,
    dir: Option<PathBuf>,
    set_tbw: Option<f64>,
    days: u64,
) -> ExitCode {
    let store = store(dir);
    let Some(number) = disk else {
        let metas = store.disks();
        if json {
            println!("{}", serde_json::to_string_pretty(&metas).expect("serializable"));
        } else if metas.is_empty() {
            println!("{}", tr(lang, "hist.none"));
        }
        for m in &metas {
            println!(
                "{}  {:<32} {:<6} last {}",
                m.key,
                m.model,
                m.bus,
                format_utc(m.last_seen)
            );
        }
        return ExitCode::SUCCESS;
    };
    let Some(info) = list_disks().into_iter().find(|d| d.number == number) else {
        eprintln!("disk {number} not found (see `diskpulse-cli list`)");
        return ExitCode::from(1);
    };
    let key = info.key();
    if let (Some(tb), Some(mut meta)) = (set_tbw, store.load_meta(&key)) {
        meta.tbw_rated_tb = (tb > 0.0).then_some(tb);
        if let Err(e) = store.save_meta(&meta) {
            eprintln!("cannot save: {e}");
            return ExitCode::from(1);
        }
    }
    let (Some(meta), samples) = (store.load_meta(&key), store.load(&key)) else {
        println!("{}", tr(lang, "hist.none"));
        return ExitCode::from(2);
    };
    let now = now_unix();
    let sum = summarize(&samples, now, &meta.bus, meta.tbw_rated_bytes());
    let shown: Vec<Sample> = samples
        .iter()
        .filter(|s| now.saturating_sub(s.t) <= days * DAY)
        .cloned()
        .collect();
    if json {
        let (w7, w30) = sum
            .as_ref()
            .map_or((WrittenStats::default(), WrittenStats::default()), |s| {
                (s.week, s.month)
            });
        let out = HistoryOutput {
            meta: &meta,
            samples: &shown,
            written_7d: written(w7),
            written_30d: written(w30),
            forecast_days: sum.as_ref().and_then(|s| s.forecast).and_then(|f| f.days_left),
            forecast_is_estimate: true,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serializable"));
        return ExitCode::SUCCESS;
    }
    println!("{}  [{}]  {}", meta.model, meta.bus, meta.serial_masked);
    if let Some(s) = &sum {
        print_summary(lang, &meta, s);
    }
    ExitCode::SUCCESS
}

fn print_summary(lang: Lang, meta: &DiskMeta, s: &Summary) {
    let row = |key: &str, v: String| println!("  {:<46} {v}", tr(lang, key));
    row("hist.samples", s.count.to_string());
    row(
        "hist.range",
        format!("{} .. {}", format_utc(s.first_t), format_utc(s.last_t)),
    );
    if let (Some(a), Some(b), Some(avg)) = (s.temp_min, s.temp_max, s.temp_avg) {
        row("hist.temp", format!("{a} / {avg:.0} / {b} °C"));
    }
    if let (Some(a), Some(b)) = (s.pct_used_first, s.pct_used_last) {
        row("hist.used", format!("{a}% -> {b}%"));
    }
    let est = meta.bus != "NVMe";
    let tag = if est {
        format!(" ({})", tr(lang, "hist.estimate"))
    } else {
        String::new()
    };
    if let Some(w) = s.written_total {
        row(
            "hist.written_total",
            format!("{}{tag}", human_bytes(lang, u128::from(w))),
        );
    }
    for (key, w) in [("hist.week", s.week), ("hist.month", s.month)] {
        let mut v = format!(
            "{}{tag}, {}",
            human_bytes(lang, u128::from(w.bytes)),
            fmt(lang, "hist.coverage", &[human_span(lang, w.covered_secs)])
        );
        if w.resets > 0 {
            v.push_str(&format!(", {}", fmt(lang, "hist.resets", &[w.resets.to_string()])));
        }
        row(key, v);
    }
    match (meta.tbw_rated_bytes(), s.forecast) {
        (None, _) => row("hist.forecast", tr(lang, "hist.forecast.no_tbw").to_owned()),
        (Some(_), None) => row("hist.forecast", tr(lang, "hist.forecast.no_data").to_owned()),
        (Some(_), Some(f)) => {
            let left = human_bytes(lang, u128::from(f.remaining_bytes));
            row(
                "hist.forecast",
                match f.days_left {
                    Some(d) => fmt(
                        lang,
                        "hist.forecast.value",
                        &[human_span(lang, (d * DAY as f64) as u64), left],
                    ),
                    None => fmt(lang, "hist.forecast.idle", &[left]),
                },
            );
        }
    }
    if est {
        println!("  {}", tr(lang, "hist.sata_estimate"));
    }
}
