//! Demo mode: `DiskPulse.exe --demo` shows invented disks (nothing real is read or written).
//! `DiskPulse.exe --screenshots <dir>` additionally walks through the screens and saves a PNG of each; that is how the
//! screenshots in the README are made, without a single real model name or serial number in them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use diskpulse_core::assess::{self, Thresholds};
use diskpulse_core::ata::{AtaAttribute, AtaSmart};
use diskpulse_core::disks::{BusType, DiskInfo};
use diskpulse_core::nvme::NvmeHealth;
use diskpulse_core::smart::{SmartData, SmartReport, UnavailableReason};
use eframe::egui::{self, Event, ViewportCommand};

use crate::{App, DiskView, Tab};

/// Time to let layout and animations settle before a screenshot.
const SETTLE: Duration = Duration::from_millis(900);

/// What the screenshot tour does, in order.
const STEPS: &[&str] = &["nvme", "sata", "hdd", "usb", "settings"];

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

impl App {
    fn demo_prepare(&mut self, ctx: &egui::Context, step: &str) {
        self.tab = Tab::Disks;
        let select = |app: &mut App, n: u32| app.selected = Some(n);
        match step {
            "nvme" => select(self, 0),
            "sata" => select(self, 1),
            "hdd" => select(self, 2),
            "usb" => select(self, 3),
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
