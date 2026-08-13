mod backend;
mod ui;

/// pullspace, as a page.
///
/// There is nothing behind this. `dx build --platform web` leaves an
/// index.html, a .wasm and a .js in one directory; any static host will serve
/// them, and the app talks to api.github.com from the tab it is open in. That
/// is the whole deployment.
fn main() {
    dioxus::launch(ui::App);
}
