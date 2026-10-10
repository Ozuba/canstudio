//! Reusable editing widgets: commit-on-enter text fields, value tables,
//! attribute values and the message bit layout.

use dbc::{AttrKind, AttrObject, AttrValue, Attributes, ByteOrder, Dbc, Message, Signal};
use eframe::egui::{self, Color32, RichText};

use crate::util;

/// A text field that edits a copy and reports the new value only when the
/// edit is committed (Enter or focus lost). Used for renames, which must
/// update references and so should not run on every keystroke.
pub fn commit_text(ui: &mut egui::Ui, id: egui::Id, value: &str, width: f32) -> Option<String> {
    let mut buf = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| value.to_string());
    let r = ui.add(egui::TextEdit::singleline(&mut buf).id(id).desired_width(width));
    if r.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(id));
        let buf = buf.trim().to_string();
        return (buf != value && !buf.is_empty()).then_some(buf);
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, buf));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    None
}

/// Optional multi-line comment editor. Returns true if changed.
pub fn comment(ui: &mut egui::Ui, c: &mut Option<String>) -> bool {
    let mut text = c.clone().unwrap_or_default();
    let changed = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(1).desired_width(f32::INFINITY).hint_text("comment")).changed();
    if changed {
        *c = (!text.is_empty()).then_some(text);
    }
    changed
}

/// An editable table of `(value, label)` rows. Returns true if changed.
pub fn edit_value_pairs(ui: &mut egui::Ui, salt: impl std::hash::Hash + std::fmt::Debug, pairs: &mut Vec<(i64, String)>) -> bool {
    let mut dirty = false;
    let mut remove = None;
    egui::Grid::new(salt).num_columns(3).spacing([6.0, 2.0]).show(ui, |ui| {
        for (i, (val, label)) in pairs.iter_mut().enumerate() {
            dirty |= ui.add(egui::DragValue::new(val)).changed();
            dirty |= ui.add(egui::TextEdit::singleline(label).desired_width(200.0)).changed();
            if ui.small_button("🗑").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        pairs.remove(i);
        dirty = true;
    }
    if ui.small_button("➕ Add value").clicked() {
        let next = pairs.iter().map(|(v, _)| v + 1).max().unwrap_or(0);
        pairs.push((next, String::new()));
        dirty = true;
    }
    dirty
}

/// Editor for one attribute value of the given kind.
pub fn attr_value(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, kind: &AttrKind, v: &mut AttrValue) -> bool {
    match (kind, v) {
        (AttrKind::Int { min, max } | AttrKind::Hex { min, max }, AttrValue::Int(i)) => {
            let mut dv = egui::DragValue::new(i);
            if max > min {
                dv = dv.range(*min..=*max);
            }
            if matches!(kind, AttrKind::Hex { .. }) {
                dv = dv.hexadecimal(1, false, true).prefix("0x");
            }
            ui.add(dv).changed()
        }
        (AttrKind::Float { min, max }, AttrValue::Float(f)) => {
            let mut dv = egui::DragValue::new(f).speed(0.1);
            if max > min {
                dv = dv.range(*min..=*max);
            }
            ui.add(dv).changed()
        }
        (AttrKind::Enum(variants), AttrValue::Int(i)) => {
            let mut changed = false;
            egui::ComboBox::from_id_salt(id).selected_text(variants.get(*i as usize).cloned().unwrap_or_else(|| i.to_string())).show_ui(
                ui,
                |ui| {
                    for (n, name) in variants.iter().enumerate() {
                        changed |= ui.selectable_value(i, n as i64, name).changed();
                    }
                },
            );
            changed
        }
        (_, AttrValue::Str(s)) => ui.text_edit_singleline(s).changed(),
        // Value of the wrong representation: show it, allow reset only.
        (_, v) => {
            ui.label(format!("{v:?}"));
            false
        }
    }
}

/// Neutral starting value for an attribute of `kind`.
pub fn zero_value(kind: &AttrKind) -> AttrValue {
    match kind {
        AttrKind::Int { min, .. } | AttrKind::Hex { min, .. } => AttrValue::Int((*min).max(0)),
        AttrKind::Float { min, .. } => AttrValue::Float(min.max(0.0)),
        AttrKind::Str => AttrValue::Str(String::new()),
        AttrKind::Enum(_) => AttrValue::Int(0),
    }
}

/// Attribute values of one object: every definition for `object`, showing
/// the default when unset. Returns true if changed.
pub fn attributes(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug + Copy,
    dbc_defs: &[dbc::AttributeDef],
    object: AttrObject,
    set: &mut Attributes,
) -> bool {
    let defs: Vec<&dbc::AttributeDef> = dbc_defs.iter().filter(|d| d.object == object).collect();
    if defs.is_empty() {
        ui.label(RichText::new(format!("No {} attributes defined (see Attribute definitions).", object.label().to_lowercase())).weak());
        return false;
    }
    let mut changed = false;
    egui::Grid::new(("attrs", salt)).num_columns(3).spacing([10.0, 2.0]).striped(true).show(ui, |ui| {
        for def in defs {
            ui.label(&def.name);
            match set.iter().position(|(n, _)| *n == def.name) {
                Some(i) => {
                    changed |= attr_value(ui, (salt, &def.name), &def.kind, &mut set[i].1);
                    if ui.small_button("↺").on_hover_text("Reset to default").clicked() {
                        set.remove(i);
                        changed = true;
                    }
                }
                None => {
                    let default = def.default.as_ref().map_or("—".into(), |v| def.kind.display(v));
                    if ui.button(RichText::new(format!("{default} (default)")).weak()).on_hover_text("Click to override").clicked() {
                        set.push((def.name.clone(), def.default.clone().unwrap_or_else(|| zero_value(&def.kind))));
                        changed = true;
                    }
                    ui.label("");
                }
            }
            ui.end_row();
        }
    });
    changed
}

// --- bit layout ----------------------------------------------------------------

/// Flat DBC bit positions (byte*8 + bit-in-byte) a signal occupies.
pub fn occupied_positions(s: &Signal) -> Vec<u64> {
    match s.byte_order {
        ByteOrder::LittleEndian => (s.start_bit..s.start_bit + s.size).collect(),
        ByteOrder::BigEndian => {
            // Motorola sawtooth: move toward the LSB, wrapping into the next byte.
            let mut out = Vec::with_capacity(s.size as usize);
            let mut pos = s.start_bit;
            for _ in 0..s.size {
                out.push(pos);
                pos = if pos.is_multiple_of(8) { pos + 15 } else { pos - 1 };
            }
            out
        }
    }
}

/// Lowest bit not used by any signal in the message.
pub fn next_free_bit(m: &Message) -> u64 {
    let bits = (m.size.max(8) * 8) as usize;
    let mut used = vec![false; bits];
    for s in &m.signals {
        for p in occupied_positions(s) {
            if let Some(u) = used.get_mut(p as usize) {
                *u = true;
            }
        }
    }
    used.iter().position(|&u| !u).unwrap_or(0) as u64
}

/// Colour-coded byte × bit grid. Clicking a bit selects its signal.
pub fn bit_layout(ui: &mut egui::Ui, m: &Message, selected: Option<usize>) -> Option<usize> {
    let bytes = m.size.clamp(1, 64) as usize;
    let mut owners: Vec<Vec<usize>> = vec![Vec::new(); bytes * 8];
    for (si, s) in m.signals.iter().enumerate() {
        for p in occupied_positions(s) {
            if let Some(o) = owners.get_mut(p as usize) {
                o.push(si);
            }
        }
    }
    let mut clicked = None;
    let cell = egui::vec2(38.0, 20.0);
    egui::ScrollArea::vertical().id_salt("bitgrid_scroll").max_height(9.0 * (cell.y + 2.0) + 4.0).show(ui, |ui| {
        egui::Grid::new("bitgrid").spacing([2.0, 2.0]).min_col_width(cell.x).show(ui, |ui| {
            ui.label("");
            for b in (0..8).rev() {
                ui.label(RichText::new(format!("{b}")).weak().monospace());
            }
            ui.end_row();
            for byte in 0..bytes {
                ui.label(RichText::new(format!("B{byte}")).weak().monospace());
                for b in (0..8).rev() {
                    let flat = byte * 8 + b;
                    let own = &owners[flat];
                    let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                    let painter = ui.painter();
                    match own.as_slice() {
                        [] => {
                            painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
                        }
                        [si, rest @ ..] => {
                            let mut color = util::palette(*si);
                            if selected.is_some_and(|s| s != *si) {
                                color = color.gamma_multiply(0.45);
                            }
                            painter.rect_filled(rect, 3.0, color);
                            if !rest.is_empty() {
                                // Overlap (possibly legitimate for multiplexed signals).
                                painter.rect_stroke(rect, 3.0, egui::Stroke::new(2.0, Color32::RED), egui::StrokeKind::Inside);
                            }
                            let s = &m.signals[*si];
                            let mark = if flat as u64 == s.start_bit { "●" } else { "" };
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                format!("{flat}{mark}"),
                                egui::FontId::monospace(10.0),
                                Color32::from_black_alpha(220),
                            );
                        }
                    }
                    let hover = if own.is_empty() {
                        format!("bit {flat}: free")
                    } else {
                        own.iter().map(|&i| m.signals[i].name.as_str()).collect::<Vec<_>>().join(", ")
                    };
                    let resp = resp.on_hover_text(hover);
                    if resp.clicked() {
                        clicked = own.first().copied();
                    }
                }
                ui.end_row();
            }
        });
    });
    ui.label(RichText::new("n● = start bit · red outline = overlapping signals").monospace().weak().small());
    clicked
}

/// Rename an attribute definition and every value that uses it.
pub fn rename_attribute(dbc: &mut Dbc, old: &str, new: &str) {
    let fix = |set: &mut Attributes| set.iter_mut().filter(|(n, _)| n == old).for_each(|(n, _)| *n = new.to_string());
    fix(&mut dbc.attributes);
    dbc.nodes.iter_mut().for_each(|n| fix(&mut n.attributes));
    for m in &mut dbc.messages {
        fix(&mut m.attributes);
        m.signals.iter_mut().for_each(|s| fix(&mut s.attributes));
    }
    if let Some(d) = dbc.attribute_defs.iter_mut().find(|d| d.name == old) {
        d.name = new.to_string();
    }
}
