//! Export of a surface-scan result as CSV or a self-contained HTML page.
//!
//! Reports contain file paths from the scanned disk (that is the point), the model name and the *masked* serial number.
//! Every dynamic string is escaped (CSV quoting with formula-injection guard, HTML entity escaping).

use crate::i18n::{Lang, fmt, tr};
use crate::ntfs::Kind;
use crate::scanmap::{Outcome, Piece};
use crate::surface::{HISTOGRAM_EDGES_MS, Report, speed_series};
use crate::units::{format_utc, human_bytes};

/// What the report says about one slow zone or unreadable range.
pub struct Row {
    pub unreadable: bool,
    pub start: u64,
    pub end: u64,
    pub max_ms: f64,
    pub mean_ms: f64,
    pub pieces: Vec<Piece>,
}

pub struct Input<'a> {
    pub lang: Lang,
    pub model: &'a str,
    pub serial_masked: &'a str,
    pub disk_size: u64,
    pub scan: &'a Report,
    pub rows: Vec<Row>,
    /// Unix seconds, shown as UTC.
    pub generated: u64,
}

/// Builds the rows from a scan and the pieces returned by [`crate::scanmap::attribute`] (zones first, then bad ranges).
pub fn rows(scan: &Report, zone_pieces: &[Piece], bad_pieces: &[Piece]) -> Vec<Row> {
    let inside = |pieces: &[Piece], a: u64, b: u64| {
        pieces
            .iter()
            .filter(|p| p.start >= a && p.end <= b)
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut out: Vec<Row> = scan
        .zones()
        .into_iter()
        .map(|z| Row {
            unreadable: false,
            start: z.start,
            end: z.end,
            max_ms: z.max_ms,
            mean_ms: z.mean_ms,
            pieces: inside(zone_pieces, z.start, z.end),
        })
        .collect();
    out.extend(scan.bad.iter().map(|r| Row {
        unreadable: true,
        start: r.start,
        end: r.end,
        max_ms: 0.0,
        mean_ms: 0.0,
        pieces: inside(bad_pieces, r.start, r.end),
    }));
    out
}

/// One line per (range, file); ranges without files get one line with the reason.
struct Line {
    unreadable: bool,
    start: u64,
    end: u64,
    max_ms: f64,
    volume: String,
    path: String,
    kind: &'static str,
    attr: String,
    bytes: Option<u64>,
    note: String,
}

fn kind_key(k: Kind) -> &'static str {
    match k {
        Kind::File => "scan.kind.file",
        Kind::Directory => "scan.kind.directory",
        Kind::Metadata => "scan.kind.metadata",
        Kind::Unknown => "scan.kind.unknown",
    }
}

/// The explanation for a piece that has no file list.
pub fn outcome_note(lang: Lang, o: &Outcome) -> String {
    match o {
        Outcome::Mapped { .. } => String::new(),
        Outcome::NoVolume => tr(lang, "scan.outcome.no_volume").to_owned(),
        Outcome::UnsupportedFs { fs, .. } => fmt(lang, "scan.outcome.fs", std::slice::from_ref(fs)),
        Outcome::MultiExtent { .. } => tr(lang, "scan.outcome.multi").to_owned(),
        Outcome::Failed { volume, error } => fmt(lang, "scan.outcome.failed", &[volume.clone(), error.clone()]),
    }
}

fn lines(i: &Input) -> Vec<Line> {
    let mut out = Vec::new();
    for r in &i.rows {
        let base = |note: String, volume: &str, path: String, kind, attr: String, bytes, (s, e): (u64, u64)| Line {
            unreadable: r.unreadable,
            start: s,
            end: e,
            max_ms: r.max_ms,
            volume: volume.into(),
            path,
            kind,
            attr,
            bytes,
            note,
        };
        if r.pieces.is_empty() {
            out.push(base(
                String::new(),
                "",
                String::new(),
                "",
                String::new(),
                None,
                (r.start, r.end),
            ));
        }
        for p in &r.pieces {
            match &p.outcome {
                Outcome::Mapped { volume, zone } => {
                    for f in &zone.files {
                        out.push(base(
                            String::new(),
                            volume,
                            f.path.clone(),
                            tr(i.lang, kind_key(f.kind)),
                            f.attr.clone(),
                            Some(f.bytes),
                            (p.start, p.end),
                        ));
                    }
                    if zone.free_bytes > 0 {
                        out.push(base(
                            tr(i.lang, "scan.free").to_owned(),
                            volume,
                            String::new(),
                            "",
                            String::new(),
                            Some(zone.free_bytes),
                            (p.start, p.end),
                        ));
                    }
                    if zone.other_allocated_bytes > 0 {
                        out.push(base(
                            tr(i.lang, "scan.other_alloc").to_owned(),
                            volume,
                            String::new(),
                            "",
                            String::new(),
                            Some(zone.other_allocated_bytes),
                            (p.start, p.end),
                        ));
                    }
                }
                other => out.push(base(
                    outcome_note(i.lang, other),
                    "",
                    String::new(),
                    "",
                    String::new(),
                    None,
                    (p.start, p.end),
                )),
            }
        }
    }
    out
}

/// Quotes a CSV cell; a cell that a spreadsheet could read as a formula gets a leading apostrophe.
pub fn csv_cell(s: &str) -> String {
    let guarded = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if guarded.contains([',', '"', '\n', '\r', ';']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

pub fn csv(i: &Input) -> String {
    let l = i.lang;
    let head = [
        "scan.col.type",
        "scan.col.start",
        "scan.col.end",
        "scan.col.size",
        "scan.col.maxms",
        "scan.col.volume",
        "scan.col.file",
        "scan.col.kind",
        "scan.col.attr",
        "scan.col.bytes",
        "scan.col.note",
    ];
    let mut s = head.iter().map(|k| csv_cell(tr(l, k))).collect::<Vec<_>>().join(",") + "\r\n";
    for x in lines(i) {
        let cells = [
            tr(
                l,
                if x.unreadable {
                    "scan.type.bad"
                } else {
                    "scan.type.slow"
                },
            )
            .to_owned(),
            x.start.to_string(),
            x.end.to_string(),
            (x.end - x.start).to_string(),
            if x.unreadable {
                String::new()
            } else {
                format!("{:.1}", x.max_ms)
            },
            x.volume,
            x.path,
            x.kind.to_owned(),
            x.attr,
            x.bytes.map_or(String::new(), |b| b.to_string()),
            x.note,
        ];
        s += &(cells.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(",") + "\r\n");
    }
    s
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn histogram_label(i: usize) -> String {
    match i {
        0 => format!("< {} ms", HISTOGRAM_EDGES_MS[0]),
        10 => format!(">= {} ms", HISTOGRAM_EDGES_MS[9]),
        n => format!("{}-{} ms", HISTOGRAM_EDGES_MS[n - 1], HISTOGRAM_EDGES_MS[n]),
    }
}

pub fn html(i: &Input) -> String {
    let l = i.lang;
    let t = |k: &str| html_escape(tr(l, k));
    let sc = i.scan;
    let mut h = String::new();
    h += &format!(
        "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\"><title>{}</title><style>\
         body{{font:14px system-ui,sans-serif;margin:24px auto;max-width:1000px;color:#1d2330}}\
         h1{{font-size:22px}}h2{{font-size:16px;margin-top:28px}}table{{border-collapse:collapse;width:100%}}\
         td,th{{border-bottom:1px solid #d9dde5;padding:4px 8px;text-align:left;vertical-align:top}}th{{color:#5b6475;font-weight:600}}\
         .num{{text-align:right;font-variant-numeric:tabular-nums}}.bar{{background:#4c8dff;height:12px;display:inline-block;border-radius:3px}}\
         .note{{color:#5b6475;font-size:12px}}.bad{{color:#c0392b}}code{{font-family:Consolas,monospace}}</style></head><body>",
        l.code(),
        t("scan.title")
    );
    h += &format!("<h1>{}</h1>", t("scan.title"));
    h += &format!(
        "<p><b>{}</b> &middot; {} &middot; {}: <code>{}</code><br><span class=\"note\">{}</span></p>",
        html_escape(i.model),
        html_escape(&human_bytes(l, u128::from(i.disk_size))),
        t("disk.serial"),
        html_escape(i.serial_masked),
        html_escape(&fmt(l, "scan.generated", &[format_utc(i.generated)]))
    );
    h += &format!("<p class=\"note\">{}</p>", t("scan.disclaimer"));
    let p = &sc.params;
    h += "<table>";
    let mut row = |k: &str, v: String| h += &format!("<tr><th>{}</th><td>{}</td></tr>", t(k), html_escape(&v));
    row("scan.block", human_bytes(l, u128::from(p.block_bytes)));
    row("scan.slow_threshold", format!("{} ms", p.slow_ms));
    row(
        "scan.tested",
        format!(
            "{} / {}",
            human_bytes(l, u128::from(sc.bytes_read)),
            human_bytes(l, u128::from(i.disk_size))
        ),
    );
    row("scan.blocks", sc.blocks.len().to_string());
    row("scan.avg", format!("{:.0} MB/s", sc.avg_mbps()));
    if sc.cancelled {
        row("scan.state", tr(l, "scan.cancelled").to_owned());
    }
    if sc.too_many_errors {
        row("scan.state", tr(l, "scan.too_many").to_owned());
    }
    h += "</table>";

    // speed chart: one polyline
    let series = speed_series(&sc.blocks, 400);
    if series.len() > 1 {
        let max = series.iter().map(|s| s.1).fold(1.0f64, f64::max);
        let (w, hh) = (900.0, 160.0);
        let span = (series.last().map_or(1, |s| s.0) - series[0].0).max(1) as f64;
        let pts: Vec<String> = series
            .iter()
            .map(|(o, v)| {
                format!(
                    "{:.1},{:.1}",
                    (o - series[0].0) as f64 / span * w,
                    hh - v / max * (hh - 6.0) - 3.0
                )
            })
            .collect();
        h += &format!(
            "<h2>{} <span class=\"note\">(max {:.0} MB/s)</span></h2><svg viewBox=\"0 0 {w} {hh}\" width=\"100%\" style=\"background:#f5f7fb;border-radius:6px\"><polyline fill=\"none\" stroke=\"#4c8dff\" stroke-width=\"1.5\" points=\"{}\"/></svg>",
            t("scan.speed"),
            max,
            pts.join(" ")
        );
    }
    // histogram
    let top = sc.histogram.iter().copied().max().unwrap_or(1).max(1);
    h += &format!("<h2>{}</h2><table>", t("scan.histogram"));
    for (n, c) in sc.histogram.iter().enumerate() {
        h += &format!(
            "<tr><td>{}</td><td class=\"num\">{c}</td><td><span class=\"bar\" style=\"width:{}px\"></span></td></tr>",
            html_escape(&histogram_label(n)),
            c * 500 / top
        );
    }
    h += "</table>";

    for (bad, key, none) in [
        (false, "scan.zones", "scan.none_zones"),
        (true, "scan.bad", "scan.none_bad"),
    ] {
        let ls: Vec<Line> = lines(i).into_iter().filter(|x| x.unreadable == bad).collect();
        h += &format!("<h2>{}</h2>", t(key));
        if ls.is_empty() {
            h += &format!("<p>{}</p>", t(none));
            continue;
        }
        h += &format!(
            "<table><tr><th>{}</th><th class=\"num\">{}</th><th>{}</th><th>{}</th><th>{}</th><th class=\"num\">{}</th><th>{}</th></tr>",
            t("scan.col.range"),
            t("scan.col.maxms"),
            t("scan.col.volume"),
            t("scan.col.file"),
            t("scan.col.attr"),
            t("scan.col.bytes"),
            t("scan.col.note")
        );
        for x in ls {
            h += &format!(
                "<tr><td><code>{}&ndash;{}</code></td><td class=\"num\">{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                x.start,
                x.end,
                if bad { String::new() } else { format!("{:.0}", x.max_ms) },
                html_escape(&x.volume),
                html_escape(&x.path),
                html_escape(&if x.kind.is_empty() {
                    x.attr
                } else {
                    format!("{} / {}", x.kind, x.attr)
                }),
                x.bytes
                    .map_or(String::new(), |b| html_escape(&human_bytes(l, u128::from(b)))),
                html_escape(&x.note)
            );
        }
        h += "</table>";
    }
    h += &format!("<p class=\"note\">{}</p></body></html>", t("scan.privacy"));
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::{FileHit, ZoneMap};
    use crate::surface::{BadRange, Block, Params};

    fn scan() -> Report {
        let blocks: Vec<Block> = (0..20)
            .map(|i| Block {
                offset: i << 22,
                len: 1 << 22,
                micros: if (5..8).contains(&i) { 500_000 } else { 9_000 },
                error: None,
            })
            .collect();
        let mut histogram = [0u64; 11];
        for b in &blocks {
            histogram[crate::surface::bucket(b.micros as f64 / 1000.0)] += 1;
        }
        Report {
            params: Params::default(),
            device_size: 20 << 22,
            blocks,
            bad: vec![BadRange {
                start: 100 << 12,
                end: 101 << 12,
                error: 23,
            }],
            histogram,
            bytes_read: 19 << 22,
            micros_total: 1_000_000,
            cancelled: false,
            too_many_errors: false,
        }
    }

    fn mapped(path: &str) -> Outcome {
        Outcome::Mapped {
            volume: "C:".into(),
            zone: ZoneMap {
                total_bytes: 4096,
                free_bytes: 0,
                other_allocated_bytes: 0,
                files: vec![FileHit {
                    path: path.into(),
                    kind: Kind::File,
                    record: 30,
                    attr: "data".into(),
                    bytes: 4096,
                }],
            },
        }
    }

    fn input<'a>(scan: &'a Report, path: &str) -> Input<'a> {
        let z = scan.zones();
        let zp = vec![Piece {
            start: z[0].start,
            end: z[0].end,
            outcome: mapped(path),
        }];
        let bp = vec![Piece {
            start: 100 << 12,
            end: 101 << 12,
            outcome: Outcome::NoVolume,
        }];
        Input {
            lang: Lang::En,
            model: "Model <X>",
            serial_masked: "****1234",
            disk_size: scan.device_size,
            scan,
            rows: rows(scan, &zp, &bp),
            generated: 1_760_000_000,
        }
    }

    #[test]
    fn csv_lists_files_per_zone_and_the_reason_for_unmapped_ranges() {
        let s = scan();
        let c = csv(&input(&s, "\\Users\\a,b.txt"));
        let lines: Vec<&str> = c.trim_end().split("\r\n").collect();
        assert_eq!(lines.len(), 3, "{c}");
        assert!(
            lines[1].starts_with("Slow zone,20971520,33554432,12582912,500.0,C:,\"\\Users\\a,b.txt\",File,data,4096,")
        );
        assert!(
            lines[2].starts_with("Unreadable,409600,413696,4096,,,,,,,")
                && lines[2].contains("not inside any mounted volume")
        );
    }

    #[test]
    fn csv_cells_are_quoted_and_formulas_neutralised() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_cell("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv_cell("-2+3"), "'-2+3");
        assert_eq!(csv_cell("\\a\\b"), "\\a\\b");
    }

    #[test]
    fn html_escapes_everything_that_comes_from_the_disk() {
        let s = scan();
        let page = html(&input(&s, "<script>alert(1)</script>\\\"x\".txt"));
        assert!(!page.contains("<script>alert"), "script tag leaked");
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(page.contains("Model &lt;X&gt;"));
        assert!(page.contains("****1234") && !page.contains("SECRET"));
        assert!(page.starts_with("<!doctype html>") && page.ends_with("</html>"));
        // chart, histogram and both tables are present
        assert!(
            page.contains("<polyline")
                && page.contains("&lt; 1 ms")
                && page.contains("Slow zones")
                && page.contains("Unreadable blocks")
        );
    }

    #[test]
    fn empty_results_say_so() {
        let mut s = scan();
        s.blocks.iter_mut().for_each(|b| b.micros = 9_000);
        s.bad.clear();
        let page = html(&Input {
            lang: Lang::Ru,
            model: "M",
            serial_masked: "****",
            disk_size: 1,
            scan: &s,
            rows: rows(&s, &[], &[]),
            generated: 0,
        });
        assert!(page.contains("Медленных зон нет") && page.contains("Нечитаемых блоков нет"));
        assert_eq!(
            csv(&Input {
                lang: Lang::En,
                model: "M",
                serial_masked: "****",
                disk_size: 1,
                scan: &s,
                rows: vec![],
                generated: 0
            })
            .lines()
            .count(),
            1
        );
    }

    #[test]
    fn every_scan_text_exists_in_both_languages() {
        for k in [
            "scan.title",
            "scan.generated",
            "scan.disclaimer",
            "scan.privacy",
            "scan.block",
            "scan.slow_threshold",
            "scan.tested",
            "scan.blocks",
            "scan.avg",
            "scan.cancelled",
            "scan.too_many",
            "scan.state",
            "scan.speed",
            "scan.histogram",
            "scan.zones",
            "scan.bad",
            "scan.none_zones",
            "scan.none_bad",
            "scan.col.type",
            "scan.col.start",
            "scan.col.end",
            "scan.col.size",
            "scan.col.maxms",
            "scan.col.volume",
            "scan.col.file",
            "scan.col.kind",
            "scan.col.attr",
            "scan.col.bytes",
            "scan.col.note",
            "scan.col.range",
            "scan.type.slow",
            "scan.type.bad",
            "scan.kind.file",
            "scan.kind.directory",
            "scan.kind.metadata",
            "scan.kind.unknown",
            "scan.free",
            "scan.other_alloc",
            "scan.outcome.no_volume",
            "scan.outcome.fs",
            "scan.outcome.multi",
            "scan.outcome.failed",
        ] {
            for lang in [Lang::Ru, Lang::En] {
                assert_ne!(tr(lang, k), crate::i18n::MISSING, "{k}");
            }
        }
    }
}
