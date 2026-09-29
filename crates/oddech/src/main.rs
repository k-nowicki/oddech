//! Oddech — detect and attenuate audible breaths in the audio tracks of
//! video recordings.

mod app;
mod detection;
mod jobs;
mod logging;
mod session;
mod settings;
mod timeline;
mod ui;

use std::path::PathBuf;

fn main() -> eframe::Result {
    logging::init();

    let initial = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Oddech")
            .with_app_id("oddech")
            .with_inner_size([1440.0, 860.0])
            .with_min_inner_size([960.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("oddech", options, Box::new(move |cc| Ok(Box::new(app::OddechApp::new(cc, initial)))))
}
