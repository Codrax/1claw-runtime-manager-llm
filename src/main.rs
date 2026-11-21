#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod config;
mod metrics;
mod supervisor;
mod theme;
mod ui_procs;
mod ui_snippets;

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Clawrun")
            .with_app_id("clawrun")
            .with_inner_size([1240.0, 780.0])
            .with_min_inner_size([820.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("Clawrun", opts, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
