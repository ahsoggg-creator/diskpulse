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
    // ---- application window ------------------------------------------------------------------------------------
    (
        "app.title",
        "DiskPulse — состояние дисков",
        "DiskPulse \u{2014} disk health",
    ),
    ("tab.disks", "Диски", "Disks"),
    ("tab.settings", "Настройки", "Settings"),
    ("btn.refresh", "Обновить", "Refresh"),
    ("btn.admin", "Открыть от администратора", "Open as administrator"),
    (
        "btn.admin.hint",
        "Запускает отдельное окно с правами администратора (один запрос UAC). Это окно остаётся без повышения.",
        "Starts a separate window with administrator rights (one UAC prompt). This window stays unelevated.",
    ),
    (
        "admin.launched",
        "Окно администратора запущено.",
        "The administrator window was started.",
    ),
    (
        "admin.failed",
        "Не удалось запустить от администратора (запрос UAC отклонён?).",
        "Could not start as administrator (UAC prompt declined?).",
    ),
    ("disks.loading", "Чтение дисков…", "Reading disks\u{2026}"),
    ("disks.none", "Физические диски не найдены.", "No physical disks found."),
    ("disks.pick", "Выберите диск слева.", "Pick a disk on the left."),
    ("disk.firmware", "Прошивка", "Firmware"),
    ("disk.volumes", "Тома", "Volumes"),
    ("disk.serial", "Серийный номер", "Serial"),
    ("disk.serial.show", "показать", "show"),
    ("disk.serial.hide", "скрыть", "hide"),
    ("disk.method", "Метод чтения", "Read method"),
    (
        "assess.none_for_unreadable",
        "Для диска без данных S.M.A.R.T. оценка состояния не выставляется: «хорошо» без данных было бы выдумкой.",
        "A disk without S.M.A.R.T. data gets no verdict: \"good\" without data would be made up.",
    ),
    (
        "assess.no_percent",
        "Состояние — худший уровень среди сработавших правил. Процента «здоровья» нет: в S.M.A.R.T. его не существует.",
        "The condition is the worst level among the rules that fired. There is no \"health percentage\": S.M.A.R.T. does not contain one.",
    ),
    ("rules.show", "Показать все правила", "Show all rules"),
    ("rules.hide", "Скрыть правила", "Hide rules"),
    ("unit.min", "мин", "min"),
    (
        "footer.demo",
        "Демо-режим: данные выдуманы, ничего не читается и не пишется",
        "Demo mode: invented data, nothing is read or written",
    ),
    (
        "footer.elevated",
        "Права администратора есть",
        "Running as administrator",
    ),
    ("footer.not_elevated", "Без прав администратора", "Not elevated"),
    (
        "footer.readonly",
        "DiskPulse только читает S.M.A.R.T.: на диск ничего не записывается.",
        "DiskPulse only reads S.M.A.R.T.: nothing is written to a disk.",
    ),
    (
        "crash.notice",
        "Прошлый запуск завершился сбоем. Отчёт: {0}",
        "The previous run crashed. Report: {0}",
    ),
    // ---- settings / updates ------------------------------------------------------------------------------------
    ("settings.language", "Язык", "Language"),
    ("settings.overheat", "Порог перегрева", "Overheat threshold"),
    (
        "settings.overheat.hint",
        "«Внимание» — от порога, «плохо» — на 10 °C выше. Для HDD обычно хватает 50–55 °C, для SSD и NVMe — 70 °C.",
        "\"Warning\" starts at the threshold, \"bad\" 10 \u{b0}C above. HDDs usually warrant 50\u{2013}55 \u{b0}C, SSD and NVMe 70 \u{b0}C.",
    ),
    ("settings.updates", "Обновления", "Updates"),
    (
        "settings.auto_update",
        "Проверять обновления раз в сутки (запрос к api.github.com)",
        "Check for updates once a day (a request to api.github.com)",
    ),
    ("update.check", "Проверить сейчас", "Check now"),
    (
        "update.up_to_date",
        "Установлена последняя версия.",
        "You have the latest version.",
    ),
    ("update.available", "Доступна версия {0}", "Version {0} is available"),
    ("update.available_btn", "Обновление {0}", "Update {0}"),
    ("update.install", "Скачать и установить", "Download and install"),
    ("update.downloading", "Загрузка… {0}%", "Downloading\u{2026} {0}%"),
    (
        "update.ready",
        "Версия {0} установлена — перезапустите программу.",
        "Version {0} is installed \u{2014} restart the program.",
    ),
    ("update.restart_btn", "Перезапустить ({0})", "Restart ({0})"),
    ("update.failed", "Не удалось обновить: {0}", "Update failed: {0}"),
    ("settings.privacy", "Приватность", "Privacy"),
    (
        "settings.privacy.body",
        "Единственный сетевой запрос — проверка новой версии на GitHub (её можно отключить). Телеметрии нет. Серийные номера дисков маскируются в журнале и отчётах о сбое.",
        "The only network request is the update check on GitHub (it can be turned off). There is no telemetry. Disk serial numbers are masked in the log and in crash reports.",
    ),
    ("settings.diag", "Диагностика", "Diagnostics"),
    ("diag.open", "Открыть папку журналов", "Open the logs folder"),
    ("diag.copy", "Скопировать диагностику", "Copy diagnostics"),
    (
        "diag.copied",
        "Диагностика скопирована в буфер обмена.",
        "Diagnostics copied to the clipboard.",
    ),
    ("settings.install", "Установка", "Installation"),
    (
        "settings.install.body",
        "Сейчас запущена переносная копия. Установка кладёт программу в %LOCALAPPDATA%\\Programs\\DiskPulse, без прав администратора.",
        "This is a portable copy. Installing puts the program in %LOCALAPPDATA%\\Programs\\DiskPulse, without administrator rights.",
    ),
    (
        "settings.install.btn",
        "Установить DiskPulse…",
        "Install DiskPulse\u{2026}",
    ),
    // ---- installer ---------------------------------------------------------------------------------------------
    ("setup.title", "Установка DiskPulse", "DiskPulse setup"),
    ("setup.version", "версия {0}", "version {0}"),
    ("setup.dir", "Папка установки", "Install folder"),
    ("setup.desktop", "Ярлык на рабочем столе", "Desktop shortcut"),
    (
        "setup.note",
        "Ярлык в меню «Пуск» создаётся всегда. Права администратора не нужны.",
        "A Start menu shortcut is always created. No administrator rights are needed.",
    ),
    ("setup.install", "Установить", "Install"),
    ("setup.upgrade", "Обновить", "Upgrade"),
    ("setup.cancel", "Отмена", "Cancel"),
    ("setup.progress", "Установка…", "Installing\u{2026}"),
    ("setup.done", "✔ Готово", "\u{2714} Done"),
    (
        "setup.done.body",
        "DiskPulse установлен. Найти его можно через поиск в меню «Пуск».",
        "DiskPulse is installed. Find it with the Start menu search.",
    ),
    ("setup.launch", "Запустить DiskPulse", "Launch DiskPulse"),
    ("setup.close", "Закрыть", "Close"),
    ("setup.failed", "Не удалось установить", "Installation failed"),
    ("setup.retry", "Попробовать ещё раз", "Try again"),
    ("unsetup.title", "Удаление DiskPulse", "DiskPulse uninstall"),
    (
        "unsetup.ask",
        "Удалить DiskPulse с этого компьютера?",
        "Remove DiskPulse from this computer?",
    ),
    (
        "unsetup.body",
        "Будут удалены программа, ярлыки, настройки и собранная история.",
        "The program, shortcuts, settings and collected history will be removed.",
    ),
    ("unsetup.remove", "Удалить", "Remove"),
    ("unsetup.progress", "Удаление…", "Removing\u{2026}"),
    ("unsetup.done", "DiskPulse удалён", "DiskPulse was removed"),
    ("unsetup.failed", "Не удалось удалить", "Could not remove"),
    // ---- alerts (0.2) ------------------------------------------------------------------------------------------
    (
        "alert.title",
        "DiskPulse: внимание к диску",
        "DiskPulse: a disk needs attention",
    ),
    (
        "alert.overheat",
        "{0}: температура {1} °C (порог {2} °C)",
        "{0}: temperature {1} \u{b0}C (threshold {2} \u{b0}C)",
    ),
    (
        "alert.critical_warning",
        "{0}: критическое предупреждение контроллера — {1}",
        "{0}: controller critical warning \u{2014} {1}",
    ),
    (
        "alert.media_errors",
        "{0}: выросло число ошибок носителя: {1} → {2}",
        "{0}: media errors increased: {1} \u{2192} {2}",
    ),
    (
        "alert.reallocated",
        "{0}: растёт число переназначенных секторов: {1} → {2}",
        "{0}: reallocated sectors are growing: {1} \u{2192} {2}",
    ),
    (
        "alert.pending",
        "{0}: растёт число нестабильных и неисправимых секторов: {1} → {2}",
        "{0}: pending and uncorrectable sectors are growing: {1} \u{2192} {2}",
    ),
    (
        "alert.spare",
        "{0}: резерв ячеек упал: {1}% → {2}%",
        "{0}: available spare dropped: {1}% \u{2192} {2}%",
    ),
    (
        "alert.write_spike",
        "{0}: за последний интервал записано {1} — резкий скачок записи",
        "{0}: {1} written during the last interval \u{2014} a sharp jump in writes",
    ),
    // ---- history (0.2) -----------------------------------------------------------------------------------------
    ("hist.title", "История", "History"),
    ("hist.none", "Истории пока нет: снимки собираются раз в 30 минут и при запуске.", "No history yet: snapshots are taken every 30 minutes and at startup."),
    ("hist.samples", "Снимков", "Snapshots"),
    ("hist.range", "Период", "Period"),
    ("hist.temp", "Температура: мин / средняя / макс", "Temperature: min / avg / max"),
    ("hist.used", "Percentage Used: первый → последний", "Percentage Used: first \u{2192} last"),
    ("hist.written_total", "Записано всего", "Total written"),
    ("hist.week", "Записано за 7 дней", "Written in 7 days"),
    ("hist.month", "Записано за 30 дней", "Written in 30 days"),
    ("hist.coverage", "данные за {0}", "data covers {0}"),
    ("hist.resets", "сбросов счётчика: {0}", "counter resets: {0}"),
    ("hist.estimate", "оценка", "estimate"),
    ("hist.sata_estimate", "Для SATA объём записи — оценка: атрибут 241, LBA по 512 Б, трактовка зависит от вендора.", "For SATA the written volume is an estimate: attribute 241, 512-byte LBAs, vendor dependent."),
    ("hist.forecast", "Прогноз ресурса TBW (оценка)", "TBW endurance forecast (estimate)"),
    ("hist.forecast.value", "≈ {0} при текущей скорости записи; осталось {1}", "\u{2248} {0} at the current write rate; {1} left"),
    ("hist.forecast.idle", "запись почти не идёт; осталось {0}", "almost no writes; {0} left"),
    ("hist.forecast.no_tbw", "Заявленный TBW не указан — прогноз не строится.", "The rated TBW is not set \u{2014} no forecast."),
    ("hist.forecast.no_data", "Мало данных для прогноза: нужно не меньше 3 дней записей.", "Not enough data for a forecast: at least 3 days of writes are needed."),
    ("hist.tbw", "Заявленный ресурс (TBW), ТБ", "Rated endurance (TBW), TB"),
    ("hist.chart.temp", "Температура, °C", "Temperature, \u{b0}C"),
    ("hist.chart.used", "Percentage Used, %", "Percentage Used, %"),
    ("hist.chart.written", "Записано всего, ГБ", "Total written, GB"),
    ("hist.collect_now", "Снять снимок сейчас", "Take a snapshot now"),
    ("hist.collected", "Снимок сохранён.", "Snapshot saved."),
    ("hist.source.service", "Данные собирает служба DiskPulse.", "The DiskPulse service collects the data."),
    ("hist.source.user", "Данные собирает программа (служба не установлена).", "The program collects the data (the service is not installed)."),
    ("unit.years", "г.", "y"),
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
