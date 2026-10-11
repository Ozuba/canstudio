//! Searchable database → message → signal tree. Signals can be clicked (add to
//! the active plot pane) or dragged onto a pane.

use eframe::egui::{self, RichText};

use super::SignalDrag;
use super::order::{self, MessageOrder};
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
    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, plots: &mut PlotWorkspace, order: &mut MessageOrder) {
        util::search_box(ui, &mut self.filter, "Filter signals / messages / ids");
        ui.horizontal(|ui| {
            order.picker(ui, "tree");
            ui.checkbox(&mut self.only_present, "With data only");
        });
        ui.separator();
        if s.db.messages.is_empty() {
            ui.label(RichText::new("No database loaded.\nOpen a .dbc (File menu) or drop one on the window.").weak());
            return;
        }
        let filter = self.filter.to_lowercase();
        let focus = plots.focus_time();
        let mut add: Option<SignalDrag> = None;

        // Matching signals per message (None = message hidden by the filters).
        let matched: Vec<Option<Vec<usize>>> =
            s.db.messages
                .iter()
                .map(|m| {
                    if self.only_present && s.store.indices(m.id).is_empty() {
                        return None;
                    }
                    let msg_match = util::matches(&filter, &[&m.name, &canbus::frame::fmt_key(m.id)]);
                    let sigs: Vec<usize> =
                        (0..m.signals.len()).filter(|&i| msg_match || util::matches(&filter, &[&m.signals[i].name])).collect();
                    (msg_match || !sigs.is_empty()).then_some(sigs)
                })
                .collect();
        let entries =
            s.db.messages
                .iter()
                .enumerate()
                .filter(|(i, _)| matched[*i].is_some())
                .map(|(index, m)| order::Entry { index, key: m.id, name: &m.name, node: &m.transmitter })
                .collect();
        let groups = order::arrange(*order, entries);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            order::show(ui, &groups, "tree", |ui, mi| {
                let m = &s.db.messages[mi];
                let Some(sig_match) = &matched[mi] else { return };
                let frames = s.store.indices(m.id).len();
                let id_txt = canbus::frame::fmt_key(m.id);
                let title = RichText::new(format!("{id_txt}  {}", m.name)).color(if frames > 0 {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().weak_text_color()
                });
                egui::CollapsingHeader::new(title)
                    .id_salt(("tree_msg", mi, m.id))
                    .open(if filter.is_empty() { None } else { Some(true) })
                    .show(ui, |ui| {
                        for &si in sig_match {
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
            });
        });
        if let Some(d) = add {
            plots.add_signal(&d, Some(plots.active));
        }
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    struct State {
        tree: SignalTree,
        session: Session,
        plots: PlotWorkspace,
        order: MessageOrder,
    }

    /// Message titles from top to bottom as drawn.
    fn drawn_order(h: &Harness<'_, State>, names: &[&str]) -> Vec<String> {
        let mut found: Vec<(f32, String)> =
            names.iter().filter_map(|n| h.query_by_label_contains(n).map(|node| (node.rect().top(), n.to_string()))).collect();
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        found.into_iter().map(|(_, n)| n).collect()
    }

    #[test]
    fn tree_follows_the_selected_order() {
        // File order deliberately unsorted, with an unassigned transmitter.
        let src = "BO_ 864 BMS_RX: 8 ECU\n SG_ a : 0|8@1+ (1,0) [0|0] \"\" X\n\
                   BO_ 37 TER_STATUS: 8 ECU\n SG_ b : 0|8@1+ (1,0) [0|0] \"\" X\n\
                   BO_ 512 BOOT: 8 Vector__XXX\n SG_ c : 0|8@1+ (1,0) [0|0] \"\" X\n\
                   BO_ 1024 BMS_TX: 8 BMS\n SG_ d : 0|8@1+ (1,0) [0|0] \"\" X\n";
        let mut session = Session::default();
        session.dbs.push(crate::session::DbFile { path: None, dbc: dbc::parse(src).unwrap(), dirty: false, errors: vec![], enabled: true });
        session.recompile();
        let state = State { tree: SignalTree::default(), session, plots: PlotWorkspace::default(), order: MessageOrder::Id };
        let mut h = Harness::builder()
            .with_size(egui::vec2(400.0, 600.0))
            .build_ui_state(|ui, st: &mut State| st.tree.ui(ui, &mut st.session, &mut st.plots, &mut st.order), state);
        let names = ["BMS_RX", "TER_STATUS", "BOOT", "BMS_TX"];

        h.run();
        assert_eq!(drawn_order(&h, &names), ["TER_STATUS", "BOOT", "BMS_RX", "BMS_TX"]);

        h.state_mut().order = MessageOrder::File;
        h.run();
        assert_eq!(drawn_order(&h, &names), names);

        h.state_mut().order = MessageOrder::Node;
        h.run();
        // BMS group, ECU group (by id), then unassigned last.
        assert_eq!(drawn_order(&h, &names), ["BMS_TX", "TER_STATUS", "BMS_RX", "BOOT"]);
        assert!(h.query_by_label_contains("(no transmitter)").is_some());
    }
}
