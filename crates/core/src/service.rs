//! Status of the optional background collection service (`DiskPulseCollector`) and where its data lives.
//! Installing and running the service is done by the app (it needs the service control handler).

use std::path::PathBuf;

use windows_service::service::{ServiceAccess, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

pub const NAME: &str = "DiskPulseCollector";

fn open_status() -> Option<windows_service::service::Service> {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?.open_service(NAME, ServiceAccess::QUERY_STATUS).ok()
}

pub fn is_installed() -> bool {
    open_status().is_some()
}

pub fn is_running() -> bool {
    open_status().and_then(|s| s.query_status().ok()).is_some_and(|st| st.current_state == ServiceState::Running)
}

/// `%ProgramData%\DiskPulse`: written by the service (LocalSystem), readable by every user.
pub fn shared_dir() -> PathBuf {
    std::env::var_os("ProgramData").map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from).join("DiskPulse")
}

/// `%APPDATA%\DiskPulse`.
pub fn user_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("DiskPulse"))
}

/// Where history is read from: the service's folder when the service is installed, otherwise the user's own.
pub fn history_dir() -> PathBuf {
    if is_installed() {
        shared_dir().join("history")
    } else {
        user_dir().unwrap_or_else(|| PathBuf::from(".")).join("history")
    }
}
