//! Human-readable numbers. Capacities and traffic use decimal units (GB = 10^9 bytes), as disk vendors do.

use crate::i18n::{Lang, tr};

/// `1_234_567_890` -> `1.23 GB` / `1,23 ГБ`.
pub fn human_bytes(lang: Lang, bytes: u128) -> String {
    const UNITS: [&str; 6] = ["unit.b", "unit.kb", "unit.mb", "unit.gb", "unit.tb", "unit.pb"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i + 1 < UNITS.len() {
        v /= 1000.0;
        i += 1;
    }
    let unit = tr(lang, UNITS[i]);
    let num = if i == 0 {
        format!("{}", bytes)
    } else if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    let num = if lang == Lang::Ru { num.replace('.', ",") } else { num };
    format!("{num} {unit}")
}

/// `12_345` hours -> `514 d 9 h` / `514 дн. 9 ч`.
pub fn human_hours(lang: Lang, hours: u128) -> String {
    let (d, h) = (hours / 24, hours % 24);
    if d == 0 {
        format!("{h} {}", tr(lang, "unit.hours"))
    } else {
        format!("{d} {} {h} {}", tr(lang, "unit.days"), tr(lang, "unit.hours"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_decimal_and_localized() {
        assert_eq!(human_bytes(Lang::En, 512), "512 B");
        assert_eq!(human_bytes(Lang::En, 1_500), "1.50 KB");
        assert_eq!(human_bytes(Lang::Ru, 1_234_567_890), "1,23 ГБ");
        assert_eq!(human_bytes(Lang::En, 477_000_000_000), "477 GB");
        assert_eq!(human_bytes(Lang::En, 2_500_000_000_000), "2.50 TB");
        assert_eq!(human_bytes(Lang::En, 12_300_000_000_000), "12.3 TB");
    }

    #[test]
    fn hours_split_into_days() {
        assert_eq!(human_hours(Lang::En, 12_345), "514 d 9 h");
        assert_eq!(human_hours(Lang::Ru, 5), "5 ч");
    }
}
