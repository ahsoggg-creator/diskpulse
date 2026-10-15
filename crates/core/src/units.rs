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

/// `2026-10-13 18:04:09` (UTC).
pub fn format_utc(unix: u64) -> String {
    let (days, rem) = (unix / 86_400, unix % 86_400);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// `3 d` / `3 дн.` for a span in seconds (whole days; under a day shows hours).
pub fn human_span(lang: Lang, secs: u64) -> String {
    let days = secs / 86_400;
    if days >= 730 {
        return format!("{} {}", days / 365, tr(lang, "unit.years"));
    }
    if days == 0 {
        human_hours(lang, u128::from(secs / 3600))
    } else {
        format!("{days} {}", tr(lang, "unit.days"))
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
    fn utc_timestamps() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00");
        assert_eq!(format_utc(951_782_400 + 86_399), "2000-02-29 23:59:59");
        assert_eq!(human_span(Lang::En, 3 * 86_400 + 5), "3 d");
        assert_eq!(human_span(Lang::En, 26_226 * 86_400), "71 y");
    }

    #[test]
    fn hours_split_into_days() {
        assert_eq!(human_hours(Lang::En, 12_345), "514 d 9 h");
        assert_eq!(human_hours(Lang::Ru, 5), "5 ч");
    }
}
