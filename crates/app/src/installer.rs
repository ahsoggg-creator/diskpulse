//! Installer / uninstaller screens. The same executable becomes the installer when it is started as
//! `DiskPulse-Setup.exe` (or with `--install`) and the uninstaller with `--uninstall`. Neither needs administrator rights.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, Align, Id, Layout, RichText, Ui, vec2};

use crate::{App, Mode, theme, winint};

pub struct Install {
    desktop: bool,
    job: Option<Receiver<Result<PathBuf, String>>>,
    result: Option<Result<PathBuf, String>>,
}

impl Default for Install {
    fn default() -> Self {
        Install {
            desktop: true,
            job: None,
            result: None,
        }
    }
}

#[derive(Default)]
pub struct Uninstall {
    job: Option<Receiver<Result<(), String>>>,
    result: Option<Result<(), String>>,
}

impl App {
    pub fn installer_ui(&mut self, ui: &mut Ui) {
        let icon = self.icon.clone();
        let ctx = ui.ctx().clone();
        let mut mode = std::mem::replace(&mut self.mode, Mode::App);
        egui::CentralPanel::default().show(ui, |ui| {
            let appear = ctx.animate_bool_with_time(Id::new("setup_in"), true, 0.3);
            ui.multiply_opacity(appear);
            ui.add_space(18.0 + (1.0 - appear) * 12.0);
            ui.vertical_centered(|ui| {
                if let Some(icon) = &icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(64.0, 64.0)));
                }
                ui.add_space(6.0);
                ui.label(RichText::new("DiskPulse").size(24.0).strong());
                ui.label(RichText::new(self.tf("setup.version", &[env!("CARGO_PKG_VERSION").to_owned()])).weak());
            });
            ui.add_space(14.0);
            match &mut mode {
                Mode::Install(s) => self.install_body(ui, s),
                Mode::Uninstall(s) => self.uninstall_body(ui, s),
                Mode::App => {}
            }
        });
        self.mode = mode;
    }

    fn install_body(&self, ui: &mut Ui, s: &mut Install) {
        if let Some(rx) = &s.job {
            if let Ok(r) = rx.try_recv() {
                s.result = Some(r);
                s.job = None;
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(20.0);
                    ui.add(egui::Spinner::new().size(32.0).color(theme::ACCENT));
                    ui.label(self.t("setup.progress"));
                });
                ui.ctx().request_repaint();
                return;
            }
        }
        match &s.result {
            Some(Ok(exe)) => {
                let exe = exe.clone();
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(self.t("setup.done"))
                            .size(18.0)
                            .color(theme::GOOD)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    ui.label(self.t("setup.done.body"));
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 260.0).max(0.0) / 2.0);
                        if theme::primary_button(ui, self.t("setup.launch")).clicked() {
                            let _ = std::process::Command::new(&exe).spawn();
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button(self.t("setup.close")).clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(self.t("setup.failed")).strong().color(theme::BAD));
                ui.label(e.as_str());
                if ui.button(self.t("setup.retry")).clicked() {
                    s.result = None;
                }
            }
            None => {
                let dir = winint::install_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                let upgrade = winint::installed_exe().is_some_and(|e| e.exists());
                theme::card(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(self.t("setup.dir")).weak());
                    ui.label(dir);
                    ui.add_space(6.0);
                    ui.checkbox(&mut s.desktop, self.t("setup.desktop"));
                    ui.label(RichText::new(self.t("setup.note")).weak().small());
                });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(self.t("setup.cancel")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let label = self.t(if upgrade { "setup.upgrade" } else { "setup.install" });
                    if theme::primary_button(ui, label).clicked() {
                        let (tx, rx) = channel();
                        let opts = winint::InstallOptions {
                            desktop_shortcut: s.desktop,
                        };
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let r = winint::install(opts);
                            crate::diag::info(format!(
                                "install: {}",
                                r.as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|e| e.clone())
                            ));
                            let _ = tx.send(r);
                            ctx.request_repaint();
                        });
                        s.job = Some(rx);
                    }
                });
            }
        }
    }

    fn uninstall_body(&self, ui: &mut Ui, s: &mut Uninstall) {
        if let Some(rx) = &s.job {
            if let Ok(r) = rx.try_recv() {
                s.result = Some(r);
                s.job = None;
            } else {
                ui.vertical_centered(|ui| {
                    ui.add(egui::Spinner::new().size(28.0).color(theme::ACCENT));
                    ui.label(self.t("unsetup.progress"));
                });
                ui.ctx().request_repaint();
                return;
            }
        }
        match &s.result {
            None => {
                ui.vertical_centered(|ui| {
                    ui.label(self.t("unsetup.ask"));
                    ui.label(RichText::new(self.t("unsetup.body")).weak());
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 180.0).max(0.0) / 2.0);
                        if ui
                            .add(
                                egui::Button::new(RichText::new(self.t("unsetup.remove")).color(egui::Color32::WHITE))
                                    .fill(theme::BAD),
                            )
                            .clicked()
                        {
                            let (tx, rx) = channel();
                            let ctx = ui.ctx().clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(winint::uninstall());
                                ctx.request_repaint();
                            });
                            s.job = Some(rx);
                        }
                        if ui.button(self.t("setup.cancel")).clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            }
            Some(Ok(())) => {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(self.t("unsetup.done")).size(17.0).strong());
                    ui.add_space(10.0);
                    if ui.button(self.t("setup.close")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(self.t("unsetup.failed")).strong().color(theme::BAD));
                ui.label(e.as_str());
            }
        }
    }
}
