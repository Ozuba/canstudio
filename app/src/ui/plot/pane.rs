//! One plot pane: a stack of traces sharing a Y axis, with value-table
//! state strips drawn over the top of the plot area.

use std::cell::Cell;

use eframe::egui::{self, Color32, RichText};
use egui_plot::{GridMark, Line, Plot, PlotPoints, Points, VLine};
use serde::{Deserialize, Serialize};

use super::{CURSOR_COLORS, FrameCtx, PaneEvents, TraceDrag, decimate::decimate};
use crate::session::Session;
use crate::ui::SignalDrag;
use crate::util;
use dbc::{CompiledSignal, SignalRef};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Autoscale {
    /// Free Y zoom/pan.
    Off,
    /// Fit Y to the samples in the visible time range, continuously.
    Visible,
    /// Fix Y to the DBC min/max of the signals.
    DbcRange,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Style {
    Line,
    Step,
    Points,
}

impl Style {
    const ALL: [(Style, &'static str); 3] = [(Style::Line, "Line"), (Style::Step, "Steps"), (Style::Points, "Points")];
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Trace {
    pub msg: String,
    pub sig: String,
    pub color: Color32,
    /// `None`: steps for value-table signals, lines otherwise.
    pub style: Option<Style>,
    pub visible: bool,
    pub width: f32,
    /// (database generation, resolved reference).
    #[serde(skip)]
    resolved: Cell<Option<(u64, Option<SignalRef>)>>,
}

impl Trace {
    pub fn new(msg: &str, sig: &str, color: Color32) -> Self {
        Trace { msg: msg.into(), sig: sig.into(), color, style: None, visible: true, width: 1.5, resolved: Cell::new(None) }
    }

    /// Resolve `msg.sig` against the current database (cached per generation).
    pub fn resolved(&self, s: &Session) -> Option<SignalRef> {
        match self.resolved.get() {
            Some((g, r)) if g == s.db_gen => r,
            _ => {
                let r = s.db.find(&self.msg, &self.sig);
                self.resolved.set(Some((s.db_gen, r)));
                r
            }
        }
    }

    fn style_for(&self, sig: &CompiledSignal) -> Style {
        self.style.unwrap_or(if sig.is_enum() || sig.size() == 1 { Style::Step } else { Style::Line })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Pane {
    pub id: u64,
    pub traces: Vec<Trace>,
    pub autoscale: Autoscale,
    /// Plot traces against the first trace instead of time.
    pub xy: bool,
    /// Draw value-table state strips over the plot.
    pub strips: bool,
    #[serde(skip)]
    menu_x: Option<f64>,
}

impl Default for Pane {
    fn default() -> Self {
        Pane::new(0)
    }
}

/// A trace resolved for this frame.
struct Live {
    idx: usize,
    r: SignalRef,
    sig: CompiledSignal,
    style: Style,
}

impl Pane {
    pub fn new(id: u64) -> Self {
        Pane { id, traces: Vec::new(), autoscale: Autoscale::Visible, xy: false, strips: true, menu_x: None }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, fc: &FrameCtx, height: f32, active: bool) -> PaneEvents {
        let mut ev = PaneEvents::default();
        let live: Vec<Live> = self
            .traces
            .iter()
            .enumerate()
            .filter_map(|(idx, t)| {
                let r = t.resolved(s)?;
                let sig = s.db.signal(r).clone();
                Some(Live { idx, r, style: t.style_for(&sig), sig })
            })
            .collect();

        self.header(ui, s, fc, &live, active, &mut ev);

        let visible: Vec<&Live> = live.iter().filter(|l| self.traces[l.idx].visible).collect();
        let strips: Vec<&Live> =
            if self.strips && !self.xy { visible.iter().copied().filter(|l| l.sig.is_enum()).collect() } else { Vec::new() };
        // Y-axis ticks become state names when exactly one value-table signal is shown.
        let enum_axis = match visible.as_slice() {
            [only] if only.sig.is_enum() => Some(only.sig.clone()),
            _ => None,
        };

        let mut plot = Plot::new(("pane", self.id))
            .height((height - 26.0).max(80.0))
            .allow_double_click_reset(false)
            .show_axes([fc.show_x_axis || self.xy, true])
            .label_formatter(|_| None)
            .y_axis_min_width(48.0);
        if self.xy {
            plot = plot.data_aspect(0.0).auto_bounds([true, true]);
        } else {
            let (absolute, t0) = (fc.absolute, fc.t0);
            plot = plot
                .link_axis(fc.link, [true, false])
                .link_cursor(fc.link, [true, false])
                .x_axis_formatter(move |mark, _| fmt_time_mark(mark, absolute, t0));
            if self.autoscale != Autoscale::Off {
                plot = plot.allow_drag([true, false]).allow_zoom([true, false]);
            }
        }
        if let Some(sig) = &enum_axis {
            let labels = sig.labels.clone();
            let (factor, offset) = (sig.factor, sig.offset);
            let sig2 = sig.clone();
            plot = plot
                .y_grid_spacer(move |input| {
                    let marks: Vec<GridMark> = labels
                        .iter()
                        .map(|(raw, _)| *raw as f64 * factor + offset)
                        .filter(|v| (input.bounds.0..=input.bounds.1).contains(v))
                        .map(|value| GridMark { value, step_size: factor.abs().max(1.0) * 10.0 })
                        .collect();
                    if marks.len() > 40 { egui_plot::uniform_grid_spacer(|_| [10.0, 1.0, 0.1])(input) } else { marks }
                })
                .y_axis_formatter(move |mark, _| sig2.label(mark.value).map_or_else(|| dbc::codec::format_num(mark.value), str::to_string));
        }

        let autoscale = self.autoscale;
        let xy = self.xy;
        let traces = &self.traces;
        let resp = plot.show(ui, |pui| {
            if let (Some((a, b)), false) = (fc.pending, xy) {
                pui.set_plot_bounds_x(a..=b);
            }
            let bounds = pui.plot_bounds();
            let (x0, x1) = (bounds.min()[0], bounds.max()[0]);
            let px = pui.transform().frame().width() as usize;
            if xy {
                draw_xy(pui, s, traces, &visible);
                return (x0, x1);
            }
            let mut y: Option<(f64, f64)> = None;
            for l in &visible {
                let t = &traces[l.idx];
                let series = s.series(l.r);
                let range = series.visible(x0, x1);
                if let Some((lo, hi)) = series.bounds(range.clone()) {
                    y = Some(y.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
                }
                let pts = decimate(&series.t, &series.v, range, x0, x1, px, l.style == Style::Step);
                let name = format!("{}.{}", t.msg, t.sig);
                match l.style {
                    Style::Points => pui.points(Points::new(name, PlotPoints::new(pts)).color(t.color).radius(t.width + 0.5)),
                    _ => pui.line(Line::new(name, PlotPoints::new(pts)).color(t.color).width(t.width)),
                }
            }
            if autoscale == Autoscale::DbcRange {
                let dbc_range = visible.iter().filter(|l| l.sig.max > l.sig.min).fold(None, |acc: Option<(f64, f64)>, l| {
                    Some(acc.map_or((l.sig.min, l.sig.max), |(a, b)| (a.min(l.sig.min), b.max(l.sig.max))))
                });
                y = dbc_range.or(y);
            }
            if autoscale != Autoscale::Off
                && let Some((lo, hi)) = y
            {
                // Leave headroom at the top for the state strips (sized in pixels).
                let strip_px = if strips.is_empty() { 0.0 } else { strips.len() as f64 * (STRIP_H as f64 + 2.0) + 4.0 };
                let frac = (strip_px / pui.transform().frame().height().max(1.0) as f64).min(0.6);
                let pad = ((hi - lo) * 0.06).max(if hi == lo { lo.abs().max(1.0) * 0.1 } else { 0.0 });
                let top = hi + pad + (hi - lo + 2.0 * pad) * frac / (1.0 - frac);
                pui.set_plot_bounds_y(lo - pad..=top);
            }
            for (i, c) in fc.cursors.iter().enumerate() {
                if let Some(t) = c {
                    pui.vline(VLine::new(["A", "B"][i], *t).color(CURSOR_COLORS[i]).width(1.0));
                }
            }
            (x0, x1)
        });

        let response = &resp.response;
        let transform = resp.transform;
        if !xy {
            ev.bounds_x = Some(resp.inner);
            if !strips.is_empty() {
                draw_strips(ui, s, &strips, &transform);
            }
        }
        if response.dragged() || (response.hovered() && ui.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO)) {
            ev.user_moved = true;
        }

        let pointer_x = response.hover_pos().map(|p| transform.value_from_position(p).x);
        if let (Some(x), false) = (pointer_x, xy)
            && !visible.is_empty()
            && !response.dragged()
        {
            let rows: Vec<(Color32, String, String)> = visible
                .iter()
                .map(|l| {
                    let t = &self.traces[l.idx];
                    let v = s.series(l.r).at(x).map_or("—".to_string(), |(_, v)| l.sig.format(v));
                    (t.color, t.sig.clone(), v)
                })
                .collect();
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.label(RichText::new(fmt_time(x, fc.absolute, fc.t0)).weak());
                egui::Grid::new("hover").spacing([8.0, 1.0]).show(ui, |ui| {
                    for (c, name, v) in rows {
                        ui.label(RichText::new(name).color(c));
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    }
                });
            });
        }
        if response.clicked() {
            ev.activate = true;
            if let (Some(x), false) = (pointer_x, xy) {
                let shift = ui.input(|i| i.modifiers.shift);
                ev.set_cursor = Some((shift as usize, x));
            }
        }
        if response.secondary_clicked() {
            self.menu_x = pointer_x;
        }
        response.context_menu(|ui| {
            if let Some(x) = self.menu_x.filter(|_| !xy) {
                if ui.button("Set cursor A here").clicked() {
                    ev.set_cursor = Some((0, x));
                    ui.close();
                }
                if ui.button("Set cursor B here").clicked() {
                    ev.set_cursor = Some((1, x));
                    ui.close();
                }
            }
            if ui.button("Clear cursors").clicked() {
                ev.clear_cursors = true;
                ui.close();
            }
            if ui.button("Fit all (F)").clicked() {
                ev.fit = true;
                ui.close();
            }
            ui.separator();
            self.options(ui, &mut ev);
        });
        if let Some(d) = response.dnd_release_payload::<SignalDrag>() {
            ev.dropped = Some((*d).clone());
        }
        if let Some(d) = response.dnd_release_payload::<TraceDrag>() {
            ev.moved_trace = Some((d.pane, d.trace));
        }
        if response.dnd_hover_payload::<SignalDrag>().is_some() || response.dnd_hover_payload::<TraceDrag>().is_some() {
            ui.painter().rect_stroke(response.rect, 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Inside);
        }
        ev
    }

    fn options(&mut self, ui: &mut egui::Ui, ev: &mut PaneEvents) {
        ui.label(RichText::new("Y axis").weak());
        ui.radio_value(&mut self.autoscale, Autoscale::Visible, "Autoscale to visible data");
        ui.radio_value(&mut self.autoscale, Autoscale::DbcRange, "DBC min / max");
        ui.radio_value(&mut self.autoscale, Autoscale::Off, "Manual");
        ui.separator();
        ui.checkbox(&mut self.strips, "Value-table strips");
        ui.checkbox(&mut self.xy, "XY mode (first signal = X)");
        ui.separator();
        if ui.button("🗑 Remove pane").clicked() {
            ev.remove = true;
            ui.close();
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, s: &mut Session, fc: &FrameCtx, live: &[Live], active: bool, ev: &mut PaneEvents) {
        let mut remove = None;
        ui.horizontal(|ui| {
            let marker = if active { "▶" } else { " " };
            if ui
                .add(egui::Label::new(RichText::new(marker).monospace()).sense(egui::Sense::click()))
                .on_hover_text("Clicking signals in the tree adds them to the active pane")
                .clicked()
            {
                ev.activate = true;
            }
            for (idx, t) in self.traces.iter_mut().enumerate() {
                let l = live.iter().find(|l| l.idx == idx);
                let value = l.and_then(|l| {
                    let at = fc.cursors[0].unwrap_or(f64::INFINITY);
                    s.series(l.r).at(at).map(|(_, v)| l.sig.format(v))
                });
                let id = egui::Id::new(("trace", self.id, idx));
                let payload = TraceDrag { pane: self.id, trace: idx };
                let resp = ui
                    .dnd_drag_source(id, payload, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            util::swatch(ui, if t.visible { t.color } else { t.color.gamma_multiply(0.25) });
                            let mut text = RichText::new(&t.sig);
                            if l.is_none() {
                                text = text.strikethrough().weak();
                            } else if !t.visible {
                                text = text.weak();
                            }
                            ui.label(text);
                            if let Some(v) = value {
                                ui.label(RichText::new(v).monospace().weak());
                            }
                        });
                    })
                    .response;
                let resp = resp.on_hover_text(match l {
                    Some(l) => format!(
                        "{}.{}\n{} [{} … {}] {}\nClick: show/hide · Drag: move to another pane · Right-click: options",
                        t.msg,
                        t.sig,
                        l.sig.value_type.label(),
                        l.sig.min,
                        l.sig.max,
                        l.sig.unit
                    ),
                    None => format!("{}.{} is not in the loaded databases", t.msg, t.sig),
                });
                if resp.clicked() {
                    t.visible = !t.visible;
                }
                resp.context_menu(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        ui.color_edit_button_srgba(&mut t.color);
                    });
                    ui.add(egui::Slider::new(&mut t.width, 0.5..=5.0).text("width"));
                    ui.label(RichText::new("Style").weak());
                    if ui.radio(t.style.is_none(), "Auto").clicked() {
                        t.style = None;
                    }
                    for (st, name) in Style::ALL {
                        if ui.radio(t.style == Some(st), name).clicked() {
                            t.style = Some(st);
                        }
                    }
                    ui.separator();
                    if ui.button("🗑 Remove").clicked() {
                        remove = Some(idx);
                        ui.close();
                    }
                });
                ui.add_space(6.0);
            }
            if self.traces.is_empty() {
                ui.label(RichText::new("Drag signals here, or click them in the signal tree").weak().italics());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("⚙", |ui| self.options(ui, ev));
            });
        });
        if let Some(i) = remove {
            self.traces.remove(i);
        }
    }
}

/// Height of one value-table strip in pixels.
const STRIP_H: f32 = 16.0;

/// Paint one state strip per value-table signal across the top of the plot.
fn draw_strips(ui: &egui::Ui, s: &mut Session, strips: &[&Live], transform: &egui_plot::PlotTransform) {
    let frame = *transform.frame();
    let painter = ui.painter_at(frame);
    let (x0, x1) = (transform.bounds().min()[0], transform.bounds().max()[0]);
    let h = STRIP_H;
    let font = egui::FontId::proportional(10.5);
    let text_color = ui.visuals().strong_text_color();
    for (row, l) in strips.iter().enumerate() {
        let top = frame.top() + 2.0 + row as f32 * (h + 2.0);
        let series = s.series(l.r);
        let range = series.visible(x0, x1);
        let (t, v) = (&series.t[range.clone()], &series.v[range]);
        let mut i = 0;
        while i < t.len() {
            let key = l.sig.table_key(v[i]);
            let start = t[i];
            while i < t.len() && l.sig.table_key(v[i]) == key {
                i += 1;
            }
            let end = t.get(i).copied().unwrap_or(x1.max(start));
            let xa = transform.position_from_point_x(start).max(frame.left());
            let xb = transform.position_from_point_x(end).min(frame.right());
            if xb - xa < 0.5 {
                continue;
            }
            let rect = egui::Rect::from_x_y_ranges(xa..=xb, top..=top + h);
            painter.rect_filled(rect, 2.0, util::state_color(key));
            // The hover tooltip of the pane shows the full label when it does not fit.
            let label = l.sig.label(v[i - 1]).map_or_else(|| key.to_string(), str::to_string);
            let galley = painter.layout_no_wrap(label, font.clone(), text_color);
            if galley.size().x + 6.0 < rect.width() {
                painter.galley(egui::pos2(rect.left() + 3.0, rect.center().y - galley.size().y / 2.0), galley, text_color);
            }
        }
        painter.text(
            egui::pos2(frame.right() - 4.0, top + h / 2.0),
            egui::Align2::RIGHT_CENTER,
            &l.sig.name,
            egui::FontId::proportional(9.5),
            ui.visuals().weak_text_color(),
        );
    }
}

/// XY mode: every visible trace after the first, against the first.
fn draw_xy(pui: &mut egui_plot::PlotUi, s: &mut Session, traces: &[Trace], visible: &[&Live]) {
    let Some((x, ys)) = visible.split_first() else { return };
    let xs = s.series(x.r).clone();
    for l in ys {
        let t = &traces[l.idx];
        let series = s.series(l.r);
        let step = (series.len() / 20_000).max(1);
        let pts: Vec<[f64; 2]> = (0..series.len()).step_by(step).filter_map(|i| Some([xs.at(series.t[i])?.1, series.v[i]])).collect();
        pui.points(Points::new(format!("{} vs {}", t.sig, x.sig.name), PlotPoints::new(pts)).color(t.color).radius(1.5));
    }
}

fn fmt_time_mark(mark: GridMark, absolute: bool, t0: f64) -> String {
    if absolute {
        let s = crate::util::fmt_wallclock(t0 + mark.value, false);
        // Trim milliseconds on coarse grids.
        if mark.step_size >= 1.0 { s[..8].to_string() } else { s }
    } else {
        let decimals = (-mark.step_size.log10().floor()).clamp(0.0, 9.0) as usize;
        format!("{:.*}", decimals, mark.value)
    }
}

pub fn fmt_time(t: f64, absolute: bool, t0: f64) -> String {
    if absolute { crate::util::fmt_wallclock(t0 + t, true) } else { format!("{t:.6} s") }
}
