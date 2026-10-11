//! Application shell: menus, toolbar, docked tabs, file drop, persistence.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::ui::Tab;
use crate::ui::dbc_editor::DbcEditor;
use crate::ui::diagnostics::Diagnostics;
use crate::ui::order::MessageOrder;
use crate::ui::plot::PlotWorkspace;
use crate::ui::signal_tree::SignalTree;
use crate::ui::trace::{MonitorView, TraceView};
use crate::ui::transmit::Transmit;
use crate::util::{self, dialog};

const MAX_RECENT: usize = 10;

/// Command line: files to open and panes to create.
#[derive(Default)]
pub struct Args {
    pub files: Vec<PathBuf>,
    /// One pane per `--plot`, each a comma-separated list of `Message.Signal`.
    pub plots: Vec<Vec<String>>,
    /// SocketCAN interface to connect to at startup.
    pub connect: Option<String>,
}

impl Args {
    pub fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Self {
        let mut out = Args::default();
        let mut args = args.peekable();
        while let Some(a) = args.next() {
            if a == "--connect" {
                out.connect = args.next().map(|i| i.to_string_lossy().into_owned());
            } else if a == "--plot" {
                if let Some(list) = args.next() {
                    out.plots.push(list.to_string_lossy().split(',').map(|s| s.trim().to_string()).collect());
                }
            } else {
                out.files.push(a.into());
            }
        }
        out
    }
}

/// Persisted between runs.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Persisted {
    dock: DockState<Tab>,
    plots: PlotWorkspace,
    transmit: Transmit,
    diagnostics: Diagnostics,
    /// Databases to reopen: (path, enabled).
    dbcs: Vec<(PathBuf, bool)>,
    recent_logs: Vec<PathBuf>,
    iface: String,
    bitrate: canbus::Bitrate,
    show_tree: bool,
    dark: bool,
    /// Display order of message lists (editor and signal tree).
    message_order: MessageOrder,
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            dock: default_dock(),
            plots: Default::default(),
            transmit: Default::default(),
            diagnostics: Default::default(),
            dbcs: Vec::new(),
            recent_logs: Vec::new(),
            iface: "can0".into(),
            bitrate: Default::default(),
            show_tree: true,
            dark: true,
            message_order: MessageOrder::default(),
        }
    }
}

fn default_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Plots, Tab::DbcEditor]);
    let surface = dock.main_surface_mut();
    let [_, bottom] = surface.split_below(NodeIndex::root(), 0.68, vec![Tab::Trace, Tab::Monitor, Tab::Diagnostics, Tab::Transmit]);
    let _ = bottom;
    dock
}

pub struct App {
    p: Persisted,
    session: Session,
    tree: SignalTree,
    trace: TraceView,
    monitor: MonitorView,
    editor: DbcEditor,
    ifaces: (Vec<String>, Option<Instant>),
    /// Development aid: save one frame to this PPM file and exit (`CANSTUDIO_SCREENSHOT`).
    screenshot: Option<(PathBuf, u32)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Self {
        let p: Persisted = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default();
        cc.egui_ctx.set_visuals(if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
        let mut app = App {
            session: Session::default(),
            tree: Default::default(),
            trace: Default::default(),
            monitor: Default::default(),
            editor: Default::default(),
            ifaces: (Vec::new(), None),
            // Development aid only; compiled out of the hook unless `--features dev-screenshot`.
            screenshot: cfg!(feature = "dev-screenshot").then(|| std::env::var_os("CANSTUDIO_SCREENSHOT")).flatten().map(|p| (p.into(), 0)),
            p,
        };
        app.session.bitrate = app.p.bitrate;
        for (path, enabled) in app.p.dbcs.clone() {
            if path.exists() {
                app.session.open_dbc(&path);
                if let Some(db) = app.session.dbs.last_mut() {
                    db.enabled = enabled;
                }
            }
        }
        app.session.recompile();
        for path in &args.files {
            app.open_path(path);
        }
        #[cfg(target_os = "linux")]
        if let Some(iface) = &args.connect {
            app.p.iface = iface.clone();
            app.session.connect(iface, &cc.egui_ctx);
        }
        if !args.plots.is_empty() {
            app.p.plots.panes.clear();
            for list in &args.plots {
                let pane = app.p.plots.panes.len();
                for path in list {
                    if let Some((msg, sig)) = path.split_once('.') {
                        let drag = crate::ui::SignalDrag { msg: msg.into(), sig: sig.into() };
                        app.p.plots.add_signal(&drag, Some(pane));
                    }
                }
            }
        }
        app
    }

    fn open_path(&mut self, path: &Path) {
        let is_dbc = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dbc"));
        if is_dbc {
            self.session.open_dbc(path);
        } else {
            self.p.recent_logs.retain(|p| p != path);
            self.p.recent_logs.insert(0, path.to_path_buf());
            self.p.recent_logs.truncate(MAX_RECENT);
            self.session.open_log(path);
        }
    }

    fn focus_tab(&mut self, tab: Tab) {
        match self.p.dock.find_tab(&tab) {
            Some(path) => {
                let _ = self.p.dock.set_active_tab(path);
            }
            None => self.p.dock.push_to_focused_leaf(tab),
        }
    }

    fn interfaces(&mut self) -> &[String] {
        if self.ifaces.1.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            #[cfg(target_os = "linux")]
            {
                self.ifaces.0 = canbus::socket::list_interfaces();
            }
            self.ifaces.1 = Some(Instant::now());
        }
        &self.ifaces.0
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for path in dropped {
            self.open_path(&path);
        }
        let (open_log, open_dbc) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::D)),
            )
        });
        if open_log && let Some(p) = dialog::open_log() {
            self.open_path(&p);
        }
        if open_dbc {
            for p in dialog::open_dbc().unwrap_or_default() {
                self.session.open_dbc(&p);
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open DBC…  (Ctrl+D)").clicked() {
                    for p in dialog::open_dbc().unwrap_or_default() {
                        self.session.open_dbc(&p);
                    }
                    ui.close();
                }
                if ui.button("New DBC").clicked() {
                    let i = self.session.new_dbc();
                    self.editor.select_db(i);
                    self.focus_tab(Tab::DbcEditor);
                    ui.close();
                }
                ui.separator();
                if ui.button("Open candump log…  (Ctrl+O)").clicked() {
                    if let Some(p) = dialog::open_log() {
                        self.open_path(&p);
                    }
                    ui.close();
                }
                ui.menu_button("Recent logs", |ui| {
                    for p in self.p.recent_logs.clone() {
                        if ui.button(util::file_name(&p)).on_hover_text(p.display().to_string()).clicked() {
                            self.open_path(&p);
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.p.show_tree, "Signal tree");
                ui.separator();
                for tab in Tab::ALL {
                    if ui.button(tab.title()).clicked() {
                        self.focus_tab(tab);
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Reset layout").clicked() {
                    self.p.dock = default_dock();
                    ui.close();
                }
                if ui.checkbox(&mut self.p.dark, "Dark theme").changed() {
                    ui.ctx().set_visuals(if self.p.dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label(RichText::new("Plots").strong());
                ui.label("Drag: pan · Scroll / Ctrl+scroll: zoom · Right-drag: box zoom");
                ui.label("Click: cursor A · Shift+click: cursor B · F: fit all");
                ui.label("Left/Right: pan · +/-: zoom · Home/End: jump · Space: follow (live)");
                ui.separator();
                ui.label(RichText::new("Files").strong());
                ui.label("Drop .dbc or candump logs anywhere on the window.");
                ui.label("Ctrl+O: open log · Ctrl+D: open DBC · Ctrl+S / Ctrl+Z / Ctrl+Y in the DBC editor");
                ui.separator();
                ui.label(RichText::new(format!("canstudio {}", crate::VERSION)).weak());
                ui.hyperlink_to("github.com/Ozuba/canstudio", "https://github.com/Ozuba/canstudio");
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📂 Log").on_hover_text("Open a candump log (Ctrl+O)").clicked()
                && let Some(p) = dialog::open_log()
            {
                self.open_path(&p);
            }
            if ui.button("📁 DBC").on_hover_text("Open DBC files (Ctrl+D)").clicked() {
                for p in dialog::open_dbc().unwrap_or_default() {
                    self.session.open_dbc(&p);
                }
            }
            ui.separator();
            self.bus_controls(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(self.session.source_label()).strong());
                for db in self.session.dbs.iter_mut().rev() {
                    let name = format!("{}{}", db.name(), if db.dirty { " •" } else { "" });
                    if ui.toggle_value(&mut db.enabled, name).on_hover_text("Use this database for decoding").changed() {
                        self.editor.db_toggled = true;
                    }
                }
                if std::mem::take(&mut self.editor.db_toggled) {
                    self.session.recompile();
                }
            });
        });
    }

    #[cfg(target_os = "linux")]
    fn bus_controls(&mut self, ui: &mut egui::Ui) {
        let live = self.session.is_live();
        ui.add_enabled_ui(!live, |ui| {
            let ifaces = self.interfaces().to_vec();
            egui::ComboBox::from_id_salt("iface").width(90.0).selected_text(&self.p.iface).show_ui(ui, |ui| {
                for i in &ifaces {
                    ui.selectable_value(&mut self.p.iface, i.clone(), i);
                }
                if ifaces.is_empty() {
                    ui.label(RichText::new("No CAN interfaces found").weak());
                }
                ui.text_edit_singleline(&mut self.p.iface);
            });
        });
        if live {
            if ui.button("⏹ Disconnect").clicked() {
                self.session.disconnect();
            }
            let label = if self.session.paused { "▶ Resume" } else { "⏸ Pause" };
            if ui.button(label).on_hover_text("Freeze the views; capture continues").clicked() {
                self.session.paused = !self.session.paused;
            }
            match self.session.recording() {
                Some((name, n)) => {
                    if ui
                        .button(RichText::new(format!("⏺ {name} ({n})")).color(egui::Color32::from_rgb(0xff, 0x55, 0x55)))
                        .on_hover_text("Stop recording")
                        .clicked()
                    {
                        self.session.stop_recording();
                    }
                }
                None => {
                    if ui.button("⏺ Record").on_hover_text("Record the live bus to a candump log").clicked() {
                        self.session.start_recording();
                    }
                }
            }
        } else if ui.button("🔌 Connect").on_hover_text("Open the SocketCAN interface").clicked() {
            let iface = self.p.iface.clone();
            self.session.connect(&iface, ui.ctx());
        }
        if ui.button("🗑 Clear").on_hover_text("Discard captured/loaded frames").clicked() {
            self.session.clear_data();
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn bus_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Live SocketCAN is Linux-only").weak());
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some((name, frac)) = self.session.loading_progress() {
                ui.add(egui::ProgressBar::new(frac).desired_width(220.0).show_percentage().text(format!("Loading {name}")));
            }
            let s = &self.session;
            if let Some((a, b)) = s.store.time_range() {
                ui.label(format!(
                    "{} frames · {} ids · {} · {:.0} MB",
                    s.store.len(),
                    s.store.keys.len(),
                    util::fmt_duration(b - a),
                    s.store.memory_bytes() as f64 / 1e6
                ));
                ui.separator();
            }
            ui.label(format!("{} messages · {} databases", s.db.messages.len(), s.dbs.len()));
            if let Some(n) = &s.notice {
                ui.separator();
                let mut text = RichText::new(&n.text);
                if n.error {
                    text = text.color(ui.visuals().error_fg_color);
                } else if n.at.elapsed() > Duration::from_secs(15) {
                    text = text.weak();
                }
                ui.label(text);
            }
        });
    }
}

struct Tabs<'a> {
    session: &'a mut Session,
    plots: &'a mut PlotWorkspace,
    trace: &'a mut TraceView,
    monitor: &'a mut MonitorView,
    diagnostics: &'a mut Diagnostics,
    transmit: &'a mut Transmit,
    editor: &'a mut DbcEditor,
    message_order: &'a mut MessageOrder,
}

impl TabViewer for Tabs<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        if *tab == Tab::DbcEditor && self.session.dbs.iter().any(|d| d.dirty) {
            return format!("{} •", tab.title()).into();
        }
        tab.title().into()
    }

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Plots => self.plots.ui(ui, self.session),
            Tab::Trace => self.trace.ui(ui, self.session, self.plots),
            Tab::Monitor => self.monitor.ui(ui, self.session, self.plots),
            Tab::Diagnostics => self.diagnostics.ui(ui, self.session, self.plots),
            Tab::Transmit => self.transmit.ui(ui, self.session),
            Tab::DbcEditor => self.editor.ui(ui, self.session, self.message_order),
        }
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }
}

impl App {
    /// Once data is loaded and a few frames have settled, capture and exit.
    fn screenshot(&mut self, ctx: &egui::Context) {
        let loading = self.session.loading_progress().is_some();
        let Some((_, frames)) = &mut self.screenshot else { return };
        if !loading {
            *frames += 1;
        }
        let n = *frames;
        if n == 1 {
            // Optional: focus tabs given as `CANSTUDIO_SCREENSHOT_TABS=Monitor,DbcEditor`.
            let tabs = std::env::var("CANSTUDIO_SCREENSHOT_TABS").unwrap_or_default();
            for tab in Tab::ALL.into_iter().filter(|t| tabs.split(',').any(|n| n == format!("{t:?}"))) {
                self.focus_tab(tab);
            }
        }
        let after: u32 = std::env::var("CANSTUDIO_SCREENSHOT_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
        if n == after {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let mut ppm = format!("P6 {} {} 255\n", img.size[0], img.size[1]).into_bytes();
            ppm.extend(img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]));
            if let Some((path, _)) = &self.screenshot
                && let Err(e) = std::fs::write(path, ppm)
            {
                log::error!("screenshot: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.session.poll(ctx);
        self.p.transmit.sync(&mut self.session);
        self.handle_input(ctx);
        self.screenshot(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show(ui, |ui| {
            self.menu_bar(ui);
            self.toolbar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.p.show_tree {
            egui::Panel::left("tree").default_size(290.0).show(ui, |ui| {
                ui.add_space(4.0);
                self.tree.ui(ui, &mut self.session, &mut self.p.plots, &mut self.p.message_order);
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let mut tabs = Tabs {
                session: &mut self.session,
                plots: &mut self.p.plots,
                trace: &mut self.trace,
                monitor: &mut self.monitor,
                diagnostics: &mut self.p.diagnostics,
                transmit: &mut self.p.transmit,
                editor: &mut self.editor,
                message_order: &mut self.p.message_order,
            };
            DockArea::new(&mut self.p.dock)
                .style(Style::from_egui(ui.style()))
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut tabs);
        });

        let ctx = ui.ctx().clone();
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let rect = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop .dbc or candump log",
                egui::FontId::proportional(28.0),
                egui::Color32::WHITE,
            );
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.p.dbcs = self.session.dbs.iter().filter_map(|d| Some((d.path.clone()?, d.enabled))).collect();
        self.p.bitrate = self.session.bitrate;
        eframe::set_value(storage, eframe::APP_KEY, &self.p);
    }
}
