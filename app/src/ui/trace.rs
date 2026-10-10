//! Frame-level views: a scrolling trace of every frame and a cansniffer-like
//! monitor showing the latest frame of each identifier at a point in time.

use std::collections::HashMap;

use canbus::frame::{flags, fmt_key, is_error_key};
use canbus::stats::describe_error;
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};

use super::SignalDrag;
use super::plot::PlotWorkspace;
use crate::session::Session;
use crate::util;

const ROW_H: f32 = 18.0;

fn flag_text(f: u8) -> String {
    let mut s = String::new();
    for (bit, c) in
        [(flags::EXT, 'X'), (flags::FD, 'F'), (flags::BRS, 'B'), (flags::ESI, 'E'), (flags::RTR, 'R'), (flags::ERR, '!'), (flags::TX, 'T')]
    {
        if f & bit != 0 {
            s.push(c);
        }
    }
    s
}

/// One-line decoded summary of a frame.
fn decoded(s: &Session, key: u32, data: &[u8]) -> String {
    if is_error_key(key) {
        return describe_error(key & !canbus::frame::ERROR_KEY, data);
    }
    let Some(m) = s.db.message(key) else { return String::new() };
    m.decode_all(data).map(|(i, v)| format!("{}={}", m.signals[i].name, m.signals[i].format(v))).collect::<Vec<_>>().join("  ")
}

/// Decoded signals of one frame as a grid; clicking a name plots it.
fn signal_grid(ui: &mut egui::Ui, s: &Session, plots: &mut PlotWorkspace, key: u32, data: &[u8]) {
    let Some(m) = s.db.message(key) else {
        ui.label(RichText::new("Not in the loaded databases").weak());
        return;
    };
    egui::Grid::new(("sigs", key)).striped(true).spacing([16.0, 2.0]).show(ui, |ui| {
        for (i, v) in m.decode_all(data) {
            let sig = &m.signals[i];
            let name = if plots.plotted(&m.name, &sig.name) { RichText::new(&sig.name).strong() } else { RichText::new(&sig.name) };
            if ui.link(name).on_hover_text("Plot in the active pane").clicked() {
                let d = SignalDrag { msg: m.name.clone(), sig: sig.name.clone() };
                plots.add_signal(&d, Some(plots.active));
            }
            ui.label(RichText::new(sig.format(v)).monospace());
            ui.label(RichText::new(format!("raw {}", sig.layout.and_then(|l| l.extract(data)).unwrap_or(0))).weak().small());
            ui.end_row();
        }
    });
}

// --- trace ---------------------------------------------------------------------

#[derive(Default)]
pub struct TraceView {
    filter: String,
    errors_only: bool,
    hide_decoded: bool,
    follow: bool,
    selected: Option<usize>,
    /// Row to scroll to once (set by "Go to plot view").
    scroll_to: Option<usize>,
    /// Frame indices passing the filter, and what they were built for.
    rows: Vec<u32>,
    built: (u64, String, bool, usize),
    key_match: HashMap<u32, bool>,
}

impl TraceView {
    fn filter_active(&self) -> bool {
        !self.filter.is_empty() || self.errors_only
    }

    /// Does a frame key pass the filter? Tokens match an id prefix or a message name.
    fn key_matches(&mut self, s: &Session, key: u32) -> bool {
        let errors_only = self.errors_only;
        let filter = &self.filter;
        *self.key_match.entry(key).or_insert_with(|| {
            if errors_only {
                return is_error_key(key);
            }
            let id = fmt_key(key).to_lowercase();
            let name = s.db.message(key).map(|m| m.name.to_lowercase()).unwrap_or_default();
            filter.to_lowercase().split([' ', ',']).filter(|t| !t.is_empty()).any(|t| {
                id.trim_start_matches('0').starts_with(t.trim_start_matches("0x").trim_start_matches('0')) || id == t || name.contains(t)
            })
        })
    }

    fn update_rows(&mut self, s: &Session) {
        let key = (s.store_gen ^ s.db_gen.rotate_left(32), self.filter.clone(), self.errors_only, 0);
        if (key.0, &key.1, key.2) != (self.built.0, &self.built.1, self.built.2) {
            self.rows.clear();
            self.key_match.clear();
            self.built = key;
        }
        if !self.filter_active() {
            return;
        }
        for i in self.built.3..s.store.len() {
            if self.key_matches(s, s.store.key[i]) {
                self.rows.push(i as u32);
            }
        }
        self.built.3 = s.store.len();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, plots: &mut PlotWorkspace) {
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("ids or names, e.g. 1A0 BMS 18FEF1").desired_width(220.0));
            ui.checkbox(&mut self.errors_only, "Error frames only");
            let mut decode = !self.hide_decoded;
            if ui.checkbox(&mut decode, "Decode").changed() {
                self.hide_decoded = !decode;
            }
            if s.is_live() {
                ui.checkbox(&mut self.follow, "Follow");
            }
            if let Some((a, b)) = plots.view()
                && ui.button("Go to plot view").on_hover_text("Scroll to the first frame in the visible plot range").clicked()
            {
                let first = s.store.range(a, b).start;
                let row = if self.filter_active() { self.rows.partition_point(|&i| (i as usize) < first) } else { first };
                self.selected = Some(row);
                self.scroll_to = Some(row);
                self.follow = false;
            }
        });
        self.update_rows(s);
        let n = if self.filter_active() { self.rows.len() } else { s.store.len() };
        ui.label(RichText::new(format!("{n} frames · double-click a row to show it in the plots")).weak().small());

        let detail_h = if self.selected.is_some() { 140.0 } else { 0.0 };
        let t0 = s.store.t0;
        let mut clicked = None;
        let mut jump = None;
        let table_h = ui.available_height() - detail_h;
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .stick_to_bottom(self.follow)
            .max_scroll_height(table_h)
            .column(Column::initial(110.0))
            .column(Column::initial(46.0))
            .column(Column::initial(80.0))
            .column(Column::initial(160.0))
            .column(Column::initial(34.0))
            .column(Column::initial(30.0))
            .column(Column::initial(200.0))
            .column(Column::remainder().clip(true));
        if let Some(row) = self.scroll_to.take() {
            table = table.scroll_to_row(row, Some(egui::Align::Center));
        }
        table
            .header(20.0, |mut h| {
                for title in ["Time (s)", "Chan", "ID", "Message", "Flags", "Len", "Data", "Decoded"] {
                    h.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, n, |mut row| {
                    let r = row.index();
                    let i = if self.filter_active() { self.rows[r] as usize } else { r };
                    let f = s.store.get(i);
                    row.set_selected(self.selected == Some(r));
                    let err = is_error_key(f.key);
                    let color = if err { Some(Color32::from_rgb(0xff, 0x66, 0x66)) } else { None };
                    let text = |t: String| {
                        let rt = RichText::new(t).monospace();
                        match color {
                            Some(c) => rt.color(c),
                            None => rt,
                        }
                    };
                    row.col(|ui| {
                        ui.label(text(format!("{:.6}", f.ts)));
                    });
                    row.col(|ui| {
                        ui.label(text(s.store.channels.get(f.channel as usize).cloned().unwrap_or_default()));
                    });
                    row.col(|ui| {
                        ui.label(text(fmt_key(f.key)));
                    });
                    row.col(|ui| {
                        ui.label(s.db.message(f.key).map_or("", |m| m.name.as_str()));
                    });
                    row.col(|ui| {
                        ui.label(text(flag_text(f.flags)));
                    });
                    row.col(|ui| {
                        ui.label(text(f.data.len().to_string()));
                    });
                    row.col(|ui| {
                        ui.label(text(util::hex_bytes(f.data)));
                    });
                    row.col(|ui| {
                        if !self.hide_decoded || err {
                            ui.label(RichText::new(decoded(s, f.key, f.data)).small());
                        }
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        clicked = Some(r);
                    }
                    if resp.double_clicked() {
                        jump = Some(f.ts);
                    }
                    resp.on_hover_text(util::fmt_wallclock(t0 + f.ts, true));
                });
            });
        if let Some(r) = clicked {
            self.selected = Some(r);
            self.follow = false;
        }
        if let Some(t) = jump {
            plots.jump_to(t);
        }
        if let Some(r) = self.selected.filter(|&r| r < n) {
            let i = if self.filter_active() { self.rows[r] as usize } else { r };
            let f = s.store.get(i);
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong(format!("{} {}", fmt_key(f.key), s.db.message(f.key).map_or("", |m| m.name.as_str())));
                ui.label(RichText::new(format!("@ {:.6} s", f.ts)).weak());
                if ui.small_button("Show in plots").clicked() {
                    plots.jump_to(f.ts);
                }
                if ui.small_button("✖").clicked() {
                    self.selected = None;
                }
            });
            egui::ScrollArea::vertical().id_salt("trace_detail").show(ui, |ui| {
                if is_error_key(f.key) {
                    ui.label(decoded(s, f.key, f.data));
                } else {
                    signal_grid(ui, s, plots, f.key, f.data);
                }
            });
        }
    }
}

// --- monitor -------------------------------------------------------------------

#[derive(Default)]
pub struct MonitorView {
    filter: String,
    selected: Option<u32>,
    /// Highlight bytes changed within this many seconds.
    highlight: f64,
}

impl MonitorView {
    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, plots: &mut PlotWorkspace) {
        if self.highlight == 0.0 {
            self.highlight = 1.0;
        }
        let live = s.is_live() && !s.paused;
        let at = if live { s.now_rel() } else { plots.focus_time().unwrap_or(f64::INFINITY) };
        ui.horizontal(|ui| {
            util::search_box(ui, &mut self.filter, "Filter ids / names");
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if live {
                    "Live state".to_string()
                } else {
                    format!("State at {} (cursor A or plot view end)", if at.is_finite() { format!("{at:.3} s") } else { "end".into() })
                })
                .weak(),
            );
            ui.add(egui::DragValue::new(&mut self.highlight).range(0.05..=60.0).speed(0.05).prefix("highlight changes < ").suffix(" s"));
        });
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        let filter = self.filter.to_lowercase();
        let mut keys: Vec<u32> = s
            .store
            .keys
            .iter()
            .copied()
            .filter(|&k| util::matches(&filter, &[&fmt_key(k), s.db.message(k).map_or("", |m| m.name.as_str())]))
            .collect();
        keys.sort_unstable();

        struct Row {
            key: u32,
            count: usize,
            period: Option<f64>,
            age: f64,
            data: Vec<u8>,
            changed: Vec<Option<f64>>,
        }
        let rows: Vec<Row> = keys
            .iter()
            .filter_map(|&key| {
                let idx = s.store.indices(key);
                let count = idx.partition_point(|&i| s.store.ts[i as usize] <= at);
                let last = *idx.get(count.checked_sub(1)?)? as usize;
                let data = s.store.data(last).to_vec();
                let period = (count >= 2).then(|| {
                    let back = count.saturating_sub(11);
                    (s.store.ts[last] - s.store.ts[idx[back] as usize]) / (count - 1 - back) as f64
                });
                // Age of the last change of each byte, looking back a bounded number of frames.
                let mut changed = vec![None; data.len()];
                let mut cur = data.clone();
                for &j in idx[count.saturating_sub(200)..count.saturating_sub(1)].iter().rev() {
                    let prev = s.store.data(j as usize);
                    let t = s.store.ts[j as usize];
                    for (b, slot) in changed.iter_mut().enumerate() {
                        if slot.is_none() && prev.get(b) != cur.get(b) {
                            *slot = Some(t);
                        }
                    }
                    cur = prev.to_vec();
                    if changed.iter().all(Option::is_some) || at - t > self.highlight {
                        break;
                    }
                }
                Some(Row { key, count, period, age: at.min(s.now_rel()) - s.store.ts[last], data, changed })
            })
            .collect();

        let detail_h = if self.selected.is_some() { 180.0 } else { 0.0 };
        let mut clicked = None;
        let hl = self.highlight;
        let table_h = ui.available_height() - detail_h;
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .max_scroll_height(table_h)
            .column(Column::initial(80.0))
            .column(Column::initial(170.0))
            .column(Column::initial(70.0))
            .column(Column::initial(80.0))
            .column(Column::initial(70.0))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for title in ["ID", "Message", "Count", "Period", "Age", "Data"] {
                    h.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    row.set_selected(self.selected == Some(r.key));
                    row.col(|ui| {
                        ui.monospace(fmt_key(r.key));
                    });
                    row.col(|ui| {
                        ui.label(s.db.message(r.key).map_or("", |m| m.name.as_str()));
                    });
                    row.col(|ui| {
                        ui.monospace(r.count.to_string());
                    });
                    row.col(|ui| {
                        ui.monospace(r.period.map_or("—".into(), util::fmt_duration));
                    });
                    row.col(|ui| {
                        let stale = r.period.is_some_and(|p| r.age > 3.0 * p.max(0.001));
                        let t = RichText::new(util::fmt_duration(r.age)).monospace();
                        ui.label(if stale { t.color(ui.visuals().warn_fg_color) } else { t.weak() });
                    });
                    row.col(|ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            for (b, byte) in r.data.iter().enumerate() {
                                let mut t = RichText::new(format!("{byte:02X}")).monospace();
                                if let Some(ct) = r.changed[b] {
                                    let fade = (1.0 - ((at.min(s.now_rel()) - ct) / hl).clamp(0.0, 1.0)) as f32;
                                    if fade > 0.0 {
                                        t = t
                                            .background_color(Color32::from_rgb(0xd9, 0x82, 0x1e).gamma_multiply(fade))
                                            .color(Color32::WHITE);
                                    }
                                }
                                ui.label(t);
                            }
                        });
                    });
                    if row.response().clicked() {
                        clicked = Some(r.key);
                    }
                });
            });
        if let Some(k) = clicked {
            self.selected = if self.selected == Some(k) { None } else { Some(k) };
        }
        if let Some(key) = self.selected
            && let Some(r) = rows.iter().find(|r| r.key == key)
        {
            ui.separator();
            ui.strong(format!("{} {}", fmt_key(key), s.db.message(key).map_or("", |m| m.name.as_str())));
            egui::ScrollArea::vertical().id_salt("monitor_detail").show(ui, |ui| {
                if is_error_key(key) {
                    ui.label(decoded(s, key, &r.data));
                } else {
                    signal_grid(ui, s, plots, key, &r.data);
                }
            });
        }
    }
}
