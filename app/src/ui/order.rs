//! Display order for message lists (DBC editor, signal tree). Only the view is
//! ordered: the database keeps file order, so saving never reshuffles it.

use dbc::{EXTENDED_FLAG, VECTOR_XXX};
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageOrder {
    /// Ascending id, standard before extended (bus priority order).
    #[default]
    Id,
    /// Grouped by transmitting node, each group by id.
    Node,
    /// Alphabetical by name.
    Name,
    /// As written in the .dbc file.
    File,
}

impl MessageOrder {
    const ALL: [MessageOrder; 4] = [MessageOrder::Id, MessageOrder::Node, MessageOrder::Name, MessageOrder::File];

    fn label(self) -> &'static str {
        match self {
            MessageOrder::Id => "ID",
            MessageOrder::Node => "Node",
            MessageOrder::Name => "Name",
            MessageOrder::File => "File order",
        }
    }

    /// Compact selector, shared by every message list.
    pub fn picker(&mut self, ui: &mut egui::Ui, salt: &str) {
        egui::ComboBox::from_id_salt(("message_order", salt))
            .selected_text(format!("Sort: {}", self.label()))
            .show_ui(ui, |ui| {
                for o in Self::ALL {
                    ui.selectable_value(self, o, o.label());
                }
            })
            .response
            .on_hover_text("Display order only; the file keeps its own order when saved");
    }
}

/// Messages shown together; `node` is set when grouping by transmitter.
#[derive(Debug, PartialEq)]
pub struct Group {
    pub node: Option<String>,
    pub items: Vec<usize>,
}

/// One message to arrange: its index, frame key (id | extended flag), name and transmitter.
pub struct Entry<'a> {
    pub index: usize,
    pub key: u32,
    pub name: &'a str,
    pub node: &'a str,
}

/// Sort key: standard ids before extended, then ascending raw id.
fn id_key(key: u32) -> (bool, u32) {
    (key & EXTENDED_FLAG != 0, key & !EXTENDED_FLAG)
}

const NO_NODE: &str = "(no transmitter)";

/// Arrange already-filtered messages for display.
pub fn arrange(order: MessageOrder, mut entries: Vec<Entry<'_>>) -> Vec<Group> {
    match order {
        MessageOrder::File => entries.sort_by_key(|e| e.index),
        MessageOrder::Id => entries.sort_by_key(|e| (id_key(e.key), e.index)),
        MessageOrder::Name => entries.sort_by_key(|e| (e.name.to_lowercase(), id_key(e.key))),
        MessageOrder::Node => {
            // Real nodes alphabetically, unassigned messages last; ids ascending inside.
            entries.sort_by_key(|e| (e.node == VECTOR_XXX, e.node.to_lowercase(), id_key(e.key)));
            let mut groups: Vec<Group> = Vec::new();
            for e in entries {
                let node = if e.node == VECTOR_XXX { NO_NODE } else { e.node };
                match groups.last_mut() {
                    Some(g) if g.node.as_deref() == Some(node) => g.items.push(e.index),
                    _ => groups.push(Group { node: Some(node.to_string()), items: vec![e.index] }),
                }
            }
            return groups;
        }
    }
    vec![Group { node: None, items: entries.into_iter().map(|e| e.index).collect() }]
}

/// Draw groups: node groups get a collapsible header, plain lists are drawn flat.
pub fn show(ui: &mut egui::Ui, groups: &[Group], salt: &str, mut item: impl FnMut(&mut egui::Ui, usize)) {
    for g in groups {
        match &g.node {
            Some(node) => {
                egui::CollapsingHeader::new(format!("{node} ({})", g.items.len()))
                    .id_salt(("msg_group", salt, node))
                    .default_open(true)
                    .show(ui, |ui| g.items.iter().for_each(|&i| item(ui, i)));
            }
            None => g.items.iter().for_each(|&i| item(ui, i)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry<'static>> {
        // File order as in a merged DBC: unsorted, mixed nodes, one extended id.
        [
            (0x360, "BMS_RX_CTRL", "ECU"),
            (0x400, "BMS_TX_CMD", "BMS"),
            (0x10 | EXTENDED_FLAG, "Ext_Low", "Inverter"),
            (0x202, "BOOT_TX", "Vector__XXX"),
            (0x025, "TER_STATUS", "ECU"),
            (0x200, "boot_data", "Vector__XXX"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (key, name, node))| Entry { index, key, name, node })
        .collect()
    }

    fn flat(groups: Vec<Group>) -> Vec<usize> {
        groups.into_iter().flat_map(|g| g.items).collect()
    }

    #[test]
    fn id_order_puts_standard_before_extended() {
        // 0x025, 0x200, 0x202, 0x360, 0x400, then extended 0x10.
        assert_eq!(flat(arrange(MessageOrder::Id, entries())), [4, 5, 3, 0, 1, 2]);
    }

    #[test]
    fn name_order_is_case_insensitive() {
        let names: Vec<usize> = flat(arrange(MessageOrder::Name, entries()));
        assert_eq!(names, [0, 1, 5, 3, 2, 4]); // BMS_RX, BMS_TX, boot_data, BOOT_TX, Ext_Low, TER
    }

    #[test]
    fn file_order_is_untouched() {
        assert_eq!(flat(arrange(MessageOrder::File, entries())), [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn node_groups_are_alphabetical_with_unassigned_last() {
        let groups = arrange(MessageOrder::Node, entries());
        let shape: Vec<(String, Vec<usize>)> = groups.into_iter().map(|g| (g.node.unwrap(), g.items)).collect();
        assert_eq!(
            shape,
            [
                ("BMS".into(), vec![1]),
                ("ECU".into(), vec![4, 0]), // by id inside the group
                ("Inverter".into(), vec![2]),
                (NO_NODE.into(), vec![5, 3]),
            ]
        );
    }
}
