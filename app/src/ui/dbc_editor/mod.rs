//! DBC editor (evolved from rsdbc): edits one loaded database at a time with
//! undo/redo, validation and immediate re-decoding of the loaded data.

mod message;
mod validate;
mod widgets;

use dbc::{AttrKind, AttrObject, AttributeDef, Dbc, Message, Node, ValueTable};
use eframe::egui::{self, RichText};

use super::order::{self, MessageOrder};
use crate::session::Session;
use crate::util;
use widgets::commit_text;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Network,
    Message(usize),
    Node(usize),
    ValueTables,
    AttributeDefs,
    Problems,
    Raw,
}

#[derive(Default)]
pub struct DbcEditor {
    db: usize,
    page: Page,
    signal: Option<usize>,
    search: String,
    /// State before the edits of the current gesture.
    snapshot: Option<(usize, Dbc)>,
    undo: Vec<Dbc>,
    redo: Vec<Dbc>,
    /// Edits made but not yet committed to the undo stack.
    pending: bool,
    problems: Vec<validate::Problem>,
    /// A database was enabled/disabled in the toolbar: recompile.
    pub db_toggled: bool,
}

const MAX_UNDO: usize = 100;

impl DbcEditor {
    pub fn select_db(&mut self, i: usize) {
        self.db = i;
        self.page = Page::Network;
        self.signal = None;
        self.snapshot = None;
        self.undo.clear();
        self.redo.clear();
    }

    fn select(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.signal = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session, order: &mut MessageOrder) {
        if s.dbs.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.heading("No database open");
                ui.label("Open a .dbc (Ctrl+D), drop one on the window, or start a new one.");
                if ui.button("🆕 New database").clicked() {
                    let i = s.new_dbc();
                    self.select_db(i);
                }
            });
            return;
        }
        if self.db >= s.dbs.len() {
            self.select_db(0);
        }
        if self.snapshot.as_ref().is_none_or(|(i, _)| *i != self.db) {
            self.snapshot = Some((self.db, s.dbs[self.db].dbc.clone()));
            self.problems = validate::check(&s.dbs[self.db].dbc);
        }

        self.shortcuts(ui, s);
        let mut changed = false;
        egui::Panel::left("dbc_sidebar").resizable(true).default_size(270.0).show(ui, |ui| {
            changed |= self.sidebar(ui, s, order);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                changed |= self.page_ui(ui, s);
            });
        });

        if changed {
            self.pending = true;
            s.dbs[self.db].dirty = true;
        }
        // Commit a gesture once the user lets go (no drag, no focused text field).
        let idle = ui.ctx().input(|i| !i.pointer.any_down()) && ui.ctx().memory(|m| m.focused().is_none());
        if self.pending && idle {
            self.commit(s);
        }
    }

    fn commit(&mut self, s: &mut Session) {
        self.pending = false;
        let current = s.dbs[self.db].dbc.clone();
        if let Some((_, before)) = self.snapshot.replace((self.db, current))
            && before != s.dbs[self.db].dbc
        {
            self.undo.push(before);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.problems = validate::check(&s.dbs[self.db].dbc);
        s.recompile();
    }

    fn restore(&mut self, s: &mut Session, dbc: Dbc) {
        s.dbs[self.db].dbc = dbc.clone();
        s.dbs[self.db].dirty = true;
        self.snapshot = Some((self.db, dbc));
        self.problems = validate::check(&s.dbs[self.db].dbc);
        if let Page::Message(i) = self.page
            && i >= s.dbs[self.db].dbc.messages.len()
        {
            self.page = Page::Network;
        }
        self.signal = None;
        s.recompile();
    }

    fn shortcuts(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let (undo, redo, save) = ui.ctx().input_mut(|i| {
            (
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
                    || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S)),
            )
        });
        if undo {
            self.undo_redo(s, true);
        }
        if redo {
            self.undo_redo(s, false);
        }
        if save {
            s.save_dbc(self.db, false);
        }
    }

    fn undo_redo(&mut self, s: &mut Session, undo: bool) {
        if self.pending {
            self.commit(s);
        }
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        if let Some(prev) = from.pop() {
            to.push(s.dbs[self.db].dbc.clone());
            self.restore(s, prev);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, s: &mut Session, order: &mut MessageOrder) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("dbc_pick").width(170.0).selected_text(s.dbs[self.db].name()).show_ui(ui, |ui| {
                for i in 0..s.dbs.len() {
                    if ui.selectable_label(i == self.db, s.dbs[i].name()).clicked() && i != self.db {
                        self.select_db(i);
                    }
                }
            });
            ui.menu_button("☰", |ui| {
                if ui.button("💾 Save (Ctrl+S)").clicked() {
                    s.save_dbc(self.db, false);
                    ui.close();
                }
                if ui.button("Save as…").clicked() {
                    s.save_dbc(self.db, true);
                    ui.close();
                }
                if ui.button("🆕 New database").clicked() {
                    let i = s.new_dbc();
                    self.select_db(i);
                    ui.close();
                }
                ui.separator();
                if ui.button("Close database").clicked() {
                    s.close_dbc(self.db);
                    self.select_db(0);
                    ui.close();
                }
            });
        });
        if s.dbs.is_empty() {
            return false;
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).clicked() {
                self.undo_redo(s, true);
            }
            if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo")).clicked() {
                self.undo_redo(s, false);
            }
            if ui.button("💾").on_hover_text("Save (Ctrl+S)").clicked() {
                s.save_dbc(self.db, false);
            }
        });
        ui.label(RichText::new(format!("Saved as {}", s.dbs[self.db].encoding.label())).weak().small())
            .on_hover_text("The file's text encoding is kept when saving. Windows-1252 is the DBC convention (Vector, cantools).");
        let errors = s.dbs[self.db].errors.len();
        if errors > 0 {
            ui.label(RichText::new(format!("{errors} statements skipped while loading")).color(ui.visuals().warn_fg_color))
                .on_hover_text(s.dbs[self.db].errors.iter().take(20).map(ToString::to_string).collect::<Vec<_>>().join("\n"));
        }
        ui.separator();

        let problems =
            if self.problems.is_empty() { "✔ No problems".to_string() } else { format!("⚠ Problems ({})", self.problems.len()) };
        let problem_msgs: Vec<usize> = self.problems.iter().filter_map(|p| p.msg).collect();
        let dbc = &mut s.dbs[self.db].dbc;
        let mut nav = |ui: &mut egui::Ui, page: Page, label: String| {
            if ui.selectable_label(self.page == page, label).clicked() {
                self.select(page);
            }
        };
        nav(ui, Page::Network, "🌐 Network".into());
        nav(ui, Page::ValueTables, format!("📑 Value tables ({})", dbc.value_tables.len()));
        nav(ui, Page::AttributeDefs, format!("🏷 Attribute definitions ({})", dbc.attribute_defs.len()));
        nav(ui, Page::Problems, problems);
        nav(ui, Page::Raw, "📄 DBC text".into());
        ui.separator();

        util::search_box(ui, &mut self.search, "filter nodes / messages");
        order.picker(ui, "editor");
        let filter = self.search.to_lowercase();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::CollapsingHeader::new(format!("🖧 Nodes ({})", dbc.nodes.len())).show(ui, |ui| {
                for (i, n) in dbc.nodes.iter().enumerate() {
                    if util::matches(&filter, &[&n.name]) && ui.selectable_label(self.page == Page::Node(i), &n.name).clicked() {
                        self.select(Page::Node(i));
                    }
                }
                if ui.small_button("➕ Add node").clicked() {
                    dbc.nodes.push(Node::new(format!("NODE_{}", dbc.nodes.len() + 1)));
                    self.select(Page::Node(dbc.nodes.len() - 1));
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.strong(format!("✉ Messages ({})", dbc.messages.len()));
                if ui.small_button("➕").on_hover_text("Add message").clicked() {
                    let id = dbc.messages.iter().filter(|m| !m.is_extended()).map(|m| m.raw_id() + 1).max().unwrap_or(0x100).min(0x7FF);
                    dbc.messages.push(Message::new(id, format!("NEW_MSG_{id:X}")));
                    self.select(Page::Message(dbc.messages.len() - 1));
                    changed = true;
                }
            });
            let shown: Vec<order::Entry> = dbc
                .messages
                .iter()
                .enumerate()
                .filter(|(_, m)| util::matches(&filter, &[&m.name, &format!("{:X}", m.raw_id())]))
                .map(|(index, m)| order::Entry { index, key: m.id, name: &m.name, node: &m.transmitter })
                .collect();
            order::show(ui, &order::arrange(*order, shown), "editor", |ui, i| {
                let m = &dbc.messages[i];
                let mut label = RichText::new(format!("{:>8}  {}", canbus::frame::fmt_key(m.id), m.name)).monospace();
                if problem_msgs.contains(&i) {
                    label = label.color(ui.visuals().warn_fg_color);
                }
                if ui.selectable_label(self.page == Page::Message(i), label).clicked() {
                    self.select(Page::Message(i));
                }
            });
        });
        changed
    }

    fn page_ui(&mut self, ui: &mut egui::Ui, s: &mut Session) -> bool {
        let db = self.db;
        match self.page {
            Page::Message(mi) if mi < s.dbs[db].dbc.messages.len() => {
                let mut dbc = std::mem::take(&mut s.dbs[db].dbc);
                let out = message::page(ui, s, &mut dbc, mi, self.signal);
                if out.delete {
                    dbc.messages.remove(mi);
                    self.page = Page::Network;
                }
                s.dbs[db].dbc = dbc;
                if let Some(sel) = out.select_signal {
                    self.signal = sel;
                }
                out.changed || out.delete
            }
            Page::Node(ni) if ni < s.dbs[db].dbc.nodes.len() => self.node_page(ui, &mut s.dbs[db].dbc, ni),
            Page::ValueTables => value_tables(ui, &mut s.dbs[db].dbc),
            Page::AttributeDefs => attribute_defs(ui, &mut s.dbs[db].dbc),
            Page::Problems => {
                if self.problems.is_empty() {
                    ui.label("No problems found.");
                }
                for p in &self.problems {
                    if ui.link(&p.text).clicked()
                        && let Some(m) = p.msg
                    {
                        self.page = Page::Message(m);
                        self.signal = p.sig;
                    }
                }
                false
            }
            Page::Raw => {
                let text = dbc::write(&s.dbs[db].dbc);
                ui.horizontal(|ui| {
                    ui.heading("Generated DBC");
                    if ui.button("📋 Copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
                ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY));
                false
            }
            _ => network_page(ui, &mut s.dbs[db].dbc),
        }
    }

    fn node_page(&mut self, ui: &mut egui::Ui, dbc: &mut Dbc, ni: usize) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.heading("Node");
            if let Some(n) = commit_text(ui, egui::Id::new(("node_name", ni)), &dbc.nodes[ni].name.clone(), 200.0) {
                dbc.rename_node(ni, &n);
                changed = true;
            }
            if ui.button("🗑 Delete node").clicked() {
                dbc.nodes.remove(ni);
                self.page = Page::Network;
                changed = true;
            }
        });
        if changed {
            return true;
        }
        changed |= widgets::comment(ui, &mut dbc.nodes[ni].comment);
        let defs = dbc.attribute_defs.clone();
        ui.strong("Attributes");
        changed |= widgets::attributes(ui, ("node", ni), &defs, AttrObject::Node, &mut dbc.nodes[ni].attributes);
        let name = dbc.nodes[ni].name.clone();
        ui.separator();
        ui.strong("Transmits");
        for (i, m) in dbc.messages.iter().enumerate().filter(|(_, m)| m.transmitter == name) {
            if ui.link(format!("{} {}", canbus::frame::fmt_key(m.id), m.name)).clicked() {
                self.page = Page::Message(i);
            }
        }
        ui.strong("Receives");
        ui.horizontal_wrapped(|ui| {
            for (i, m) in dbc.messages.iter().enumerate() {
                for s in m.signals.iter().filter(|s| s.receivers.contains(&name)) {
                    if ui.link(format!("{}.{}", m.name, s.name)).clicked() {
                        self.page = Page::Message(i);
                    }
                }
            }
        });
        changed
    }
}

fn network_page(ui: &mut egui::Ui, dbc: &mut Dbc) -> bool {
    let mut changed = false;
    ui.heading("Network");
    egui::Grid::new("net").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Version");
        changed |= ui.text_edit_singleline(&mut dbc.version).changed();
        ui.end_row();
        ui.label("Comment");
        changed |= widgets::comment(ui, &mut dbc.comment);
        ui.end_row();
        ui.label("Contents");
        ui.label(format!(
            "{} nodes · {} messages · {} signals · {} unmodelled statements kept verbatim",
            dbc.nodes.len(),
            dbc.messages.len(),
            dbc.signal_count(),
            dbc.extra.len()
        ));
        ui.end_row();
    });
    ui.add_space(8.0);
    ui.strong("Network attributes");
    let defs = dbc.attribute_defs.clone();
    changed |= widgets::attributes(ui, "net", &defs, AttrObject::Network, &mut dbc.attributes);
    changed
}

fn value_tables(ui: &mut egui::Ui, dbc: &mut Dbc) -> bool {
    let mut changed = false;
    let mut remove = None;
    ui.heading("Value tables");
    ui.label(RichText::new("Reusable enumerations. Copy one into a signal from the signal's value table.").weak());
    for (i, vt) in dbc.value_tables.iter_mut().enumerate() {
        egui::CollapsingHeader::new(format!("{}  ({})", vt.name, vt.values.len())).id_salt(("vt", i)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name");
                changed |= ui.text_edit_singleline(&mut vt.name).changed();
                if ui.small_button("🗑").clicked() {
                    remove = Some(i);
                }
            });
            changed |= widgets::edit_value_pairs(ui, ("vt_pairs", i), &mut vt.values);
        });
    }
    if let Some(i) = remove {
        dbc.value_tables.remove(i);
        changed = true;
    }
    if ui.button("➕ Add table").clicked() {
        dbc.value_tables
            .push(ValueTable { name: format!("Table_{}", dbc.value_tables.len() + 1), values: vec![(0, "Off".into()), (1, "On".into())] });
        changed = true;
    }
    changed
}

fn attribute_defs(ui: &mut egui::Ui, dbc: &mut Dbc) -> bool {
    let mut changed = false;
    let mut remove = None;
    let mut rename = None;
    ui.heading("Attribute definitions");
    egui::Grid::new("attr_defs").striped(true).num_columns(5).spacing([10.0, 4.0]).show(ui, |ui| {
        for h in ["Name", "Object", "Type", "Range / values", "Default"] {
            ui.strong(h);
        }
        ui.end_row();
        for (i, d) in dbc.attribute_defs.iter_mut().enumerate() {
            if let Some(n) = commit_text(ui, egui::Id::new(("attr_def", i)), &d.name.clone(), 180.0) {
                rename = Some((d.name.clone(), n));
            }
            egui::ComboBox::from_id_salt(("attr_obj", i)).selected_text(d.object.label()).show_ui(ui, |ui| {
                for o in AttrObject::ALL {
                    changed |= ui.selectable_value(&mut d.object, o, o.label()).changed();
                }
            });
            let kinds = [
                AttrKind::Int { min: 0, max: 0 },
                AttrKind::Hex { min: 0, max: 0 },
                AttrKind::Float { min: 0.0, max: 0.0 },
                AttrKind::Str,
                AttrKind::Enum(vec![]),
            ];
            egui::ComboBox::from_id_salt(("attr_kind", i)).selected_text(d.kind.label()).show_ui(ui, |ui| {
                for k in kinds {
                    if ui.selectable_label(d.kind.label() == k.label(), k.label()).clicked() && d.kind.label() != k.label() {
                        d.kind = k;
                        d.default = None;
                        changed = true;
                    }
                }
            });
            match &mut d.kind {
                AttrKind::Int { min, max } | AttrKind::Hex { min, max } => {
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(min)).changed();
                        changed |= ui.add(egui::DragValue::new(max)).changed();
                    });
                }
                AttrKind::Float { min, max } => {
                    ui.horizontal(|ui| {
                        changed |= ui.add(egui::DragValue::new(min).speed(0.1)).changed();
                        changed |= ui.add(egui::DragValue::new(max).speed(0.1)).changed();
                    });
                }
                AttrKind::Str => {
                    ui.label("");
                }
                AttrKind::Enum(variants) => {
                    let mut text = variants.join(", ");
                    if ui.add(egui::TextEdit::singleline(&mut text).hint_text("A, B, C")).changed() {
                        *variants = text.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
                        changed = true;
                    }
                }
            }
            ui.horizontal(|ui| {
                let mut has = d.default.is_some();
                if ui.checkbox(&mut has, "").changed() {
                    d.default = has.then(|| widgets::zero_value(&d.kind));
                    changed = true;
                }
                if let Some(v) = &mut d.default {
                    changed |= widgets::attr_value(ui, ("attr_default", i), &d.kind, v);
                }
                if ui.small_button("🗑").clicked() {
                    remove = Some(i);
                }
            });
            ui.end_row();
        }
    });
    if let Some((old, new)) = rename {
        widgets::rename_attribute(dbc, &old, &new);
        changed = true;
    }
    if let Some(i) = remove {
        let name = dbc.attribute_defs.remove(i).name;
        let strip = |set: &mut dbc::Attributes| set.retain(|(n, _)| *n != name);
        strip(&mut dbc.attributes);
        dbc.nodes.iter_mut().for_each(|n| strip(&mut n.attributes));
        for m in &mut dbc.messages {
            strip(&mut m.attributes);
            m.signals.iter_mut().for_each(|s| strip(&mut s.attributes));
        }
        changed = true;
    }
    ui.horizontal(|ui| {
        if ui.button("➕ Add definition").clicked() {
            dbc.attribute_defs.push(AttributeDef {
                name: format!("NewAttribute{}", dbc.attribute_defs.len() + 1),
                object: AttrObject::Message,
                kind: AttrKind::Int { min: 0, max: 0 },
                default: Some(dbc::AttrValue::Int(0)),
            });
            changed = true;
        }
        if ui.button("➕ Common message attributes").on_hover_text("GenMsgCycleTime, GenMsgSendType, VFrameFormat").clicked() {
            for d in common_defs() {
                if dbc.attribute_def(&d.name).is_none() {
                    dbc.attribute_defs.push(d);
                    changed = true;
                }
            }
        }
    });
    changed
}

fn common_defs() -> Vec<AttributeDef> {
    let e = |v: &[&str]| AttrKind::Enum(v.iter().map(|s| s.to_string()).collect());
    vec![
        AttributeDef {
            name: "GenMsgCycleTime".into(),
            object: AttrObject::Message,
            kind: AttrKind::Int { min: 0, max: 65535 },
            default: Some(dbc::AttrValue::Int(0)),
        },
        AttributeDef {
            name: "GenMsgSendType".into(),
            object: AttrObject::Message,
            kind: e(&["Cyclic", "NotUsed", "IfActive", "NoMsgSendType"]),
            default: Some(dbc::AttrValue::Int(0)),
        },
        AttributeDef {
            name: "VFrameFormat".into(),
            object: AttrObject::Message,
            kind: e(&["StandardCAN", "ExtendedCAN", "StandardCAN_FD", "ExtendedCAN_FD"]),
            default: Some(dbc::AttrValue::Int(0)),
        },
        AttributeDef {
            name: "GenSigStartValue".into(),
            object: AttrObject::Signal,
            kind: AttrKind::Float { min: 0.0, max: 0.0 },
            default: Some(dbc::AttrValue::Float(0.0)),
        },
    ]
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use crate::session::DbFile;
    use egui_kittest::{Harness, kittest::Queryable};

    struct State {
        editor: DbcEditor,
        session: Session,
        order: MessageOrder,
    }

    fn harness() -> Harness<'static, State> {
        let src = "BU_: ECU\nBO_ 291 Status: 8 ECU\n SG_ Speed : 0|16@1+ (0.1,0) [0|6553.5] \"km/h\" Vector__XXX\n";
        let mut session = Session::default();
        session.dbs.push(DbFile {
            path: None,
            dbc: dbc::parse(src).unwrap(),
            dirty: false,
            errors: vec![],
            enabled: true,
            encoding: Default::default(),
        });
        session.recompile();
        let state = State { editor: DbcEditor::default(), session, order: MessageOrder::Id };
        let mut h = Harness::builder()
            .with_size(egui::vec2(1400.0, 1000.0))
            .build_ui_state(|ui, st: &mut State| st.editor.ui(ui, &mut st.session, &mut st.order), state);
        h.run();
        h
    }

    fn signals(h: &Harness<'_, State>) -> Vec<String> {
        h.state().session.dbs[0].dbc.messages[0].signals.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn add_signal_then_undo_and_redo() {
        let mut h = harness();
        h.get_by_label_contains("Status").click();
        h.run();
        h.get_by_label("➕ Add signal").click();
        h.run();
        assert_eq!(signals(&h), ["Speed", "Signal_2"]);
        assert!(h.state().session.dbs[0].dirty);
        // The edit is committed and recompiled once the gesture ends.
        assert!(h.state().session.db.messages[0].signal_index("Signal_2").is_some(), "decoder not updated");

        h.get_by_label("Undo").click();
        h.run();
        assert_eq!(signals(&h), ["Speed"]);
        h.get_by_label("Redo").click();
        h.run();
        assert_eq!(signals(&h), ["Speed", "Signal_2"]);
    }

    #[test]
    fn rename_and_delete_message_are_undoable() {
        let mut h = harness();
        h.get_by_label_contains("Status").click();
        h.run();
        // Rename: commit happens on Enter.
        let name_field = || egui_kittest::kittest::by().role(egui::accesskit::Role::TextInput).value("Status");
        h.get(name_field()).click();
        h.run();
        h.key_combination_modifiers(egui::Modifiers::COMMAND, &[egui::Key::A]);
        h.get(name_field()).type_text("Engine");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().session.dbs[0].dbc.messages[0].name, "Engine");

        h.get_by_label("🗑 Delete message").click();
        h.run();
        assert!(h.state().session.dbs[0].dbc.messages.is_empty());
        h.get_by_label("Undo").click();
        h.run();
        assert_eq!(h.state().session.dbs[0].dbc.messages[0].name, "Engine");
        h.get_by_label("Undo").click();
        h.run();
        assert_eq!(h.state().session.dbs[0].dbc.messages[0].name, "Status");
    }
}
