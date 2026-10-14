//! Parsers against pages recorded from real drives (`diskpulse-cli smart <n> --dump-raw`).
//! The pages contain no serial numbers: the NVMe SMART log and the ATA SMART pages carry counters only.

use diskpulse_core::assess::{Level, Thresholds, assess_nvme};
use diskpulse_core::nvme::parse_health_log;

const NVME_LOW_USE: &[u8] = include_bytes!("fixtures/nvme_health_512g_lowuse.bin");
const NVME_WORN: &[u8] = include_bytes!("fixtures/nvme_health_256g_worn.bin");

#[test]
fn recorded_nvme_page_of_a_lightly_used_drive() {
    let h = parse_health_log(NVME_LOW_USE).unwrap();
    assert_eq!(
        (h.available_spare, h.available_spare_threshold, h.percentage_used),
        (100, 10, 0)
    );
    assert_eq!((h.power_on_hours, h.unsafe_shutdowns, h.media_errors), (6482, 171, 0));
    assert_eq!(h.data_units_written, 13_511_147);
    assert_eq!(h.temperature_c, Some(53));
    assert_eq!(assess_nvme(&h, Thresholds::default()).level, Level::Good);
}

#[test]
fn recorded_nvme_page_of_a_worn_system_drive() {
    let h = parse_health_log(NVME_WORN).unwrap();
    assert_eq!((h.percentage_used, h.available_spare_threshold), (59, 5));
    assert_eq!((h.power_on_hours, h.unsafe_shutdowns), (20_868, 640));
    assert_eq!(h.data_units_written, 299_635_192);
    // 299 635 192 units x 512 000 bytes = about 153 TB
    assert_eq!(h.bytes_written(), 299_635_192u128 * 512_000);
    assert_eq!(assess_nvme(&h, Thresholds::default()).level, Level::Good);
}
