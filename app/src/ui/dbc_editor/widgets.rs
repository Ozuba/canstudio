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
    // Positions past a 64-byte frame are irrelevant; capping also keeps a corrupt
    // length (e.g. 4e9 bits) from allocating gigabytes.
    let size = s.size.min(MAX_BITS);
    match s.byte_order {
        ByteOrder::LittleEndian => (0..size).map(|i| s.start_bit.saturating_add(i)).collect(),
        ByteOrder::BigEndian => {
            // Motorola sawtooth: move toward the LSB, wrapping into the next byte.
            let mut out = Vec::with_capacity(size as usize);
            let mut pos = s.start_bit;
            for _ in 0..size {
                out.push(pos);
                pos = if pos.is_multiple_of(8) { pos.saturating_add(15) } else { pos - 1 };
            }
            out
        }
    }
}

/// Bits in the largest (CAN-FD) frame.
const MAX_BITS: u64 = 64 * 8;

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

/// Something the user did in the bit layout.
pub enum LayoutEvent {
    Select(usize),
    /// Move signal `sig` so it starts at `start_bit` (applied live while dragging).
    Move {
        sig: usize,
        start_bit: u64,
    },
    /// Create a little-endian signal over free bits.
    Create {
        start_bit: u64,
        size: u64,
    },
}

/// Motorola linear index of a DBC bit position (MSB-first walk order). Its own inverse.
fn motorola_lin(p: u64) -> u64 {
    (p / 8) * 8 + 7 - p % 8
}

/// Start bit placing occupied bit `k` of `s` on position `target`, if the result fits `bytes`.
fn start_for(s: &Signal, k: u64, target: u64, bytes: u64) -> Option<u64> {
    let start = match s.byte_order {
        ByteOrder::LittleEndian => target.checked_sub(k)?,
        ByteOrder::BigEndian => motorola_lin(motorola_lin(target).checked_sub(k)?),
    };
    let layout = dbc::codec::BitLayout::new(start, s.size, s.byte_order)?;
    (layout.required_len() as u64 <= bytes).then_some(start)
}

#[derive(Clone, Copy)]
enum Drag {
    /// Moving signal `sig`, holding its `k`-th occupied bit.
    Move { sig: usize, k: u64 },
    /// Selecting free bits from `from`.
    Create { from: u64 },
}

/// Colour-coded byte × bit grid with drag-to-move, drag-to-create and
/// MSB→LSB order arrows for the selected (or hovered) signal.
pub fn bit_layout(ui: &mut egui::Ui, m: &Message, selected: Option<usize>) -> Option<LayoutEvent> {
    let bytes = m.size.clamp(1, 64);
    let nbits = (bytes * 8) as usize;
    let positions: Vec<Vec<u64>> = m.signals.iter().map(occupied_positions).collect();
    let mut owners: Vec<Vec<usize>> = vec![Vec::new(); nbits];
    for (si, ps) in positions.iter().enumerate() {
        for &p in ps {
            if let Some(o) = owners.get_mut(p as usize) {
                o.push(si);
            }
        }
    }

    let (cell, gap, label_w, head_h) = (egui::vec2(38.0, 20.0), 2.0, 28.0, 16.0);
    let pitch = cell + egui::vec2(gap, gap);
    let size = egui::vec2(label_w + 8.0 * pitch.x, head_h + bytes as f32 * pitch.y);
    let drag_id = ui.id().with("bit_drag");
    let mut event = None;

    egui::ScrollArea::vertical().id_salt("bitgrid_scroll").max_height(head_h + 8.0 * pitch.y + 4.0).show(ui, |ui| {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let origin = rect.min + egui::vec2(label_w, head_h);
        let cell_rect = |p: u64| {
            let (row, col) = ((p / 8) as f32, (7 - p % 8) as f32);
            egui::Rect::from_min_size(origin + egui::vec2(col * pitch.x, row * pitch.y), cell)
        };
        let bit_at = |pos: egui::Pos2| -> Option<u64> {
            let d = pos - origin;
            let (col, row) = ((d.x / pitch.x).floor(), (d.y / pitch.y).floor());
            (d.x >= 0.0 && d.y >= 0.0 && col < 8.0 && (row as u64) < bytes).then(|| row as u64 * 8 + 7 - col as u64)
        };
        let owner = |p: u64| owners.get(p as usize).and_then(|o| o.first().copied());

        // --- interaction ---
        let pointer_bit = resp.hover_pos().or(resp.interact_pointer_pos()).and_then(bit_at);
        let mut drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id));
        if resp.drag_started() {
            let press = ui.input(|i| i.pointer.press_origin()).and_then(bit_at);
            drag = press.map(|b| match owner(b) {
                Some(sig) => Drag::Move { sig, k: positions[sig].iter().position(|&p| p == b).unwrap_or(0) as u64 },
                None => Drag::Create { from: b },
            });
            if let Some(Drag::Move { sig, .. }) = drag {
                event = Some(LayoutEvent::Select(sig));
            }
        }
        if let (Some(Drag::Move { sig, k }), Some(target), true) = (drag, pointer_bit, resp.dragged())
            && let Some(start) = start_for(&m.signals[sig], k, target, bytes)
            && start != m.signals[sig].start_bit
        {
            event = Some(LayoutEvent::Move { sig, start_bit: start });
        }
        let creating = match (drag, pointer_bit) {
            (Some(Drag::Create { from }), Some(to)) => Some((from.min(to), from.max(to))),
            _ => None,
        };
        if resp.drag_stopped() {
            if let Some((a, b)) = creating.filter(|&(a, b)| (a..=b).all(|p| owner(p).is_none())) {
                event = Some(LayoutEvent::Create { start_bit: a, size: b - a + 1 });
            }
            drag = None;
        }
        ui.data_mut(|d| match drag {
            Some(v) => {
                d.insert_temp(drag_id, v);
            }
            None => d.remove::<Drag>(drag_id),
        });
        if resp.clicked()
            && let Some(si) = pointer_bit.and_then(owner)
        {
            event = Some(LayoutEvent::Select(si));
        }

        // --- painting ---
        let painter = ui.painter_at(rect.expand(2.0));
        let weak = ui.visuals().weak_text_color();
        for b in 0..8u64 {
            let r = cell_rect(b);
            painter.text(
                egui::pos2(r.center().x, rect.top() + head_h / 2.0),
                egui::Align2::CENTER_CENTER,
                b.to_string(),
                egui::FontId::monospace(10.0),
                weak,
            );
        }
        for byte in 0..bytes {
            let r = cell_rect(byte * 8);
            painter.text(
                egui::pos2(rect.left() + 2.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                format!("B{byte}"),
                egui::FontId::monospace(10.0),
                weak,
            );
        }
        let focus = match drag {
            Some(Drag::Move { sig, .. }) => Some(sig),
            _ => selected.or_else(|| pointer_bit.and_then(owner)),
        };
        for p in 0..bytes * 8 {
            let r = cell_rect(p);
            match owners[p as usize].as_slice() {
                [] => {
                    let in_new = creating.is_some_and(|(a, b)| (a..=b).contains(&p));
                    let fill = if in_new { ui.visuals().selection.bg_fill } else { ui.visuals().faint_bg_color };
                    painter.rect_filled(r, 3.0, fill);
                }
                [si, rest @ ..] => {
                    let mut color = util::palette(*si);
                    if focus.is_some_and(|f| f != *si) {
                        color = color.gamma_multiply(0.45);
                    }
                    painter.rect_filled(r, 3.0, color);
                    if !rest.is_empty() {
                        // Overlap (possibly legitimate for multiplexed signals).
                        painter.rect_stroke(r, 3.0, egui::Stroke::new(2.0, Color32::RED), egui::StrokeKind::Inside);
                    }
                    painter.text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        p.to_string(),
                        egui::FontId::monospace(10.0),
                        Color32::from_black_alpha(200),
                    );
                }
            }
        }
        if let Some(sig) = focus {
            draw_order(&painter, &m.signals[sig], &positions[sig], bytes, &cell_rect);
        }

        // --- hover help ---
        let hover = match (drag, pointer_bit) {
            (Some(Drag::Move { sig, .. }), _) => format!("Moving {} — start bit {}", m.signals[sig].name, m.signals[sig].start_bit),
            (Some(Drag::Create { .. }), _) => match creating {
                Some((a, b)) if (a..=b).all(|p| owner(p).is_none()) => format!("New signal: start {a}, {} bits", b - a + 1),
                _ => "Range overlaps an existing signal".into(),
            },
            (None, Some(p)) => match owners[p as usize].as_slice() {
                [] => format!("bit {p}: free — drag across free bits to create a signal"),
                own => format!(
                    "bit {p}: {} — drag to move, click to select",
                    own.iter().map(|&i| m.signals[i].name.as_str()).collect::<Vec<_>>().join(", ")
                ),
            },
            _ => String::new(),
        };
        if !hover.is_empty() {
            resp.on_hover_text_at_pointer(hover);
        }
    });
    ui.label(
        RichText::new("Drag signals to move them · drag over free bits to create one · arrows run from MSB to LSB · red outline = overlap")
            .weak()
            .small(),
    );
    event
}

/// Arrows through the signal's bits from MSB to LSB, one polyline per byte row.
fn draw_order(painter: &egui::Painter, s: &Signal, positions: &[u64], bytes: u64, cell_rect: &dyn Fn(u64) -> egui::Rect) {
    // occupied_positions runs LSB→MSB for Intel and MSB→LSB for Motorola.
    let mut order: Vec<u64> = positions.iter().copied().filter(|&p| p < bytes * 8).collect();
    if s.byte_order == ByteOrder::LittleEndian {
        order.reverse();
    }
    let Some((&msb, &lsb)) = order.first().zip(order.last()) else { return };
    let stroke = egui::Stroke::new(1.6, Color32::from_white_alpha(230));
    let mut run: Vec<egui::Pos2> = Vec::new();
    let flush = |run: &mut Vec<egui::Pos2>| {
        if let [.., a, b] = run.as_slice() {
            // Dark halo first so the arrow reads on any signal colour.
            painter.line(run.clone(), egui::Stroke::new(3.6, Color32::from_black_alpha(160)));
            painter.line(run.clone(), stroke);
            // Arrow head at the end of the row run.
            let dir = (*b - *a).normalized();
            let n = egui::vec2(-dir.y, dir.x);
            let head = vec![*b + dir * 4.0, *b - dir * 4.0 + n * 4.0, *b - dir * 4.0 - n * 4.0];
            painter.add(egui::Shape::convex_polygon(head, stroke.color, egui::Stroke::new(1.0, Color32::from_black_alpha(160))));
        }
        run.clear();
    };
    let mut row = None;
    for &p in &order {
        if row.is_some_and(|r| r != p / 8) {
            flush(&mut run);
        }
        row = Some(p / 8);
        let c = cell_rect(p).center();
        run.push(egui::pos2(c.x, c.y + 6.0));
    }
    flush(&mut run);
    let tag = |p: u64, t: &str| {
        let r = cell_rect(p);
        painter.text(
            r.left_top() + egui::vec2(2.0, 0.0),
            egui::Align2::LEFT_TOP,
            t,
            egui::FontId::monospace(8.0),
            Color32::from_black_alpha(230),
        );
    };
    tag(msb, "M");
    if lsb != msb {
        tag(lsb, "L");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(start: u64, size: u64, order: ByteOrder) -> Signal {
        let mut s = Signal::new("s", start);
        s.size = size;
        s.byte_order = order;
        s
    }

    #[test]
    fn intel_moves_keep_the_grabbed_bit_under_the_pointer() {
        let s = sig(4, 8, ByteOrder::LittleEndian); // bits 4..=11
        assert_eq!(start_for(&s, 0, 20, 8), Some(20));
        assert_eq!(start_for(&s, 3, 20, 8), Some(17));
        assert_eq!(start_for(&s, 3, 1, 8), None); // would start before bit 0
        assert_eq!(start_for(&s, 0, 60, 8), None); // runs past byte 7
    }

    #[test]
    fn motorola_moves_follow_the_sawtooth() {
        let s = sig(3, 12, ByteOrder::BigEndian); // 3,2,1,0,15,14,…,8
        assert_eq!(occupied_positions(&s)[4], 15);
        let start = start_for(&s, 4, 23, 8).unwrap();
        assert_eq!(start, 11);
        assert_eq!(occupied_positions(&sig(start, 12, ByteOrder::BigEndian))[4], 23);
        assert_eq!(start_for(&s, 0, 63, 8), None); // last byte cannot hold 12 bits after bit 63's byte
    }

    #[test]
    fn corrupt_lengths_do_not_explode() {
        for order in ByteOrder::ALL {
            let s = sig(u64::MAX - 3, 4_000_000_000, order);
            assert_eq!(occupied_positions(&s).len(), MAX_BITS as usize);
        }
    }

    #[test]
    fn occupied_positions_match_the_codec() {
        // Setting exactly the occupied bits must make the decoder read all ones.
        for order in ByteOrder::ALL {
            for (start, size) in [(0, 1), (7, 16), (13, 9), (3, 12), (20, 32)] {
                let s = sig(start, size, order);
                let Some(layout) = dbc::codec::BitLayout::new(start, size, order) else { continue };
                if layout.required_len() > 8 {
                    continue;
                }
                let mut data = [0u8; 8];
                for p in occupied_positions(&s) {
                    data[(p / 8) as usize] |= 1 << (p % 8);
                }
                assert_eq!(layout.extract(&data), Some((1u64 << size) - 1), "{order:?} {start}|{size}");
            }
        }
    }
}
