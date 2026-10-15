//! The optional background service `DiskPulseCollector`: collects snapshots on a schedule with no window and no logged-in
//! user, with administrator rights (so SATA drives are read as well), into `%ProgramData%\DiskPulse\history`.
//!
//! Security: the service runs as LocalSystem, so its executable is copied to `%ProgramFiles%\DiskPulse`, which only
//! administrators can write; it never runs from the user-writable install folder. The service only writes history and
//! reads its small `service.json`, whose values are clamped. It cannot be asked to do anything.
//! Installing and removing needs administrator rights: the app starts itself elevated with `--install-service` /
//! `--uninstall-service` (one UAC prompt).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use diskpulse_core::alerts::AlertConfig;
use diskpulse_core::assess::Thresholds;
use diskpulse_core::collector::{self, Config};
use diskpulse_core::history::HistoryStore;
use diskpulse_core::service::{NAME, shared_dir};
use diskpulse_core::units::now_unix;
use serde::{Deserialize, Serialize};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo,
    ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::settings::Settings;

const DISPLAY: &str = "DiskPulse - S.M.A.R.T. history collector";
const DESCRIPTION: &str =
    "Collects disk S.M.A.R.T. snapshots on a schedule for DiskPulse history and warnings. Read-only.";

define_windows_service!(ffi_service_main, service_main);

pub fn program_files_dir() -> PathBuf {
    std::env::var_os("ProgramFiles")
        .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from)
        .join("DiskPulse")
}

pub fn service_exe() -> PathBuf {
    program_files_dir().join("DiskPulse.exe")
}

/// `%ProgramData%\DiskPulse\service.json`, written at install time from the user's settings.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceConfig {
    pub interval_min: u32,
    pub overheat_c: i32,
    pub write_spike_gb: u32,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        ServiceConfig {
            interval_min: 30,
            overheat_c: 70,
            write_spike_gb: 200,
        }
    }
}

impl ServiceConfig {
    pub fn from_settings(s: &Settings) -> ServiceConfig {
        ServiceConfig {
            interval_min: s.collect_interval_min,
            overheat_c: s.overheat_c,
            write_spike_gb: s.write_spike_gb,
        }
        .clamped()
    }

    /// The file is readable by anyone who can write there: keep every value in a sane range.
    pub fn clamped(self) -> ServiceConfig {
        ServiceConfig {
            interval_min: self.interval_min.clamp(5, 24 * 60),
            overheat_c: self.overheat_c.clamp(30, 100),
            write_spike_gb: self.write_spike_gb.clamp(1, 100_000),
        }
    }

    pub fn parse(bytes: &[u8]) -> ServiceConfig {
        serde_json::from_slice::<ServiceConfig>(bytes)
            .unwrap_or_default()
            .clamped()
    }

    pub fn collector_config(&self) -> Config {
        Config {
            thresholds: Thresholds {
                overheat_c: self.overheat_c,
            },
            alerts: AlertConfig {
                overheat_c: self.overheat_c,
                write_spike_bytes: u64::from(self.write_spike_gb) * 1_000_000_000,
            },
        }
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.interval_min) * 60)
    }
}

fn config_path() -> PathBuf {
    shared_dir().join("service.json")
}

fn log(msg: &str) {
    // The service has no user profile; its own small log lives next to the data.
    let line = format!("{} {msg}\n", diskpulse_core::units::format_utc(now_unix()));
    let path = shared_dir().join("service.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 << 10) {
        let _ = std::fs::remove_file(&path);
    }
    use std::io::Write as _;
    let _ = std::fs::create_dir_all(shared_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Entry point of `DiskPulse.exe --service` (started by the Service Control Manager).
pub fn run_dispatcher() -> Result<(), String> {
    service_dispatcher::start(NAME, ffi_service_main).map_err(|e| e.to_string())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run() {
        log(&format!("service stopped with an error: {e}"));
    }
}

fn run() -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<()>();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(NAME, handler).map_err(|e| e.to_string())?;
    let report = |state: ServiceState, accept: ServiceControlAccept| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });
    };
    report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    );
    log("service started");
    let store = HistoryStore::new(shared_dir().join("history"));
    loop {
        let cfg = ServiceConfig::parse(&std::fs::read(config_path()).unwrap_or_default());
        let results = collector::collect_all(&store, &cfg.collector_config(), now_unix());
        let ok = results.iter().filter(|o| o.recorded.sample.is_some()).count();
        let alerts: usize = results.iter().map(|o| o.recorded.alerts.len()).sum();
        log(&format!(
            "collected {} disk(s): {ok} snapshot(s), {alerts} warning(s)",
            results.len()
        ));
        collector::AlertLog::new(store.dir()).trim();
        match rx.recv_timeout(cfg.interval()) {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            _ => break,
        }
    }
    log("service stopping");
    report(ServiceState::Stopped, ServiceControlAccept::empty());
    Ok(())
}

fn manager(access: ServiceManagerAccess) -> Result<ServiceManager, String> {
    ServiceManager::local_computer(None::<&str>, access).map_err(|e| e.to_string())
}

/// Installs and starts the service (needs administrator rights).
pub fn install(settings: &Settings) -> Result<(), String> {
    if !crate::winint::is_elevated() {
        return Err("administrator rights are required".into());
    }
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = program_files_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::create_dir_all(shared_dir()).map_err(|e| e.to_string())?;
    std::fs::write(
        config_path(),
        serde_json::to_vec_pretty(&ServiceConfig::from_settings(settings)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mgr = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    // reinstall: stop and remove the old one first
    if let Ok(old) = mgr.open_service(
        NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        let _ = old.stop();
        wait_stopped(&old);
        let _ = old.delete();
    }
    let dst = service_exe();
    std::fs::copy(&src, &dst).map_err(|e| format!("copy to {}: {e}", dst.display()))?;
    let info = ServiceInfo {
        name: OsString::from(NAME),
        display_name: OsString::from(DISPLAY),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: dst,
        launch_arguments: vec![OsString::from("--service")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let svc = mgr
        .create_service(&info, ServiceAccess::START | ServiceAccess::CHANGE_CONFIG)
        .map_err(|e| e.to_string())?;
    let _ = svc.set_description(DESCRIPTION);
    svc.start::<OsString>(&[]).map_err(|e| e.to_string())
}

fn wait_stopped(svc: &windows_service::service::Service) {
    for _ in 0..50 {
        if svc
            .query_status()
            .is_ok_and(|s| s.current_state == ServiceState::Stopped)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stops and removes the service and its Program Files copy; the collected history stays (it is the user's data).
pub fn uninstall() -> Result<(), String> {
    if !crate::winint::is_elevated() {
        return Err("administrator rights are required".into());
    }
    let mgr = manager(ServiceManagerAccess::CONNECT)?;
    if let Ok(svc) = mgr.open_service(
        NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        let _ = svc.stop();
        wait_stopped(&svc);
        svc.delete().map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(program_files_dir());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_clamped_whatever_the_file_says() {
        let c = ServiceConfig::parse(br#"{"interval_min":0,"overheat_c":500,"write_spike_gb":0}"#);
        assert_eq!(
            c,
            ServiceConfig {
                interval_min: 5,
                overheat_c: 100,
                write_spike_gb: 1
            }
        );
        assert_eq!(ServiceConfig::parse(b"not json"), ServiceConfig::default());
        assert_eq!(ServiceConfig::parse(b"{}").interval().as_secs(), 30 * 60);
    }

    #[test]
    fn service_exe_lives_under_program_files_not_the_user_profile() {
        let p = service_exe();
        assert!(p.ends_with(r"DiskPulse\DiskPulse.exe"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            assert!(!p.starts_with(local));
        }
    }
}
