//! Message page: properties, attributes, bit layout, signal table, signal
//! editor and a live decode preview of the latest matching frame.

use dbc::{AttrObject, ByteOrder, Dbc, EXTENDED_FLAG, ExtMux, Multiplexer, Signal, VECTOR_XXX, ValueType};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};

use super::widgets::{self, commit_text};
use crate::session::Session;
use crate::util;

/// What the message page changed.
#[derive(Default)]
pub struct Outcome {
    pub changed: bool,
    pub delete: bool,
    pub select_signal: Option<Option<usize>>,
}

pub fn page(ui: &mut egui::Ui, s: &Session, dbc: &mut Dbc, mi: usize, selected_sig: Option<usize>) -> Outcome {
    let mut out = Outcome::default();
    let nodes: Vec<String> = dbc.nodes.iter().map(|n| n.name.clone()).collect();

    ui.horizontal(|ui| {
        ui.heading(&dbc.messages[mi].name);
        ui.label(RichText::new(format!("0x{:X}", dbc.messages[mi].raw_id())).monospace().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("🗑 Delete message").clicked() {
                out.delete = true;
            }
        });
    });
    ui.separator();

    egui::Grid::new("msg_props").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Name");
        if let Some(n) = commit_text(ui, egui::Id::new(("msg_name", mi)), &dbc.messages[mi].name.clone(), 240.0) {
            dbc.messages[mi].name = n;
            out.changed = true;
        }
        ui.end_row();

        ui.label("ID");
        ui.horizontal(|ui| {
            let m = &dbc.messages[mi];
            let ext = m.is_extended();
            let mut raw = m.raw_id();
            let max = if ext { 0x1FFF_FFFF } else { 0x7FF };
            let r1 = ui.add(egui::DragValue::new(&mut raw).range(0..=max).hexadecimal(3, false, true).prefix("0x"));
            let mut ext2 = ext;
            let r2 = ui.checkbox(&mut ext2, "Extended (29-bit)");
            if r1.changed() || r2.changed() {
                dbc.set_message_id(mi, raw.min(if ext2 { 0x1FFF_FFFF } else { 0x7FF }) | if ext2 { EXTENDED_FLAG } else { 0 });
                out.changed = true;
            }
            ui.label(RichText::new(format!("dec {raw}")).weak());
        });
        ui.end_row();

        let m = &mut dbc.messages[mi];
        ui.label("Length");
        ui.horizontal(|ui| {
            out.changed |= ui.add(egui::DragValue::new(&mut m.size).range(0..=64).suffix(" bytes")).changed();
            for n in [8u64, 64] {
                if ui.small_button(format!("{n}")).clicked() {
                    m.size = n;
                    out.changed = true;
                }
            }
        });
        ui.end_row();

        ui.label("Transmitter");
        egui::ComboBox::from_id_salt("transmitter").selected_text(&m.transmitter).show_ui(ui, |ui| {
            out.changed |= ui.selectable_value(&mut m.transmitter, VECTOR_XXX.to_string(), VECTOR_XXX).changed();
            for n in &nodes {
                out.changed |= ui.selectable_value(&mut m.transmitter, n.clone(), n).changed();
            }
        });
        ui.end_row();

        ui.label("Comment");
        out.changed |= widgets::comment(ui, &mut m.comment);
        ui.end_row();
    });

    egui::CollapsingHeader::new("Attributes").id_salt(("msg_attrs", mi)).show(ui, |ui| {
        let defs = dbc.attribute_defs.clone();
        out.changed |= widgets::attributes(ui, ("msg", mi), &defs, AttrObject::Message, &mut dbc.messages[mi].attributes);
    });

    egui::CollapsingHeader::new("Bit layout").default_open(true).show(ui, |ui| {
        match widgets::bit_layout(ui, &dbc.messages[mi], selected_sig) {
            Some(widgets::LayoutEvent::Select(si)) => out.select_signal = Some(Some(si)),
            Some(widgets::LayoutEvent::Move { sig, start_bit }) => {
                dbc.messages[mi].signals[sig].start_bit = start_bit;
                out.select_signal = Some(Some(sig));
                out.changed = true;
            }
            Some(widgets::LayoutEvent::Create { start_bit, size }) => {
                let m = &mut dbc.messages[mi];
                let mut name = format!("Signal_{}", m.signals.len() + 1);
                while m.signals.iter().any(|s| s.name == name) {
                    name.push('_');
                }
                let mut sig = Signal::new(name, start_bit);
                sig.size = size;
                m.signals.push(sig);
                out.select_signal = Some(Some(m.signals.len() - 1));
                out.changed = true;
            }
            None => {}
        }
    });

    decode_preview(ui, s, dbc, mi);

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.strong(format!("Signals ({})", dbc.messages[mi].signals.len()));
        if ui.button("➕ Add signal").clicked() {
            let m = &mut dbc.messages[mi];
            let start = widgets::next_free_bit(m);
            let mut name = format!("Signal_{}", m.signals.len() + 1);
            while m.signals.iter().any(|s| s.name == name) {
                name.push('_');
            }
            m.signals.push(Signal::new(name, start));
            out.select_signal = Some(Some(m.signals.len() - 1));
            out.changed = true;
        }
        if let Some(si) = selected_sig.filter(|&i| i < dbc.messages[mi].signals.len()) {
            if ui.button("📋 Duplicate").clicked() {
                let m = &mut dbc.messages[mi];
                let mut copy = m.signals[si].clone();
                copy.name = format!("{}_copy", copy.name);
                copy.start_bit = widgets::next_free_bit(m);
                m.signals.insert(si + 1, copy);
                out.select_signal = Some(Some(si + 1));
                out.changed = true;
            }
            if ui.add_enabled(si > 0, egui::Button::new("⏶")).clicked() {
                dbc.messages[mi].signals.swap(si - 1, si);
                out.select_signal = Some(Some(si - 1));
                out.changed = true;
            }
            if ui.add_enabled(si + 1 < dbc.messages[mi].signals.len(), egui::Button::new("⏷")).clicked() {
                dbc.messages[mi].signals.swap(si, si + 1);
                out.select_signal = Some(Some(si + 1));
                out.changed = true;
            }
            if ui.button("🗑 Delete signal").clicked() {
                dbc.messages[mi].signals.remove(si);
                out.select_signal = Some(None);
                out.changed = true;
            }
        }
    });
    if let Some(si) = signal_table(ui, &dbc.messages[mi], selected_sig) {
        out.select_signal = Some(if selected_sig == Some(si) { None } else { Some(si) });
    }

    if let Some(si) = selected_sig.filter(|&i| i < dbc.messages[mi].signals.len()) {
        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            out.changed |= signal_editor(ui, dbc, mi, si);
        });
    }
    out
}

/// Compact selectable overview of the message's signals.
fn signal_table(ui: &mut egui::Ui, m: &dbc::Message, selected: Option<usize>) -> Option<usize> {
    let mut clicked = None;
    let cols = ["", "Name", "Start", "Len", "Order", "Type", "Factor", "Offset", "Min", "Max", "Unit", "Mux", "Values"];
    TableBuilder::new(ui)
        .id_salt("sig_table")
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .vscroll(false)
        .column(Column::exact(14.0))
        .column(Column::initial(170.0))
        .columns(Column::auto(), cols.len() - 2)
        .header(18.0, |mut h| {
            for c in cols {
                h.col(|ui| {
                    ui.strong(c);
                });
            }
        })
        .body(|body| {
            body.rows(18.0, m.signals.len(), |mut row| {
                let i = row.index();
                let s = &m.signals[i];
                row.set_selected(selected == Some(i));
                row.col(|ui| {
                    util::swatch(ui, util::palette(i));
                });
                let mux = match &s.multiplexer {
                    Multiplexer::None => String::new(),
                    Multiplexer::Multiplexor => "M".into(),
                    Multiplexer::Multiplexed(v) => format!("m{v}"),
                    Multiplexer::MultiplexedMultiplexor(v) => format!("m{v}M"),
                };
                let cells = [
                    s.name.clone(),
                    s.start_bit.to_string(),
                    s.size.to_string(),
                    if s.byte_order == ByteOrder::LittleEndian { "Intel" } else { "Motorola" }.into(),
                    s.value_type.label().into(),
                    dbc::writer::fmt_f64(s.factor),
                    dbc::writer::fmt_f64(s.offset),
                    dbc::writer::fmt_f64(s.min),
                    dbc::writer::fmt_f64(s.max),
                    s.unit.clone(),
                    mux,
                    if s.value_descriptions.is_empty() { String::new() } else { s.value_descriptions.len().to_string() },
                ];
                for c in cells {
                    row.col(|ui| {
                        ui.label(c);
                    });
                }
                if row.response().clicked() {
                    clicked = Some(i);
                }
            });
        });
    clicked
}

/// Every field of one signal. Returns true if changed.
fn signal_editor(ui: &mut egui::Ui, dbc: &mut Dbc, mi: usize, si: usize) -> bool {
    let mut changed = false;
    let switches: Vec<String> = dbc.messages[mi].signals.iter().filter(|s| s.multiplexer.is_switch()).map(|s| s.name.clone()).collect();
    let tables: Vec<(String, Vec<(i64, String)>)> = dbc.value_tables.iter().map(|t| (t.name.clone(), t.values.clone())).collect();
    let msg_bytes = dbc.messages[mi].size.max(1);

    ui.horizontal(|ui| {
        ui.label("Signal");
        let name = dbc.messages[mi].signals[si].name.clone();
        if let Some(n) = commit_text(ui, egui::Id::new(("sig_name", mi, si)), &name, 260.0) {
            dbc.rename_signal(mi, si, &n);
            changed = true;
        }
    });

    let s = &mut dbc.messages[mi].signals[si];
    ui.columns(2, |cols| {
        egui::Grid::new(("sig_layout", mi, si)).num_columns(2).spacing([10.0, 4.0]).show(&mut cols[0], |ui| {
            ui.label("Start bit");
            changed |= ui.add(egui::DragValue::new(&mut s.start_bit).range(0..=msg_bytes * 8 - 1)).changed();
            ui.end_row();
            ui.label("Length");
            changed |= ui.add(egui::DragValue::new(&mut s.size).range(1..=64).suffix(" bits")).changed();
            ui.end_row();
            ui.label("Byte order");
            ui.horizontal(|ui| {
                for o in ByteOrder::ALL {
                    changed |= ui.selectable_value(&mut s.byte_order, o, o.label()).changed();
                }
            });
            ui.end_row();
            ui.label("Value type");
            ui.horizontal(|ui| {
                for t in ValueType::ALL {
                    if ui.selectable_value(&mut s.value_type, t, t.label()).changed() {
                        changed = true;
                        match t {
                            ValueType::Float32 => s.size = 32,
                            ValueType::Float64 => s.size = 64,
                            _ => {}
                        }
                    }
                }
            });
            ui.end_row();
            ui.label("Unit");
            changed |= ui.add(egui::TextEdit::singleline(&mut s.unit).desired_width(100.0)).changed();
            ui.end_row();
            ui.label("Receivers");
            let mut recv = s.receivers.join(", ");
            if ui.text_edit_singleline(&mut recv).changed() {
                s.receivers = recv.split(',').map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
                if s.receivers.is_empty() {
                    s.receivers.push(VECTOR_XXX.to_string());
                }
                changed = true;
            }
            ui.end_row();
        });
        egui::Grid::new(("sig_scale", mi, si)).num_columns(2).spacing([10.0, 4.0]).show(&mut cols[1], |ui| {
            ui.label("Factor");
            changed |= ui.add(egui::DragValue::new(&mut s.factor).speed(0.001).max_decimals(9)).changed();
            ui.end_row();
            ui.label("Offset");
            changed |= ui.add(egui::DragValue::new(&mut s.offset).speed(0.01).max_decimals(9)).changed();
            ui.end_row();
            ui.label("Minimum");
            changed |= ui.add(egui::DragValue::new(&mut s.min).max_decimals(9)).changed();
            ui.end_row();
            ui.label("Maximum");
            changed |= ui.add(egui::DragValue::new(&mut s.max).max_decimals(9)).changed();
            ui.end_row();
            ui.label("");
            if ui.small_button("Range from bits").on_hover_text("Set min/max to the full raw range scaled by factor/offset").clicked() {
                let (lo, hi) = raw_range(s);
                let (a, b) = (lo * s.factor + s.offset, hi * s.factor + s.offset);
                (s.min, s.max) = (a.min(b), a.max(b));
                changed = true;
            }
            ui.end_row();
            ui.label("Multiplexing");
            changed |= mux_editor(ui, s, &switches);
            ui.end_row();
        });
    });

    ui.label("Comment");
    changed |= widgets::comment(ui, &mut s.comment);

    egui::CollapsingHeader::new(format!("Value table ({})", s.value_descriptions.len()))
        .id_salt(("vals", mi, si))
        .default_open(!s.value_descriptions.is_empty())
        .show(ui, |ui| {
            if !tables.is_empty() {
                egui::ComboBox::from_id_salt(("apply_table", mi, si)).selected_text("Copy from table…").show_ui(ui, |ui| {
                    for (name, values) in &tables {
                        if ui.selectable_label(false, name).clicked() {
                            s.value_descriptions = values.clone();
                            changed = true;
                        }
                    }
                });
            }
            changed |= widgets::edit_value_pairs(ui, ("valdesc", mi, si), &mut s.value_descriptions);
        });
    let defs = dbc.attribute_defs.clone();
    egui::CollapsingHeader::new("Attributes").id_salt(("sig_attrs", mi, si)).show(ui, |ui| {
        changed |= widgets::attributes(ui, ("sig", mi, si), &defs, AttrObject::Signal, &mut dbc.messages[mi].signals[si].attributes);
    });
    changed
}

fn raw_range(s: &Signal) -> (f64, f64) {
    let bits = s.size.clamp(1, 64) as i32;
    match s.value_type {
        ValueType::Unsigned => (0.0, 2f64.powi(bits) - 1.0),
        ValueType::Signed => (-(2f64.powi(bits - 1)), 2f64.powi(bits - 1) - 1.0),
        ValueType::Float32 => (f32::MIN as f64, f32::MAX as f64),
        ValueType::Float64 => (f64::MIN, f64::MAX),
    }
}

fn mux_editor(ui: &mut egui::Ui, s: &mut Signal, switches: &[String]) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let value = s.multiplexer.mux_value().unwrap_or(0);
            let options = [
                (Multiplexer::None, "Plain"),
                (Multiplexer::Multiplexor, "Switch (M)"),
                (Multiplexer::Multiplexed(value), "Multiplexed (m)"),
                (Multiplexer::MultiplexedMultiplexor(value), "Both (mM)"),
            ];
            for (m, label) in options {
                let on = std::mem::discriminant(&s.multiplexer) == std::mem::discriminant(&m);
                if ui.selectable_label(on, label).clicked() && !on {
                    s.multiplexer = m;
                    changed = true;
                }
            }
        });
        if let Multiplexer::Multiplexed(v) | Multiplexer::MultiplexedMultiplexor(v) = &mut s.multiplexer {
            ui.horizontal(|ui| {
                ui.label("when switch =");
                changed |= ui.add(egui::DragValue::new(v)).changed();
                let mut ext = s.ext_mux.is_some();
                if ui.checkbox(&mut ext, "extended (SG_MUL_VAL_)").changed() {
                    s.ext_mux = ext.then(|| ExtMux {
                        switch: switches.iter().find(|n| **n != s.name).cloned().unwrap_or_default(),
                        ranges: vec![(*v, *v)],
                    });
                    changed = true;
                }
            });
            if let Some(e) = &mut s.ext_mux {
                ui.horizontal(|ui| {
                    ui.label("switch");
                    egui::ComboBox::from_id_salt(("extmux", &s.name)).selected_text(&e.switch).show_ui(ui, |ui| {
                        for n in switches.iter().filter(|n| **n != s.name) {
                            changed |= ui.selectable_value(&mut e.switch, n.clone(), n).changed();
                        }
                    });
                });
                let mut remove = None;
                for (i, (lo, hi)) in e.ranges.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(lo)).changed();
                        ui.label("…");
                        changed |= ui.add(egui::DragValue::new(hi)).changed();
                        if ui.small_button("🗑").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    e.ranges.remove(i);
                    changed = true;
                }
                if ui.small_button("➕ range").clicked() {
                    e.ranges.push((0, 0));
                    changed = true;
                }
            }
        }
    });
    changed
}

/// Decode the most recent frame of this message with the *edited* definition.
fn decode_preview(ui: &mut egui::Ui, s: &Session, dbc: &Dbc, mi: usize) {
    let m = &dbc.messages[mi];
    let idx = s.store.indices(m.id);
    let Some(&last) = idx.last() else { return };
    let data = s.store.data(last as usize);
    egui::CollapsingHeader::new(format!("🔬 Decode preview ({} frames in data)", idx.len())).id_salt("decode_preview").show(ui, |ui| {
        let tmp = Dbc { messages: vec![m.clone()], attribute_defs: dbc.attribute_defs.clone(), ..Default::default() };
        let db = dbc::CompiledDb::new([&tmp]);
        let cm = &db.messages[0];
        ui.label(RichText::new(format!("latest: {}  @ {:.6} s", util::hex_bytes(data), s.store.ts[last as usize])).monospace());
        egui::Grid::new("preview").striped(true).spacing([16.0, 2.0]).show(ui, |ui| {
            for (i, sig) in cm.signals.iter().enumerate() {
                ui.label(&sig.name);
                match cm.decode(i, data) {
                    Some(v) => ui.label(RichText::new(sig.format(v)).monospace()),
                    None => ui.label(RichText::new("not present").weak()),
                };
                ui.end_row();
            }
        });
    });
}
