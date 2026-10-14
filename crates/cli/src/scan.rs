//! `scan <disk>`: the read-only surface test, with slow zones and unreadable blocks attributed to files.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use diskpulse_core::disks::{list_disks, volumes};
use diskpulse_core::i18n::{Lang, tr};
use diskpulse_core::ntfs::Kind;
use diskpulse_core::report::{self, Input, Row};
use diskpulse_core::scanmap::{self, Outcome, Piece, open_volume};
use diskpulse_core::surface::{self, MIB, Params, ReadOnlyDisk, Report};
use diskpulse_core::units::{human_bytes, now_unix};
use serde_json::json;

static CANCEL: AtomicBool = AtomicBool::new(false);

fn install_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_: u32) -> i32 {
        CANCEL.store(true, Ordering::Relaxed);
        1 // handled: the scan stops after the current block and still prints what it has
    }
    // SAFETY: registering a handler that only stores to an atomic.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}

pub struct Args {
    pub disk: u32,
    pub block_mib: u64,
    pub start_gb: f64,
    pub len_gb: f64,
    pub slow_ms: f64,
    pub no_map: bool,
    pub json: bool,
    pub csv: Option<PathBuf>,
    pub html: Option<PathBuf>,
}

fn gb(v: f64) -> u64 {
    (v * 1e9) as u64 / surface::SECTOR * surface::SECTOR
}

pub fn run(lang: Lang, a: Args) -> ExitCode {
    let Some(disk) = list_disks().into_iter().find(|d| d.number == a.disk) else {
        eprintln!("disk {} not found (see `diskpulse-cli list`)", a.disk);
        return ExitCode::from(1);
    };
    let src = match ReadOnlyDisk::open(disk.number, disk.size_bytes) {
        Ok(s) => s,
        Err(e) if e.is_access_denied() => {
            eprintln!("{}", tr(lang, "unavail.needs_admin"));
            eprintln!("{}", tr(lang, "scan.needs_admin_cli"));
            return ExitCode::from(3);
        }
        Err(e) => {
            eprintln!("cannot open the disk for reading: {e}");
            return ExitCode::from(1);
        }
    };
    let params = Params {
        block_bytes: a.block_mib * MIB,
        start: gb(a.start_gb),
        len: gb(a.len_gb),
        slow_ms: a.slow_ms,
        ..Params::default()
    };
    install_ctrl_c();
    eprintln!(
        "{} [{}] {}: read-only scan, {} MiB blocks. Ctrl+C stops it.",
        disk.model,
        disk.bus.label(),
        human_bytes(lang, u128::from(disk.size_bytes)),
        a.block_mib
    );
    let mut next_report = 0u64;
    let rep = match surface::scan(&src, params, &CANCEL, &mut |p| {
        let pct = p.done_bytes * 100 / p.total_bytes.max(1);
        if pct >= next_report && !a.json {
            eprint!("\r{pct:>3}%  {:>6.0} MB/s   ", p.last_mbps);
            next_report = pct + 1;
        }
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\n{e}");
            return ExitCode::from(1);
        }
    };
    eprintln!();
    let (zone_pieces, bad_pieces) = if a.no_map {
        (vec![], vec![])
    } else {
        attribute(&disk, &rep)
    };
    let rows = report::rows(&rep, &zone_pieces, &bad_pieces);
    let input = Input {
        lang,
        model: &disk.model,
        serial_masked: &disk.serial_masked(),
        disk_size: disk.size_bytes,
        scan: &rep,
        rows,
        generated: now_unix(),
    };
    for (path, content) in [(&a.csv, report::csv(&input)), (&a.html, report::html(&input))] {
        if let Some(p) = path {
            match std::fs::write(p, content) {
                Ok(()) => eprintln!("report saved: {}", p.display()),
                Err(e) => eprintln!("cannot save {}: {e}", p.display()),
            }
        }
    }
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&to_json(&disk.model, &disk.serial_masked(), &rep, &input.rows))
                .expect("serializable")
        );
    } else {
        print_text(lang, &rep, &input.rows);
    }
    if rep.bad.is_empty() && !rep.cancelled && !rep.too_many_errors {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(4)
    }
}

fn attribute(disk: &diskpulse_core::disks::DiskInfo, rep: &Report) -> (Vec<Piece>, Vec<Piece>) {
    let vols = volumes();
    let zones: Vec<(u64, u64)> = rep.zones().iter().map(|z| (z.start, z.end)).collect();
    let bad: Vec<(u64, u64)> = rep.bad.iter().map(|b| (b.start, b.end)).collect();
    if zones.is_empty() && bad.is_empty() {
        return (vec![], vec![]);
    }
    eprintln!(
        "looking up files for {} slow zone(s) and {} unreadable range(s)...",
        zones.len(),
        bad.len()
    );
    let open = |l: &str| open_volume(l);
    let mut quiet = |_: u64, _: u64| {};
    // one pass over each volume for both lists
    let mut all = zones.clone();
    all.extend(&bad);
    let pieces = scanmap::attribute(disk.number, &all, &vols, &open, &mut quiet);
    let split = |list: &[(u64, u64)]| {
        pieces
            .iter()
            .filter(|p| list.iter().any(|(a, b)| p.start >= *a && p.end <= *b))
            .cloned()
            .collect::<Vec<_>>()
    };
    (split(&zones), split(&bad))
}

fn print_text(lang: Lang, rep: &Report, rows: &[Row]) {
    println!(
        "{}: {} / {}, {} {:.0} MB/s, {} {}",
        tr(lang, "scan.tested"),
        human_bytes(lang, u128::from(rep.bytes_read)),
        human_bytes(lang, u128::from(rep.device_size)),
        tr(lang, "scan.avg"),
        rep.avg_mbps(),
        tr(lang, "scan.blocks"),
        rep.blocks.len()
    );
    if rep.cancelled {
        println!("{}", tr(lang, "scan.cancelled"));
    }
    if rep.too_many_errors {
        println!("{}", tr(lang, "scan.too_many"));
    }
    println!("{}:", tr(lang, "scan.histogram"));
    let top = rep.histogram.iter().copied().max().unwrap_or(1).max(1);
    for (i, c) in rep.histogram.iter().enumerate() {
        println!("  {:>14} {:>8} {}", label(i), c, "#".repeat((c * 40 / top) as usize));
    }
    for (bad, key, none) in [
        (false, "scan.zones", "scan.none_zones"),
        (true, "scan.bad", "scan.none_bad"),
    ] {
        println!(
            "{} (>{} ms):",
            tr(lang, key),
            if bad { 0.0 } else { rep.params.slow_ms }
        );
        let mut any = false;
        for r in rows.iter().filter(|r| r.unreadable == bad) {
            any = true;
            let mb = |b: u64| b as f64 / 1_048_576.0;
            println!(
                "  {:>10.1} .. {:>10.1} MiB{}",
                mb(r.start),
                mb(r.end),
                if bad {
                    String::new()
                } else {
                    format!("   max {:.0} ms, mean {:.0} ms", r.max_ms, r.mean_ms)
                }
            );
            for p in &r.pieces {
                match &p.outcome {
                    Outcome::Mapped { volume, zone } => {
                        for f in zone.files.iter().take(8) {
                            let k = match f.kind {
                                Kind::File => "",
                                Kind::Directory => "[dir] ",
                                Kind::Metadata => "[NTFS] ",
                                Kind::Unknown => "[?] ",
                            };
                            println!(
                                "      {volume}{} {k}{} ({})",
                                f.path,
                                f.attr,
                                human_bytes(lang, u128::from(f.bytes))
                            );
                        }
                        if zone.files.len() > 8 {
                            println!("      ... +{}", zone.files.len() - 8);
                        }
                        if zone.free_bytes > 0 {
                            println!(
                                "      {} ({})",
                                tr(lang, "scan.free"),
                                human_bytes(lang, u128::from(zone.free_bytes))
                            );
                        }
                    }
                    o => println!("      {}", report::outcome_note(lang, o)),
                }
            }
        }
        if !any {
            println!("  {}", tr(lang, none));
        }
    }
}

fn label(i: usize) -> String {
    let e = surface::HISTOGRAM_EDGES_MS;
    match i {
        0 => format!("< {} ms", e[0]),
        10 => format!(">= {} ms", e[9]),
        n => format!("{}-{} ms", e[n - 1], e[n]),
    }
}

fn to_json(model: &str, serial: &str, rep: &Report, rows: &[Row]) -> serde_json::Value {
    let rows_json = |bad: bool| {
        rows.iter()
            .filter(|r| r.unreadable == bad)
            .map(|r| {
                let files: Vec<_> = r
                    .pieces
                    .iter()
                    .map(|p| match &p.outcome {
                        Outcome::Mapped { volume, zone } => json!({
                            "volume": volume, "free_bytes": zone.free_bytes, "other_allocated_bytes": zone.other_allocated_bytes,
                            "files": zone.files.iter().map(|f| json!({"path": f.path, "attr": f.attr, "bytes": f.bytes, "metadata": f.kind == Kind::Metadata, "directory": f.kind == Kind::Directory})).collect::<Vec<_>>(),
                        }),
                        o => json!({"unresolved": report::outcome_note(Lang::En, o)}),
                    })
                    .collect();
                json!({"start": r.start, "end": r.end, "max_ms": r.max_ms, "mean_ms": r.mean_ms, "pieces": files})
            })
            .collect::<Vec<_>>()
    };
    json!({
        "disk": {"model": model, "serial": serial, "size_bytes": rep.device_size},
        "params": {"block_bytes": rep.params.block_bytes, "start": rep.params.start, "slow_ms": rep.params.slow_ms},
        "bytes_read": rep.bytes_read, "blocks": rep.blocks.len(), "avg_mb_s": rep.avg_mbps(),
        "cancelled": rep.cancelled, "too_many_errors": rep.too_many_errors,
        "histogram_ms_edges": surface::HISTOGRAM_EDGES_MS, "histogram": rep.histogram,
        "slow_zones": rows_json(false), "unreadable": rows_json(true),
    })
}
