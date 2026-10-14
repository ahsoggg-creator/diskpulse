//! Demo mode: `DiskPulse.exe --demo` shows invented disks (nothing real is read or written).
//! `DiskPulse.exe --screenshots <dir>` additionally walks through the screens and saves a PNG of each; that is how the
//! screenshots in the README are made, without a single real model name or serial number in them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use diskpulse_core::assess::{self, Thresholds};
use diskpulse_core::ata::{AtaAttribute, AtaSmart};
use diskpulse_core::disks::{BusType, DiskInfo};
use diskpulse_core::history::{DiskMeta, Sample};
use diskpulse_core::nvme::NvmeHealth;
use diskpulse_core::smart::{SmartData, SmartReport, UnavailableReason};
use eframe::egui::{self, Event, ViewportCommand};

use crate::{App, DiskView, Tab};

/// Time to let layout and animations settle before a screenshot.
const SETTLE: Duration = Duration::from_millis(900);

/// What the screenshot tour does, in order.
const STEPS: &[&str] = &["nvme", "sata", "hdd", "usb", "history", "scan", "settings"];

pub struct Driver {
    pub dir: Option<PathBuf>,
    step: usize,
    prepared: bool,
    asked: bool,
    since: Instant,
}

impl Driver {
    /// `Some` in demo mode; with a screenshot directory when started with `--screenshots <dir>`.
    pub fn from_args() -> Option<Driver> {
        if !crate::settings::is_demo() {
            return None;
        }
        let mut args = std::env::args().skip_while(|a| a != "--screenshots");
        args.next();
        Some(Driver {
            dir: args.next().map(PathBuf::from),
            step: 0,
            prepared: false,
            asked: false,
            since: Instant::now(),
        })
    }
}

fn info(number: u32, model: &str, bus: BusType, gb: u64, rotational: bool, volumes: &[&str]) -> DiskInfo {
    DiskInfo {
        number,
        model: model.to_owned(),
        // invented serial numbers
        serial: format!("DEMO-{number:04}-X{}", 7000 + number * 37),
        firmware: format!("FW{}.{}", number + 1, 2 * number),
        bus,
        size_bytes: gb * 1_000_000_000,
        removable: bus == BusType::Usb,
        rotational: Some(rotational),
        volumes: volumes.iter().map(|v| (*v).to_owned()).collect(),
    }
}

fn nvme_page() -> NvmeHealth {
    NvmeHealth {
        critical_warning: 0,
        temperature_c: Some(41),
        available_spare: 100,
        available_spare_threshold: 10,
        percentage_used: 12,
        data_units_read: 61_800_000,
        data_units_written: 94_300_000,
        host_read_commands: 812_000_000,
        host_write_commands: 523_000_000,
        controller_busy_minutes: 4_210,
        power_cycles: 512,
        power_on_hours: 9_140,
        unsafe_shutdowns: 23,
        media_errors: 0,
        error_log_entries: 0,
        warning_temp_minutes: 0,
        critical_temp_minutes: 0,
        sensors_c: vec![41, 44],
    }
}

fn a(id: u8, flags: u16, cur: u8, worst: u8, raw: u64, thr: u8) -> AtaAttribute {
    AtaAttribute::new(id, flags, cur, worst, raw, Some(thr))
}

fn sata_ssd() -> AtaSmart {
    AtaSmart {
        revision: 0x10,
        attributes: vec![
            a(5, 0x33, 100, 100, 0, 10),
            a(9, 0x32, 97, 97, 11_820, 0),
            a(12, 0x32, 99, 99, 1_204, 0),
            a(171, 0x32, 100, 100, 0, 0),
            a(194, 0x22, 64, 52, 36 | (20 << 16) | (56 << 32), 0),
            a(197, 0x32, 100, 100, 3, 0),
            a(198, 0x30, 100, 100, 0, 0),
            a(231, 0x13, 94, 94, 0, 0),
            a(241, 0x30, 100, 100, 18_400_000_000, 0),
            a(242, 0x30, 100, 100, 41_700_000_000, 0),
        ],
    }
}

fn hdd() -> AtaSmart {
    AtaSmart {
        revision: 0x10,
        attributes: vec![
            a(1, 0x2F, 200, 200, 0, 51),
            a(5, 0x33, 188, 188, 96, 140),
            a(9, 0x32, 71, 71, 21_300, 0),
            a(12, 0x32, 100, 100, 880, 0),
            a(194, 0x22, 118, 98, 32 | (18 << 16) | (49 << 32), 0),
            a(196, 0x32, 188, 188, 12, 0),
            a(197, 0x32, 200, 200, 8, 0),
            a(198, 0x30, 200, 200, 2, 0),
        ],
    }
}

/// The invented disks. Used by `--demo` and by the screenshot tour.
pub fn disks(t: Thresholds) -> Vec<DiskView> {
    let make = |info: DiskInfo, report: SmartReport| {
        let assessment = assess::assess_report(&report, t);
        DiskView {
            info,
            report,
            assessment,
        }
    };
    vec![
        make(
            info(0, "Demo NVMe SSD 1TB", BusType::Nvme, 1000, false, &["C:"]),
            SmartReport::Ok {
                method: "NVMe log page 02h (IOCTL_STORAGE_QUERY_PROPERTY)".into(),
                data: SmartData::Nvme(nvme_page()),
            },
        ),
        make(
            info(1, "Demo SATA SSD 500GB", BusType::Sata, 500, false, &["D:"]),
            SmartReport::Ok {
                method: "ATA SMART (IOCTL_ATA_PASS_THROUGH)".into(),
                data: SmartData::Ata(sata_ssd()),
            },
        ),
        make(
            info(2, "Demo HDD 2TB", BusType::Sata, 2000, true, &["E:", "F:"]),
            SmartReport::Ok {
                method: "ATA SMART (IOCTL_ATA_PASS_THROUGH)".into(),
                data: SmartData::Ata(hdd()),
            },
        ),
        make(
            info(3, "Demo Portable USB 1TB", BusType::Usb, 1000, true, &["G:"]),
            SmartReport::unavailable(UnavailableReason::UsbBridge, "SCSI status 2, sense key 0x5, ASC 0x20"),
        ),
    ]
}

/// Invented 45-day histories for the three disks with SMART data (deterministic; used by the History tab in `--demo`).
pub fn history() -> Vec<(DiskMeta, Vec<Sample>)> {
    let end: u64 = 1_760_000_000;
    let make = |number: u32, gb_per_day: f64, base_gb: f64, temp: f64, used: Option<(u8, u8)>, tbw: Option<f64>| {
        let d = disks(Thresholds::default())
            .into_iter()
            .find(|v| v.info.number == number)
            .expect("demo disk")
            .info;
        let steps = 180u64; // every 6 hours
        let samples = (0..steps)
            .map(|i| {
                let t = end - (steps - 1 - i) * 6 * 3600;
                let days = i as f64 / 4.0;
                let wave = (days * 2.3).sin() * 4.0 + (days * 0.35).sin() * 2.0;
                Sample {
                    t,
                    temp_c: Some((temp + wave).round() as i32),
                    pct_used: used.map(|(a, b)| if i < 120 { a } else { b }),
                    written: Some(((base_gb + gb_per_day * days) * 1e9) as u64),
                    ..Sample::default()
                }
            })
            .collect();
        let meta = DiskMeta {
            key: d.key(),
            model: d.model.clone(),
            serial_masked: d.serial_masked(),
            bus: d.bus.label().to_owned(),
            size_bytes: d.size_bytes,
            tbw_rated_tb: tbw,
            first_seen: end - 45 * 86_400,
            last_seen: end,
        };
        (meta, samples)
    };
    vec![
        make(0, 21.0, 48_300.0, 41.0, Some((11, 12)), Some(600.0)),
        make(1, 7.5, 9_400.0, 36.0, None, None),
        make(2, 2.0, 61_000.0, 33.0, None, None),
    ]
}

/// An invented read-test result: a 24 GB range of the demo HDD with two slow zones and one unreadable sector pair.
pub fn scan_done() -> crate::scan_view::Done {
    use diskpulse_core::ntfs::{FileHit, Kind, ZoneMap};
    use diskpulse_core::scanmap::{Outcome, Piece};
    use diskpulse_core::surface::{BadRange, Block, MIB, Params, Report, bucket};
    let disk = disks(Thresholds::default()).into_iter().nth(2).expect("demo hdd").info;
    let n = 3000u64;
    let blocks: Vec<Block> = (0..n)
        .map(|i| {
            let speed = 188.0 - 46.0 * (i as f64 / n as f64) + ((i * 37 % 11) as f64 - 5.0) * 0.8; // MB/s, falling towards the inner tracks
            let mut micros = (8.0 * MIB as f64 / speed) as u64;
            let mut error = None;
            if (640..643).contains(&i) {
                micros = 420_000;
            }
            if (2050..2052).contains(&i) {
                micros = 910_000;
            }
            if i == 1340 {
                micros = 1_850_000;
                error = Some(23);
            }
            Block {
                offset: i * 8 * MIB,
                len: (8 * MIB) as u32,
                micros,
                error,
            }
        })
        .collect();
    let mut histogram = [0u64; 11];
    for b in &blocks {
        histogram[bucket(b.micros as f64 / 1000.0)] += 1;
    }
    let bad_start = 1340 * 8 * MIB + 3 * 4096;
    let report = Report {
        params: Params {
            block_bytes: 8 * MIB,
            len: n * 8 * MIB,
            ..Params::default()
        },
        device_size: disk.size_bytes,
        bytes_read: (n - 1) * 8 * MIB,
        micros_total: blocks.iter().map(|b| b.micros).sum(),
        blocks,
        bad: vec![BadRange {
            start: bad_start,
            end: bad_start + 8192,
            error: 23,
        }],
        histogram,
        cancelled: false,
        too_many_errors: false,
    };
    let file = |path: &str, attr: &str, bytes: u64| FileHit {
        path: path.into(),
        kind: Kind::File,
        record: 4000,
        attr: attr.into(),
        bytes,
    };
    let mapped = |files: Vec<FileHit>, free: u64| Outcome::Mapped {
        volume: "E:".into(),
        zone: ZoneMap {
            total_bytes: 0,
            free_bytes: free,
            other_allocated_bytes: 0,
            files,
        },
    };
    let zones = report.zones();
    let zone_pieces = vec![
        Piece {
            start: zones[0].start,
            end: zones[0].end,
            outcome: mapped(
                vec![
                    file("\\\\Video\\\\holiday-2023.mkv", "data", 20 * MIB),
                    file("\\\\Video\\\\clip-03.mp4", "data", 4 * MIB),
                ],
                0,
            ),
        },
        Piece {
            start: zones[1].start,
            end: zones[1].end,
            outcome: mapped(vec![file("\\\\Downloads\\\\setup-demo.iso", "data", 8 * MIB)], 0),
        },
        Piece {
            start: zones[2].start,
            end: zones[2].end,
            outcome: mapped(
                vec![
                    file("\\\\Backup\\\\photos-archive.zip", "data", 8 * MIB),
                    file("\\\\$LogFile", "data", 4 * MIB),
                ],
                4 * MIB,
            ),
        },
    ];
    let bad_pieces = vec![Piece {
        start: bad_start,
        end: bad_start + 8192,
        outcome: mapped(vec![file("\\\\Downloads\\\\setup-demo.iso", "data", 8192)], 0),
    }];
    crate::scan_view::Done {
        disk,
        report,
        zone_pieces,
        bad_pieces,
        mapped: true,
    }
}

impl App {
    fn demo_prepare(&mut self, ctx: &egui::Context, step: &str) {
        self.tab = Tab::Disks;
        let select = |app: &mut App, n: u32| app.selected = Some(n);
        match step {
            "nvme" => select(self, 0),
            "sata" => select(self, 1),
            "hdd" => select(self, 2),
            "usb" => select(self, 3),
            "history" => {
                self.tab = Tab::History;
                self.hist.key = None;
                self.hist.loaded = None;
            }
            "scan" => {
                self.tab = Tab::Scan;
                self.scan.disk = Some(2);
                self.scan.done = Some(scan_done());
            }
            "settings" => self.tab = Tab::Settings,
            _ => {}
        }
        ctx.request_repaint();
    }

    /// Drives the screenshot tour (no-op unless started with `--screenshots <dir>`).
    pub fn demo_step(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.demo.take() else { return };
        if let Some(dir) = d.dir.clone() {
            ctx.request_repaint();
            match STEPS.get(d.step) {
                None => ctx.send_viewport_cmd(ViewportCommand::Close),
                Some(&step) => {
                    if !d.prepared && self.loaded_once {
                        self.demo_prepare(ctx, step);
                        d.prepared = true;
                        d.since = Instant::now();
                    }
                    let shot = ctx.input(|i| {
                        i.events.iter().find_map(|e| match e {
                            Event::Screenshot { image, .. } => Some(image.clone()),
                            _ => None,
                        })
                    });
                    if let Some(img) = shot {
                        let file = dir.join(format!("{:02}-{step}.png", d.step + 1));
                        match save_png(&file, &img) {
                            Ok(()) => println!("saved {}", file.display()),
                            Err(e) => eprintln!("cannot save {}: {e}", file.display()),
                        }
                        d.step += 1;
                        d.prepared = false;
                        d.asked = false;
                    } else if d.prepared && !d.asked && d.since.elapsed() > SETTLE {
                        ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
                        d.asked = true;
                    }
                }
            }
        }
        self.demo = Some(d);
    }
}

fn save_png(path: &Path, img: &egui::ColorImage) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.width() as u32, img.height() as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(img.as_raw()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_disks_cover_good_warning_bad_and_unavailable() {
        use diskpulse_core::assess::Level;
        let v = disks(Thresholds::default());
        let levels: Vec<Option<Level>> = v.iter().map(|d| d.assessment.as_ref().map(|a| a.level)).collect();
        assert_eq!(
            levels,
            vec![Some(Level::Good), Some(Level::Warning), Some(Level::Bad), None]
        );
    }

    #[test]
    fn demo_serials_are_invented() {
        for d in disks(Thresholds::default()) {
            assert!(d.info.serial.starts_with("DEMO-"));
        }
    }
}
