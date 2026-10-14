//! ATA S.M.A.R.T.: attribute table parser (pure), the allow-list of commands DiskPulse may send, and the Windows
//! transports (`IOCTL_ATA_PASS_THROUGH`, legacy `SMART_RCV_DRIVE_DATA`, SCSI-ATA Translation for USB bridges).
//!
//! Only read-only SMART commands exist in [`AtaCommand`]; there is no way to express anything else.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const PAGE_LEN: usize = 512;
const ATTR_COUNT: usize = 30;
const ATTR_LEN: usize = 12;

/// The only ATA commands DiskPulse ever issues (all read-only SMART log reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtaCommand {
    /// SMART READ DATA (B0h / feature D0h).
    SmartReadData,
    /// SMART READ ATTRIBUTE THRESHOLDS (B0h / feature D1h).
    SmartReadThresholds,
}

/// ATA task file registers in the order of `IDEREGS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskFile {
    pub features: u8,
    pub count: u8,
    pub lba_low: u8,
    pub lba_mid: u8,
    pub lba_high: u8,
    pub device: u8,
    pub command: u8,
}

impl AtaCommand {
    pub fn task_file(self) -> TaskFile {
        let feature = match self {
            AtaCommand::SmartReadData => 0xD0,
            AtaCommand::SmartReadThresholds => 0xD1,
        };
        TaskFile {
            features: feature,
            count: 1,
            lba_low: 1,
            lba_mid: 0x4F,
            lba_high: 0xC2,
            device: 0xA0,
            command: 0xB0,
        }
    }
}

/// SCSI-ATA Translation `ATA PASS-THROUGH (16)` CDB for a PIO data-in command of one sector.
pub fn sat_cdb16(cmd: AtaCommand) -> [u8; 16] {
    let t = cmd.task_file();
    let mut c = [0u8; 16];
    c[0] = 0x85;
    c[1] = 4 << 1; // PIO data-in
    c[2] = 0x0E; // T_DIR=to host, BYTE_BLOCK=blocks, T_LENGTH=sector count field
    c[4] = t.features;
    c[6] = t.count;
    c[8] = t.lba_low;
    c[10] = t.lba_mid;
    c[12] = t.lba_high;
    c[13] = 0x40;
    c[14] = t.command;
    c
}

/// `ATA PASS-THROUGH (12)` CDB (some older bridges only understand this form).
pub fn sat_cdb12(cmd: AtaCommand) -> [u8; 12] {
    let t = cmd.task_file();
    let mut c = [0u8; 12];
    c[0] = 0xA1;
    c[1] = 4 << 1;
    c[2] = 0x0E;
    c[3] = t.features;
    c[4] = t.count;
    c[5] = t.lba_low;
    c[6] = t.lba_mid;
    c[7] = t.lba_high;
    c[8] = 0x40;
    c[9] = t.command;
    c
}

// ---- attribute model --------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtaAttribute {
    pub id: u8,
    pub flags: u16,
    /// Normalized current value (1..=253 for valid attributes).
    pub current: u8,
    pub worst: u8,
    /// 48-bit raw value, little endian.
    pub raw: u64,
    /// Failure threshold from the thresholds page, when it could be read.
    pub threshold: Option<u8>,
    /// Known name, empty for attributes DiskPulse does not decode.
    pub name: String,
    /// Raw encoding differs between vendors; the decoded value is an interpretation, not a fact.
    pub vendor_dependent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    Count(u64),
    Hours(u64),
    Celsius(i32),
    Lbas(u64),
    /// Not decoded: shown as-is.
    Raw(u64),
}

struct Known {
    id: u8,
    name: &'static str,
    vendor_dependent: bool,
}

const KNOWN: &[Known] = &[
    Known {
        id: 5,
        name: "Reallocated Sector Count",
        vendor_dependent: false,
    },
    Known {
        id: 9,
        name: "Power-On Hours",
        vendor_dependent: true,
    },
    Known {
        id: 12,
        name: "Power Cycle Count",
        vendor_dependent: false,
    },
    Known {
        id: 177,
        name: "Wear Leveling Count",
        vendor_dependent: true,
    },
    Known {
        id: 187,
        name: "Reported Uncorrectable Errors",
        vendor_dependent: true,
    },
    Known {
        id: 194,
        name: "Temperature",
        vendor_dependent: true,
    },
    Known {
        id: 196,
        name: "Reallocation Event Count",
        vendor_dependent: false,
    },
    Known {
        id: 197,
        name: "Current Pending Sector Count",
        vendor_dependent: false,
    },
    Known {
        id: 198,
        name: "Offline Uncorrectable Sector Count",
        vendor_dependent: false,
    },
    Known {
        id: 231,
        name: "SSD Life Left",
        vendor_dependent: true,
    },
    Known {
        id: 233,
        name: "Media Wearout Indicator",
        vendor_dependent: true,
    },
    Known {
        id: 241,
        name: "Total LBAs Written",
        vendor_dependent: true,
    },
    Known {
        id: 242,
        name: "Total LBAs Read",
        vendor_dependent: true,
    },
];

fn known(id: u8) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.id == id)
}

impl AtaAttribute {
    /// Builds an attribute and fills in its known name / vendor flag.
    pub fn new(id: u8, flags: u16, current: u8, worst: u8, raw: u64, threshold: Option<u8>) -> AtaAttribute {
        let k = known(id);
        AtaAttribute {
            id,
            flags,
            current,
            worst,
            raw: raw & 0xFFFF_FFFF_FFFF,
            threshold,
            name: k.map_or(String::new(), |k| k.name.to_owned()),
            vendor_dependent: k.is_some_and(|k| k.vendor_dependent),
        }
    }

    /// Attribute is pre-failure (as opposed to old-age) according to its flags.
    pub fn prefailure(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn decode(&self) -> Decoded {
        match self.id {
            5 | 12 | 187 | 196 | 197 | 198 => Decoded::Count(self.raw),
            9 => Decoded::Hours(self.raw & 0xFFFF_FFFF),
            194 => {
                let t = (self.raw & 0xFF) as i32;
                if (0..=127).contains(&t) {
                    Decoded::Celsius(t)
                } else {
                    Decoded::Raw(self.raw)
                }
            }
            241 | 242 => Decoded::Lbas(self.raw),
            _ => Decoded::Raw(self.raw),
        }
    }

    /// `current <= threshold` for a real threshold: the drive itself says this attribute is failing now.
    pub fn failing_now(&self) -> bool {
        self.threshold.is_some_and(|t| t > 0 && self.current <= t)
    }

    /// `worst <= threshold`: it failed in the past even if it recovered.
    pub fn failed_in_past(&self) -> bool {
        self.threshold.is_some_and(|t| t > 0 && self.worst <= t)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtaSmart {
    pub revision: u16,
    pub attributes: Vec<AtaAttribute>,
}

impl AtaSmart {
    pub fn get(&self, id: u8) -> Option<&AtaAttribute> {
        self.attributes.iter().find(|a| a.id == id)
    }

    pub fn raw(&self, id: u8) -> Option<u64> {
        self.get(id).map(|a| a.raw)
    }

    pub fn temperature_c(&self) -> Option<i32> {
        match self.get(194)?.decode() {
            Decoded::Celsius(t) => Some(t),
            _ => None,
        }
    }

    pub fn power_on_hours(&self) -> Option<u64> {
        self.get(9).map(|a| a.raw & 0xFFFF_FFFF)
    }

    /// Bytes written, assuming 512-byte LBAs (attribute 241 is vendor dependent: an estimate).
    pub fn bytes_written_estimate(&self) -> Option<u128> {
        self.raw(241).map(|l| u128::from(l) * 512)
    }

    pub fn bytes_read_estimate(&self) -> Option<u128> {
        self.raw(242).map(|l| u128::from(l) * 512)
    }
}

fn checksum_ok(page: &[u8]) -> bool {
    page[..PAGE_LEN].iter().fold(0u8, |s, &b| s.wrapping_add(b)) == 0
}

/// Parses the 512-byte SMART READ DATA page. `thresholds` is the 512-byte SMART READ THRESHOLDS page, if available.
///
/// Rejects all-zero pages and pages with a bad checksum: a USB bridge that fakes the command must not look like data.
pub fn parse_smart(data: &[u8], thresholds: Option<&[u8]>) -> Result<AtaSmart> {
    if data.len() < PAGE_LEN {
        return Err(Error::Parse(format!(
            "SMART data page is {} bytes, expected {PAGE_LEN}",
            data.len()
        )));
    }
    if data[..PAGE_LEN].iter().all(|&b| b == 0) {
        return Err(Error::Parse("SMART data page is all zeros".into()));
    }
    if !checksum_ok(data) {
        return Err(Error::Parse("SMART data page checksum mismatch".into()));
    }
    let thr: Option<&[u8]> =
        thresholds.filter(|t| t.len() >= PAGE_LEN && checksum_ok(t) && t[..PAGE_LEN].iter().any(|&b| b != 0));
    let mut attributes = Vec::new();
    for i in 0..ATTR_COUNT {
        let o = 2 + i * ATTR_LEN;
        let id = data[o];
        if id == 0 {
            continue;
        }
        let mut raw = 0u64;
        for (k, b) in data[o + 5..o + 11].iter().enumerate() {
            raw |= u64::from(*b) << (8 * k);
        }
        let threshold = thr.and_then(|t| {
            (0..ATTR_COUNT)
                .map(|j| 2 + j * ATTR_LEN)
                .find(|&to| t[to] == id)
                .map(|to| t[to + 1])
        });
        attributes.push(AtaAttribute::new(
            id,
            u16::from_le_bytes([data[o + 1], data[o + 2]]),
            data[o + 3],
            data[o + 4],
            raw,
            threshold,
        ));
    }
    if attributes.is_empty() {
        return Err(Error::Parse("SMART data page has no attributes".into()));
    }
    Ok(AtaSmart {
        revision: u16::from_le_bytes([data[0], data[1]]),
        attributes,
    })
}

// ---- Windows transports -----------------------------------------------------------------------------------------

/// Why an ATA SMART read did not produce data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtaFailure {
    /// The process lacks the rights to send pass-through commands.
    AccessDenied,
    /// The device or bridge refused / does not implement the command.
    Rejected(String),
    /// A reply came back but is not a valid SMART page.
    Invalid(String),
}

#[cfg(windows)]
pub use win::{RawPages, read_raw_ata, read_raw_sat, read_smart_ata, read_smart_sat};

#[cfg(windows)]
mod win {
    use std::mem::{offset_of, size_of};

    use windows_sys::Win32::Storage::IscsiDisc::{
        ATA_FLAGS_DATA_IN, ATA_FLAGS_DRDY_REQUIRED, ATA_PASS_THROUGH_EX, IOCTL_ATA_PASS_THROUGH,
        IOCTL_SCSI_PASS_THROUGH_DIRECT, SCSI_PASS_THROUGH_DIRECT,
    };
    use windows_sys::Win32::System::Ioctl::SMART_RCV_DRIVE_DATA;

    use super::*;
    use crate::winapi::{Device, GENERIC_READ, GENERIC_WRITE};

    const SCSI_IOCTL_DATA_IN: u8 = 1;

    #[repr(C, align(512))]
    struct Sector([u8; PAGE_LEN]);

    #[repr(C, align(512))]
    struct AtaBuf {
        hdr: ATA_PASS_THROUGH_EX,
        _pad: [u8; 512 - size_of::<ATA_PASS_THROUGH_EX>()],
        data: Sector,
    }

    #[repr(C, align(512))]
    struct SptBuf {
        sptd: SCSI_PASS_THROUGH_DIRECT,
        _pad: [u8; 512 - size_of::<SCSI_PASS_THROUGH_DIRECT>()],
        data: Sector,
        sense: [u8; 32],
    }

    /// Opens a physical drive for SMART pass-through. Windows requires read+write access for these IOCTLs even though
    /// only the read-only commands of [`AtaCommand`] are ever sent.
    fn open(path: &str) -> Result<Device> {
        Device::open(path, GENERIC_READ | GENERIC_WRITE, 0)
    }

    fn classify(e: Error) -> AtaFailure {
        if e.is_access_denied() {
            AtaFailure::AccessDenied
        } else {
            AtaFailure::Rejected(e.to_string())
        }
    }

    fn ata_passthrough(dev: &Device, cmd: AtaCommand) -> std::result::Result<[u8; PAGE_LEN], AtaFailure> {
        let t = cmd.task_file();
        // SAFETY: AtaBuf is plain data; all-zero is a valid value for every field.
        let mut b: Box<AtaBuf> = Box::new(unsafe { std::mem::zeroed() });
        b.hdr.Length = size_of::<ATA_PASS_THROUGH_EX>() as u16;
        b.hdr.AtaFlags = (ATA_FLAGS_DATA_IN | ATA_FLAGS_DRDY_REQUIRED) as u16;
        b.hdr.DataTransferLength = PAGE_LEN as u32;
        b.hdr.TimeOutValue = 5;
        b.hdr.DataBufferOffset = offset_of!(AtaBuf, data) as _;
        b.hdr.CurrentTaskFile = [
            t.features, t.count, t.lba_low, t.lba_mid, t.lba_high, t.device, t.command, 0,
        ];
        let len = size_of::<AtaBuf>();
        // SAFETY: `b` is a live, correctly laid out ATA_PASS_THROUGH_EX request followed by its data buffer.
        unsafe { dev.ioctl_inout(IOCTL_ATA_PASS_THROUGH, (&mut *b as *mut AtaBuf).cast(), len) }.map_err(classify)?;
        let status = b.hdr.CurrentTaskFile[6];
        if status & 0x01 != 0 {
            return Err(AtaFailure::Rejected(format!(
                "ATA error {:#04x}",
                b.hdr.CurrentTaskFile[0]
            )));
        }
        Ok(b.data.0)
    }

    /// Legacy `SMART_RCV_DRIVE_DATA` (SENDCMDINPARAMS / SENDCMDOUTPARAMS), used when pass-through is refused.
    fn smart_rcv(dev: &Device, number: u32, cmd: AtaCommand) -> std::result::Result<[u8; PAGE_LEN], AtaFailure> {
        let t = cmd.task_file();
        let mut input = [0u8; 36];
        input[0..4].copy_from_slice(&(PAGE_LEN as u32).to_le_bytes()); // cBufferSize
        input[4..12].copy_from_slice(&[
            t.features,
            t.count,
            t.lba_low,
            t.lba_mid,
            t.lba_high,
            0xA0 | (((number & 1) as u8) << 4),
            t.command,
            0,
        ]);
        input[12] = number as u8; // bDriveNumber
        let mut out = vec![0u8; 16 + PAGE_LEN];
        let n = dev.ioctl(SMART_RCV_DRIVE_DATA, &input, &mut out).map_err(classify)? as usize;
        if n < 16 + PAGE_LEN {
            return Err(AtaFailure::Rejected("short SMART_RCV_DRIVE_DATA reply".into()));
        }
        if out[4] != 0 {
            return Err(AtaFailure::Rejected(format!(
                "driver error {:#04x}, IDE error {:#04x}",
                out[4], out[5]
            )));
        }
        let mut page = [0u8; PAGE_LEN];
        page.copy_from_slice(&out[16..16 + PAGE_LEN]);
        Ok(page)
    }

    /// The two raw 512-byte pages: SMART data and (optionally) thresholds.
    pub type RawPages = ([u8; PAGE_LEN], Option<[u8; PAGE_LEN]>);

    fn read_pages<F>(mut get: F) -> std::result::Result<RawPages, AtaFailure>
    where
        F: FnMut(AtaCommand) -> std::result::Result<[u8; PAGE_LEN], AtaFailure>,
    {
        let data = get(AtaCommand::SmartReadData)?;
        // The thresholds page is optional (obsolete in newer ATA standards); without it we still show the table.
        Ok((data, get(AtaCommand::SmartReadThresholds).ok()))
    }

    fn parse_pages(p: RawPages) -> std::result::Result<AtaSmart, AtaFailure> {
        parse_smart(&p.0, p.1.as_ref().map(|t| t.as_slice())).map_err(|e| AtaFailure::Invalid(e.to_string()))
    }

    /// Native SATA/ATA path: `IOCTL_ATA_PASS_THROUGH`, then the legacy `SMART_RCV_DRIVE_DATA`. Returns unparsed pages.
    pub fn read_raw_ata(path: &str, number: u32) -> std::result::Result<RawPages, AtaFailure> {
        let dev = open(path).map_err(classify)?;
        match read_pages(|c| ata_passthrough(&dev, c)) {
            Ok(s) => Ok(s),
            Err(AtaFailure::AccessDenied) => Err(AtaFailure::AccessDenied),
            Err(first) => read_pages(|c| smart_rcv(&dev, number, c)).map_err(|_| first),
        }
    }

    pub fn read_smart_ata(path: &str, number: u32) -> std::result::Result<AtaSmart, AtaFailure> {
        parse_pages(read_raw_ata(path, number)?)
    }

    fn sat_once(dev: &Device, cdb: &[u8]) -> std::result::Result<[u8; PAGE_LEN], AtaFailure> {
        // SAFETY: SptBuf is plain data; all-zero is valid.
        let mut b: Box<SptBuf> = Box::new(unsafe { std::mem::zeroed() });
        b.sptd.Length = size_of::<SCSI_PASS_THROUGH_DIRECT>() as u16;
        b.sptd.CdbLength = cdb.len() as u8;
        b.sptd.SenseInfoLength = 32;
        b.sptd.DataIn = SCSI_IOCTL_DATA_IN;
        b.sptd.DataTransferLength = PAGE_LEN as u32;
        b.sptd.TimeOutValue = 5;
        b.sptd.DataBuffer = b.data.0.as_mut_ptr().cast();
        b.sptd.SenseInfoOffset = offset_of!(SptBuf, sense) as u32;
        b.sptd.Cdb[..cdb.len()].copy_from_slice(cdb);
        let len = size_of::<SptBuf>();
        // SAFETY: `b` is a live SCSI_PASS_THROUGH_DIRECT request; DataBuffer points into the same box.
        unsafe { dev.ioctl_inout(IOCTL_SCSI_PASS_THROUGH_DIRECT, (&mut *b as *mut SptBuf).cast(), len) }
            .map_err(classify)?;
        if b.sptd.ScsiStatus != 0 {
            let (key, asc) = (b.sense[2] & 0x0F, b.sense[12]);
            return Err(AtaFailure::Rejected(format!(
                "SCSI status {}, sense key {key:#x}, ASC {asc:#04x}",
                b.sptd.ScsiStatus
            )));
        }
        Ok(b.data.0)
    }

    /// USB path: SCSI-ATA Translation, ATA PASS-THROUGH (16) then (12). Returns unparsed pages.
    pub fn read_raw_sat(path: &str) -> std::result::Result<RawPages, AtaFailure> {
        let dev = open(path).map_err(classify)?;
        match read_pages(|c| sat_once(&dev, &sat_cdb16(c))) {
            Ok(s) => Ok(s),
            Err(AtaFailure::AccessDenied) => Err(AtaFailure::AccessDenied),
            Err(first) => read_pages(|c| sat_once(&dev, &sat_cdb12(c))).map_err(|_| first),
        }
    }

    pub fn read_smart_sat(path: &str) -> std::result::Result<AtaSmart, AtaFailure> {
        parse_pages(read_raw_sat(path)?)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn windows_struct_layout_assumptions() {
            assert_eq!(offset_of!(AtaBuf, data) % 512, 0);
            assert_eq!(offset_of!(SptBuf, data) % 512, 0);
            assert!(size_of::<ATA_PASS_THROUGH_EX>() <= 512 && size_of::<SCSI_PASS_THROUGH_DIRECT>() <= 512);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn put_attr(page: &mut [u8], slot: usize, id: u8, flags: u16, cur: u8, worst: u8, raw: u64) {
        let o = 2 + slot * ATTR_LEN;
        page[o] = id;
        page[o + 1..o + 3].copy_from_slice(&flags.to_le_bytes());
        page[o + 3] = cur;
        page[o + 4] = worst;
        page[o + 5..o + 11].copy_from_slice(&raw.to_le_bytes()[..6]);
    }

    pub(crate) fn seal(page: &mut [u8]) {
        page[511] = 0;
        let sum = page[..511].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        page[511] = 0u8.wrapping_sub(sum);
    }

    /// Synthetic SATA SSD page: attributes 5, 9, 12, 194, 197, 241 (+ one unknown, 0xAB).
    pub(crate) fn sample_data() -> Vec<u8> {
        let mut p = vec![0u8; PAGE_LEN];
        p[0] = 0x10;
        put_attr(&mut p, 0, 5, 0x0033, 100, 100, 0);
        put_attr(&mut p, 1, 9, 0x0032, 99, 99, 12_345);
        put_attr(&mut p, 2, 12, 0x0032, 99, 99, 321);
        put_attr(&mut p, 3, 194, 0x0022, 67, 50, (45u64) | (20 << 16) | (60 << 32));
        put_attr(&mut p, 4, 197, 0x0032, 100, 100, 4);
        put_attr(&mut p, 5, 241, 0x0030, 100, 100, 2_000_000_000);
        put_attr(&mut p, 6, 0xAB, 0x0032, 77, 70, 0x0102_0304_0506);
        seal(&mut p);
        p
    }

    pub(crate) fn sample_thresholds() -> Vec<u8> {
        let mut p = vec![0u8; PAGE_LEN];
        for (slot, (id, thr)) in [(5u8, 10u8), (9, 0), (12, 0), (194, 0), (197, 0), (241, 0), (0xAB, 50)]
            .into_iter()
            .enumerate()
        {
            let o = 2 + slot * ATTR_LEN;
            p[o] = id;
            p[o + 1] = thr;
        }
        seal(&mut p);
        p
    }

    #[test]
    fn parses_attributes_with_thresholds() {
        let s = parse_smart(&sample_data(), Some(&sample_thresholds())).unwrap();
        assert_eq!(s.attributes.len(), 7);
        let a5 = s.get(5).unwrap();
        assert_eq!((a5.current, a5.threshold, a5.raw), (100, Some(10), 0));
        assert!(a5.prefailure() && !a5.failing_now());
        assert_eq!(s.power_on_hours(), Some(12_345));
        assert_eq!(s.temperature_c(), Some(45));
        assert_eq!(s.raw(197), Some(4));
        assert_eq!(s.bytes_written_estimate(), Some(2_000_000_000u128 * 512));
        assert_eq!(s.revision, 0x10);
    }

    #[test]
    fn unknown_attribute_is_shown_as_is() {
        let s = parse_smart(&sample_data(), Some(&sample_thresholds())).unwrap();
        let a = s.get(0xAB).unwrap();
        assert!(a.name.is_empty() && !a.vendor_dependent);
        assert_eq!(a.raw, 0x0102_0304_0506);
        assert_eq!(a.decode(), Decoded::Raw(0x0102_0304_0506));
        assert_eq!(a.threshold, Some(50));
        assert!(!a.failing_now());
    }

    #[test]
    fn raw_is_exactly_48_bits() {
        let mut p = sample_data();
        put_attr(&mut p, 5, 241, 0x0030, 100, 100, u64::MAX);
        seal(&mut p);
        assert_eq!(parse_smart(&p, None).unwrap().raw(241), Some(0xFFFF_FFFF_FFFF));
    }

    #[test]
    fn failing_and_past_failures_use_thresholds() {
        let mut p = sample_data();
        put_attr(&mut p, 0, 5, 0x0033, 8, 8, 500);
        seal(&mut p);
        let s = parse_smart(&p, Some(&sample_thresholds())).unwrap();
        assert!(s.get(5).unwrap().failing_now());
        put_attr(&mut p, 0, 5, 0x0033, 90, 9, 500);
        seal(&mut p);
        let s = parse_smart(&p, Some(&sample_thresholds())).unwrap();
        let a = s.get(5).unwrap();
        assert!(!a.failing_now() && a.failed_in_past());
    }

    #[test]
    fn works_without_thresholds_page() {
        let s = parse_smart(&sample_data(), None).unwrap();
        assert!(s.attributes.iter().all(|a| a.threshold.is_none() && !a.failing_now()));
        // a broken thresholds page is ignored rather than trusted
        let mut bad = sample_thresholds();
        bad[20] ^= 0xFF;
        assert!(
            parse_smart(&sample_data(), Some(&bad))
                .unwrap()
                .attributes
                .iter()
                .all(|a| a.threshold.is_none())
        );
    }

    #[test]
    fn rejects_zero_page_bad_checksum_and_short_data() {
        assert!(parse_smart(&[0u8; PAGE_LEN], None).is_err());
        let mut p = sample_data();
        p[100] ^= 1;
        assert!(parse_smart(&p, None).unwrap_err().to_string().contains("checksum"));
        assert!(parse_smart(&p[..100], None).is_err());
        // valid checksum but no attributes
        let mut empty = vec![0u8; PAGE_LEN];
        empty[0] = 1;
        seal(&mut empty);
        assert!(parse_smart(&empty, None).is_err());
    }

    #[test]
    fn temperature_decoding_ignores_min_max_and_rejects_garbage() {
        let s = parse_smart(&sample_data(), None).unwrap();
        assert_eq!(s.get(194).unwrap().decode(), Decoded::Celsius(45));
        let mut p = sample_data();
        put_attr(&mut p, 3, 194, 0x0022, 67, 50, 200);
        seal(&mut p);
        assert_eq!(parse_smart(&p, None).unwrap().temperature_c(), None);
    }

    #[test]
    fn command_allowlist_is_smart_read_only() {
        for c in [AtaCommand::SmartReadData, AtaCommand::SmartReadThresholds] {
            let t = c.task_file();
            assert_eq!((t.command, t.lba_mid, t.lba_high), (0xB0, 0x4F, 0xC2));
        }
        assert_eq!(AtaCommand::SmartReadData.task_file().features, 0xD0);
        assert_eq!(AtaCommand::SmartReadThresholds.task_file().features, 0xD1);
    }

    #[test]
    fn sat_cdbs_encode_the_same_task_file() {
        let c16 = sat_cdb16(AtaCommand::SmartReadData);
        assert_eq!(
            (c16[0], c16[1], c16[2], c16[4], c16[6], c16[10], c16[12], c16[14]),
            (0x85, 0x08, 0x0E, 0xD0, 1, 0x4F, 0xC2, 0xB0)
        );
        let c12 = sat_cdb12(AtaCommand::SmartReadThresholds);
        assert_eq!((c12[0], c12[3], c12[6], c12[7], c12[9]), (0xA1, 0xD1, 0x4F, 0xC2, 0xB0));
    }
}
