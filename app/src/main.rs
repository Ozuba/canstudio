//! canstudio — a CAN bus tool suite.

mod app;
mod session;
mod ui;
mod util;

/// Usage: `canstudio [FILE.dbc | FILE.log]... [--version] [--connect IFACE] [--plot Msg.Sig[,Msg.Sig...]]...`
/// Version with the git commit it was built from, e.g. `0.1.0 (66f30c0)`.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CANSTUDIO_GIT_SHA"), ")");

fn main() -> eframe::Result {
    // The AppImage launcher changes directory; restore the user's so relative paths work.
    if std::env::var_os("APPIMAGE").is_some()
        && let Some(owd) = std::env::var_os("OWD")
    {
        let _ = std::env::set_current_dir(owd);
    }
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("canstudio {VERSION}");
        return Ok(());
    }
    env_logger::init();
    let args = app::Args::parse(std::env::args_os().skip(1));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("canstudio")
            .with_app_id("canstudio")
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/256x256.png")).expect("embedded icon")),
        ..Default::default()
    };
    eframe::run_native("canstudio", options, Box::new(|cc| Ok(Box::new(app::App::new(cc, args)))))
}
