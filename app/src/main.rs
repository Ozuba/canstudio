//! canstudio — a CAN bus tool suite.

mod app;
mod session;
mod ui;
mod util;

/// Usage: `canstudio [FILE.dbc | FILE.log]... [--connect IFACE] [--plot Msg.Sig[,Msg.Sig...]]...`
fn main() -> eframe::Result {
    env_logger::init();
    let args = app::Args::parse(std::env::args_os().skip(1));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("canstudio")
            .with_app_id("canstudio")
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("canstudio", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, args)))))
}
