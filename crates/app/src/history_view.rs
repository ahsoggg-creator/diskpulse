//! The "History" tab: temperature, Percentage Used and written-volume charts, "written per week / month" and the
//! (optional) TBW forecast. Everything is read from the history store; nothing here touches a disk.

use std::time::{Duration, Instant};

use diskpulse_core::history::{DAY, DiskMeta, HistoryStore, Sample, Summary, summarize};
use diskpulse_core::i18n::fmt;
use diskpulse_core::service;
use diskpulse_core::units::{format_utc, human_bytes, human_span, now_unix};
use eframe::egui::{self, Color32, RichText, Ui};
use egui_plot::{Line, Plot, PlotPoints};

use crate::{App, demo, settings, theme};

const RELOAD_EVERY: Duration = Duration::from_secs(30);

/// Cached view of the store.
#[derive(Default)]
pub struct HistoryState {
    pub metas: Vec<DiskMeta>,
    pub key: Option<String>,
    pub samples: Vec<Sample>,
    pub loaded: Option<Instant>,
    /// Text of the "rated TBW" field.
    pub tbw_text: String,
    pub from_service: bool,
}

impl App {
    pub fn history_reload(&mut self) {
        let h = &mut self.hist;
        if settings::is_demo() {
            let demo = demo::history();
            h.metas = demo.iter().map(|(m, _)| m.clone()).collect();
            if h.key.is_none() {
                h.key = h.metas.first().map(|m| m.key.clone());
            }
            h.samples = demo
                .into_iter()
                .find(|(m, _)| Some(&m.key) == h.key.as_ref())
                .map(|(_, s)| s)
                .unwrap_or_default();
            h.from_service = false;
        } else {
            let store = HistoryStore::new(service::history_dir());
            h.from_service = service::is_installed();
            h.metas = store.disks();
            if h.key.as_ref().is_none_or(|k| !h.metas.iter().any(|m| &m.key == k)) {
                h.key = h.metas.first().map(|m| m.key.clone());
            }
            h.samples = h.key.as_ref().map(|k| store.load(k)).unwrap_or_default();
        }
        let tbw = h
            .key
            .as_ref()
            .and_then(|k| h.metas.iter().find(|m| &m.key == k))
            .and_then(|m| m.tbw_rated_tb);
        h.tbw_text = tbw.map_or(String::new(), |t| format!("{t}"));
        h.loaded = Some(Instant::now());
    }

    fn save_tbw(&mut self) {
        if settings::is_demo() {
            return;
        }
        let Some(key) = self.hist.key.clone() else { return };
        let store = HistoryStore::new(service::history_dir());
        if let Some(mut meta) = store.load_meta(&key) {
            meta.tbw_rated_tb = self
                .hist
                .tbw_text
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|t| *t > 0.0);
            if store.save_meta(&meta).is_err() {
                self.toast(self.t("hist.save_failed"));
            }
        }
        self.history_reload();
    }

    pub fn history_view(&mut self, ui: &mut Ui) {
        if self.hist.loaded.is_none_or(|t| t.elapsed() > RELOAD_EVERY) {
            self.history_reload();
        }
        let lang = self.lang();
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        if self.hist.metas.is_empty() {
            ui.label(RichText::new(self.t("hist.none")).weak());
            ui.add_space(6.0);
            if !settings::is_demo() && ui.button(self.t("hist.collect_now")).clicked() {
                self.collect_now(&ctx);
            }
            return;
        }
        let mut changed = None;
        ui.horizontal(|ui| {
            let cur = self
                .hist
                .metas
                .iter()
                .find(|m| Some(&m.key) == self.hist.key.as_ref())
                .map_or(String::new(), |m| m.model.clone());
            egui::ComboBox::from_id_salt("hist_disk")
                .selected_text(cur)
                .width(280.0)
                .show_ui(ui, |ui| {
                    for m in &self.hist.metas {
                        if ui
                            .selectable_label(
                                Some(&m.key) == self.hist.key.as_ref(),
                                format!("{} ({})", m.model, m.bus),
                            )
                            .clicked()
                        {
                            changed = Some(m.key.clone());
                        }
                    }
                });
            if !settings::is_demo() && ui.button(self.t("hist.collect_now")).clicked() {
                self.collect_now(&ctx);
            }
            let src = self.t(if self.hist.from_service {
                "hist.source.service"
            } else {
                "hist.source.user"
            });
            ui.label(RichText::new(src).small().weak());
        });
        if let Some(k) = changed {
            self.hist.key = Some(k);
            self.history_reload();
        }
        let Some(meta) = self
            .hist
            .metas
            .iter()
            .find(|m| Some(&m.key) == self.hist.key.as_ref())
            .cloned()
        else {
            return;
        };
        let now = if settings::is_demo() {
            self.hist.samples.last().map_or(now_unix(), |s| s.t)
        } else {
            now_unix()
        };
        let summary = summarize(&self.hist.samples, now, &meta.bus, meta.tbw_rated_bytes());
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if let Some(s) = &summary {
                self.summary_card(ui, &meta, s);
            }
            ui.add_space(8.0);
            let samples = self.hist.samples.clone();
            let gb = |s: &Sample| s.written.map(|w| w as f64 / 1e9);
            let charts: [(&str, Color32, Box<dyn Fn(&Sample) -> Option<f64>>); 3] = [
                ("hist.chart.temp", theme::WARN, Box::new(|s| s.temp_c.map(f64::from))),
                (
                    "hist.chart.used",
                    theme::ACCENT,
                    Box::new(|s| s.pct_used.map(f64::from)),
                ),
                ("hist.chart.written", theme::GOOD, Box::new(gb)),
            ];
            for (title, color, f) in charts {
                let pts: Vec<[f64; 2]> = samples
                    .iter()
                    .filter_map(|s| f(s).map(|y| [(s.t as f64 - now as f64) / DAY as f64, y]))
                    .collect();
                if pts.len() < 2 {
                    continue;
                }
                ui.label(RichText::new(self.t(title)).strong());
                Plot::new(title)
                    .height(150.0)
                    .allow_scroll(false)
                    .show_axes([true, true])
                    .x_axis_formatter(move |m, _| {
                        format!("{} {}", m.value.round(), diskpulse_core::i18n::tr(lang, "unit.days"))
                    })
                    .show(ui, |p| {
                        p.line(Line::new(title, PlotPoints::from(pts)).color(color).width(1.8));
                    });
                ui.add_space(6.0);
            }
        });
    }

    fn summary_card(&mut self, ui: &mut Ui, meta: &DiskMeta, s: &Summary) {
        let lang = self.lang();
        let est = meta.bus != "NVMe";
        let tag = if est {
            format!(" ({})", self.t("hist.estimate"))
        } else {
            String::new()
        };
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("hist_sum")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    let mut row = |key: &str, v: String| {
                        ui.label(RichText::new(self.t(key)).weak());
                        ui.label(v);
                        ui.end_row();
                    };
                    row("hist.samples", s.count.to_string());
                    row(
                        "hist.range",
                        format!("{} .. {}", format_utc(s.first_t), format_utc(s.last_t)),
                    );
                    if let (Some(a), Some(b), Some(avg)) = (s.temp_min, s.temp_max, s.temp_avg) {
                        row("hist.temp", format!("{a} / {avg:.0} / {b} °C"));
                    }
                    if let (Some(a), Some(b)) = (s.pct_used_first, s.pct_used_last) {
                        row("hist.used", format!("{a}% -> {b}%"));
                    }
                    if let Some(w) = s.written_total {
                        row(
                            "hist.written_total",
                            format!("{}{tag}", human_bytes(lang, u128::from(w))),
                        );
                    }
                    for (key, w) in [("hist.week", s.week), ("hist.month", s.month)] {
                        let mut v = format!(
                            "{}{tag}, {}",
                            human_bytes(lang, u128::from(w.bytes)),
                            self.tf("hist.coverage", &[human_span(lang, w.covered_secs)])
                        );
                        if w.resets > 0 {
                            v.push_str(&format!(", {}", self.tf("hist.resets", &[w.resets.to_string()])));
                        }
                        row(key, v);
                    }
                });
            ui.add_space(6.0);
            ui.label(RichText::new(self.t("hist.forecast")).strong());
            let text = match (meta.tbw_rated_bytes(), s.forecast) {
                (None, _) => self.t("hist.forecast.no_tbw").to_owned(),
                (Some(_), None) => self.t("hist.forecast.no_data").to_owned(),
                (Some(_), Some(f)) => {
                    let left = human_bytes(lang, u128::from(f.remaining_bytes));
                    match f.days_left {
                        Some(d) => fmt(
                            lang,
                            "hist.forecast.value",
                            &[human_span(lang, (d * DAY as f64) as u64), left],
                        ),
                        None => fmt(lang, "hist.forecast.idle", &[left]),
                    }
                }
            };
            ui.label(text);
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.t("hist.tbw")).weak());
                let r = ui.add(egui::TextEdit::singleline(&mut self.hist.tbw_text).desired_width(70.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) || r.lost_focus() && r.changed() {
                    self.save_tbw();
                }
            });
            if est {
                ui.label(RichText::new(self.t("hist.sata_estimate")).small().weak());
            }
        });
    }

    /// Takes one snapshot of every disk (what the tray does on a schedule) and reloads the view.
    pub fn collect_now(&mut self, ctx: &egui::Context) {
        let cfg = diskpulse_core::collector::Config {
            thresholds: self.settings.thresholds(),
            alerts: self.settings.alert_config(),
        };
        let ctx = ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let store = HistoryStore::new(service::history_dir());
            let n = diskpulse_core::collector::collect_all(&store, &cfg, now_unix())
                .iter()
                .filter(|o| o.recorded.sample.is_some())
                .count();
            let _ = tx.send(n);
            ctx.request_repaint();
        });
        self.collect_job = Some(rx);
    }

    pub fn poll_collect(&mut self) {
        if let Some(rx) = &self.collect_job
            && let Ok(n) = rx.try_recv()
        {
            self.collect_job = None;
            self.toast(self.t(if n > 0 { "hist.collected" } else { "hist.collected_none" }));
            self.history_reload();
        }
    }
}
