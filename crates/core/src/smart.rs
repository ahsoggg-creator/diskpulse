//! One entry point for reading S.M.A.R.T. from any disk, with an honest outcome: data, or a reason it is unavailable.
//! A disk that cannot be read never produces zeros or a made-up value.

use serde::{Deserialize, Serialize};

use crate::ata::AtaSmart;
use crate::nvme::NvmeHealth;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SmartData {
    Nvme(NvmeHealth),
    Ata(AtaSmart),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    /// Raw device access needs an elevated process.
    NeedsAdmin,
    /// A USB adapter that blocks (or does not implement) SMART pass-through.
    UsbBridge,
    /// The device, driver or controller does not provide SMART (virtual disks, some RAID controllers).
    Unsupported,
    /// The device answered, but not with a valid SMART structure.
    Invalid,
}

// One short-lived value per disk and refresh; boxing the data would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SmartReport {
    Ok {
        /// How the data was read, e.g. `NVMe log page 02h`.
        method: String,
        data: SmartData,
    },
    Unavailable {
        reason: UnavailableReason,
        /// Technical detail (error codes); contains no serial numbers.
        detail: String,
    },
}

impl SmartReport {
    pub fn data(&self) -> Option<&SmartData> {
        match self {
            SmartReport::Ok { data, .. } => Some(data),
            SmartReport::Unavailable { .. } => None,
        }
    }

    pub fn unavailable(reason: UnavailableReason, detail: impl Into<String>) -> SmartReport {
        SmartReport::Unavailable {
            reason,
            detail: detail.into(),
        }
    }
}

#[cfg(windows)]
pub use win::read_smart;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::ata::{self, AtaFailure};
    use crate::disks::{BusType, DiskInfo};
    use crate::winapi::{Device, GENERIC_READ, GENERIC_WRITE};

    fn ata_fail(f: AtaFailure, usb: bool) -> SmartReport {
        match f {
            AtaFailure::AccessDenied => SmartReport::unavailable(UnavailableReason::NeedsAdmin, "access denied"),
            AtaFailure::Rejected(d) if usb => SmartReport::unavailable(UnavailableReason::UsbBridge, d),
            AtaFailure::Rejected(d) => SmartReport::unavailable(UnavailableReason::Unsupported, d),
            AtaFailure::Invalid(d) if usb => SmartReport::unavailable(UnavailableReason::UsbBridge, d),
            AtaFailure::Invalid(d) => SmartReport::unavailable(UnavailableReason::Invalid, d),
        }
    }

    fn nvme(disk: &DiskInfo) -> std::result::Result<SmartReport, crate::Error> {
        // Query handle (no data access) first; some Windows builds want read access for the protocol query.
        let dev = Device::open_query(&disk.path())
            .or_else(|_| Device::open(&disk.path(), GENERIC_READ | GENERIC_WRITE, 0))?;
        let data = crate::nvme::read_health(&dev)?;
        Ok(SmartReport::Ok {
            method: "NVMe log page 02h (IOCTL_STORAGE_QUERY_PROPERTY)".into(),
            data: SmartData::Nvme(data),
        })
    }

    fn nvme_report(disk: &DiskInfo, usb: bool) -> SmartReport {
        match nvme(disk) {
            Ok(r) => r,
            Err(e) if e.is_access_denied() => SmartReport::unavailable(UnavailableReason::NeedsAdmin, e.to_string()),
            Err(e) if usb => SmartReport::unavailable(UnavailableReason::UsbBridge, e.to_string()),
            Err(e @ crate::Error::Parse(_)) => SmartReport::unavailable(UnavailableReason::Invalid, e.to_string()),
            Err(e) => SmartReport::unavailable(UnavailableReason::Unsupported, e.to_string()),
        }
    }

    fn ata_ok(method: &str, s: AtaSmart) -> SmartReport {
        SmartReport::Ok {
            method: method.into(),
            data: SmartData::Ata(s),
        }
    }

    /// Reads S.M.A.R.T. from `disk` using the transport that fits its bus.
    pub fn read_smart(disk: &DiskInfo) -> SmartReport {
        let path = disk.path();
        match disk.bus {
            BusType::Nvme => nvme_report(disk, false),
            BusType::Sata | BusType::Ata => match ata::read_smart_ata(&path, disk.number) {
                Ok(s) => ata_ok("ATA SMART (IOCTL_ATA_PASS_THROUGH)", s),
                Err(f) => ata_fail(f, false),
            },
            BusType::Usb => match ata::read_smart_sat(&path) {
                Ok(s) => ata_ok("ATA SMART via SCSI-ATA Translation (SAT)", s),
                Err(AtaFailure::AccessDenied) => ata_fail(AtaFailure::AccessDenied, true),
                Err(f) => match nvme_report(disk, true) {
                    r @ SmartReport::Ok { .. } => r,
                    _ => ata_fail(f, true),
                },
            },
            BusType::Virtual => SmartReport::unavailable(UnavailableReason::Unsupported, "virtual disk"),
            BusType::Scsi | BusType::Sas | BusType::Raid | BusType::Other => {
                match ata::read_smart_ata(&path, disk.number) {
                    Ok(s) => ata_ok("ATA SMART (IOCTL_ATA_PASS_THROUGH)", s),
                    Err(AtaFailure::AccessDenied) => ata_fail(AtaFailure::AccessDenied, false),
                    Err(first) => match ata::read_smart_sat(&path) {
                        Ok(s) => ata_ok("ATA SMART via SCSI-ATA Translation (SAT)", s),
                        Err(_) => ata_fail(first, false),
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_serializes_with_status_and_reason() {
        let r = SmartReport::unavailable(UnavailableReason::UsbBridge, "SCSI status 2");
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["status"], "unavailable");
        assert_eq!(j["reason"], "usb_bridge");
        assert!(r.data().is_none());
    }

    #[test]
    fn ok_report_roundtrips_through_json() {
        let page = crate::nvme::tests::sample_page();
        let h = crate::nvme::parse_health_log(&page).unwrap();
        let r = SmartReport::Ok {
            method: "m".into(),
            data: SmartData::Nvme(h),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"status\":\"ok\"") && s.contains("\"kind\":\"nvme\""));
        assert_eq!(serde_json::from_str::<SmartReport>(&s).unwrap(), r);
    }
}
