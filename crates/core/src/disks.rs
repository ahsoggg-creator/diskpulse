//! Physical disk enumeration: model, serial, bus, size, and the volume letters that live on each disk.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusType {
    Nvme,
    Sata,
    Ata,
    Usb,
    Scsi,
    Sas,
    Raid,
    Virtual,
    Other,
}

impl BusType {
    /// From the Windows `STORAGE_BUS_TYPE` value.
    pub fn from_raw(v: u32) -> BusType {
        match v {
            1 => BusType::Scsi,
            3 => BusType::Ata,
            7 => BusType::Usb,
            8 => BusType::Raid,
            0xA => BusType::Sas,
            0xB => BusType::Sata,
            0xE | 0xF => BusType::Virtual,
            0x11 => BusType::Nvme,
            _ => BusType::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BusType::Nvme => "NVMe",
            BusType::Sata => "SATA",
            BusType::Ata => "ATA",
            BusType::Usb => "USB",
            BusType::Scsi => "SCSI",
            BusType::Sas => "SAS",
            BusType::Raid => "RAID",
            BusType::Virtual => "Virtual",
            BusType::Other => "?",
        }
    }
}

/// A region of a physical disk occupied by a volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extent {
    pub disk: u32,
    pub start: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    /// `C:`
    pub letter: String,
    /// File system name as reported by Windows (`NTFS`, `exFAT`, ...).
    pub fs: String,
    pub extents: Vec<Extent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskInfo {
    pub number: u32,
    pub model: String,
    /// Full serial number. Serialized masked (see [`mask_serial`]); never write the full value to logs or reports.
    #[serde(serialize_with = "ser_masked")]
    pub serial: String,
    pub firmware: String,
    pub bus: BusType,
    pub size_bytes: u64,
    pub removable: bool,
    /// `Some(true)` for HDDs (seek penalty), `Some(false)` for SSDs, `None` when the drive does not say.
    pub rotational: Option<bool>,
    /// Letters of volumes on this disk (`C:`, `D:`).
    pub volumes: Vec<String>,
}

fn ser_masked<S: serde::Serializer>(s: &str, ser: S) -> std::result::Result<S::Ok, S::Error> {
    ser.serialize_str(&mask_serial(s))
}

impl DiskInfo {
    pub fn path(&self) -> String {
        format!(r"\\.\PhysicalDrive{}", self.number)
    }

    /// Stable identifier for history files: hash of model and serial, so the file name leaks nothing.
    pub fn key(&self) -> String {
        disk_key(&self.model, &self.serial)
    }

    pub fn serial_masked(&self) -> String {
        mask_serial(&self.serial)
    }
}

/// `"WD-WCC6Y1234567"` -> `"****4567"`. Serial numbers are sensitive: only the last four characters are kept.
pub fn mask_serial(serial: &str) -> String {
    let s = serial.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 4 {
        return "****".to_owned();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("****{tail}")
}

/// FNV-1a 64-bit of `model|serial` as 16 hex digits (an id, not a security measure).
pub fn disk_key(model: &str, serial: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in model
        .trim()
        .bytes()
        .chain(std::iter::once(b'|'))
        .chain(serial.trim().bytes())
    {
        h ^= u64::from(b.to_ascii_lowercase());
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Fields of `STORAGE_DEVICE_DESCRIPTOR` that matter here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub vendor: String,
    pub product: String,
    pub revision: String,
    pub serial: String,
    pub bus: BusType,
    pub removable: bool,
}

fn c_string(b: &[u8], off: u32) -> String {
    let off = off as usize;
    if off == 0 || off >= b.len() {
        return String::new();
    }
    let end = b[off..].iter().position(|&c| c == 0).map_or(b.len(), |p| off + p);
    String::from_utf8_lossy(&b[off..end])
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_owned()
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

pub fn parse_descriptor(b: &[u8]) -> Result<Descriptor> {
    if b.len() < 36 {
        return Err(Error::Parse("short STORAGE_DEVICE_DESCRIPTOR".into()));
    }
    Ok(Descriptor {
        removable: b[10] != 0,
        vendor: c_string(b, u32_at(b, 12)),
        product: c_string(b, u32_at(b, 16)),
        revision: c_string(b, u32_at(b, 20)),
        serial: c_string(b, u32_at(b, 24)),
        bus: BusType::from_raw(u32_at(b, 28)),
    })
}

impl Descriptor {
    pub fn model(&self) -> String {
        match (self.vendor.is_empty(), self.product.is_empty()) {
            (true, _) => self.product.clone(),
            // "ATA" / "NVMe" vendor ids carry no information; real vendors do (USB bridges report e.g. "Samsung")
            (false, false) if !matches!(self.vendor.as_str(), "ATA" | "NVMe") => {
                format!("{} {}", self.vendor, self.product)
            }
            _ => self.product.clone(),
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    };
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_QUERY_PROPERTY,
    };

    use super::*;
    use crate::winapi::{Device, wide};

    const MAX_DISKS: u32 = 64;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOVABLE: u32 = 2;

    fn storage_query(dev: &Device, property_id: u32, out_len: usize) -> Result<Vec<u8>> {
        let mut input = [0u8; 12];
        input[0..4].copy_from_slice(&property_id.to_le_bytes());
        let mut out = vec![0u8; out_len];
        let n = dev.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut out)? as usize;
        out.truncate(n);
        Ok(out)
    }

    pub fn open_disk_query(number: u32) -> Result<Device> {
        Device::open_query(&format!(r"\\.\PhysicalDrive{number}"))
    }

    fn disk_size(dev: &Device) -> u64 {
        let mut out = [0u8; 8];
        if dev.ioctl(IOCTL_DISK_GET_LENGTH_INFO, &[], &mut out).is_ok() {
            return u64::from_le_bytes(out);
        }
        // Works with a query-only handle: DISK_GEOMETRY_EX { DISK_GEOMETRY (24 bytes), DiskSize (i64), ... }
        let mut geo = [0u8; 64];
        match dev.ioctl(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geo) {
            Ok(n) if n >= 32 => u64::from_le_bytes(geo[24..32].try_into().expect("8 bytes")),
            _ => 0,
        }
    }

    fn rotational(dev: &Device) -> Option<bool> {
        // StorageDeviceSeekPenaltyProperty = 7; DEVICE_SEEK_PENALTY_DESCRIPTOR { Version, Size, IncursSeekPenalty }
        let out = storage_query(dev, 7, 16).ok()?;
        (out.len() >= 9).then(|| out[8] != 0)
    }

    pub fn disk_info(number: u32) -> Result<DiskInfo> {
        let dev = open_disk_query(number)?;
        let d = parse_descriptor(&storage_query(&dev, 0, 1024)?)?;
        Ok(DiskInfo {
            number,
            model: d.model(),
            serial: d.serial,
            firmware: d.revision,
            bus: d.bus,
            size_bytes: disk_size(&dev),
            removable: d.removable,
            rotational: rotational(&dev),
            volumes: Vec::new(),
        })
    }

    pub fn volumes() -> Vec<VolumeInfo> {
        // SAFETY: trivial query of the logical drive bitmask.
        let mask = unsafe { GetLogicalDrives() };
        let mut v = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = format!("{}:", (b'A' + i as u8) as char);
            let root = wide(&format!(r"{letter}\"));
            // SAFETY: `root` is NUL-terminated.
            let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
            if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
                continue;
            }
            let Ok(dev) = Device::open_query(&format!(r"\\.\{letter}")) else {
                continue;
            };
            let mut out = vec![0u8; 8 + 24 * 8];
            let Ok(n) = dev.ioctl(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut out) else {
                continue;
            };
            let count = u32_at(&out, 0) as usize;
            let extents = (0..count)
                .filter(|k| 8 + 24 * (k + 1) <= n as usize)
                .map(|k| {
                    let o = 8 + 24 * k;
                    Extent {
                        disk: u32_at(&out, o),
                        start: u64::from_le_bytes(out[o + 8..o + 16].try_into().expect("8 bytes")),
                        len: u64::from_le_bytes(out[o + 16..o + 24].try_into().expect("8 bytes")),
                    }
                })
                .collect();
            let mut fs = [0u16; 32];
            // SAFETY: `root` is NUL-terminated; only the file system name buffer (32 u16) is requested.
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs.as_mut_ptr(),
                    fs.len() as u32,
                )
            };
            let fs = if ok != 0 {
                String::from_utf16_lossy(&fs[..fs.iter().position(|&c| c == 0).unwrap_or(0)])
            } else {
                String::new()
            };
            v.push(VolumeInfo { letter, fs, extents });
        }
        v
    }

    /// All physical disks Windows exposes, with volume letters attached.
    pub fn list_disks() -> Vec<DiskInfo> {
        let vols = volumes();
        let mut disks = Vec::new();
        for n in 0..MAX_DISKS {
            if let Ok(mut d) = disk_info(n) {
                d.volumes = vols
                    .iter()
                    .filter(|v| v.extents.iter().any(|e| e.disk == n))
                    .map(|v| v.letter.clone())
                    .collect();
                disks.push(d);
            }
        }
        disks
    }
}

#[cfg(windows)]
pub use imp::{disk_info, list_disks, open_disk_query, volumes};

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor_bytes(vendor: &str, product: &str, rev: &str, serial: &str, bus: u32) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        let put = |b: &mut Vec<u8>, s: &str| -> u32 {
            let off = b.len() as u32;
            b.extend_from_slice(s.as_bytes());
            b.push(0);
            off
        };
        let (v, p, r, s) = (
            put(&mut b, vendor),
            put(&mut b, product),
            put(&mut b, rev),
            put(&mut b, serial),
        );
        b[10] = 1;
        b[12..16].copy_from_slice(&v.to_le_bytes());
        b[16..20].copy_from_slice(&p.to_le_bytes());
        b[20..24].copy_from_slice(&r.to_le_bytes());
        b[24..28].copy_from_slice(&s.to_le_bytes());
        b[28..32].copy_from_slice(&bus.to_le_bytes());
        b
    }

    #[test]
    fn parses_descriptor_strings_and_bus() {
        let d = parse_descriptor(&descriptor_bytes(
            "ATA",
            "Example SSD 256GB",
            "1.0",
            "  ABC123XYZ ",
            0xB,
        ))
        .unwrap();
        assert_eq!((d.bus, d.removable), (BusType::Sata, true));
        assert_eq!(d.model(), "Example SSD 256GB");
        assert_eq!(d.serial, "ABC123XYZ");
        assert_eq!(d.revision, "1.0");
    }

    #[test]
    fn usb_bridge_vendor_is_kept_in_model() {
        let d = parse_descriptor(&descriptor_bytes("Acme", "Portable", "0", "S", 7)).unwrap();
        assert_eq!((d.bus, d.model()), (BusType::Usb, "Acme Portable".to_owned()));
    }

    #[test]
    fn nvme_serial_trailing_dot_is_dropped() {
        let d = parse_descriptor(&descriptor_bytes("", "Fake NVMe", "R1", "0000_1111_2222_3333.", 0x11)).unwrap();
        assert_eq!(d.serial, "0000_1111_2222_3333");
        assert_eq!(d.bus, BusType::Nvme);
    }

    #[test]
    fn serial_is_masked_and_key_does_not_contain_it() {
        assert_eq!(mask_serial("WD-WCC6Y1234567"), "****4567");
        assert_eq!(mask_serial("AB"), "****");
        let key = disk_key("Model X", "SECRET-SERIAL-9999");
        assert_eq!(key.len(), 16);
        assert!(!key.contains("9999"));
        assert_eq!(key, disk_key("model x", "secret-serial-9999"));
        assert_ne!(key, disk_key("Model X", "SECRET-SERIAL-0000"));
    }

    #[test]
    fn json_never_contains_full_serial() {
        let d = DiskInfo {
            number: 0,
            model: "M".into(),
            serial: "FULLSERIAL1234".into(),
            firmware: String::new(),
            bus: BusType::Sata,
            size_bytes: 1,
            removable: false,
            rotational: None,
            volumes: vec![],
        };
        let j = serde_json::to_string(&d).unwrap();
        assert!(!j.contains("FULLSERIAL") && j.contains("****1234"), "{j}");
    }

    #[test]
    fn short_descriptor_is_an_error() {
        assert!(parse_descriptor(&[0u8; 10]).is_err());
    }
}
