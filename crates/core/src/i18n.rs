//! Localization table. Every user-visible string lives here as `(key, Russian, English)`, so a missing translation is
//! impossible by construction (and tested). Placeholders are `{0}`, `{1}`, ...

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ru,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }

    pub fn from_code(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ru" => Some(Lang::Ru),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

/// Returned for a key that is not in the table (a bug; covered by tests).
pub const MISSING: &str = "\u{27e6}?\u{27e7}";

fn index() -> &'static HashMap<&'static str, (&'static str, &'static str)> {
    static INDEX: OnceLock<HashMap<&'static str, (&'static str, &'static str)>> = OnceLock::new();
    INDEX.get_or_init(|| TABLE.iter().map(|&(k, ru, en)| (k, (ru, en))).collect())
}

pub fn tr(lang: Lang, key: &str) -> &'static str {
    match index().get(key) {
        Some(&(ru, en)) => match lang {
            Lang::Ru => ru,
            Lang::En => en,
        },
        None => MISSING,
    }
}

/// `tr` with `{0}`, `{1}`, ... replaced by `args`.
pub fn fmt(lang: Lang, key: &str, args: &[String]) -> String {
    let mut s = tr(lang, key).to_owned();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

pub const TABLE: &[(&str, &str, &str)] = &[
    // ---- assessment levels -------------------------------------------------------------------------------------
    ("assess.title", "Состояние", "Condition"),
    ("level.good", "Хорошо", "Good"),
    ("level.warning", "Внимание", "Warning"),
    ("level.bad", "Плохо", "Bad"),
    ("level.unknown", "Нет данных", "No data"),
    ("basis.spec", "по спецификации", "per specification"),
    ("basis.vendor", "зависит от вендора", "vendor dependent"),
    ("basis.heuristic", "эвристика DiskPulse", "DiskPulse heuristic"),
    (
        "assess.all_ok",
        "Ни одно из правил не сработало.",
        "None of the rules fired.",
    ),
    // ---- rules (NVMe: N*, ATA: A*) -----------------------------------------------------------------------------
    (
        "rule.N1",
        "Критическое предупреждение контроллера: {0}",
        "Controller critical warning: {0}",
    ),
    (
        "rule.N2",
        "Резерв ячеек {0}% не выше порога {1}%",
        "Available spare {0}% is not above its threshold {1}%",
    ),
    (
        "rule.N3",
        "Резерв ячеек снизился до {0}% (меньше 50%)",
        "Available spare dropped to {0}% (below 50%)",
    ),
    (
        "rule.N4",
        "Использовано {0}% заявленного ресурса — он исчерпан по оценке производителя",
        "{0}% of the rated endurance used: exhausted by the vendor's estimate",
    ),
    (
        "rule.N5",
        "Использовано {0}% заявленного ресурса (порог 80%)",
        "{0}% of the rated endurance used (threshold 80%)",
    ),
    (
        "rule.N6",
        "Ошибки целостности данных и носителя: {0}",
        "Media and data integrity errors: {0}",
    ),
    (
        "rule.N7",
        "Температура {0} °C не ниже {1} °C (порог перегрева + 10)",
        "Temperature {0} \u{b0}C is at or above {1} \u{b0}C (overheat threshold + 10)",
    ),
    (
        "rule.N8",
        "Температура {0} °C не ниже порога перегрева {1} °C",
        "Temperature {0} \u{b0}C is at or above the overheat threshold {1} \u{b0}C",
    ),
    (
        "rule.N9",
        "Диск провёл {0} мин при критической температуре",
        "The drive spent {0} min at critical temperature",
    ),
    (
        "rule.A1",
        "Атрибут {0}: значение {1} не выше порога {2} — диск сам считает его отказавшим",
        "Attribute {0}: value {1} is at or below its threshold {2} \u{2014} the drive itself reports it as failing",
    ),
    (
        "rule.A2",
        "Атрибут {0}: худшее значение {1} опускалось до порога {2}",
        "Attribute {0}: its worst value {1} reached the threshold {2}",
    ),
    (
        "rule.A3",
        "{0}: сырое значение {1} (должно быть 0)",
        "{0}: raw value {1} (should be 0)",
    ),
    (
        "rule.A4",
        "{0}: сырое значение {1} (не 0; трактовка зависит от вендора)",
        "{0}: raw value {1} (non-zero; interpretation depends on the vendor)",
    ),
    (
        "rule.A5",
        "Переназначенных, нестабильных и неисправимых секторов вместе: {0} (порог 100)",
        "Reallocated, pending and uncorrectable sectors combined: {0} (threshold 100)",
    ),
    (
        "rule.A6",
        "{0}: нормализованное значение {1} (не больше 20) — остаток ресурса по оценке вендора",
        "{0}: normalized value {1} (20 or less) \u{2014} remaining life by the vendor's estimate",
    ),
    (
        "rule.A7",
        "Температура {0} °C не ниже {1} °C (порог перегрева + 10)",
        "Temperature {0} \u{b0}C is at or above {1} \u{b0}C (overheat threshold + 10)",
    ),
    (
        "rule.A8",
        "Температура {0} °C не ниже порога перегрева {1} °C",
        "Temperature {0} \u{b0}C is at or above the overheat threshold {1} \u{b0}C",
    ),
    // ---- SMART unavailable -------------------------------------------------------------------------------------
    (
        "unavail.needs_admin",
        "Нужны права администратора",
        "Administrator rights required",
    ),
    (
        "unavail.needs_admin.hint",
        "Чтобы прочитать S.M.A.R.T. этого диска, нужен доступ к сырому устройству. Нажмите «Открыть от администратора».",
        "Reading this disk's S.M.A.R.T. needs raw device access. Click \"Open as administrator\".",
    ),
    (
        "unavail.usb_bridge",
        "SMART недоступен через этот USB-адаптер",
        "SMART is unavailable through this USB adapter",
    ),
    (
        "unavail.usb_bridge.hint",
        "Адаптер не пропускает SMART-команды (или не поддерживает SCSI-ATA Translation). Это ограничение адаптера: диск может быть исправен. Подключите его напрямую по SATA/NVMe или используйте адаптер с поддержкой SAT.",
        "The adapter does not pass SMART commands through (or lacks SCSI-ATA Translation). This is a limit of the adapter; the disk may be fine. Connect it directly over SATA/NVMe or use an adapter with SAT support.",
    ),
    (
        "unavail.unsupported",
        "S.M.A.R.T. недоступен для этого устройства",
        "S.M.A.R.T. is not available for this device",
    ),
    (
        "unavail.unsupported.hint",
        "Драйвер, контроллер или виртуальный диск не предоставляет S.M.A.R.T.",
        "The driver, controller or virtual disk does not expose S.M.A.R.T.",
    ),
    (
        "unavail.invalid",
        "Диск вернул некорректные данные",
        "The disk returned invalid data",
    ),
    (
        "unavail.invalid.hint",
        "Ответ не прошёл проверку структуры или контрольной суммы, поэтому он не показывается.",
        "The reply failed the structure or checksum check, so it is not shown.",
    ),
    ("unavail.detail", "Технические детали", "Technical details"),
    // ---- NVMe fields -------------------------------------------------------------------------------------------
    ("nv.temperature", "Температура", "Temperature"),
    ("nv.sensors", "Датчики температуры", "Temperature sensors"),
    ("nv.spare", "Резерв ячеек (Available Spare)", "Available Spare"),
    ("nv.spare_thr", "Порог резерва", "Spare threshold"),
    ("nv.used", "Использовано ресурса (Percentage Used)", "Percentage Used"),
    ("nv.read", "Прочитано", "Data read"),
    ("nv.written", "Записано", "Data written"),
    ("nv.poh", "Наработка", "Power-on time"),
    ("nv.cycles", "Включений", "Power cycles"),
    ("nv.unsafe", "Небезопасных выключений", "Unsafe shutdowns"),
    ("nv.media", "Ошибки носителя (Media Errors)", "Media errors"),
    ("nv.errlog", "Записей в журнале ошибок", "Error log entries"),
    ("nv.crit", "Критическое предупреждение", "Critical warning"),
    ("nv.crit.none", "нет", "none"),
    ("nv.busy", "Время занятости контроллера", "Controller busy time"),
    (
        "nv.warn_t",
        "Время при повышенной температуре",
        "Time above warning temperature",
    ),
    (
        "nv.crit_t",
        "Время при критической температуре",
        "Time above critical temperature",
    ),
    ("cw.spare_low", "резерв ниже порога", "spare below threshold"),
    (
        "cw.temperature",
        "температура вне допустимой",
        "temperature out of range",
    ),
    ("cw.reliability_degraded", "надёжность снижена", "reliability degraded"),
    ("cw.read_only", "носитель только для чтения", "media is read-only"),
    (
        "cw.volatile_backup_failed",
        "отказ резервного питания кэша",
        "volatile memory backup failed",
    ),
    (
        "cw.pmr_read_only",
        "PMR только для чтения",
        "persistent memory region is read-only",
    ),
    // ---- ATA table ---------------------------------------------------------------------------------------------
    ("ata.id", "ID", "ID"),
    ("ata.name", "Атрибут", "Attribute"),
    ("ata.cur", "Знач.", "Value"),
    ("ata.worst", "Худш.", "Worst"),
    ("ata.thr", "Порог", "Thresh."),
    ("ata.raw", "Сырое", "Raw"),
    ("ata.decoded", "Расшифровка", "Decoded"),
    ("ata.unknown", "неизвестный атрибут", "unknown attribute"),
    ("ata.vendor_dep", "зависит от вендора", "vendor dependent"),
    (
        "ata.thr_missing",
        "пороги диском не отданы",
        "the drive did not provide thresholds",
    ),
    ("ata.hours", "{0} ч", "{0} h"),
    (
        "ata.lbas_est",
        "{0} (оценка: LBA по 512 Б)",
        "{0} (estimate: 512-byte LBAs)",
    ),
    (
        "attr.5",
        "Переназначенные секторы: сколько секторов диск заменил запасными. Рост — признак деградации поверхности.",
        "Reallocated sectors: how many sectors the drive replaced with spares. Growth means surface degradation.",
    ),
    (
        "attr.9",
        "Наработка в часах (у некоторых вендоров единицы другие).",
        "Power-on time in hours (some vendors use other units).",
    ),
    ("attr.12", "Число включений питания.", "Number of power cycles."),
    (
        "attr.177",
        "Wear Leveling Count: износ ячеек SSD; смысл значений зависит от вендора.",
        "Wear Leveling Count: SSD cell wear; the meaning depends on the vendor.",
    ),
    (
        "attr.187",
        "Неисправимые ошибки, о которых сообщено хосту; смысл зависит от вендора.",
        "Uncorrectable errors reported to the host; the meaning depends on the vendor.",
    ),
    (
        "attr.194",
        "Температура диска (младший байт сырого значения).",
        "Drive temperature (low byte of the raw value).",
    ),
    (
        "attr.196",
        "Число событий переназначения секторов.",
        "Number of sector reallocation events.",
    ),
    (
        "attr.197",
        "Сектора, ожидающие переназначения: диск не смог их прочитать.",
        "Pending sectors: the drive could not read them and waits to reallocate.",
    ),
    (
        "attr.198",
        "Неисправимые сектора, найденные при фоновой проверке диска.",
        "Uncorrectable sectors found by offline scanning.",
    ),
    (
        "attr.231",
        "Остаток ресурса SSD (нормализованное значение); смысл зависит от вендора.",
        "SSD life left (normalized value); the meaning depends on the vendor.",
    ),
    (
        "attr.233",
        "Индикатор износа носителя; у разных вендоров означает разное.",
        "Media wearout indicator; it means different things for different vendors.",
    ),
    (
        "attr.241",
        "Всего записано LBA; размер LBA и единицы зависят от вендора.",
        "Total LBAs written; LBA size and units depend on the vendor.",
    ),
    (
        "attr.242",
        "Всего прочитано LBA; размер LBA и единицы зависят от вендора.",
        "Total LBAs read; LBA size and units depend on the vendor.",
    ),
    // ---- units -------------------------------------------------------------------------------------------------
    ("unit.b", "Б", "B"),
    ("unit.kb", "КБ", "KB"),
    ("unit.mb", "МБ", "MB"),
    ("unit.gb", "ГБ", "GB"),
    ("unit.tb", "ТБ", "TB"),
    ("unit.pb", "ПБ", "PB"),
    ("unit.days", "дн.", "d"),
    ("unit.hours", "ч", "h"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = (0..10u8).filter(|i| s.contains(&format!("{{{i}}}"))).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn keys_unique_and_texts_non_empty() {
        let mut seen = std::collections::HashSet::new();
        for (k, ru, en) in TABLE {
            assert!(seen.insert(*k), "duplicate key {k}");
            assert!(!ru.trim().is_empty() && !en.trim().is_empty(), "empty text for {k}");
        }
    }

    #[test]
    fn placeholders_match_between_languages() {
        for (k, ru, en) in TABLE {
            assert_eq!(placeholders(ru), placeholders(en), "placeholder mismatch in {k}");
        }
    }

    #[test]
    fn russian_and_english_actually_differ_where_expected() {
        assert_eq!(tr(Lang::Ru, "level.good"), "Хорошо");
        assert_eq!(tr(Lang::En, "level.good"), "Good");
        assert_eq!(tr(Lang::En, "no.such.key"), MISSING);
    }

    #[test]
    fn fmt_substitutes_arguments() {
        assert_eq!(
            fmt(Lang::En, "rule.N6", &["7".into()]),
            "Media and data integrity errors: 7"
        );
        assert_eq!(
            fmt(Lang::Ru, "rule.N2", &["8".into(), "10".into()]),
            "Резерв ячеек 8% не выше порога 10%"
        );
    }

    #[test]
    fn language_codes_roundtrip() {
        assert_eq!(Lang::from_code(" EN "), Some(Lang::En));
        assert_eq!(Lang::from_code(Lang::Ru.code()), Some(Lang::Ru));
        assert_eq!(Lang::from_code("de"), None);
    }
}
