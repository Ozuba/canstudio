//! Bus diagnostics: load, per-identifier timing against the DBC, error frames.

use std::time::{Duration, Instant};

use canbus::frame::{fmt_key, is_error_key};
use canbus::stats::{self, BusStats, ERROR_CLASSES, IdStats};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Line, Plot, PlotPoints};
use serde::{Deserialize, Serialize};

use super::plot::PlotWorkspace;
use crate::session::Session;
use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Scope {
    All,
    PlotView,
    /// Rolling window ending now (live) or at the end of the data.
    Recent,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum SortBy {
    Id,
    Name,
    Count,
    Rate,
    Jitter,
    Load,
    Status,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Diagnostics {
    scope: Scope,
    recent_secs: f64,
    sort: SortBy,
    descending: bool,
    /// Allowed deviation of the mean period from `GenMsgCycleTime`, in percent.
    tolerance: f64,
    #[serde(skip)]
    cache: Option<Cached>,
}

struct Cached {
    key: (u64, u64, usize, u64, u64),
    at: Instant,
    stats: BusStats,
    timeline: Vec<[f64; 2]>,
    end: f64,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Diagnostics { scope: Scope::Recent, recent_secs: 2.0, sort: SortBy::Id, descending: false, tolerance: 20.0, cache: None }
    }
}

/// Health of one identifier, worst first when sorted.
#[derive(Clone, PartialEq, PartialOrd)]
enum Status {
    Timeout(f64),
    ErrorFrames,
    LengthMismatch(usize),
    PeriodOff(f64),
    Unknown,
    Ok,
}

impl Status {
    fn text(&self) -> String {
        match self {
            Status::Timeout(age) => format!("⛔ silent for {}", util::fmt_duration(*age)),
            Status::ErrorFrames => "⚠ error frames".into(),
            Status::LengthMismatch(n) => format!("⚠ length ≠ DBC ({n})"),
            Status::PeriodOff(p) => format!("⚠ period {p:+.0}% vs DBC"),
            Status::Unknown => "❔ not in DBC".into(),
            Status::Ok => "✔".into(),
        }
    }

    fn color(&self, ui: &egui::Ui) -> Color32 {
        match self {
            Status::Timeout(_) | Status::ErrorFrames => ui.visuals().error_fg_color,
            Status::LengthMismatch(_) | Status::PeriodOff(_) => ui.visuals().warn_fg_color,
            Status::Unknown => ui.visuals().weak_text_color(),
            Status::Ok => Color32::from_rgb(0x59, 0xc1, 0x4f),
        }
    }
}

impl Diagnostics {
    fn range(&self, s: &Session, plots: &PlotWorkspace) -> (f64, f64) {
        let (a, b) = s.store.time_range().unwrap_or((0.0, 0.0));
        match self.scope {
            Scope::All => (a, b + 1e-9),
            Scope::PlotView => plots.view().unwrap_or((a, b + 1e-9)),
            Scope::Recent => {
                let end = s.now_rel();
                (end - self.recent_secs, end)
            }
        }
    }

    fn refresh(&mut self, s: &Session, plots: &PlotWorkspace) {
        let (a, b) = self.range(s, plots);
        let live = s.is_live() && !s.paused;
        let key =
            (s.store_gen, s.db_gen, s.store.len(), a.to_bits(), b.to_bits() ^ s.bitrate.nominal as u64 ^ (s.bitrate.data as u64) << 32);
        let fresh = self.cache.as_ref().is_some_and(|c| if live { c.at.elapsed() < Duration::from_millis(250) } else { c.key == key });
        if fresh {
            return;
        }
        let range = s.store.range(a, b);
        let span = (self.scope != Scope::All).then_some(b - a);
        let stats = stats::compute(&s.store, range.clone(), s.bitrate, span);
        let bin = ((b - a) / 150.0).max(0.01);
        let timeline = stats::load_timeline(&s.store, range, bin, s.bitrate);
        self.cache = Some(Cached { key, at: Instant::now(), stats, timeline, end: b });
    }

    fn status(&self, s: &Session, st: &IdStats, end: f64) -> Status {
        if is_error_key(st.key) {
            return Status::ErrorFrames;
        }
        let Some(m) = s.db.message(st.key) else { return Status::Unknown };
        let expected = m.cycle_time_ms.map(|c| c / 1e3).or((st.count > 2).then_some(st.period_mean));
        if let Some(p) = expected.filter(|p| *p > 0.0) {
            let age = end - st.last;
            if age > 3.0 * p + 0.05 {
                return Status::Timeout(age);
            }
        }
        if st.len_max != m.size || st.len_min != m.size {
            return Status::LengthMismatch(st.len_max);
        }
        if let (Some(c), true) = (m.cycle_time_ms, st.count > 2) {
            let dev = (st.period_mean * 1e3 / c - 1.0) * 100.0;
            if dev.abs() > self.tolerance {
                return Status::PeriodOff(dev);
            }
        }
        Status::Ok
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, plots: &mut PlotWorkspace) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Scope");
            ui.selectable_value(&mut self.scope, Scope::Recent, "Recent");
            ui.selectable_value(&mut self.scope, Scope::PlotView, "Plot view");
            ui.selectable_value(&mut self.scope, Scope::All, "Everything");
            if self.scope == Scope::Recent {
                ui.add(egui::DragValue::new(&mut self.recent_secs).range(0.1..=600.0).speed(0.1).suffix(" s"));
            }
            ui.separator();
            ui.label("Bitrate");
            ui.add(egui::DragValue::new(&mut s.bitrate.nominal).range(10_000..=1_000_000).speed(1000).suffix(" bit/s"));
            ui.label("FD data");
            ui.add(egui::DragValue::new(&mut s.bitrate.data).range(10_000..=12_000_000).speed(10_000).suffix(" bit/s"));
            ui.separator();
            ui.label("Period tolerance");
            ui.add(egui::DragValue::new(&mut self.tolerance).range(1.0..=100.0).suffix(" %"));
        });
        if s.store.is_empty() {
            ui.label(RichText::new("No data. Load a log or connect to a bus.").weak());
            return;
        }
        self.refresh(s, plots);
        let Some(c) = self.cache.take() else { return };
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        self.summary(ui, &c);
        ui.separator();
        self.id_table(ui, s, plots, &c);
        self.cache = Some(c);
    }

    fn summary(&self, ui: &mut egui::Ui, c: &Cached) {
        let st = &c.stats;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(260.0);
                let load = st.load as f32;
                let color = match load {
                    l if l > 0.8 => Color32::from_rgb(0xe8, 0x57, 0x59),
                    l if l > 0.5 => Color32::from_rgb(0xed, 0xc9, 0x48),
                    _ => Color32::from_rgb(0x59, 0xc1, 0x4f),
                };
                ui.add(egui::ProgressBar::new(load.min(1.0)).fill(color).text(format!("Bus load {:.1} %", load * 100.0)));
                if load > 1.0 {
                    ui.label(
                        RichText::new("Load above 100 %: the configured bitrate is lower than the bus's.")
                            .color(ui.visuals().warn_fg_color)
                            .small(),
                    );
                }
                egui::Grid::new("diag_summary").spacing([12.0, 2.0]).show(ui, |ui| {
                    let mut row = |k: &str, v: String| {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    };
                    row("Frames", format!("{} ({:.0}/s)", st.frames, st.frame_rate()));
                    row("Span", util::fmt_duration(st.duration));
                    row("Identifiers", st.ids.len().to_string());
                    row("Extended / FD / RTR", format!("{} / {} / {}", st.ext_frames, st.fd_frames, st.rtr_frames));
                    row("Error frames", st.error_frames.to_string());
                });
                if st.error_frames > 0 {
                    ui.add_space(4.0);
                    for (n, (_, name)) in ERROR_CLASSES.iter().enumerate() {
                        if st.error_classes[n] > 0 {
                            ui.label(RichText::new(format!("{name}: {}", st.error_classes[n])).color(ui.visuals().error_fg_color));
                        }
                    }
                }
            });
            let pts: Vec<[f64; 2]> = c.timeline.iter().map(|p| [p[0], p[1] * 100.0]).collect();
            Plot::new("bus_load").height(130.0).include_y(0.0).include_y(100.0).allow_scroll(false).y_axis_label("load %").show(
                ui,
                |pui| {
                    pui.line(Line::new("bus load", PlotPoints::new(pts)).fill(0.0).color(Color32::from_rgb(0x4e, 0x9a, 0xf0)));
                },
            );
        });
    }

    fn id_table(&mut self, ui: &mut egui::Ui, s: &Session, plots: &mut PlotWorkspace, c: &Cached) {
        let mut rows: Vec<(&IdStats, Status)> = c.stats.ids.iter().map(|st| (st, self.status(s, st, c.end))).collect();
        let name = |k: u32| s.db.message(k).map_or(String::new(), |m| m.name.clone());
        rows.sort_by(|(a, sa), (b, sb)| {
            let o = match self.sort {
                SortBy::Id => a.key.cmp(&b.key),
                SortBy::Name => name(a.key).cmp(&name(b.key)),
                SortBy::Count => a.count.cmp(&b.count),
                SortBy::Rate => a.rate().total_cmp(&b.rate()),
                SortBy::Jitter => a.period_std.total_cmp(&b.period_std),
                SortBy::Load => a.load.total_cmp(&b.load),
                SortBy::Status => sa.partial_cmp(sb).unwrap_or(std::cmp::Ordering::Equal),
            };
            if self.descending { o.reverse() } else { o }
        });

        // DBC messages with a cycle time that never appeared in this scope.
        let missing: Vec<&dbc::CompiledMessage> =
            s.db.messages.iter().filter(|m| m.cycle_time_ms.is_some() && !c.stats.ids.iter().any(|st| st.key == m.id)).collect();
        if !missing.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new(format!("⛔ {} cyclic DBC messages not seen", missing.len())).color(ui.visuals().warn_fg_color),
            )
            .id_salt("missing")
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for m in missing {
                        ui.label(
                            RichText::new(format!("{} {} ({} ms)", fmt_key(m.id), m.name, m.cycle_time_ms.unwrap_or(0.0))).monospace(),
                        );
                    }
                });
            });
        }

        let columns: [(&str, Option<SortBy>); 12] = [
            ("ID", Some(SortBy::Id)),
            ("Message", Some(SortBy::Name)),
            ("Count", Some(SortBy::Count)),
            ("Rate Hz", Some(SortBy::Rate)),
            ("Period", None),
            ("Min", None),
            ("Max", None),
            ("Jitter σ", Some(SortBy::Jitter)),
            ("DBC cycle", None),
            ("Len", None),
            ("Load", Some(SortBy::Load)),
            ("Status", Some(SortBy::Status)),
        ];
        let mut jump = None;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .column(Column::auto().at_least(70.0))
            .column(Column::initial(170.0).clip(true))
            .columns(Column::auto().at_least(54.0), 9)
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for (title, sort) in columns {
                    h.col(|ui| match sort {
                        Some(by) => {
                            let arrow = if self.sort == by { if self.descending { " ⏷" } else { " ⏶" } } else { "" };
                            if ui.add(egui::Button::new(RichText::new(format!("{title}{arrow}")).strong()).frame(false)).clicked() {
                                self.descending = self.sort == by && !self.descending;
                                self.sort = by;
                            }
                        }
                        None => {
                            ui.strong(title);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, rows.len(), |mut row| {
                    let (st, status) = &rows[row.index()];
                    let m = s.db.message(st.key);
                    let ms = |v: f64| if st.count > 1 { util::fmt_duration(v) } else { "—".into() };
                    let cells = [
                        fmt_key(st.key),
                        m.map_or(String::new(), |m| m.name.clone()),
                        st.count.to_string(),
                        format!("{:.1}", st.rate()),
                        ms(st.period_mean),
                        ms(st.period_min),
                        ms(st.period_max),
                        ms(st.period_std),
                        m.and_then(|m| m.cycle_time_ms).map_or("—".into(), |c| format!("{c} ms")),
                        match (m, st.len_min == st.len_max) {
                            (Some(m), true) => format!("{} / {}", st.len_max, m.size),
                            (Some(m), false) => format!("{}–{} / {}", st.len_min, st.len_max, m.size),
                            (None, _) => st.len_max.to_string(),
                        },
                        format!("{:.2} %", st.load * 100.0),
                    ];
                    for cell in cells {
                        row.col(|ui| {
                            ui.label(RichText::new(cell).monospace());
                        });
                    }
                    row.col(|ui| {
                        ui.label(RichText::new(status.text()).color(status.color(ui)));
                    });
                    if row.response().double_clicked() {
                        jump = Some(st.last);
                    }
                });
            });
        if let Some(t) = jump {
            plots.jump_to(t);
        }
    }
}
