//! The "Read test" tab: a read-only surface pass with a live speed chart, then a latency histogram, slow zones,
//! unreadable blocks and the files that own them, with CSV / HTML export. The test cannot write (see `surface.rs`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use diskpulse_core::disks::{DiskInfo, volumes};
use diskpulse_core::ntfs::Kind;
use diskpulse_core::report::{self, Input, Row};
use diskpulse_core::scanmap::{self, Outcome, Piece};
use diskpulse_core::surface::{self, MIB, Params, ReadOnlyDisk, Report};
use diskpulse_core::units::{human_bytes, now_unix};
use eframe::egui::{self, RichText, Ui};
use egui_plot::{Line, Plot, PlotPoints};

use crate::{App, settings, theme, winint};

/// Progress shared between the worker thread and the window.
#[derive(Default)]
pub struct Live {
    pub done: u64,
    pub total: u64,
    pub mbps: f64,
    pub blocks: u64,
    /// `(offset in GB, MB/s)`, decimated so it never exceeds a few thousand points.
    pub points: Vec<(f64, f64)>,
    stride: u64,
}

impl Live {
    fn push(&mut self, offset: u64, mbps: f64) {
        self.stride = self.stride.max(1);
        if self.blocks.is_multiple_of(self.stride) {
            self.points.push((offset as f64 / 1e9, mbps));
            if self.points.len() > 4000 {
                self.points = self.points.iter().step_by(2).copied().collect();
                self.stride *= 2;
            }
        }
        self.blocks += 1;
    }
}

pub struct Done {
    pub disk: DiskInfo,
    pub report: Report,
    pub zone_pieces: Vec<Piece>,
    pub bad_pieces: Vec<Piece>,
    pub mapped: bool,
}

pub enum Msg {
    Finished(Result<Report, String>),
    Mapped(Vec<Piece>, Vec<Piece>),
}

pub struct ScanState {
    pub disk: Option<u32>,
    pub block_mib: u64,
    /// 0 = the whole disk.
    pub limit_gb: u32,
    pub slow_ms: f64,
    pub live: Arc<Mutex<Live>>,
    pub cancel: Arc<AtomicBool>,
    pub rx: Option<Receiver<Msg>>,
    pub running: Option<(DiskInfo, Instant)>,
    pub mapping: bool,
    pub done: Option<Done>,
    pub error: Option<String>,
    pub needs_admin: bool,
}

impl Default for ScanState {
    fn default() -> Self {
        ScanState {
            disk: None,
            block_mib: 4,
            limit_gb: 0,
            slow_ms: 150.0,
            live: Arc::default(),
            cancel: Arc::default(),
            rx: None,
            running: None,
            mapping: false,
            done: None,
            error: None,
            needs_admin: false,
        }
    }
}

impl App {
    fn start_scan(&mut self, ctx: &egui::Context, disk: DiskInfo) {
        let st = &mut self.scan;
        st.error = None;
        st.needs_admin = false;
        st.done = None;
        st.cancel.store(false, Ordering::Relaxed);
        *st.live.lock().unwrap_or_else(|e| e.into_inner()) = Live::default();
        let params = Params {
            block_bytes: st.block_mib * MIB,
            len: u64::from(st.limit_gb) * 1_000_000_000 / surface::SECTOR * surface::SECTOR,
            slow_ms: st.slow_ms,
            ..Params::default()
        };
        let (tx, rx) = channel();
        let (live, cancel, ctx2, d) = (st.live.clone(), st.cancel.clone(), ctx.clone(), disk.clone());
        st.rx = Some(rx);
        st.running = Some((disk, Instant::now()));
        crate::diag::info(format!(
            "read test started: {} MiB blocks, slow {} ms",
            st.block_mib, st.slow_ms
        ));
        std::thread::spawn(move || {
            let r = match ReadOnlyDisk::open(d.number, d.size_bytes) {
                Err(e) => Err(if e.is_access_denied() {
                    "access".to_owned()
                } else {
                    e.to_string()
                }),
                Ok(src) => surface::scan(&src, params, &cancel, &mut |p| {
                    let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
                    l.done = p.done_bytes;
                    l.total = p.total_bytes;
                    l.mbps = p.last_mbps;
                    let off = d.size_bytes.min(p.done_bytes);
                    l.push(off, p.last_mbps);
                    ctx2.request_repaint();
                })
                .map_err(|e| e.to_string()),
            };
            let _ = tx.send(Msg::Finished(r));
            ctx2.request_repaint();
        });
    }

    fn start_mapping(&mut self, ctx: &egui::Context) {
        let Some(done) = &self.scan.done else { return };
        let (disk, zones, bad) = (done.disk.number, done.report.zones(), done.report.bad.clone());
        let ranges: Vec<(u64, u64)> = zones
            .iter()
            .map(|z| (z.start, z.end))
            .chain(bad.iter().map(|b| (b.start, b.end)))
            .collect();
        let nz = zones.len();
        if ranges.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        self.scan.rx = Some(rx);
        self.scan.mapping = true;
        std::thread::spawn(move || {
            let pieces = scanmap::attribute(disk, &ranges, &volumes(), &|l| scanmap::open_volume(l), &mut |_, _| {});
            let zr = &ranges[..nz];
            let pick = |list: &[(u64, u64)]| {
                pieces
                    .iter()
                    .filter(|p| list.iter().any(|(a, b)| p.start >= *a && p.end <= *b))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let _ = tx.send(Msg::Mapped(pick(zr), pick(&ranges[nz..])));
            ctx2.request_repaint();
        });
    }

    pub fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.scan.rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.scan.rx = None;
        match msg {
            Msg::Finished(Ok(report)) => {
                let disk = self.scan.running.take().map(|(d, _)| d);
                let Some(disk) = disk else { return };
                crate::diag::info(format!(
                    "read test finished: {} blocks, {} slow zone(s), {} unreadable range(s), cancelled={}",
                    report.blocks.len(),
                    report.zones().len(),
                    report.bad.len(),
                    report.cancelled
                ));
                self.scan.done = Some(Done {
                    disk,
                    report,
                    zone_pieces: vec![],
                    bad_pieces: vec![],
                    mapped: false,
                });
                self.start_mapping(ctx);
            }
            Msg::Finished(Err(e)) => {
                self.scan.running = None;
                if e == "access" {
                    self.scan.needs_admin = true;
                } else {
                    crate::diag::error(format!("read test failed: {e}"));
                    self.scan.error = Some(e);
                }
            }
            Msg::Mapped(z, b) => {
                self.scan.mapping = false;
                if let Some(d) = &mut self.scan.done {
                    d.zone_pieces = z;
                    d.bad_pieces = b;
                    d.mapped = true;
                }
            }
        }
    }

    fn export(&mut self, html: bool) {
        let lang = self.lang();
        let Some(done) = &self.scan.done else { return };
        let rows = report::rows(&done.report, &done.zone_pieces, &done.bad_pieces);
        let serial = done.disk.serial_masked();
        let input = Input {
            lang,
            model: &done.disk.model,
            serial_masked: &serial,
            disk_size: done.disk.size_bytes,
            scan: &done.report,
            rows,
            generated: now_unix(),
        };
        let (ext, content) = if html {
            ("html", report::html(&input))
        } else {
            ("csv", report::csv(&input))
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("diskpulse-scan.{ext}"))
            .add_filter(ext, &[ext])
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, content) {
            Ok(()) => self.toast(self.tf("scan.ui.saved", &[path.display().to_string()])),
            Err(e) => self.toast(e.to_string()),
        }
    }

    pub fn scan_view(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll_scan(&ctx);
        if self.scan.disk.is_none() {
            self.scan.disk = self.disks.first().map(|d| d.info.number);
        }
        ui.add_space(6.0);
        let running = self.scan.running.is_some();
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(self.t("scan.ui.safe")).small().weak());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let cur = self
                    .disks
                    .iter()
                    .find(|d| Some(d.info.number) == self.scan.disk)
                    .map_or(String::new(), |d| format!("#{} {}", d.info.number, d.info.model));
                ui.add_enabled_ui(!running, |ui| {
                    egui::ComboBox::from_id_salt("scan_disk")
                        .selected_text(cur)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for d in &self.disks {
                                if ui
                                    .selectable_label(
                                        Some(d.info.number) == self.scan.disk,
                                        format!(
                                            "#{} {} ({})",
                                            d.info.number,
                                            d.info.model,
                                            human_bytes(self.lang(), u128::from(d.info.size_bytes))
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.scan.disk = Some(d.info.number);
                                }
                            }
                        });
                    ui.label(self.t("scan.block"));
                    egui::ComboBox::from_id_salt("scan_block")
                        .selected_text(format!("{} MiB", self.scan.block_mib))
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for m in [1u64, 2, 4, 8] {
                                ui.selectable_value(&mut self.scan.block_mib, m, format!("{m} MiB"));
                            }
                        });
                    ui.label(self.t("scan.ui.range"));
                    ui.add(
                        egui::DragValue::new(&mut self.scan.limit_gb)
                            .range(0..=100_000)
                            .suffix(" GB"),
                    );
                    ui.label(RichText::new(self.t("scan.ui.range_hint")).small().weak());
                    ui.label(self.t("scan.slow_threshold"));
                    ui.add(
                        egui::DragValue::new(&mut self.scan.slow_ms)
                            .range(10.0..=5000.0)
                            .suffix(" ms"),
                    );
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if running {
                    if ui.button(self.t("scan.ui.cancel")).clicked() {
                        self.scan.cancel.store(true, Ordering::Relaxed);
                    }
                } else if let Some(d) = self
                    .scan
                    .disk
                    .and_then(|n| self.disks.iter().find(|d| d.info.number == n))
                    .map(|d| d.info.clone())
                    && theme::primary_button(ui, self.t("scan.ui.start")).clicked()
                    && !settings::is_demo()
                {
                    self.start_scan(&ctx, d);
                }
                if !self.elevated && !settings::is_demo() && ui.button(self.t("btn.admin")).clicked() {
                    match winint::relaunch_elevated("--elevated") {
                        Ok(()) => self.toast(self.t("admin.launched")),
                        Err(_) => self.toast(self.t("admin.failed")),
                    }
                }
            });
            if self.scan.needs_admin {
                ui.colored_label(theme::WARN, self.t("scan.needs_admin_gui"));
            }
            if let Some(e) = &self.scan.error {
                ui.colored_label(theme::BAD, e);
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if running {
                self.live_view(ui);
            } else if self.scan.done.is_some() {
                self.result_view(ui);
            }
        });
    }

    fn live_view(&self, ui: &mut Ui) {
        let (l, started) = {
            let l = self.scan.live.lock().unwrap_or_else(|e| e.into_inner());
            (
                Live {
                    done: l.done,
                    total: l.total,
                    mbps: l.mbps,
                    blocks: l.blocks,
                    points: l.points.clone(),
                    stride: 0,
                },
                self.scan.running.as_ref().map(|r| r.1),
            )
        };
        let frac = if l.total > 0 {
            l.done as f32 / l.total as f32
        } else {
            0.0
        };
        let elapsed = started.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let eta = if frac > 0.01 {
            elapsed / f64::from(frac) * (1.0 - f64::from(frac))
        } else {
            0.0
        };
        ui.add(egui::ProgressBar::new(frac).show_percentage().text(format!(
            "{:.0}%  ·  {:.0} MB/s  ·  {} / {}  ·  ~{} min",
            frac * 100.0,
            l.mbps,
            human_bytes(self.lang(), u128::from(l.done)),
            human_bytes(self.lang(), u128::from(l.total)),
            (eta / 60.0).ceil() as u64
        )));
        ui.add_space(6.0);
        self.speed_plot(ui, &l.points, "live_speed");
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn speed_plot(&self, ui: &mut Ui, pts: &[(f64, f64)], id: &str) {
        ui.label(RichText::new(self.t("scan.speed")).strong());
        let pts: Vec<[f64; 2]> = pts.iter().map(|(x, y)| [*x, *y]).collect();
        Plot::new(id)
            .height(190.0)
            .allow_scroll(false)
            .include_y(0.0)
            .show(ui, |p| {
                p.line(Line::new("MB/s", PlotPoints::from(pts)).color(theme::ACCENT).width(1.6));
            });
    }

    fn result_view(&mut self, ui: &mut Ui) {
        let lang = self.lang();
        let Some(done) = &self.scan.done else { return };
        let rep = &done.report;
        let series: Vec<(f64, f64)> = surface::speed_series(&rep.blocks, 1500)
            .into_iter()
            .map(|(o, v)| (o as f64 / 1e9, v))
            .collect();
        let zones = rep.zones();
        let summary = format!(
            "{}: {} / {}  ·  {} {:.0} MB/s  ·  {} {}",
            self.t("scan.tested"),
            human_bytes(lang, u128::from(rep.bytes_read)),
            human_bytes(lang, u128::from(rep.device_size)),
            self.t("scan.avg"),
            rep.avg_mbps(),
            self.t("scan.blocks"),
            rep.blocks.len()
        );
        let (cancelled, too_many, mapped, mapping) =
            (rep.cancelled, rep.too_many_errors, done.mapped, self.scan.mapping);
        let mut export: Option<bool> = None;
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(summary).strong());
            if cancelled {
                ui.colored_label(theme::WARN, self.t("scan.cancelled"));
            }
            if too_many {
                ui.colored_label(theme::BAD, self.t("scan.too_many"));
            }
            let (nz, nb) = (zones.len(), rep.bad.len());
            ui.horizontal(|ui| {
                theme::dot(
                    ui,
                    if nb > 0 {
                        theme::BAD
                    } else if nz > 0 {
                        theme::WARN
                    } else {
                        theme::GOOD
                    },
                    6.0,
                );
                ui.label(self.tf("scan.ui.found", &[nz.to_string(), nb.to_string()]));
                if mapping {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new(self.t("scan.ui.mapping")).weak());
                }
            });
            ui.horizontal(|ui| {
                if ui.button(self.t("scan.ui.export_csv")).clicked() {
                    export = Some(false);
                }
                if ui.button(self.t("scan.ui.export_html")).clicked() {
                    export = Some(true);
                }
            });
            ui.label(RichText::new(self.t("scan.disclaimer")).small().weak());
        });
        if let Some(html) = export {
            self.export(html);
        }
        ui.add_space(8.0);
        self.speed_plot(ui, &series, "result_speed");
        ui.add_space(8.0);
        let Some(done) = &self.scan.done else { return };
        self.histogram(ui, &done.report.histogram);
        ui.add_space(8.0);
        let rows: Vec<Row> = report::rows(&done.report, &done.zone_pieces, &done.bad_pieces);
        for (bad, key, none) in [
            (false, "scan.zones", "scan.none_zones"),
            (true, "scan.bad", "scan.none_bad"),
        ] {
            ui.label(RichText::new(self.t(key)).strong().size(16.0));
            let mut any = false;
            for (i, r) in rows.iter().enumerate().filter(|(_, r)| r.unreadable == bad) {
                any = true;
                let head = format!(
                    "{:.2} – {:.2} GB  ·  {}{}",
                    r.start as f64 / 1e9,
                    r.end as f64 / 1e9,
                    human_bytes(lang, u128::from(r.end - r.start)),
                    if bad {
                        String::new()
                    } else {
                        format!("  ·  max {:.0} ms, avg {:.0} ms", r.max_ms, r.mean_ms)
                    }
                );
                egui::CollapsingHeader::new(RichText::new(head).color(if bad { theme::BAD } else { theme::WARN }))
                    .id_salt((key, i))
                    .default_open(i < 3)
                    .show(ui, |ui| {
                        if !mapped && !r.unreadable && r.pieces.is_empty() {
                            ui.label(RichText::new(self.t("scan.ui.mapping")).weak());
                        }
                        for p in &r.pieces {
                            match &p.outcome {
                                Outcome::Mapped { volume, zone } => {
                                    for f in zone.files.iter().take(40) {
                                        let tag = match f.kind {
                                            Kind::File => "",
                                            Kind::Directory => "▸ ",
                                            Kind::Metadata => "NTFS ",
                                            Kind::Unknown => "? ",
                                        };
                                        ui.label(format!(
                                            "{tag}{volume}{}  —  {} ({})",
                                            f.path,
                                            f.attr,
                                            human_bytes(lang, u128::from(f.bytes))
                                        ));
                                    }
                                    if zone.files.len() > 40 {
                                        ui.label(RichText::new(format!("… +{}", zone.files.len() - 40)).weak());
                                    }
                                    if zone.free_bytes > 0 {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} ({})",
                                                self.t("scan.free"),
                                                human_bytes(lang, u128::from(zone.free_bytes))
                                            ))
                                            .weak(),
                                        );
                                    }
                                }
                                o => {
                                    ui.label(RichText::new(report::outcome_note(lang, o)).weak());
                                }
                            }
                        }
                    });
            }
            if !any {
                ui.label(RichText::new(self.t(none)).weak());
            }
            ui.add_space(6.0);
        }
    }

    fn histogram(&self, ui: &mut Ui, h: &[u64; 11]) {
        ui.label(RichText::new(self.t("scan.histogram")).strong());
        let top = h.iter().copied().max().unwrap_or(1).max(1);
        let e = surface::HISTOGRAM_EDGES_MS;
        egui::Grid::new("hist_grid")
            .num_columns(3)
            .spacing([12.0, 3.0])
            .show(ui, |ui| {
                for (i, c) in h.iter().enumerate() {
                    let label = match i {
                        0 => format!("< {} ms", e[0]),
                        10 => format!("≥ {} ms", e[9]),
                        n => format!("{}–{} ms", e[n - 1], e[n]),
                    };
                    ui.label(RichText::new(label).weak());
                    ui.label(c.to_string());
                    theme::bar(
                        ui,
                        *c as f32 / top as f32,
                        if i >= 7 { theme::WARN } else { theme::ACCENT },
                        260.0,
                    );
                    ui.end_row();
                }
            });
    }
}
