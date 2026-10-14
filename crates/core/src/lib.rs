//! DiskPulse core: S.M.A.R.T. reading and parsing, assessment rules, history, read-only surface scan, NTFS mapping.
//! No UI. Pure logic is platform independent and unit tested; the Windows device layer is `cfg(windows)`.

pub mod alerts;
pub mod assess;
pub mod ata;
pub mod collector;
pub mod disks;
pub mod error;
pub mod history;
pub mod i18n;
pub mod nvme;
#[cfg(windows)]
pub mod service;
pub mod smart;
pub mod surface;
pub mod units;
#[cfg(windows)]
pub mod winapi;

pub use error::{Error, Result};
