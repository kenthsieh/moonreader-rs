// MoonReader(Rust/egui 桌面版)— 主進入點。
use moonreader_rs::app::MoonReader;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MoonReader",
        options,
        Box::new(|cc| Ok(Box::new(MoonReader::new(cc)))),
    )
}
