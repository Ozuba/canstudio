//! Whole-recording minimap: frame density over time with the visible window
//! highlighted. Click or drag to move the view.

use eframe::egui;

use crate::session::Session;

#[derive(Default)]
pub struct Overview {
    /// Frame counts per bin, and the (store generation, length) they were built for.
    bins: Vec<u32>,
    built_for: (u64, usize),
    span: (f64, f64),
}

const BINS: usize = 600;

impl Overview {
    fn rebuild(&mut self, s: &Session) {
        let key = (s.store_gen, s.store.len());
        // Live data: rebuild at most every ~1 % growth.
        if self.built_for.0 == key.0 && key.1 < self.built_for.1 + self.built_for.1 / 100 + 64 {
            return;
        }
        self.built_for = key;
        let Some((a, b)) = s.store.time_range() else { return };
        let b = b.max(a + 1e-3);
        self.span = (a, b);
        self.bins = vec![0; BINS];
        let scale = BINS as f64 / (b - a);
        for &t in &s.store.ts {
            self.bins[(((t - a) * scale) as usize).min(BINS - 1)] += 1;
        }
    }

    /// Draw the overview; returns a new view range if the user moved it.
    pub fn ui(&mut self, ui: &mut egui::Ui, s: &Session, view: Option<(f64, f64)>) -> Option<(f64, f64)> {
        self.rebuild(s);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);

        let (a, b) = self.span;
        let max = self.bins.iter().copied().max().unwrap_or(1).max(1) as f32;
        let w = rect.width() / BINS as f32;
        let color = visuals.widgets.inactive.fg_stroke.color.gamma_multiply(0.6);
        for (i, &n) in self.bins.iter().enumerate() {
            if n > 0 {
                let h = (n as f32 / max).sqrt() * (rect.height() - 4.0);
                let x = rect.left() + i as f32 * w;
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - 2.0 - h), egui::pos2(x + w.max(1.0), rect.bottom() - 2.0)),
                    0.0,
                    color,
                );
            }
        }
        let to_x = |t: f64| rect.left() + ((t - a) / (b - a)) as f32 * rect.width();
        let to_t = |x: f32| a + ((x - rect.left()) / rect.width()) as f64 * (b - a);
        if let Some((v0, v1)) = view {
            let r = egui::Rect::from_x_y_ranges(to_x(v0).max(rect.left())..=to_x(v1).min(rect.right()).max(to_x(v0) + 2.0), rect.y_range());
            painter.rect_filled(r, 2.0, visuals.selection.bg_fill.gamma_multiply(0.35));
            painter.rect_stroke(r, 2.0, visuals.selection.stroke, egui::StrokeKind::Inside);
        }
        let resp = resp.on_hover_text("Recording overview — click or drag to move the view");
        if resp.clicked() || resp.dragged() {
            let p = resp.interact_pointer_pos()?;
            let half = view.map_or((b - a) / 20.0, |(v0, v1)| (v1 - v0) / 2.0);
            let c = to_t(p.x);
            return Some((c - half, c + half));
        }
        None
    }
}
