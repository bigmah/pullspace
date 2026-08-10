#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod ui;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};

fn main() {
    let window = WindowBuilder::new()
        .with_title("gittit")
        .with_inner_size(LogicalSize::new(1440.0, 920.0));
    dioxus::LaunchBuilder::desktop()
        .with_cfg(Config::new().with_window(window))
        .launch(ui::App);
}
