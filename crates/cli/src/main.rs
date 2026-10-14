//! `diskpulse-cli`: the same engine as the GUI, from a console. Read-only: nothing here writes to a disk.

mod hist;
mod scan;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use diskpulse_core::assess::{self, Assessment, Level, Thresholds};
use diskpulse_core::ata::Decoded;
use diskpulse_core::disks::{DiskInfo, list_disks};
use diskpulse_core::i18n::{Lang, fmt, tr};
use diskpulse_core::smart::{SmartData, SmartReport, UnavailableReason, read_smart};
use diskpulse_core::units::{human_bytes, human_hours};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "diskpulse-cli",
    version,
    about = "DiskPulse: S.M.A.R.T., history and read-only surface scan"
)]
struct Cli {
    /// Output language: ru or en.
    #[arg(long, global = true, default_value = "ru")]
    lang: String,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List physical disks.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Read S.M.A.R.T. of a disk (by number from `list`).
    Smart {
        disk: u32,
        #[arg(long)]
        json: bool,
        /// Overheat threshold in degrees C used by the assessment (warning at N, bad at N+10).
        #[arg(long, default_value_t = 70)]
        overheat: i32,
        /// Also save the raw SMART pages (no serial numbers inside) into this folder; used to record test fixtures.
        #[arg(long, value_name = "DIR")]
        dump_raw: Option<PathBuf>,
    },
    /// Take one snapshot of every readable disk into the history (what the tray and the service do on a schedule).
    Collect {
        /// History folder (default: the service's folder when it is installed, else the user's).
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        #[arg(long, default_value_t = 70)]
        overheat: i32,
    },
    /// Read-only surface test of a disk (needs administrator rights): timed sequential reads, slow zones, unreadable
    /// blocks and the files that own them. Nothing is ever written.
    Scan {
        disk: u32,
        /// Block size in MiB (1-8).
        #[arg(long, default_value_t = 4)]
        block_mib: u64,
        /// Start of the tested range, in GB.
        #[arg(long, default_value_t = 0.0)]
        start_gb: f64,
        /// Length of the tested range in GB (0 = to the end).
        #[arg(long, default_value_t = 0.0)]
        len_gb: f64,
        /// A block slower than this is "slow".
        #[arg(long, default_value_t = 150.0)]
        slow_ms: f64,
        /// Do not look up files for the zones.
        #[arg(long)]
        no_map: bool,
        #[arg(long)]
        json: bool,
        /// Save a CSV report.
        #[arg(long, value_name = "FILE")]
        csv: Option<PathBuf>,
        /// Save an HTML report.
        #[arg(long, value_name = "FILE")]
        html: Option<PathBuf>,
    },
    /// Show the history of a disk (without a number: the disks that have history).
    History {
        disk: Option<u32>,
        #[arg(long)]
        json: bool,
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
        /// Set the manufacturer's rated endurance in TB (TBW); 0 clears it. Enables the TBW forecast.
        #[arg(long, value_name = "TB")]
        set_tbw: Option<f64>,
        /// With --json: how many days of samples to include.
        #[arg(long, default_value_t = 90)]
        days: u64,
    },
}

#[derive(Serialize)]
struct SmartOutput<'a> {
    disk: &'a DiskInfo,
    smart: &'a SmartReport,
    assessment: Option<&'a Assessment>,
}

fn lang_of(s: &str) -> Lang {
    Lang::from_code(s).unwrap_or_default()
}

fn find(number: u32) -> Option<DiskInfo> {
    list_disks().into_iter().find(|d| d.number == number)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let lang = lang_of(&cli.lang);
    match cli.command {
        Cmd::List { json } => list(lang, json),
        Cmd::Smart {
            disk,
            json,
            overheat,
            dump_raw,
        } => smart(lang, disk, json, Thresholds { overheat_c: overheat }, dump_raw),
        Cmd::Scan {
            disk,
            block_mib,
            start_gb,
            len_gb,
            slow_ms,
            no_map,
            json,
            csv,
            html,
        } => scan::run(
            lang,
            scan::Args {
                disk,
                block_mib,
                start_gb,
                len_gb,
                slow_ms,
                no_map,
                json,
                csv,
                html,
            },
        ),
        Cmd::Collect { dir, overheat } => hist::collect(lang, dir, overheat),
        Cmd::History {
            disk,
            json,
            dir,
            set_tbw,
            days,
        } => hist::history(lang, disk, json, dir, set_tbw, days),
    }
}

fn list(lang: Lang, json: bool) -> ExitCode {
    let disks = list_disks();
    if json {
        println!("{}", serde_json::to_string_pretty(&disks).expect("serializable"));
        return ExitCode::SUCCESS;
    }
    if disks.is_empty() {
        eprintln!("no physical disks found");
        return ExitCode::from(1);
    }
    println!(
        "{:>2}  {:<32} {:<8} {:>10}  {:<10} Volumes",
        "#", "Model", "Bus", "Size", "Serial"
    );
    for d in &disks {
        println!(
            "{:>2}  {:<32} {:<8} {:>10}  {:<10} {}",
            d.number,
            d.model,
            d.bus.label(),
            human_bytes(lang, u128::from(d.size_bytes)),
            d.serial_masked(),
            d.volumes.join(" ")
        );
    }
    ExitCode::SUCCESS
}

fn smart(lang: Lang, number: u32, json: bool, t: Thresholds, dump_raw: Option<PathBuf>) -> ExitCode {
    let Some(disk) = find(number) else {
        eprintln!("disk {number} not found (see `diskpulse-cli list`)");
        return ExitCode::from(1);
    };
    if let Some(dir) = &dump_raw {
        match dump(&disk, dir) {
            Ok(files) => eprintln!("raw pages saved: {}", files.join(", ")),
            Err(e) => eprintln!("raw dump failed: {e}"),
        }
    }
    let report = read_smart(&disk);
    let assessment = assess::assess_report(&report, t);
    if json {
        let out = SmartOutput {
            disk: &disk,
            smart: &report,
            assessment: assessment.as_ref(),
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serializable"));
    } else {
        print_report(lang, &disk, &report, assessment.as_ref());
    }
    if report.data().is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

fn dump(disk: &DiskInfo, dir: &std::path::Path) -> Result<Vec<String>, String> {
    use diskpulse_core::{ata, disks::BusType, nvme, winapi::Device};
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    let mut save = |name: String, bytes: &[u8]| -> Result<(), String> {
        std::fs::write(dir.join(&name), bytes).map_err(|e| e.to_string())?;
        files.push(name);
        Ok(())
    };
    let path = disk.path();
    if disk.bus == BusType::Nvme {
        let dev = Device::open_query(&path).map_err(|e| e.to_string())?;
        save(
            format!("nvme_health_disk{}.bin", disk.number),
            &nvme::read_log_raw(&dev).map_err(|e| e.to_string())?,
        )?;
    } else {
        let pages = if disk.bus == BusType::Usb {
            ata::read_raw_sat(&path)
        } else {
            ata::read_raw_ata(&path, disk.number)
        };
        let (data, thr) = pages.map_err(|e| format!("{e:?}"))?;
        save(format!("ata_smart_disk{}.bin", disk.number), &data)?;
        if let Some(thr) = thr {
            save(format!("ata_thresholds_disk{}.bin", disk.number), &thr)?;
        }
    }
    Ok(files)
}

fn level_label(lang: Lang, l: Level) -> &'static str {
    tr(
        lang,
        match l {
            Level::Good => "level.good",
            Level::Warning => "level.warning",
            Level::Bad => "level.bad",
        },
    )
}

fn print_report(lang: Lang, d: &DiskInfo, r: &SmartReport, a: Option<&Assessment>) {
    println!(
        "{}  [{}]  {}  serial {}",
        d.model,
        d.bus.label(),
        human_bytes(lang, u128::from(d.size_bytes)),
        d.serial_masked()
    );
    match r {
        SmartReport::Unavailable { reason, detail } => {
            let key = match reason {
                UnavailableReason::NeedsAdmin => "unavail.needs_admin",
                UnavailableReason::UsbBridge => "unavail.usb_bridge",
                UnavailableReason::Unsupported => "unavail.unsupported",
                UnavailableReason::Invalid => "unavail.invalid",
            };
            println!("{}", tr(lang, key));
            println!("{}", tr(lang, &format!("{key}.hint")));
            println!("{}: {detail}", tr(lang, "unavail.detail"));
        }
        SmartReport::Ok { method, data } => {
            println!("S.M.A.R.T.: {method}");
            if let Some(a) = a {
                println!("{}: {}", tr(lang, "assess.title"), level_label(lang, a.level));
                if a.reasons.is_empty() {
                    println!("  {}", tr(lang, "assess.all_ok"));
                }
                for reason in &a.reasons {
                    println!(
                        "  [{}] {}",
                        reason.rule,
                        fmt(lang, &format!("rule.{}", reason.rule), &reason.args)
                    );
                }
            }
            match data {
                SmartData::Nvme(h) => print_nvme(lang, h),
                SmartData::Ata(s) => print_ata(lang, s),
            }
        }
    }
}

fn print_nvme(lang: Lang, h: &diskpulse_core::nvme::NvmeHealth) {
    let row = |key: &str, v: String| println!("  {:<44} {v}", tr(lang, key));
    row(
        "nv.temperature",
        h.temperature_c.map_or("-".into(), |t| format!("{t} °C")),
    );
    row(
        "nv.spare",
        format!(
            "{}% ({}: {}%)",
            h.available_spare,
            tr(lang, "nv.spare_thr"),
            h.available_spare_threshold
        ),
    );
    row("nv.used", format!("{}%", h.percentage_used));
    row("nv.written", human_bytes(lang, h.bytes_written()));
    row("nv.read", human_bytes(lang, h.bytes_read()));
    row("nv.poh", human_hours(lang, h.power_on_hours));
    row("nv.cycles", h.power_cycles.to_string());
    row("nv.unsafe", h.unsafe_shutdowns.to_string());
    row("nv.media", h.media_errors.to_string());
    row("nv.errlog", h.error_log_entries.to_string());
    let cw = diskpulse_core::nvme::critical_warning_flags(h.critical_warning);
    row(
        "nv.crit",
        if cw.is_empty() {
            tr(lang, "nv.crit.none").to_owned()
        } else {
            cw.iter()
                .map(|f| tr(lang, &format!("cw.{f}")))
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
}

fn print_ata(lang: Lang, s: &diskpulse_core::ata::AtaSmart) {
    println!(
        "  {:>3}  {:<36} {:>4} {:>5} {:>6} {:>14}  {}",
        tr(lang, "ata.id"),
        tr(lang, "ata.name"),
        tr(lang, "ata.cur"),
        tr(lang, "ata.worst"),
        tr(lang, "ata.thr"),
        tr(lang, "ata.raw"),
        tr(lang, "ata.decoded")
    );
    for a in &s.attributes {
        let name = if a.name.is_empty() {
            format!("{} 0x{:02X}", tr(lang, "ata.unknown"), a.id)
        } else {
            a.name.clone()
        };
        let decoded = match a.decode() {
            Decoded::Celsius(t) => format!("{t} °C"),
            Decoded::Hours(h) => human_hours(lang, u128::from(h)),
            Decoded::Lbas(l) => human_bytes(lang, u128::from(l) * 512),
            Decoded::Count(c) => c.to_string(),
            Decoded::Raw(_) => String::new(),
        };
        let mark = if a.vendor_dependent && !decoded.is_empty() {
            format!(" ({})", tr(lang, "ata.vendor_dep"))
        } else {
            String::new()
        };
        println!(
            "  {:>3}  {:<36} {:>4} {:>5} {:>6} {:>14}  {decoded}{mark}",
            a.id,
            name,
            a.current,
            a.worst,
            a.threshold.map_or("-".to_owned(), |t| t.to_string()),
            a.raw
        );
    }
    if s.attributes.iter().all(|a| a.threshold.is_none()) {
        println!("  ({})", tr(lang, "ata.thr_missing"));
    }
}
