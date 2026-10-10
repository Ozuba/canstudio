//! The plotting workspace: stacked panes sharing one time axis, time
//! navigation (presets, follow, overview), measurement cursors and export.

mod decimate;
mod export;
mod overview;
mod pane;

use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::ui::SignalDrag;
use crate::util;
pub use pane::{Pane, Trace};

/// Visible-window presets in seconds (`None` = everything).
const PRESETS: [(&str, Option<f64>); 7] = [
    ("10 ms", Some(0.01)),
    ("100 ms", Some(0.1)),
    ("1 s", Some(1.0)),
    ("10 s", Some(10.0)),
    ("1 min", Some(60.0)),
    ("10 min", Some(600.0)),
    ("All", None),
];

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct PlotWorkspace {
    pub panes: Vec<Pane>,
    /// Width of the time window in follow mode (seconds).
    pub window: f64,
    /// Keep the newest data in view (live).
    pub follow: bool,
    /// Wall-clock time axis instead of seconds since start.
    pub absolute_time: bool,
    /// Measurement cursors A and B (relative time).
    pub cursors: [Option<f64>; 2],
    /// Pane receiving signals added by clicking in the tree.
    pub active: usize,
    next_id: u64,
    next_color: usize,
    #[serde(skip)]
    view: Option<(f64, f64)>,
    #[serde(skip)]
    pending: Option<(f64, f64)>,
    #[serde(skip)]
    seen_store_gen: u64,
    #[serde(skip)]
    overview: overview::Overview,
    #[serde(skip)]
    show_stats: bool,
}

impl Default for PlotWorkspace {
    fn default() -> Self {
        PlotWorkspace {
            panes: vec![Pane::new(0)],
            window: 10.0,
            follow: true,
            absolute_time: false,
            cursors: [None, None],
            active: 0,
            next_id: 1,
            next_color: 0,
            view: None,
            pending: None,
            seen_store_gen: u64::MAX,
            overview: Default::default(),
            show_stats: true,
        }
    }
}

/// Per-frame context shared by all panes.
#[derive(Clone)]
pub struct FrameCtx {
    pub link: egui::Id,
    pub pending: Option<(f64, f64)>,
    pub absolute: bool,
    pub t0: f64,
    pub cursors: [Option<f64>; 2],
    pub show_x_axis: bool,
}

/// What a pane asks the workspace to do after drawing.
#[derive(Default)]
pub struct PaneEvents {
    pub bounds_x: Option<(f64, f64)>,
    pub user_moved: bool,
    pub set_cursor: Option<(usize, f64)>,
    pub clear_cursors: bool,
    pub fit: bool,
    pub remove: bool,
    pub activate: bool,
    pub dropped: Option<SignalDrag>,
    pub moved_trace: Option<(u64, usize)>,
}

/// Payload for dragging a trace from one pane to another.
#[derive(Clone)]
pub struct TraceDrag {
    pub pane: u64,
    pub trace: usize,
}

impl PlotWorkspace {
    /// Currently visible time range (relative seconds).
    pub fn view(&self) -> Option<(f64, f64)> {
        self.view
    }

    pub fn set_view(&mut self, a: f64, b: f64) {
        if b > a {
            self.pending = Some((a, b));
            self.follow = false;
        }
    }

    /// Centre the view on `t` and put cursor A there.
    pub fn jump_to(&mut self, t: f64) {
        let half = self.view.map_or(0.5, |(a, b)| (b - a) / 2.0);
        self.set_view(t - half, t + half);
        self.cursors[0] = Some(t);
    }

    /// Reference time for "state at" views (monitor): cursor A, else view end.
    pub fn focus_time(&self) -> Option<f64> {
        self.cursors[0].or(self.view.map(|v| v.1))
    }

    fn new_pane(&mut self) -> usize {
        self.panes.push(Pane::new(self.next_id));
        self.next_id += 1;
        self.panes.len() - 1
    }

    /// Add a signal to pane `pane` (a new pane when out of range).
    pub fn add_signal(&mut self, drag: &SignalDrag, pane: Option<usize>) {
        let p = match pane {
            Some(p) if p < self.panes.len() => p,
            _ => self.new_pane(),
        };
        if self.panes[p].traces.iter().any(|t| t.msg == drag.msg && t.sig == drag.sig) {
            return;
        }
        let color = util::palette(self.next_color);
        self.next_color += 1;
        self.panes[p].traces.push(Trace::new(&drag.msg, &drag.sig, color));
        self.active = p;
    }

    pub fn plotted(&self, msg: &str, sig: &str) -> bool {
        self.panes.iter().any(|p| p.traces.iter().any(|t| t.msg == msg && t.sig == sig))
    }

    fn full_range(s: &Session) -> Option<(f64, f64)> {
        s.store.time_range().map(|(a, b)| (a, b.max(a + 1e-3)))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        let live = s.is_live() && !s.paused;
        if s.store_gen != self.seen_store_gen {
            self.seen_store_gen = s.store_gen;
            self.follow = s.is_live();
            self.cursors = [None, None];
            self.pending = Self::full_range(s);
        }
        if self.follow && live {
            let now = s.now_rel();
            self.pending = Some((now - self.window, now));
        }

        self.toolbar(ui, s);
        if !s.store.is_empty()
            && let Some((a, b)) = self.overview.ui(ui, s, self.view)
        {
            self.set_view(a, b);
        }
        self.keyboard(ui, s);

        let readout = self.cursors.iter().any(Option::is_some) && self.show_stats;
        let readout_h = if readout { 28.0 + 20.0 * self.panes.iter().map(|p| p.traces.len()).sum::<usize>().min(8) as f32 } else { 0.0 };
        let drop_h = 26.0;
        let avail = ui.available_height() - readout_h - drop_h - 8.0;
        let pane_h = (avail / self.panes.len().max(1) as f32).max(140.0);

        // NaN marks "fit everything".
        if self.pending.is_some_and(|p| p.0.is_nan()) {
            self.pending = Self::full_range(s);
        }
        let ctx = FrameCtx {
            link: ui.id().with("plot_link"),
            pending: self.pending.take(),
            absolute: self.absolute_time,
            t0: s.store.t0,
            cursors: self.cursors,
            show_x_axis: true,
        };

        let mut events: Vec<(usize, PaneEvents)> = Vec::new();
        egui::ScrollArea::vertical().max_height(avail + 4.0).auto_shrink([false, true]).show(ui, |ui| {
            let n = self.panes.len();
            for (i, pane) in self.panes.iter_mut().enumerate() {
                let fc = FrameCtx { show_x_axis: i + 1 == n, ..ctx.clone() };
                events.push((i, pane.ui(ui, s, &fc, pane_h, i == self.active)));
            }
        });
        self.drop_zone(ui);
        self.apply(events);

        if readout {
            ui.separator();
            self.cursor_readout(ui, s);
        }
    }

    fn apply(&mut self, events: Vec<(usize, PaneEvents)>) {
        let mut remove = None;
        for (i, ev) in events {
            if (i == 0 || self.view.is_none())
                && let Some(b) = ev.bounds_x
            {
                self.view = Some(b);
            }
            if ev.user_moved {
                self.follow = false;
            }
            if let Some((c, t)) = ev.set_cursor {
                self.cursors[c] = Some(t);
            }
            if ev.clear_cursors {
                self.cursors = [None, None];
            }
            if ev.fit {
                self.pending = Some((f64::NAN, f64::NAN));
            }
            if ev.activate {
                self.active = i;
            }
            if ev.remove {
                remove = Some(i);
            }
            if let Some(d) = ev.dropped {
                self.add_signal(&d, Some(i));
            }
            if let Some((from, idx)) = ev.moved_trace
                && let Some(src) = self.panes.iter().position(|p| p.id == from)
                && src != i
                && idx < self.panes[src].traces.len()
            {
                let t = self.panes[src].traces.remove(idx);
                self.panes[i].traces.push(t);
            }
        }
        if let Some(i) = remove {
            self.panes.remove(i);
            if self.panes.is_empty() {
                self.new_pane();
            }
            self.active = self.active.min(self.panes.len() - 1);
        }
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
        let hovering = resp.dnd_hover_payload::<SignalDrag>().is_some() || resp.dnd_hover_payload::<TraceDrag>().is_some();
        let stroke = if hovering { ui.visuals().selection.stroke } else { ui.visuals().widgets.noninteractive.bg_stroke };
        ui.painter().rect_stroke(rect.shrink(1.0), 4.0, stroke, egui::StrokeKind::Inside);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Drop a signal here for a new pane",
            egui::FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
        if let Some(d) = resp.dnd_release_payload::<SignalDrag>() {
            self.add_signal(&d, None);
        }
        if let Some(d) = resp.dnd_release_payload::<TraceDrag>()
            && let Some(src) = self.panes.iter().position(|p| p.id == d.pane)
            && d.trace < self.panes[src].traces.len()
        {
            let t = self.panes[src].traces.remove(d.trace);
            let p = self.new_pane();
            self.panes[p].traces.push(t);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        ui.horizontal_wrapped(|ui| {
            for (label, secs) in PRESETS {
                if ui.small_button(label).clicked() {
                    match secs {
                        None => self.pending = Some((f64::NAN, f64::NAN)),
                        Some(w) => {
                            self.window = w;
                            let end = if self.follow && s.is_live() {
                                s.now_rel()
                            } else {
                                self.view.map_or(s.now_rel(), |(a, b)| (a + b) / 2.0 + w / 2.0)
                            };
                            self.pending = Some((end - w, end));
                        }
                    }
                }
            }
            ui.separator();
            let (a, b) = self.view.unwrap_or((0.0, 1.0));
            if ui.small_button("➖").on_hover_text("Zoom out (−)").clicked() {
                self.zoom(2.0);
            }
            if ui.small_button("➕").on_hover_text("Zoom in (+)").clicked() {
                self.zoom(0.5);
            }
            if ui.small_button("⏮").on_hover_text("Jump to start (Home)").clicked()
                && let Some((t0, _)) = Self::full_range(s)
            {
                self.set_view(t0, t0 + (b - a));
            }
            if ui.small_button("⏭").on_hover_text("Jump to end (End)").clicked() {
                let end = s.now_rel();
                self.set_view(end - (b - a), end);
            }
            if let [Some(ca), Some(cb)] = self.cursors
                && ui.small_button("↔ A–B").on_hover_text("Zoom to the cursors").clicked()
            {
                let (lo, hi) = (ca.min(cb), ca.max(cb));
                let pad = (hi - lo) * 0.05;
                self.set_view(lo - pad, hi + pad);
            }
            ui.separator();
            let mut from = a;
            let mut to = b;
            ui.label("from");
            let r1 = ui.add(egui::DragValue::new(&mut from).speed((b - a) * 0.01).max_decimals(6).suffix(" s"));
            ui.label("to");
            let r2 = ui.add(egui::DragValue::new(&mut to).speed((b - a) * 0.01).max_decimals(6).suffix(" s"));
            if (r1.changed() || r2.changed()) && to > from {
                self.set_view(from, to);
            }
            ui.separator();
            if s.is_live() {
                ui.toggle_value(&mut self.follow, "▶ Follow").on_hover_text("Keep the newest data in view");
                ui.add(egui::DragValue::new(&mut self.window).range(0.001..=3600.0).speed(0.1).suffix(" s window"));
            }
            ui.toggle_value(&mut self.absolute_time, "🕑 Wall clock").on_hover_text("Absolute time axis");
            ui.separator();
            let has_cursors = self.cursors.iter().any(Option::is_some);
            ui.add_enabled_ui(has_cursors, |ui| {
                ui.toggle_value(&mut self.show_stats, "📏 Readout");
                if ui.small_button("✖ Cursors").clicked() {
                    self.cursors = [None, None];
                }
            });
            ui.menu_button("💾 Export", |ui| {
                let (lo, hi) = self.export_range(s);
                ui.label(RichText::new(format!("Range: {} … {}", util::fmt_duration(lo), util::fmt_duration(hi))).weak());
                if ui.button("Plotted signals → CSV (wide, sample & hold)").clicked() {
                    export::csv(s, &self.panes, lo, hi, true);
                    ui.close();
                }
                if ui.button("Plotted signals → CSV (long, raw samples)").clicked() {
                    export::csv(s, &self.panes, lo, hi, false);
                    ui.close();
                }
                if ui.button("Frames in range → candump log").clicked() {
                    s.save_range(lo, hi);
                    ui.close();
                }
            })
            .response
            .on_hover_text("Exports the cursor A–B range, or the visible range without cursors");
            if ui.small_button("➕ Pane").clicked() {
                self.active = self.new_pane();
            }
        });
    }

    /// Cursor A–B if both set, else the visible range.
    fn export_range(&self, s: &Session) -> (f64, f64) {
        match self.cursors {
            [Some(a), Some(b)] => (a.min(b), a.max(b)),
            _ => self.view.or_else(|| Self::full_range(s)).unwrap_or((0.0, 0.0)),
        }
    }

    fn zoom(&mut self, factor: f64) {
        if let Some((a, b)) = self.view {
            let c = self.cursors[0].filter(|c| (a..=b).contains(c)).unwrap_or((a + b) / 2.0);
            self.set_view(c - (c - a) * factor, c + (b - c) * factor);
        }
    }

    fn keyboard(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        if ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        let Some((a, b)) = self.view else { return };
        let w = b - a;
        ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.set_view(a - w * 0.25, b - w * 0.25);
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                self.set_view(a + w * 0.25, b + w * 0.25);
            }
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                self.zoom(0.5);
            }
            if i.key_pressed(egui::Key::Minus) {
                self.zoom(2.0);
            }
            if i.key_pressed(egui::Key::F) {
                self.pending = Some((f64::NAN, f64::NAN));
            }
        });
        if ui.input(|i| i.key_pressed(egui::Key::Space)) && s.is_live() {
            self.follow = !self.follow;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Home))
            && let Some((t0, _)) = Self::full_range(s)
        {
            self.set_view(t0, t0 + w);
        }
        if ui.input(|i| i.key_pressed(egui::Key::End)) {
            let end = s.now_rel();
            self.set_view(end - w, end);
        }
    }

    /// Table of values at the cursors, their difference and A–B statistics.
    fn cursor_readout(&self, ui: &mut egui::Ui, s: &mut Session) {
        let [ca, cb] = self.cursors;
        let fmt_t = |t: Option<f64>| t.map_or("—".into(), |t| format!("{t:.6} s"));
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("A {}", fmt_t(ca))).color(CURSOR_COLORS[0]));
            ui.label(RichText::new(format!("B {}", fmt_t(cb))).color(CURSOR_COLORS[1]));
            if let (Some(a), Some(b)) = (ca, cb) {
                let dt = b - a;
                ui.label(format!("Δt {}", util::fmt_duration(dt)));
                if dt != 0.0 {
                    ui.label(format!("1/Δt {:.3} Hz", 1.0 / dt.abs()));
                }
            }
        });
        egui::ScrollArea::vertical().id_salt("readout").max_height(ui.available_height()).show(ui, |ui| {
            egui::Grid::new("readout_grid").striped(true).spacing([16.0, 2.0]).show(ui, |ui| {
                for h in ["Signal", "@A", "@B", "Δ", "min", "max", "mean"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for trace in self.panes.iter().flat_map(|p| &p.traces) {
                    let Some(r) = trace.resolved(s) else { continue };
                    let sig = s.db.signal(r).clone();
                    let series = s.series(r);
                    let va = ca.and_then(|t| series.at(t)).map(|p| p.1);
                    let vb = cb.and_then(|t| series.at(t)).map(|p| p.1);
                    let stats = match (ca, cb) {
                        (Some(a), Some(b)) => {
                            let rng = series.visible(a.min(b), a.max(b));
                            let vals = &series.v[rng];
                            (!vals.is_empty()).then(|| {
                                let (lo, hi) = vals.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &v| (l.min(v), h.max(v)));
                                (lo, hi, vals.iter().sum::<f64>() / vals.len() as f64)
                            })
                        }
                        _ => None,
                    };
                    let f = |v: Option<f64>| v.map_or("—".to_string(), |v| sig.format(v));
                    ui.horizontal(|ui| {
                        util::swatch(ui, trace.color);
                        ui.label(&trace.sig);
                    });
                    ui.label(f(va));
                    ui.label(f(vb));
                    ui.label(match (va, vb) {
                        (Some(a), Some(b)) => dbc::codec::format_num(b - a),
                        _ => "—".into(),
                    });
                    match stats {
                        Some((lo, hi, mean)) => {
                            ui.label(dbc::codec::format_num(lo));
                            ui.label(dbc::codec::format_num(hi));
                            ui.label(dbc::codec::format_num(mean));
                        }
                        None => {
                            for _ in 0..3 {
                                ui.label("—");
                            }
                        }
                    }
                    ui.end_row();
                }
            });
        });
    }
}

pub const CURSOR_COLORS: [Color32; 2] = [Color32::from_rgb(0xff, 0xd1, 0x66), Color32::from_rgb(0x6e, 0xe7, 0xff)];
