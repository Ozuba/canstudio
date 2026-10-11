//! Searchable database → message → signal tree. Signals can be clicked (add to
//! the active plot pane) or dragged onto a pane.

use eframe::egui::{self, RichText};

use super::SignalDrag;
use super::plot::PlotWorkspace;
use crate::session::Session;
use crate::util;

#[derive(Default)]
pub struct SignalTree {
    filter: String,
    /// Hide messages that have no frames in the current data.
    only_present: bool,
}

impl SignalTree {
    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, plots: &mut PlotWorkspace) {
        util::search_box(ui, &mut self.filter, "Filter signals / messages / ids");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.only_present, "Only messages with data");
        });
        ui.separator();
        if s.db.messages.is_empty() {
            ui.label(RichText::new("No database loaded.\nOpen a .dbc (File menu) or drop one on the window.").weak());
            return;
        }
        let filter = self.filter.to_lowercase();
        let focus = plots.focus_time();
        let mut add: Option<SignalDrag> = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (mi, m) in s.db.messages.iter().enumerate() {
                let frames = s.store.indices(m.id).len();
                if self.only_present && frames == 0 {
                    continue;
                }
                let id_txt = canbus::frame::fmt_key(m.id);
                let msg_match = util::matches(&filter, &[&m.name, &id_txt]);
                let sig_match: Vec<usize> =
                    (0..m.signals.len()).filter(|&i| msg_match || util::matches(&filter, &[&m.signals[i].name])).collect();
                if sig_match.is_empty() && !msg_match {
                    continue;
                }
                let title = RichText::new(format!("{id_txt}  {}", m.name)).color(if frames > 0 {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().weak_text_color()
                });
                egui::CollapsingHeader::new(title)
                    .id_salt(("tree_msg", mi, m.id))
                    .open(if filter.is_empty() { None } else { Some(true) })
                    .show(ui, |ui| {
                        for &si in &sig_match {
                            let sig = &m.signals[si];
                            let drag = SignalDrag { msg: m.name.clone(), sig: sig.name.clone() };
                            // Latest value at the focus time, from the raw frames (cheap).
                            let value =
                                focus.and_then(|t| s.store.last_before(m.id, t)).and_then(|f| m.decode(si, f.data)).map(|v| sig.format(v));
                            let plotted = plots.plotted(&m.name, &sig.name);
                            let id = egui::Id::new(("tree_sig", mi, si, m.id));
                            let resp = util::drag_item(ui, id, drag.clone(), |ui| {
                                ui.horizontal(|ui| {
                                    let mut name = RichText::new(&sig.name);
                                    if plotted {
                                        name = name.strong().underline();
                                    }
                                    ui.label(name);
                                    if let Some(v) = value {
                                        ui.label(RichText::new(v).monospace().weak().small());
                                    }
                                });
                            });
                            let resp = resp.on_hover_text(format!(
                                "{} bit {}{}\nscale ×{} {:+}\nrange [{} … {}] {}\n{}Click to plot, drag onto a pane.",
                                sig.size(),
                                sig.value_type.label(),
                                if sig.mux.is_some() { " (multiplexed)" } else { "" },
                                sig.factor,
                                sig.offset,
                                sig.min,
                                sig.max,
                                sig.unit,
                                if sig.is_enum() { format!("{} value labels\n", sig.labels.len()) } else { String::new() },
                            ));
                            if resp.clicked() {
                                add = Some(drag);
                            }
                        }
                    });
            }
        });
        if let Some(d) = add {
            plots.add_signal(&d, Some(plots.active));
        }
    }
}
