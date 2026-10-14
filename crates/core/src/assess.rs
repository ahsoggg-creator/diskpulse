//! Transparent condition assessment: "good / warning / bad", always with the rules that fired.
//!
//! There is deliberately no "health percentage": SMART does not contain one. Every verdict is the worst level among the
//! rules below, and the rule ids are shown to the user. Rules marked `heuristic` are DiskPulse's own conventions
//! (not part of NVMe or ATA specifications); those marked `vendor` depend on how the manufacturer fills an attribute.

use serde::{Deserialize, Serialize};

use crate::ata::AtaSmart;
use crate::nvme::{NvmeHealth, critical_warning_flags};
use crate::smart::{SmartData, SmartReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Good,
    Warning,
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    /// Temperature (C) at which a disk is reported as "warning"; "bad" is 10 C above.
    pub overheat_c: i32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { overheat_c: 70 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    /// Rule id, e.g. `N4`; the text lives in the localization table under `rule.<id>`.
    pub rule: String,
    pub level: Level,
    /// Values substituted into the rule text (`{0}`, `{1}`).
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    pub level: Level,
    /// Rules that fired, most severe first. Empty means every rule passed.
    pub reasons: Vec<Reason>,
}

/// How a rule is grounded; shown next to the rule so the user knows how far to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    Spec,
    Vendor,
    Heuristic,
}

pub struct RuleInfo {
    pub id: &'static str,
    pub level: Level,
    pub basis: Basis,
}

/// Every rule that exists (for the "show rules" list and for tests that each has a text).
pub const RULES: &[RuleInfo] = &[
    RuleInfo {
        id: "N1",
        level: Level::Bad,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "N2",
        level: Level::Bad,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "N3",
        level: Level::Warning,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "N4",
        level: Level::Bad,
        basis: Basis::Vendor,
    },
    RuleInfo {
        id: "N5",
        level: Level::Warning,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "N6",
        level: Level::Warning,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "N7",
        level: Level::Bad,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "N8",
        level: Level::Warning,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "N9",
        level: Level::Warning,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "A1",
        level: Level::Bad,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "A2",
        level: Level::Warning,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "A3",
        level: Level::Warning,
        basis: Basis::Spec,
    },
    RuleInfo {
        id: "A4",
        level: Level::Warning,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "A5",
        level: Level::Bad,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "A6",
        level: Level::Warning,
        basis: Basis::Vendor,
    },
    RuleInfo {
        id: "A7",
        level: Level::Bad,
        basis: Basis::Heuristic,
    },
    RuleInfo {
        id: "A8",
        level: Level::Warning,
        basis: Basis::Heuristic,
    },
];

fn reason(rule: &str, level: Level, args: &[String]) -> Reason {
    Reason {
        rule: rule.to_owned(),
        level,
        args: args.to_vec(),
    }
}

fn finish(mut reasons: Vec<Reason>) -> Assessment {
    reasons.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.rule.cmp(&b.rule)));
    Assessment {
        level: reasons.first().map_or(Level::Good, |r| r.level),
        reasons,
    }
}

pub fn assess_nvme(h: &NvmeHealth, t: Thresholds) -> Assessment {
    let mut r = Vec::new();
    if h.critical_warning != 0 {
        r.push(reason(
            "N1",
            Level::Bad,
            &[critical_warning_flags(h.critical_warning).join(", ")],
        ));
    }
    if h.available_spare <= h.available_spare_threshold {
        r.push(reason(
            "N2",
            Level::Bad,
            &[h.available_spare.to_string(), h.available_spare_threshold.to_string()],
        ));
    } else if h.available_spare < 50 {
        r.push(reason("N3", Level::Warning, &[h.available_spare.to_string()]));
    }
    if h.percentage_used >= 100 {
        r.push(reason("N4", Level::Bad, &[h.percentage_used.to_string()]));
    } else if h.percentage_used >= 80 {
        r.push(reason("N5", Level::Warning, &[h.percentage_used.to_string()]));
    }
    if h.media_errors > 0 {
        r.push(reason("N6", Level::Warning, &[h.media_errors.to_string()]));
    }
    if let Some(temp) = h.temperature_c {
        if temp >= t.overheat_c + 10 {
            r.push(reason(
                "N7",
                Level::Bad,
                &[temp.to_string(), (t.overheat_c + 10).to_string()],
            ));
        } else if temp >= t.overheat_c {
            r.push(reason(
                "N8",
                Level::Warning,
                &[temp.to_string(), t.overheat_c.to_string()],
            ));
        }
    }
    if h.critical_temp_minutes > 0 {
        r.push(reason("N9", Level::Warning, &[h.critical_temp_minutes.to_string()]));
    }
    finish(r)
}

fn label(a: &crate::ata::AtaAttribute) -> String {
    if a.name.is_empty() {
        format!("{} (0x{:02X})", a.id, a.id)
    } else {
        format!("{} {}", a.id, a.name)
    }
}

pub fn assess_ata(s: &AtaSmart, t: Thresholds) -> Assessment {
    let mut r = Vec::new();
    for a in &s.attributes {
        let args = [label(a), a.current.to_string(), a.threshold.unwrap_or(0).to_string()];
        if a.failing_now() && a.prefailure() {
            r.push(reason("A1", Level::Bad, &args));
        } else if a.failing_now() || a.failed_in_past() {
            r.push(reason(
                "A2",
                Level::Warning,
                &[label(a), a.worst.to_string(), a.threshold.unwrap_or(0).to_string()],
            ));
        }
    }
    for (id, rule) in [(5u8, "A3"), (196, "A3"), (197, "A3"), (198, "A3"), (187, "A4")] {
        if let Some(a) = s.get(id).filter(|a| a.raw > 0) {
            r.push(reason(rule, Level::Warning, &[label(a), a.raw.to_string()]));
        }
    }
    let bad_sectors: u64 = [5u8, 197, 198].iter().filter_map(|&id| s.raw(id)).sum();
    if bad_sectors >= 100 {
        r.push(reason("A5", Level::Bad, &[bad_sectors.to_string()]));
    }
    for id in [177u8, 231, 233] {
        // The normalized value counts down from 100 on drives that use these as "life left"; vendor dependent.
        if let Some(a) = s
            .get(id)
            .filter(|a| a.current <= 20 && a.current > 0 && !a.failing_now())
        {
            r.push(reason("A6", Level::Warning, &[label(a), a.current.to_string()]));
        }
    }
    if let Some(temp) = s.temperature_c() {
        if temp >= t.overheat_c + 10 {
            r.push(reason(
                "A7",
                Level::Bad,
                &[temp.to_string(), (t.overheat_c + 10).to_string()],
            ));
        } else if temp >= t.overheat_c {
            r.push(reason(
                "A8",
                Level::Warning,
                &[temp.to_string(), t.overheat_c.to_string()],
            ));
        }
    }
    finish(r)
}

pub fn assess(data: &SmartData, t: Thresholds) -> Assessment {
    match data {
        SmartData::Nvme(h) => assess_nvme(h, t),
        SmartData::Ata(s) => assess_ata(s, t),
    }
}

/// `None` when SMART could not be read: an unreadable disk gets no verdict, not a "good" one.
pub fn assess_report(r: &SmartReport, t: Thresholds) -> Option<Assessment> {
    r.data().map(|d| assess(d, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ata::parse_smart;
    use crate::ata::tests::{put_attr, sample_data, sample_thresholds, seal};
    use crate::nvme::{parse_health_log, tests::sample_page};

    /// The sample page with its media errors and critical-temperature time cleared, then `f` applied.
    fn nvme(f: impl FnOnce(&mut Vec<u8>)) -> NvmeHealth {
        let mut p = sample_page();
        p[160..176].fill(0);
        p[196..200].fill(0);
        f(&mut p);
        parse_health_log(&p).unwrap()
    }

    fn rules(a: &Assessment) -> Vec<&str> {
        a.reasons.iter().map(|r| r.rule.as_str()).collect()
    }

    #[test]
    fn nvme_media_errors_and_critical_temperature_time_are_warnings() {
        let a = assess_nvme(&parse_health_log(&sample_page()).unwrap(), Thresholds::default());
        assert_eq!((a.level, rules(&a)), (Level::Warning, vec!["N6", "N9"]));
    }

    #[test]
    fn healthy_nvme_is_good_with_no_reasons() {
        let a = assess_nvme(&nvme(|_| {}), Thresholds::default());
        assert_eq!(
            a,
            Assessment {
                level: Level::Good,
                reasons: vec![]
            }
        );
    }

    #[test]
    fn nvme_critical_warning_and_spare_are_bad() {
        let a = assess_nvme(
            &nvme(|p| {
                p[0] = 0b1;
                p[3] = 8;
            }),
            Thresholds::default(),
        );
        assert_eq!(a.level, Level::Bad);
        assert_eq!(rules(&a), vec!["N1", "N2"]);
        assert_eq!(a.reasons[0].args, vec!["spare_low".to_owned()]);
    }

    #[test]
    fn nvme_wear_levels() {
        let q = |used: u8| assess_nvme(&nvme(move |p| p[5] = used), Thresholds::default());
        assert_eq!(q(79).level, Level::Good);
        assert_eq!((q(80).level, rules(&q(80))), (Level::Warning, vec!["N5"]));
        assert_eq!((q(100).level, rules(&q(100))), (Level::Bad, vec!["N4"]));
    }

    #[test]
    fn temperature_rules_follow_the_configurable_threshold() {
        let hot = nvme(|p| p[1..3].copy_from_slice(&(75u16 + 273).to_le_bytes()));
        assert_eq!(assess_nvme(&hot, Thresholds { overheat_c: 70 }).level, Level::Warning);
        assert_eq!(assess_nvme(&hot, Thresholds { overheat_c: 65 }).level, Level::Bad);
        assert_eq!(assess_nvme(&hot, Thresholds { overheat_c: 80 }).level, Level::Good);
    }

    fn ata(f: impl FnOnce(&mut Vec<u8>)) -> AtaSmart {
        let mut p = sample_data();
        f(&mut p);
        seal(&mut p);
        parse_smart(&p, Some(&sample_thresholds())).unwrap()
    }

    #[test]
    fn ata_pending_sectors_warn_and_show_the_attribute() {
        let a = assess_ata(&ata(|_| {}), Thresholds::default());
        assert_eq!((a.level, rules(&a)), (Level::Warning, vec!["A3"]));
        assert!(a.reasons[0].args[0].contains("Current Pending"));
    }

    #[test]
    fn ata_clean_drive_is_good() {
        let a = assess_ata(
            &ata(|p| put_attr(p, 4, 197, 0x0032, 100, 100, 0)),
            Thresholds::default(),
        );
        assert_eq!(a.level, Level::Good);
    }

    #[test]
    fn ata_attribute_below_its_own_threshold_is_bad() {
        let a = assess_ata(&ata(|p| put_attr(p, 0, 5, 0x0033, 8, 8, 0)), Thresholds::default());
        assert_eq!(a.level, Level::Bad);
        assert!(rules(&a).contains(&"A1"));
    }

    #[test]
    fn ata_many_bad_sectors_is_bad_by_heuristic() {
        let a = assess_ata(&ata(|p| put_attr(p, 0, 5, 0x0033, 90, 90, 150)), Thresholds::default());
        assert_eq!(a.level, Level::Bad);
        assert!(rules(&a).contains(&"A5"));
    }

    #[test]
    fn unreadable_disk_gets_no_verdict() {
        let r = SmartReport::unavailable(crate::smart::UnavailableReason::UsbBridge, "x");
        assert!(assess_report(&r, Thresholds::default()).is_none());
    }

    #[test]
    fn every_rule_has_localized_text() {
        for r in RULES {
            for lang in [crate::i18n::Lang::Ru, crate::i18n::Lang::En] {
                let key = format!("rule.{}", r.id);
                assert_ne!(crate::i18n::tr(lang, &key), crate::i18n::MISSING, "{key}");
            }
        }
    }
}
