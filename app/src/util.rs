//! Small shared helpers: palette, formatting, file dialogs.

use eframe::egui::{self, Color32};

/// Categorical palette tuned for dark and light backgrounds.
pub const PALETTE: [Color32; 12] = [
    Color32::from_rgb(0x4e, 0x9a, 0xf0),
    Color32::from_rgb(0xf2, 0x8e, 0x2b),
    Color32::from_rgb(0x59, 0xc1, 0x4f),
    Color32::from_rgb(0xe8, 0x57, 0x59),
    Color32::from_rgb(0x76, 0xc7, 0xc2),
    Color32::from_rgb(0xed, 0xc9, 0x48),
    Color32::from_rgb(0xc0, 0x7a, 0xd1),
    Color32::from_rgb(0xff, 0x9d, 0xa7),
    Color32::from_rgb(0xb5, 0x8d, 0x6f),
    Color32::from_rgb(0x9a, 0xa5, 0xb5),
    Color32::from_rgb(0x2f, 0xd0, 0x9e),
    Color32::from_rgb(0xd6, 0x6b, 0xb8),
];

pub fn palette(i: usize) -> Color32 {
    PALETTE[i % PALETTE.len()]
}

/// Stable colour for an enum state, so the same label always gets the same hue.
pub fn state_color(key: i64) -> Color32 {
    palette(key.unsigned_abs() as usize).gamma_multiply(0.55)
}

pub fn hex_bytes(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// Parse a hex payload, tolerating spaces, `.` and `0x` prefixes.
pub fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let clean: String = s.split_whitespace().map(|t| t.trim_start_matches("0x")).collect::<String>().replace('.', "");
    if !clean.len().is_multiple_of(2) {
        return None;
    }
    (0..clean.len()).step_by(2).map(|i| u8::from_str_radix(&clean[i..i + 2], 16).ok()).collect()
}

/// Human-friendly duration: `850 µs`, `12.5 ms`, `3.20 s`, `4m 05s`.
pub fn fmt_duration(s: f64) -> String {
    let a = s.abs();
    if a < 1e-3 {
        format!("{:.0} µs", s * 1e6)
    } else if a < 1.0 {
        format!("{:.2} ms", s * 1e3)
    } else if a < 120.0 {
        format!("{s:.3} s")
    } else {
        format!("{}m {:02}s", (s / 60.0) as i64, (s % 60.0) as i64)
    }
}

/// Wall-clock rendering of an absolute UNIX timestamp in local time.
pub fn fmt_wallclock(abs: f64, with_date: bool) -> String {
    use chrono::{Local, TimeZone};
    let secs = abs.floor() as i64;
    let nanos = ((abs - abs.floor()) * 1e9) as u32;
    match Local.timestamp_opt(secs, nanos).single() {
        Some(t) if with_date => t.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        Some(t) => t.format("%H:%M:%S%.3f").to_string(),
        None => format!("{abs:.6}"),
    }
}

pub fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

pub mod dialog {
    use std::path::PathBuf;

    pub fn open_dbc() -> Option<Vec<PathBuf>> {
        rfd::FileDialog::new().add_filter("DBC", &["dbc", "DBC"]).add_filter("All", &["*"]).pick_files()
    }

    pub fn open_log() -> Option<PathBuf> {
        rfd::FileDialog::new().add_filter("candump log", &["log", "txt", "candump"]).add_filter("All", &["*"]).pick_file()
    }

    pub fn save(name: &str, ext: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_file_name(name).add_filter(ext, &[ext]).save_file()
    }
}

/// A drag-and-drop source that is also clickable. Unlike `Ui::dnd_drag_source`
/// (drag only), clicks and context menus on it still work; a drag starts
/// only once the pointer moves, and the contents follow the pointer.
pub fn drag_item<P: std::any::Any + Send + Sync>(
    ui: &mut egui::Ui,
    id: egui::Id,
    payload: P,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    if ui.ctx().is_being_dragged(id) {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);
        let layer = egui::LayerId::new(egui::Order::Tooltip, id);
        let resp = ui.scope_builder(egui::UiBuilder::new().layer_id(layer), add_contents).response;
        if let Some(p) = ui.ctx().pointer_interact_pos() {
            ui.ctx().transform_layer_shapes(layer, egui::emath::TSTransform::from_translation(p - resp.rect.center()));
        }
        return resp;
    }
    let rect = ui.scope(add_contents).response.rect;
    let resp = ui.interact(rect, id, egui::Sense::click_and_drag());
    resp.dnd_set_drag_payload(payload);
    resp
}

/// A small coloured square, used in legends.
pub fn swatch(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 2.0, color);
    resp
}

/// Text field + clear button used for every filter box.
pub fn search_box(ui: &mut egui::Ui, text: &mut String, hint: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(ui.available_width() - 24.0));
        if !text.is_empty() && ui.small_button("✖").clicked() {
            text.clear();
        }
        r
    })
    .inner
}

/// Case-insensitive substring match against any of the given haystacks.
pub fn matches(filter: &str, hay: &[&str]) -> bool {
    filter.is_empty() || hay.iter().any(|h| h.to_lowercase().contains(filter))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex("de AD.be 0xEF"), Some(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        assert_eq!(parse_hex("abc"), None);
        assert_eq!(hex_bytes(&[1, 0xAB]), "01 AB");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(0.0005), "500 µs");
        assert_eq!(fmt_duration(0.0125), "12.50 ms");
        assert_eq!(fmt_duration(185.0), "3m 05s");
    }
}
