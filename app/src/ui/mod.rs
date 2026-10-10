//! Views. Each dock tab is a struct with a `ui(&mut self, ui, &mut Session, …)`.

pub mod dbc_editor;
pub mod diagnostics;
pub mod plot;
pub mod signal_tree;
pub mod trace;
pub mod transmit;

use serde::{Deserialize, Serialize};

/// Drag-and-drop payload for a signal (by name, so it survives recompiles).
#[derive(Clone, Debug)]
pub struct SignalDrag {
    pub msg: String,
    pub sig: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Plots,
    Trace,
    Monitor,
    Diagnostics,
    Transmit,
    DbcEditor,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Plots, Tab::Trace, Tab::Monitor, Tab::Diagnostics, Tab::Transmit, Tab::DbcEditor];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Plots => "📈 Plots",
            Tab::Trace => "📜 Trace",
            Tab::Monitor => "🔎 Monitor",
            Tab::Diagnostics => "⚕ Diagnostics",
            Tab::Transmit => "📤 Transmit",
            Tab::DbcEditor => "📝 DBC Editor",
        }
    }
}
