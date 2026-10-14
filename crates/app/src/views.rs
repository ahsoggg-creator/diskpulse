//! The window: toolbar, disk list, disk details (NVMe / ATA), settings.

use diskpulse_core::assess::{Basis, Level, RULES};
use diskpulse_core::ata::{AtaSmart, Decoded};
use diskpulse_core::disks::BusType;
use diskpulse_core::i18n::Lang;
use diskpulse_core::nvme::{NvmeHealth, critical_warning_flags};
use diskpulse_core::smart::{SmartData, SmartReport, UnavailableReason};
use diskpulse_core::units::{human_bytes, human_hours};
use eframe::egui::{self, Align, Layout, RichText, Ui, vec2};

use crate::{App, DiskView, Tab, theme, update, winint};

fn reason_key(r: UnavailableReason) -> &'static str {
    match r {
        UnavailableReason::NeedsAdmin => "unavail.needs_admin",
        UnavailableReason::UsbBridge => "unavail.usb_bridge",
        UnavailableReason::Unsupported => "unavail.unsupported",
        UnavailableReason::Invalid => "unavail.invalid",
    }
}

pub fn level_key(l: Option<Level>) -> &'static str {
    match l {
        Some(Level::Good) => "level.good",
        Some(Level::Warning) => "level.warning",
        Some(Level::Bad) => "level.bad",
        None => "level.unknown",
    }
}

fn basis_key(b: Basis) -> &'static str {
    match b {
        Basis::Spec => "basis.spec",
        Basis::Vendor => "basis.vendor",
        Basis::Heuristic => "basis.heuristic",
    }
}

fn bus_icon(b: BusType) -> &'static str {
    match b {
        BusType::Nvme => "M.2",
        BusType::Usb => "USB",
        BusType::Virtual => "VM",
        _ => "HDD",
    }
}

impl App {
    pub fn toolbar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if let Some(icon) = &self.icon {
                ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(22.0, 22.0)));
            }
            ui.label(RichText::new("DiskPulse").strong().size(17.0));
            ui.label(RichText::new(env!("CARGO_PKG_VERSION")).weak());
            ui.add_space(14.0);
            for (tab, key) in [
                (Tab::Disks, "tab.disks"),
                (Tab::History, "tab.history"),
                (Tab::Settings, "tab.settings"),
            ] {
                if ui
                    .selectable_label(self.tab == tab, RichText::new(self.t(key)).size(14.5))
                    .clicked()
                {
                    self.tab = tab;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.update_button(ui, &ctx);
                if !self.elevated
                    && !crate::settings::is_demo()
                    && ui
                        .button(self.t("btn.admin"))
                        .on_hover_text(self.t("btn.admin.hint"))
                        .clicked()
                {
                    match winint::relaunch_elevated("--elevated") {
                        Ok(()) => self.toast(self.t("admin.launched")),
                        Err(_) => self.toast(self.t("admin.failed")),
                    }
                }
                let busy = self.loading();
                if ui
                    .add_enabled(!busy, egui::Button::new(self.t("btn.refresh")))
                    .clicked()
                {
                    self.refresh(&ctx);
                }
                if busy {
                    ui.add(egui::Spinner::new().size(16.0));
                }
            });
        });
        ui.add_space(4.0);
    }

    fn update_button(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let state = self.update.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match state {
            update::State::Available { version, .. } => {
                if theme::primary_button(ui, &self.tf("update.available_btn", &[version])).clicked() {
                    self.check_updates_now(ctx, true);
                }
            }
            update::State::Downloading { done, total, .. } => {
                let pct = (done * 100).checked_div(total).unwrap_or(0);
                ui.label(self.tf("update.downloading", &[pct.to_string()]));
            }
            update::State::Ready { version } => {
                if theme::primary_button(ui, &self.tf("update.restart_btn", &[version])).clicked()
                    && update::restart().is_ok()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
    }

    pub fn footer(&mut self, ui: &mut Ui) {
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let (txt, color) = if crate::settings::is_demo() {
                (self.t("footer.demo"), theme::WARN)
            } else if self.elevated {
                (self.t("footer.elevated"), theme::GOOD)
            } else {
                (self.t("footer.not_elevated"), theme::NONE)
            };
            theme::dot(ui, color, 4.0);
            ui.label(RichText::new(txt).small().weak());
            ui.label(RichText::new("·").weak());
            ui.label(RichText::new(self.t("footer.readonly")).small().weak());
        });
        ui.add_space(3.0);
        if let Some(p) = self.crash_notice.clone() {
            let msg = self.tf("crash.notice", &[p.display().to_string()]);
            ui.horizontal(|ui| {
                ui.colored_label(theme::BAD, msg);
                if ui.small_button("×").clicked() {
                    self.crash_notice = None;
                }
            });
        }
    }

    pub fn toast_overlay(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let text = self
            .toasts
            .iter()
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-16.0, -40.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(text);
                });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    pub fn disk_list(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        if self.disks.is_empty() {
            if self.loading() || !self.loaded_once {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(self.t("disks.loading"));
                });
            } else {
                ui.label(self.t("disks.none"));
            }
            return;
        }
        let lang = self.lang();
        let mut pick = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for d in &self.disks {
                let level = d.assessment.as_ref().map(|a| a.level);
                let selected = self.selected == Some(d.info.number);
                let frame = theme::card(ui).fill(if selected {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().window_fill
                });
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            theme::dot(ui, theme::level_color(level), 6.0);
                            ui.label(RichText::new(&d.info.model).strong());
                        });
                        ui.horizontal(|ui| {
                            theme::badge(ui, d.info.bus.label(), theme::ACCENT);
                            ui.label(RichText::new(human_bytes(lang, u128::from(d.info.size_bytes))).weak());
                            if !d.info.volumes.is_empty() {
                                ui.label(RichText::new(d.info.volumes.join(" ")).weak());
                            }
                        });
                        let (label, temp) = (self.t(level_key(level)), disk_temperature(d));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(label).color(theme::level_color(level)).strong());
                            if let Some(t) = temp {
                                ui.label(RichText::new(format!("{t} °C")).weak());
                            }
                        });
                    })
                    .response
                    .interact(egui::Sense::click());
                if resp.clicked() {
                    pick = Some(d.info.number);
                }
                ui.add_space(2.0);
            }
        });
        if pick.is_some() {
            self.selected = pick;
        }
    }

    pub fn disk_detail(&mut self, ui: &mut Ui) {
        let Some(d) = self.selected_disk().cloned() else {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(self.t("disks.pick")).weak()));
            return;
        };
        let lang = self.lang();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(bus_icon(d.info.bus)).size(22.0).weak());
                ui.label(RichText::new(&d.info.model).size(21.0).strong());
            });
            ui.horizontal_wrapped(|ui| {
                theme::badge(ui, d.info.bus.label(), theme::ACCENT);
                ui.label(human_bytes(lang, u128::from(d.info.size_bytes)));
                if let Some(rot) = d.info.rotational {
                    ui.label(RichText::new(if rot { "HDD" } else { "SSD" }).weak());
                }
                if !d.info.firmware.is_empty() {
                    ui.label(RichText::new(format!("{} {}", self.t("disk.firmware"), d.info.firmware)).weak());
                }
                if !d.info.volumes.is_empty() {
                    ui.label(RichText::new(format!("{} {}", self.t("disk.volumes"), d.info.volumes.join(" "))).weak());
                }
                let serial = if self.show_serial {
                    d.info.serial.clone()
                } else {
                    d.info.serial_masked()
                };
                ui.label(RichText::new(format!("{} {serial}", self.t("disk.serial"))).weak());
                let toggle = self.t(if self.show_serial {
                    "disk.serial.hide"
                } else {
                    "disk.serial.show"
                });
                if ui.small_button(toggle).clicked() {
                    self.show_serial = !self.show_serial;
                }
            });
            ui.add_space(8.0);
            match &d.report {
                SmartReport::Unavailable { reason, detail } => self.unavailable_card(ui, *reason, detail),
                SmartReport::Ok { method, data } => {
                    self.verdict_card(ui, &d);
                    ui.add_space(8.0);
                    match data {
                        SmartData::Nvme(h) => self.nvme_card(ui, h),
                        SmartData::Ata(s) => self.ata_card(ui, s),
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{}: {method}", self.t("disk.method")))
                            .small()
                            .weak(),
                    );
                }
            }
        });
    }

    fn unavailable_card(&mut self, ui: &mut Ui, reason: UnavailableReason, detail: &str) {
        let key = reason_key(reason);
        let (title, hint) = (self.t(key), self.t(&format!("{key}.hint")));
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::dot(ui, theme::NONE, 7.0);
                ui.label(RichText::new(title).size(17.0).strong());
            });
            ui.add_space(4.0);
            ui.label(hint);
            if reason == UnavailableReason::NeedsAdmin && !self.elevated && !crate::settings::is_demo() {
                ui.add_space(6.0);
                if theme::primary_button(ui, self.t("btn.admin")).clicked() {
                    match winint::relaunch_elevated("--elevated") {
                        Ok(()) => self.toast(self.t("admin.launched")),
                        Err(_) => self.toast(self.t("admin.failed")),
                    }
                }
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("{}: {detail}", self.t("unavail.detail")))
                    .small()
                    .weak(),
            );
            ui.label(RichText::new(self.t("assess.none_for_unreadable")).small().weak());
        });
    }

    fn verdict_card(&mut self, ui: &mut Ui, d: &DiskView) {
        let Some(a) = &d.assessment else { return };
        let color = theme::level_color(Some(a.level));
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                theme::dot(ui, color, 8.0);
                ui.label(
                    RichText::new(self.t(level_key(Some(a.level))))
                        .size(20.0)
                        .strong()
                        .color(color),
                );
                ui.label(RichText::new(self.t("assess.title")).weak());
            });
            ui.add_space(2.0);
            if a.reasons.is_empty() {
                ui.label(self.t("assess.all_ok"));
            }
            for r in &a.reasons {
                ui.horizontal_wrapped(|ui| {
                    theme::badge(ui, &r.rule, theme::level_color(Some(r.level)));
                    ui.label(self.tf(&format!("rule.{}", r.rule), &r.args));
                    if let Some(info) = RULES.iter().find(|i| i.id == r.rule) {
                        ui.label(
                            RichText::new(format!("({})", self.t(basis_key(info.basis))))
                                .small()
                                .weak(),
                        );
                    }
                });
            }
            ui.add_space(4.0);
            ui.label(RichText::new(self.t("assess.no_percent")).small().weak());
            let label = self.t(if self.rules_open { "rules.hide" } else { "rules.show" });
            if ui.small_button(label).clicked() {
                self.rules_open = !self.rules_open;
            }
            if self.rules_open {
                for info in RULES.iter().filter(|i| {
                    i.id.starts_with(if matches!(d.report.data(), Some(SmartData::Nvme(_))) {
                        'N'
                    } else {
                        'A'
                    })
                }) {
                    ui.horizontal_wrapped(|ui| {
                        theme::badge(ui, info.id, theme::level_color(Some(info.level)));
                        ui.label(
                            RichText::new(
                                self.t(&format!("rule.{}", info.id))
                                    .replace("{0}", "…")
                                    .replace("{1}", "…")
                                    .replace("{2}", "…"),
                            )
                            .small(),
                        );
                        ui.label(
                            RichText::new(format!("({})", self.t(basis_key(info.basis))))
                                .small()
                                .weak(),
                        );
                    });
                }
            }
        });
    }

    fn nvme_card(&self, ui: &mut Ui, h: &NvmeHealth) {
        let lang = self.lang();
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let used_color = if h.percentage_used >= 80 {
                theme::WARN
            } else {
                theme::ACCENT
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}: {}%", self.t("nv.used"), h.percentage_used)).strong());
                theme::bar(ui, f32::from(h.percentage_used.min(100)) / 100.0, used_color, 220.0);
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}: {}%", self.t("nv.spare"), h.available_spare)).strong());
                theme::bar(ui, f32::from(h.available_spare.min(100)) / 100.0, theme::GOOD, 220.0);
                ui.label(RichText::new(format!("{} {}%", self.t("nv.spare_thr"), h.available_spare_threshold)).weak());
            });
            ui.add_space(6.0);
            egui::Grid::new("nvme_grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    let mut row = |key: &str, v: String| {
                        ui.label(RichText::new(self.t(key)).weak());
                        ui.label(v);
                        ui.end_row();
                    };
                    row(
                        "nv.temperature",
                        h.temperature_c.map_or("—".into(), |t| format!("{t} °C")),
                    );
                    if h.sensors_c.len() > 1 {
                        row(
                            "nv.sensors",
                            h.sensors_c
                                .iter()
                                .map(|t| format!("{t} °C"))
                                .collect::<Vec<_>>()
                                .join(", "),
                        );
                    }
                    row("nv.written", human_bytes(lang, h.bytes_written()));
                    row("nv.read", human_bytes(lang, h.bytes_read()));
                    row("nv.poh", human_hours(lang, h.power_on_hours));
                    row("nv.cycles", h.power_cycles.to_string());
                    row("nv.unsafe", h.unsafe_shutdowns.to_string());
                    row("nv.media", h.media_errors.to_string());
                    row("nv.errlog", h.error_log_entries.to_string());
                    row(
                        "nv.busy",
                        format!("{} {}", h.controller_busy_minutes, self.t("unit.min")),
                    );
                    row(
                        "nv.warn_t",
                        format!("{} {}", h.warning_temp_minutes, self.t("unit.min")),
                    );
                    row(
                        "nv.crit_t",
                        format!("{} {}", h.critical_temp_minutes, self.t("unit.min")),
                    );
                    let cw = critical_warning_flags(h.critical_warning);
                    let cw_text = if cw.is_empty() {
                        self.t("nv.crit.none").to_owned()
                    } else {
                        cw.iter()
                            .map(|f| self.t(&format!("cw.{f}")))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    row("nv.crit", cw_text);
                });
        });
    }

    fn ata_card(&self, ui: &mut Ui, s: &AtaSmart) {
        let lang = self.lang();
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("ata_grid")
                .num_columns(7)
                .spacing([14.0, 5.0])
                .striped(true)
                .show(ui, |ui| {
                    for key in [
                        "ata.id",
                        "ata.name",
                        "ata.cur",
                        "ata.worst",
                        "ata.thr",
                        "ata.raw",
                        "ata.decoded",
                    ] {
                        ui.label(RichText::new(self.t(key)).weak().small());
                    }
                    ui.end_row();
                    for a in &s.attributes {
                        let bad = a.failing_now();
                        let col = if bad {
                            theme::BAD
                        } else if a.failed_in_past() {
                            theme::WARN
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(RichText::new(a.id.to_string()).color(col));
                        if a.name.is_empty() {
                            ui.label(
                                RichText::new(format!("{} 0x{:02X}", self.t("ata.unknown"), a.id))
                                    .weak()
                                    .italics(),
                            );
                        } else {
                            let r = ui.label(RichText::new(&a.name).color(col));
                            let key = format!("attr.{}", a.id);
                            let help = self.t(&key);
                            if help != diskpulse_core::i18n::MISSING {
                                r.on_hover_text(help);
                            }
                        }
                        ui.label(RichText::new(a.current.to_string()).color(col));
                        ui.label(a.worst.to_string());
                        ui.label(a.threshold.map_or("—".to_owned(), |t| t.to_string()));
                        ui.label(RichText::new(a.raw.to_string()).monospace());
                        let decoded = match a.decode() {
                            Decoded::Celsius(t) => format!("{t} °C"),
                            Decoded::Hours(h) => human_hours(lang, u128::from(h)),
                            Decoded::Lbas(l) => self.tf("ata.lbas_est", &[human_bytes(lang, u128::from(l) * 512)]),
                            Decoded::Count(c) => c.to_string(),
                            Decoded::Raw(_) => String::new(),
                        };
                        ui.horizontal(|ui| {
                            ui.label(decoded);
                            if a.vendor_dependent {
                                ui.label(RichText::new(format!("({})", self.t("ata.vendor_dep"))).small().weak());
                            }
                        });
                        ui.end_row();
                    }
                });
            if s.attributes.iter().all(|a| a.threshold.is_none()) {
                ui.add_space(4.0);
                ui.label(RichText::new(self.t("ata.thr_missing")).small().weak());
            }
        });
    }

    pub fn settings_view(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(640.0);
            ui.add_space(8.0);
            let mut changed = false;
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(self.t("settings.language"));
                    for (lang, name) in [(Lang::Ru, "Русский"), (Lang::En, "English")] {
                        if ui.selectable_label(self.settings.lang == lang, name).clicked() {
                            self.settings.lang = lang;
                            changed = true;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(self.t("settings.overheat"));
                    if ui
                        .add(egui::Slider::new(&mut self.settings.overheat_c, 40..=90).suffix(" °C"))
                        .changed()
                    {
                        changed = true;
                        self.reassess();
                    }
                });
                ui.label(RichText::new(self.t("settings.overheat.hint")).small().weak());
            });
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(self.t("settings.updates")).strong());
                let auto_label = self.t("settings.auto_update");
                if ui.checkbox(&mut self.settings.auto_update, auto_label).changed() {
                    changed = true;
                }
                ui.horizontal(|ui| {
                    if ui.button(self.t("update.check")).clicked() {
                        self.check_updates_now(&ctx, false);
                    }
                    let state = self.update.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    match state {
                        update::State::Checking => {
                            ui.add(egui::Spinner::new().size(14.0));
                        }
                        update::State::UpToDate => {
                            ui.label(self.t("update.up_to_date"));
                        }
                        update::State::Available { version, .. } => {
                            ui.label(self.tf("update.available", &[version]));
                            if theme::primary_button(ui, self.t("update.install")).clicked() {
                                self.check_updates_now(&ctx, true);
                            }
                        }
                        update::State::Downloading { done, total, .. } => {
                            let pct = (done * 100).checked_div(total).unwrap_or(0);
                            ui.label(self.tf("update.downloading", &[pct.to_string()]));
                        }
                        update::State::Ready { version } => {
                            ui.label(self.tf("update.ready", &[version]));
                        }
                        update::State::Failed(e) => {
                            ui.colored_label(theme::BAD, self.tf("update.failed", &[e]));
                        }
                        update::State::Idle => {}
                    }
                });
            });
            ui.add_space(8.0);
            self.background_card(ui, &ctx, &mut changed);
            ui.add_space(8.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(self.t("settings.privacy")).strong());
                ui.label(self.t("settings.privacy.body"));
                ui.add_space(6.0);
                ui.label(RichText::new(self.t("settings.diag")).strong());
                ui.horizontal(|ui| {
                    if ui.button(self.t("diag.open")).clicked()
                        && let Some(dir) = crate::diag::logs_dir()
                    {
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    }
                    if ui.button(self.t("diag.copy")).clicked() {
                        ctx.copy_text(crate::diag::report());
                        self.toast(self.t("diag.copied"));
                    }
                });
                if !crate::settings::is_demo() && !winint::running_installed() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(self.t("settings.install")).strong());
                    ui.label(RichText::new(self.t("settings.install.body")).small().weak());
                    if ui.button(self.t("settings.install.btn")).clicked() {
                        match winint::relaunch_normal("--install") {
                            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                            Err(e) => self.toast(e),
                        }
                    }
                }
            });
            if changed {
                self.settings.save();
            }
        });
    }
}

impl App {
    fn refresh_service_state(&mut self) {
        if self
            .svc_checked
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3))
        {
            self.svc = if crate::settings::is_demo() {
                (false, false)
            } else {
                (
                    diskpulse_core::service::is_installed(),
                    diskpulse_core::service::is_running(),
                )
            };
            self.svc_checked = Some(std::time::Instant::now());
        }
    }

    /// Snapshot schedule, notifications, tray icon and the optional background service.
    fn background_card(&mut self, ui: &mut Ui, ctx: &egui::Context, changed: &mut bool) {
        self.refresh_service_state();
        ctx.request_repaint_after(std::time::Duration::from_secs(3));
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(self.t("settings.background")).strong());
            let min_suffix = format!(" {}", self.t("unit.min"));
            ui.horizontal(|ui| {
                ui.label(self.t("settings.interval"));
                *changed |= ui
                    .add(egui::Slider::new(&mut self.settings.collect_interval_min, 5..=240).suffix(min_suffix))
                    .changed();
            });
            let notify_label = self.t("settings.notifications");
            *changed |= ui.checkbox(&mut self.settings.notifications, notify_label).changed();
            ui.horizontal(|ui| {
                ui.label(self.t("settings.spike"));
                *changed |= ui
                    .add(egui::DragValue::new(&mut self.settings.write_spike_gb).range(1..=100_000))
                    .changed();
            });
            ui.add_space(4.0);
            ui.label(RichText::new(self.t("settings.tray.hint")).small().weak());
            #[cfg(windows)]
            {
                let mut auto = winint::autostart_enabled();
                let label = self.t("settings.tray_autostart");
                if ui.checkbox(&mut auto, label).changed() && !crate::settings::is_demo() {
                    let exe = winint::installed_exe()
                        .filter(|e| e.exists())
                        .or_else(|| std::env::current_exe().ok());
                    if let Some(exe) = exe {
                        let _ = winint::set_autostart(auto, &exe);
                    }
                }
                if ui.button(self.t("settings.tray_start")).clicked() && !crate::settings::is_demo() {
                    let _ = winint::relaunch_normal("--tray");
                    self.toast(self.t("settings.tray_started"));
                }
            }
            ui.add_space(8.0);
            ui.label(RichText::new(self.t("settings.service")).strong());
            ui.label(RichText::new(self.t("settings.service.body")).small().weak());
            let (installed, running) = self.svc;
            let state_key = match (installed, running) {
                (true, true) => "settings.service.state.running",
                (true, false) => "settings.service.state.stopped",
                _ => "settings.service.state.none",
            };
            ui.label(self.t(state_key));
            if !crate::settings::is_demo() {
                let (arg, key) = if installed {
                    ("--uninstall-service", "settings.service.remove")
                } else {
                    ("--install-service", "settings.service.install")
                };
                if ui.button(self.t(key)).clicked() {
                    self.settings.save();
                    match winint::relaunch_elevated(arg) {
                        Ok(()) => self.toast(self.t("settings.service.requested")),
                        Err(_) => self.toast(self.t("admin.failed")),
                    }
                    self.svc_checked = None;
                }
            }
        });
    }
}

pub fn disk_temperature(d: &DiskView) -> Option<i32> {
    match d.report.data()? {
        SmartData::Nvme(h) => h.temperature_c,
        SmartData::Ata(s) => s.temperature_c(),
    }
}
