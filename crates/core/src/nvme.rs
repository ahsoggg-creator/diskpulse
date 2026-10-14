//! NVMe SMART / Health Information log (log page 02h): parser (pure) and the Windows query.
//!
//! Layout: NVM Express Base Specification, "SMART / Health Information (Log Identifier 02h)", 512 bytes.
//! Counters are 128-bit little-endian; they are kept as `u128` (a `u64` would silently truncate).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const LOG_LEN: usize = 512;
/// One "data unit" of the Data Units Read/Written counters is 1000 x 512 bytes.
pub const DATA_UNIT_BYTES: u128 = 512_000;

/// JSON carries these 128-bit counters as numbers up to `u64::MAX` (serde's buffered enums cannot hold `u128`);
/// in memory they stay `u128`. A real drive cannot reach 2^64 data units.
mod wide {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(u64::try_from(*v).unwrap_or(u64::MAX))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        u64::deserialize(d).map(u128::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvmeHealth {
    /// Raw Critical Warning byte; see [`critical_warning_flags`].
    pub critical_warning: u8,
    /// Composite temperature in degrees C (the log stores Kelvin); `None` when the controller reports 0.
    pub temperature_c: Option<i32>,
    pub available_spare: u8,
    pub available_spare_threshold: u8,
    /// The vendor estimate of life used; may exceed 100.
    pub percentage_used: u8,
    #[serde(with = "wide")]
    pub data_units_read: u128,
    #[serde(with = "wide")]
    pub data_units_written: u128,
    #[serde(with = "wide")]
    pub host_read_commands: u128,
    #[serde(with = "wide")]
    pub host_write_commands: u128,
    #[serde(with = "wide")]
    pub controller_busy_minutes: u128,
    #[serde(with = "wide")]
    pub power_cycles: u128,
    #[serde(with = "wide")]
    pub power_on_hours: u128,
    #[serde(with = "wide")]
    pub unsafe_shutdowns: u128,
    #[serde(with = "wide")]
    pub media_errors: u128,
    #[serde(with = "wide")]
    pub error_log_entries: u128,
    pub warning_temp_minutes: u32,
    pub critical_temp_minutes: u32,
    /// Additional temperature sensors (degrees C), only those the drive implements.
    pub sensors_c: Vec<i32>,
}

impl NvmeHealth {
    pub fn bytes_written(&self) -> u128 {
        self.data_units_written * DATA_UNIT_BYTES
    }

    pub fn bytes_read(&self) -> u128 {
        self.data_units_read * DATA_UNIT_BYTES
    }
}

fn u128_at(b: &[u8], off: usize) -> u128 {
    u128::from_le_bytes(b[off..off + 16].try_into().expect("16 bytes"))
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("4 bytes"))
}

fn kelvin(k: u16) -> Option<i32> {
    (k != 0).then(|| i32::from(k) - 273)
}

pub fn parse_health_log(b: &[u8]) -> Result<NvmeHealth> {
    if b.len() < LOG_LEN {
        return Err(Error::Parse(format!(
            "NVMe health log is {} bytes, expected {LOG_LEN}",
            b.len()
        )));
    }
    // An all-zero page is what some bridges return for an unsupported command: never present it as data.
    if b[..LOG_LEN].iter().all(|&x| x == 0) {
        return Err(Error::Parse("NVMe health log is all zeros".into()));
    }
    let sensors_c = (0..8).filter_map(|i| kelvin(u16_at(b, 200 + 2 * i))).collect();
    Ok(NvmeHealth {
        critical_warning: b[0],
        temperature_c: kelvin(u16_at(b, 1)),
        available_spare: b[3],
        available_spare_threshold: b[4],
        percentage_used: b[5],
        data_units_read: u128_at(b, 32),
        data_units_written: u128_at(b, 48),
        host_read_commands: u128_at(b, 64),
        host_write_commands: u128_at(b, 80),
        controller_busy_minutes: u128_at(b, 96),
        power_cycles: u128_at(b, 112),
        power_on_hours: u128_at(b, 128),
        unsafe_shutdowns: u128_at(b, 144),
        media_errors: u128_at(b, 160),
        error_log_entries: u128_at(b, 176),
        warning_temp_minutes: u32_at(b, 192),
        critical_temp_minutes: u32_at(b, 196),
        sensors_c,
    })
}

/// Names of the set bits of the Critical Warning byte (stable ids, localized by the UI).
pub fn critical_warning_flags(cw: u8) -> Vec<&'static str> {
    const NAMES: [&str; 6] = [
        "spare_low",
        "temperature",
        "reliability_degraded",
        "read_only",
        "volatile_backup_failed",
        "pmr_read_only",
    ];
    NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| cw & (1 << i) != 0)
        .map(|(_, n)| *n)
        .collect()
}

// ---- Windows query ----------------------------------------------------------------------------------------------

/// `STORAGE_PROPERTY_QUERY` header (8 bytes) + `STORAGE_PROTOCOL_SPECIFIC_DATA` (40 bytes) + the log page.
pub const QUERY_HEADER: usize = 8;
pub const PROTO_DATA_LEN: usize = 40;

/// Builds the `IOCTL_STORAGE_QUERY_PROPERTY` input for `StorageDeviceProtocolSpecificProperty` asking for log page 02h.
pub fn build_health_query() -> Vec<u8> {
    let mut buf = vec![0u8; QUERY_HEADER + PROTO_DATA_LEN + LOG_LEN];
    let put = |buf: &mut Vec<u8>, off: usize, v: u32| buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut buf, 0, 50); // StorageDeviceProtocolSpecificProperty
    put(&mut buf, 4, 0); // PropertyStandardQuery
    let p = QUERY_HEADER;
    put(&mut buf, p, 3); // ProtocolTypeNvme
    put(&mut buf, p + 4, 2); // NVMeDataTypeLogPage
    put(&mut buf, p + 8, 2); // ProtocolDataRequestValue = NVME_LOG_PAGE_HEALTH_INFO
    put(&mut buf, p + 12, 0); // ProtocolDataRequestSubValue (log offset, low)
    put(&mut buf, p + 16, PROTO_DATA_LEN as u32); // ProtocolDataOffset
    put(&mut buf, p + 20, LOG_LEN as u32); // ProtocolDataLength
    buf
}

/// Cuts the log page out of an `IOCTL_STORAGE_QUERY_PROPERTY` response
/// (`STORAGE_PROTOCOL_DATA_DESCRIPTOR`: 8-byte header, then protocol data whose `ProtocolDataOffset` locates the page).
pub fn extract_log(resp: &[u8]) -> Result<&[u8]> {
    const DESC_HEADER: usize = 8;
    if resp.len() < DESC_HEADER + PROTO_DATA_LEN {
        return Err(Error::Parse("short NVMe query response".into()));
    }
    let p = DESC_HEADER;
    let off = u32_at(resp, p + 16) as usize;
    let len = u32_at(resp, p + 20) as usize;
    let start = p + off;
    if len < LOG_LEN || start.checked_add(LOG_LEN).is_none_or(|e| e > resp.len()) {
        return Err(Error::Parse(
            "NVMe query response does not contain the health log".into(),
        ));
    }
    Ok(&resp[start..start + LOG_LEN])
}

/// The raw 512-byte log page as returned by the controller.
#[cfg(windows)]
pub fn read_log_raw(dev: &crate::winapi::Device) -> Result<Vec<u8>> {
    use windows_sys::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY;
    let input = build_health_query();
    let mut out = vec![0u8; input.len()];
    let n = dev.ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut out)? as usize;
    Ok(extract_log(&out[..n.min(out.len())])?.to_vec())
}

#[cfg(windows)]
pub fn read_health(dev: &crate::winapi::Device) -> Result<NvmeHealth> {
    parse_health_log(&read_log_raw(dev)?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic page (no real drive): 36 C, 100 % spare (threshold 10), 3 % used, 1 234 567 units written, ...
    /// laid out exactly as in the NVMe specification.
    pub(crate) fn sample_page() -> Vec<u8> {
        let mut b = vec![0u8; LOG_LEN];
        b[1..3].copy_from_slice(&(36u16 + 273).to_le_bytes());
        b[3] = 100;
        b[4] = 10;
        b[5] = 3;
        b[32..48].copy_from_slice(&9_876_543u128.to_le_bytes());
        b[48..64].copy_from_slice(&1_234_567u128.to_le_bytes());
        b[64..80].copy_from_slice(&111u128.to_le_bytes());
        b[80..96].copy_from_slice(&222u128.to_le_bytes());
        b[96..112].copy_from_slice(&333u128.to_le_bytes());
        b[112..128].copy_from_slice(&444u128.to_le_bytes());
        b[128..144].copy_from_slice(&5_555u128.to_le_bytes());
        b[144..160].copy_from_slice(&66u128.to_le_bytes());
        b[160..176].copy_from_slice(&7u128.to_le_bytes());
        b[176..192].copy_from_slice(&88u128.to_le_bytes());
        b[192..196].copy_from_slice(&9u32.to_le_bytes());
        b[196..200].copy_from_slice(&1u32.to_le_bytes());
        b[200..202].copy_from_slice(&(38u16 + 273).to_le_bytes());
        b[202..204].copy_from_slice(&(41u16 + 273).to_le_bytes());
        b
    }

    #[test]
    fn parses_known_page() {
        let h = parse_health_log(&sample_page()).unwrap();
        assert_eq!(h.temperature_c, Some(36));
        assert_eq!(
            (h.available_spare, h.available_spare_threshold, h.percentage_used),
            (100, 10, 3)
        );
        assert_eq!(h.data_units_written, 1_234_567);
        assert_eq!(h.bytes_written(), 1_234_567 * 512_000);
        assert_eq!(h.data_units_read, 9_876_543);
        assert_eq!((h.power_on_hours, h.unsafe_shutdowns, h.media_errors), (5_555, 66, 7));
        assert_eq!((h.warning_temp_minutes, h.critical_temp_minutes), (9, 1));
        assert_eq!(h.sensors_c, vec![38, 41]);
        assert_eq!(h.power_cycles, 444);
    }

    #[test]
    fn counters_keep_all_128_bits() {
        let mut b = sample_page();
        let big = (1u128 << 100) + 5;
        b[48..64].copy_from_slice(&big.to_le_bytes());
        assert_eq!(parse_health_log(&b).unwrap().data_units_written, big);
    }

    #[test]
    fn rejects_zero_and_short_pages() {
        assert!(parse_health_log(&[0u8; LOG_LEN]).is_err());
        assert!(parse_health_log(&sample_page()[..100]).is_err());
    }

    #[test]
    fn zero_kelvin_means_no_sensor() {
        let mut b = sample_page();
        b[1..3].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(parse_health_log(&b).unwrap().temperature_c, None);
    }

    #[test]
    fn critical_warning_bits() {
        assert!(critical_warning_flags(0).is_empty());
        assert_eq!(critical_warning_flags(0b0000_1001), vec!["spare_low", "read_only"]);
    }

    #[test]
    fn query_layout_and_extract_roundtrip() {
        let q = build_health_query();
        assert_eq!(q.len(), 8 + 40 + 512);
        assert_eq!(u32::from_le_bytes(q[0..4].try_into().unwrap()), 50);
        // simulate the driver: STORAGE_PROTOCOL_DATA_DESCRIPTOR = {Version, Size} + the same protocol data + page
        let mut resp = q.clone();
        resp[0..4].copy_from_slice(&1u32.to_le_bytes());
        resp[8 + 40..8 + 40 + 512].copy_from_slice(&sample_page());
        assert_eq!(extract_log(&resp).unwrap(), sample_page().as_slice());
        assert!(extract_log(&resp[..30]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn struct_sizes_match_windows_headers() {
        use windows_sys::Win32::System::Ioctl::{STORAGE_PROPERTY_QUERY, STORAGE_PROTOCOL_SPECIFIC_DATA};
        assert_eq!(std::mem::size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>(), PROTO_DATA_LEN);
        assert_eq!(
            std::mem::offset_of!(STORAGE_PROPERTY_QUERY, AdditionalParameters),
            QUERY_HEADER
        );
    }
}
