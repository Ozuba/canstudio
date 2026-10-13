//! Transmit panel: raw or DBC-encoded frames, sent once or periodically by
//! the bus scheduler thread.

use std::time::Duration;

use canbus::frame::{Frame, flags, fmt_key};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::util;

#[derive(Clone, Serialize, Deserialize)]
enum Payload {
    Raw {
        id: u32,
        extended: bool,
        data: String,
    },
    /// Physical values by signal name.
    Dbc {
        msg: String,
        values: Vec<(String, f64)>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct Job {
    id: u64,
    payload: Payload,
    period_ms: f64,
    fd: bool,
    brs: bool,
    /// Periodic transmission running. Never restored on startup.
    #[serde(skip)]
    running: bool,
    #[serde(skip)]
    open: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Transmit {
    jobs: Vec<Job>,
    next_id: u64,
    #[serde(skip)]
    removed: Vec<u64>,
    #[serde(skip)]
    pick: String,
}

impl Job {
    /// Build the frame, or explain why it cannot be built.
    fn frame(&self, s: &Session) -> Result<Frame, String> {
        let mut f = match &self.payload {
            Payload::Raw { id, extended, data } => {
                let bytes = util::parse_hex(data).ok_or("invalid hex payload")?;
                let max_id = if *extended { 0x1FFF_FFFF } else { 0x7FF };
                if *id > max_id {
                    return Err(format!("id exceeds {max_id:X}"));
                }
                Frame::new(*id, *extended, &bytes)
            }
            Payload::Dbc { msg, values } => {
                let m = s.db.messages.iter().find(|m| &m.name == msg).ok_or_else(|| format!("{msg} not in the loaded databases"))?;
                let phys: Vec<f64> = m
                    .signals
                    .iter()
                    .map(|sig| values.iter().find(|(n, _)| *n == sig.name).map_or_else(|| default_value(sig), |(_, v)| *v))
                    .collect();
                let (id, ext) = canbus::frame::split_key(m.id);
                Frame::new(id, ext, &m.encode(&phys))
            }
        };
        if f.len > 64 {
            return Err("payload longer than 64 bytes".into());
        }
        if self.fd || f.len > 8 {
            f.flags |= flags::FD;
            if self.brs {
                f.flags |= flags::BRS;
            }
        }
        Ok(f)
    }

    fn title(&self) -> String {
        match &self.payload {
            Payload::Raw { id, extended, data } => format!("{} #{}", canbus::frame::fmt_id(*id, *extended), data.replace(' ', "")),
            Payload::Dbc { msg, .. } => msg.clone(),
        }
    }
}

fn default_value(sig: &dbc::CompiledSignal) -> f64 {
    if sig.max > sig.min { 0.0f64.clamp(sig.min, sig.max) } else { 0.0 }
}

impl Transmit {
    /// Keep the scheduler in step with the jobs (called every frame).
    pub fn sync(&mut self, s: &mut Session) {
        #[cfg(target_os = "linux")]
        {
            let Some(bus) = s.live() else {
                self.jobs.iter_mut().for_each(|j| j.running = false);
                return;
            };
            for id in self.removed.drain(..) {
                bus.scheduler.set(id, None);
            }
            for job in &self.jobs {
                let periodic =
                    job.running.then(|| job.frame(s).ok()).flatten().map(|f| (f, Duration::from_secs_f64(job.period_ms.max(0.1) / 1e3)));
                bus.scheduler.set(job.id, periodic);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = s;
    }

    fn add(&mut self, payload: Payload, period_ms: f64) {
        self.jobs.push(Job { id: self.next_id, payload, period_ms, fd: false, brs: false, running: false, open: true });
        self.next_id += 1;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, s: &mut Session) {
        let live = s.is_live();
        ui.horizontal(|ui| {
            if ui.button("➕ Raw frame").clicked() {
                self.add(Payload::Raw { id: 0x123, extended: false, data: "00 00 00 00 00 00 00 00".into() }, 100.0);
            }
            let mut chosen = None;
            egui::ComboBox::from_id_salt("tx_add_msg").selected_text("➕ DBC message…").height(400.0).show_ui(ui, |ui| {
                ui.text_edit_singleline(&mut self.pick);
                let f = self.pick.to_lowercase();
                for m in &s.db.messages {
                    if util::matches(&f, &[&m.name, &fmt_key(m.id)])
                        && ui.selectable_label(false, format!("{}  {}", fmt_key(m.id), m.name)).clicked()
                    {
                        chosen = Some((m.name.clone(), m.cycle_time_ms.unwrap_or(100.0)));
                    }
                }
            });
            if let Some((msg, period)) = chosen {
                self.add(Payload::Dbc { msg, values: Vec::new() }, period);
            }
            #[cfg(target_os = "linux")]
            if let Some(bus) = s.live() {
                let (sent, err) = bus.scheduler.status();
                ui.label(RichText::new(format!("{sent} periodic frames queued")).weak())
                    .on_hover_text("Handed to the interface. Frames only leave the adapter once another node acknowledges them.");
                if let Some(e) = err {
                    ui.label(RichText::new(tx_error_hint(&e)).color(ui.visuals().error_fg_color)).on_hover_text(e);
                }
            }
        });
        if !live {
            ui.label(RichText::new("Connect to a bus to transmit. Jobs can be prepared offline.").color(ui.visuals().warn_fg_color));
        }
        ui.separator();

        let mut remove = None;
        let mut send_now = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, job) in self.jobs.iter_mut().enumerate() {
                let frame = job.frame(s);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let arrow = if job.open { "⏷" } else { "⏵" };
                        if ui.small_button(arrow).clicked() {
                            job.open = !job.open;
                        }
                        ui.strong(job.title());
                        ui.add_enabled_ui(live && frame.is_ok(), |ui| {
                            if ui.button("Send").clicked()
                                && let Ok(f) = &frame
                            {
                                send_now.push(*f);
                            }
                            let label = if job.running { "⏹ Stop" } else { "🔁 Periodic" };
                            ui.toggle_value(&mut job.running, label);
                        });
                        ui.add(egui::DragValue::new(&mut job.period_ms).range(0.1..=60_000.0).speed(1.0).suffix(" ms"));
                        ui.checkbox(&mut job.fd, "FD");
                        ui.add_enabled(job.fd, egui::Checkbox::new(&mut job.brs, "BRS"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("🗑").clicked() {
                                remove = Some(i);
                            }
                            match &frame {
                                Ok(f) => ui.label(RichText::new(util::hex_bytes(f.data())).monospace().weak()),
                                Err(e) => ui.label(RichText::new(e).color(ui.visuals().error_fg_color)),
                            };
                        });
                    });
                    if job.open {
                        editor(ui, s, job);
                    }
                });
            }
        });
        if let Some(i) = remove {
            self.removed.push(self.jobs.remove(i).id);
        }
        #[cfg(target_os = "linux")]
        if let Some(bus) = s.live() {
            let errors: Vec<String> = send_now.iter().filter_map(|f| bus.send(f).err().map(|e| e.to_string())).collect();
            if let Some(e) = errors.first() {
                s.error(tx_error_hint(e));
            }
        }
    }
}

fn editor(ui: &mut egui::Ui, s: &Session, job: &mut Job) {
    match &mut job.payload {
        Payload::Raw { id, extended, data } => {
            ui.horizontal(|ui| {
                ui.label("ID");
                let max = if *extended { 0x1FFF_FFFF } else { 0x7FF };
                ui.add(egui::DragValue::new(id).range(0..=max).hexadecimal(if *extended { 8 } else { 3 }, false, true).prefix("0x"));
                ui.checkbox(extended, "Extended");
                ui.label("Data");
                ui.add(egui::TextEdit::singleline(data).desired_width(360.0).font(egui::TextStyle::Monospace));
            });
        }
        Payload::Dbc { msg, values } => {
            let Some(m) = s.db.messages.iter().find(|m| &m.name == msg) else { return };
            let phys: Vec<f64> = m
                .signals
                .iter()
                .map(|sig| values.iter().find(|(n, _)| *n == sig.name).map_or_else(|| default_value(sig), |(_, v)| *v))
                .collect();
            let data = m.encode(&phys);
            egui::Grid::new(("tx_sigs", job.id)).striped(true).spacing([12.0, 2.0]).show(ui, |ui| {
                for (i, sig) in m.signals.iter().enumerate() {
                    if !m.is_active(i, &data) {
                        continue;
                    }
                    let mut v = phys[i];
                    ui.label(&sig.name);
                    let changed = if sig.is_enum() {
                        let mut changed = false;
                        egui::ComboBox::from_id_salt(("tx_enum", job.id, i)).selected_text(sig.format(v)).show_ui(ui, |ui| {
                            for (raw, label) in &sig.labels {
                                let p = *raw as f64 * sig.factor + sig.offset;
                                if ui.selectable_label(sig.table_key(v) == *raw, format!("{label} ({raw})")).clicked() {
                                    v = p;
                                    changed = true;
                                }
                            }
                        });
                        changed
                    } else {
                        let mut dv = egui::DragValue::new(&mut v).speed(sig.factor.abs().max(1e-6)).max_decimals(6);
                        if sig.max > sig.min {
                            dv = dv.range(sig.min..=sig.max);
                        }
                        if !sig.unit.is_empty() {
                            dv = dv.suffix(format!(" {}", sig.unit));
                        }
                        ui.add(dv).changed()
                    };
                    ui.label(
                        RichText::new(format!("raw {}", sig.to_raw(v) & if sig.size() >= 64 { u64::MAX } else { (1 << sig.size()) - 1 }))
                            .weak()
                            .small(),
                    );
                    ui.end_row();
                    if changed {
                        match values.iter_mut().find(|(n, _)| *n == sig.name) {
                            Some(slot) => slot.1 = v,
                            None => values.push((sig.name.clone(), v)),
                        }
                    }
                }
            });
        }
    }
}

/// Explain the usual transmit failures in bus terms.
pub fn tx_error_hint(err: &str) -> String {
    if err.contains("os error 105") {
        "Transmit queue full: frames are not being acknowledged (no other powered node on the bus, missing termination, or wrong bitrate?)"
            .into()
    } else if err.contains("os error 100") {
        "Interface is down or bus-off: restart it (sudo ip link set <iface> down && up)".into()
    } else {
        format!("Transmit failed: {err}")
    }
}
