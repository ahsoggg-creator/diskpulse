#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod demo;
mod diag;
mod history_view;
mod installer;
mod settings;
mod theme;
mod tray;
mod update;
mod views;
mod winint;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use diskpulse_core::assess::{self, Assessment};
use diskpulse_core::disks::{DiskInfo, list_disks};
use diskpulse_core::i18n::{self, Lang};
use diskpulse_core::smart::{SmartReport, read_smart};
use eframe::egui::{self, Ui};
use settings::Settings;

/// One disk with everything shown for it.
#[derive(Clone)]
pub struct DiskView {
    pub info: DiskInfo,
    pub report: SmartReport,
    pub assessment: Option<Assessment>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Disks,
    History,
    Settings,
}

pub enum Mode {
    App,
    Install(installer::Install),
    Uninstall(installer::Uninstall),
}

pub struct Toast {
    pub text: String,
    pub born: Instant,
}

pub struct App {
    pub mode: Mode,
    pub settings: Settings,
    pub tab: Tab,
    pub disks: Vec<DiskView>,
    /// Number of the selected disk.
    pub selected: Option<u32>,
    job: Option<Receiver<Vec<DiskView>>>,
    pub loaded_once: bool,
    pub update: update::Shared,
    pub icon: Option<egui::TextureHandle>,
    pub elevated: bool,
    pub show_serial: bool,
    pub toasts: Vec<Toast>,
    pub demo: Option<demo::Driver>,
    pub crash_notice: Option<PathBuf>,
    pub rules_open: bool,
    pub hist: history_view::HistoryState,
    pub collect_job: Option<Receiver<usize>>,
}

pub fn image_from_rgba(bytes: &[u8], side: usize) -> Option<egui::ColorImage> {
    (bytes.len() == side * side * 4).then(|| egui::ColorImage::from_rgba_unmultiplied([side, side], bytes))
}

impl App {
    fn new(cc: &eframe::CreationContext, mode: Mode) -> App {
        theme::install(&cc.egui_ctx);
        let icon = image_from_rgba(include_bytes!("../assets/icon64.rgba"), 64)
            .map(|img| cc.egui_ctx.load_texture("icon", img, egui::TextureOptions::LINEAR));
        let in_app = matches!(mode, Mode::App);
        let mut app = App {
            mode,
            settings: Settings::load(),
            tab: Tab::Disks,
            disks: Vec::new(),
            selected: None,
            job: None,
            loaded_once: false,
            update: Arc::new(Mutex::new(update::State::Idle)),
            icon,
            elevated: winint::is_elevated(),
            show_serial: false,
            toasts: Vec::new(),
            demo: demo::Driver::from_args(),
            crash_notice: if settings::is_demo() {
                None
            } else {
                diag::take_unseen_crash()
            },
            rules_open: false,
            hist: history_view::HistoryState::default(),
            collect_job: None,
        };
        if in_app {
            update::cleanup_old();
            app.refresh(&cc.egui_ctx);
            app.maybe_check_updates(&cc.egui_ctx);
        }
        app
    }

    pub fn lang(&self) -> Lang {
        self.settings.lang
    }

    /// Localized string for `key`.
    pub fn t(&self, key: &str) -> &'static str {
        i18n::tr(self.settings.lang, key)
    }

    /// Localized string with `{0}`, `{1}` replaced.
    pub fn tf(&self, key: &str, args: &[String]) -> String {
        i18n::fmt(self.settings.lang, key, args)
    }

    pub fn loading(&self) -> bool {
        self.job.is_some()
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            born: Instant::now(),
        });
    }

    /// Reads all disks on a worker thread.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let thresholds = self.settings.thresholds();
        let demo = settings::is_demo();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let views = if demo {
                demo::disks(thresholds)
            } else {
                list_disks()
                    .into_iter()
                    .map(|info| {
                        let report = read_smart(&info);
                        let assessment = assess::assess_report(&report, thresholds);
                        let line = format!("{}: {}", diag::disk_label(&info), summary(&report, assessment.as_ref()));
                        if report.data().is_some() {
                            diag::info(line)
                        } else {
                            diag::warn(line)
                        }
                        DiskView {
                            info,
                            report,
                            assessment,
                        }
                    })
                    .collect()
            };
            let _ = tx.send(views);
            ctx.request_repaint();
        });
        self.job = Some(rx);
    }

    fn poll_job(&mut self) {
        let Some(rx) = &self.job else { return };
        if let Ok(views) = rx.try_recv() {
            self.disks = views;
            self.job = None;
            self.loaded_once = true;
            if self
                .selected
                .is_none_or(|n| !self.disks.iter().any(|d| d.info.number == n))
            {
                self.selected = self.disks.first().map(|d| d.info.number);
            }
        }
    }

    /// Re-evaluates verdicts after the overheat threshold changed (no device access).
    pub fn reassess(&mut self) {
        let t = self.settings.thresholds();
        for d in &mut self.disks {
            d.assessment = assess::assess_report(&d.report, t);
        }
    }

    fn maybe_check_updates(&mut self, ctx: &egui::Context) {
        if settings::is_demo() || !self.settings.auto_update {
            return;
        }
        let now = diag::now();
        if now.saturating_sub(self.settings.last_update_check) < update::CHECK_INTERVAL_SECS {
            return;
        }
        self.settings.last_update_check = now;
        self.settings.save();
        let ctx = ctx.clone();
        update::spawn(self.update.clone(), false, move || ctx.request_repaint());
    }

    pub fn check_updates_now(&mut self, ctx: &egui::Context, install: bool) {
        let ctx = ctx.clone();
        update::spawn(self.update.clone(), install, move || ctx.request_repaint());
    }

    pub fn selected_disk(&self) -> Option<&DiskView> {
        self.selected
            .and_then(|n| self.disks.iter().find(|d| d.info.number == n))
    }

    fn app_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll_job();
        self.poll_collect();
        self.demo_step(&ctx);
        self.toasts.retain(|t| t.born.elapsed() < Duration::from_secs(6));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("footer").show(ui, |ui| self.footer(ui));
        match self.tab {
            Tab::Disks => {
                egui::Panel::left("disk_list")
                    .resizable(true)
                    .default_size(300.0)
                    .min_size(240.0)
                    .show(ui, |ui| self.disk_list(ui));
                egui::CentralPanel::default().show(ui, |ui| self.disk_detail(ui));
            }
            Tab::History => {
                egui::CentralPanel::default().show(ui, |ui| self.history_view(ui));
            }
            Tab::Settings => {
                egui::CentralPanel::default().show(ui, |ui| self.settings_view(ui));
            }
        }
        self.toast_overlay(&ctx);
    }
}

fn summary(report: &SmartReport, a: Option<&Assessment>) -> String {
    match (report, a) {
        (SmartReport::Ok { method, .. }, Some(a)) => format!(
            "SMART ok via {method}: {:?}, {} rule(s) fired",
            a.level,
            a.reasons.len()
        ),
        (SmartReport::Unavailable { reason, detail }, _) => format!("SMART unavailable ({reason:?}): {detail}"),
        _ => "SMART read".to_owned(),
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        match &self.mode {
            Mode::App => self.app_ui(ui),
            Mode::Install(_) | Mode::Uninstall(_) => self.installer_ui(ui),
        }
    }
}

fn main() -> eframe::Result {
    diag::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--demo") || has("--screenshots") {
        settings::set_demo();
    }
    if has("--update") {
        return run_update_headless();
    }
    if has("--tray") {
        if let Err(e) = tray::run() {
            diag::error(format!("tray: {e}"));
            std::process::exit(1);
        }
        return Ok(());
    }
    if has("--silent") && (has("--uninstall") || winint::is_setup_launch()) {
        return run_silent_setup(has("--uninstall"));
    }
    let mode = if has("--uninstall") {
        Mode::Uninstall(installer::Uninstall::default())
    } else if winint::is_setup_launch() {
        Mode::Install(installer::Install::default())
    } else {
        Mode::App
    };
    let lang = Settings::load().lang;
    let (size, min, title) = match mode {
        Mode::App => ([1180.0, 760.0], [820.0, 520.0], i18n::tr(lang, "app.title")),
        Mode::Install(_) => ([560.0, 420.0], [560.0, 420.0], i18n::tr(lang, "setup.title")),
        Mode::Uninstall(_) => ([520.0, 300.0], [520.0, 300.0], i18n::tr(lang, "unsetup.title")),
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size(min)
        .with_title(title)
        .with_app_id("DiskPulse");
    if let Some(img) = image_from_rgba(include_bytes!("../assets/icon64.rgba"), 64) {
        viewport = viewport.with_icon(egui::IconData {
            rgba: img.as_raw().to_vec(),
            width: 64,
            height: 64,
        });
    }
    if !matches!(mode, Mode::App) {
        viewport = viewport.with_resizable(false).with_maximize_button(false);
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("DiskPulse", opts, Box::new(|cc| Ok(Box::new(App::new(cc, mode)))))
}

/// `--install --silent` / `--uninstall --silent`: the installer without a window (scripts, CI).
fn run_silent_setup(uninstall: bool) -> eframe::Result {
    winint::attach_parent_console();
    let result = if uninstall {
        winint::uninstall().map(|()| "uninstalled".to_owned())
    } else {
        winint::install(winint::InstallOptions {
            desktop_shortcut: false,
        })
        .map(|p| format!("installed to {}", p.display()))
    };
    match result {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("failed: {e}");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// `DiskPulse.exe --update`: check, download, verify and swap without a window.
fn run_update_headless() -> eframe::Result {
    winint::attach_parent_console();
    let state: update::Shared = Arc::new(Mutex::new(update::State::Idle));
    println!("DiskPulse {}: checking {}", env!("CARGO_PKG_VERSION"), winint::REPO_URL);
    let result = update::run_blocking(&state, true, &|| {});
    diag::info(format!("headless update: {result:?}"));
    match result {
        update::State::UpToDate => println!("Already up to date."),
        update::State::Ready { version } => println!("Updated to {version}. Start DiskPulse again to use it."),
        update::State::Available { version, page } => {
            println!("Version {version} is available (not installed: the folder is not writable): {page}")
        }
        update::State::Failed(e) => {
            eprintln!("Update failed: {e}");
            std::process::exit(1);
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use diskpulse_core::i18n::TABLE;

    /// Every localization key mentioned in the sources of this crate exists in the table (no hard-coded or missing text).
    #[test]
    fn every_key_used_in_the_sources_exists() {
        let namespaces: std::collections::HashSet<&str> =
            TABLE.iter().filter_map(|(k, _, _)| k.split('.').next()).collect();
        let keys: std::collections::HashSet<&str> = TABLE.iter().map(|(k, _, _)| *k).collect();
        let not_keys = ["settings.json", "diskpulse.log", "diskpulse.log.1"];
        let sources = [
            include_str!("views.rs"),
            include_str!("main.rs"),
            include_str!("installer.rs"),
            include_str!("demo.rs"),
            include_str!("history_view.rs"),
            include_str!("tray.rs"),
        ];
        let (mut missing, mut checked) = (Vec::new(), 0);
        for src in sources {
            // Look at the code before the test module, so the strings of this very test are not scanned.
            let src = src.split("#[cfg(test)]").next().unwrap_or(src);
            for lit in src.split('"').skip(1).step_by(2) {
                let first = lit.split('.').next().unwrap_or("");
                let looks_like_key = lit.contains('.')
                    && lit
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
                    && namespaces.contains(first);
                if looks_like_key && !not_keys.contains(&lit) {
                    checked += 1;
                    if !keys.contains(lit) {
                        missing.push(lit.to_owned());
                    }
                }
            }
        }
        assert!(checked > 60, "the scan found only {checked} keys: the test is broken");
        assert!(missing.is_empty(), "keys missing from the i18n table: {missing:?}");
    }
}
