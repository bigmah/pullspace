#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod ui;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};

fn main() {
    let window = WindowBuilder::new()
        .with_title("pullspace")
        .with_inner_size(LogicalSize::new(1440.0, 920.0));
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                .with_window(window)
                // Dioxus disables the webview's context menu in release builds.
                // In an app that exists to show you code — and hands you a
                // sign-in code to type elsewhere — right-click copy and paste
                // are worth more than a tidier menu.
                .with_disable_context_menu(false),
        )
        .launch(ui::App);
}
