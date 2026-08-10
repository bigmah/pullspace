use dioxus::prelude::*;

use super::app::St;

#[component]
pub fn TopBar() -> Element {
    let st = use_context::<St>();
    let mut search_text = st.search_text;
    // Read (not peek) so the path re-renders after opening another repo.
    let root_str = st.root.read().to_string_lossy().into_owned();
    let index_label = match st.index.read().as_ref() {
        None => "indexing symbols…".to_string(),
        Some(idx) => format!("{} symbols", idx.len()),
    };

    rsx! {
        div { class: "topbar",
            span { class: "brand", "pullspace" }
            button {
                class: "iconbtn",
                title: "Open another repository…",
                onclick: move |_| async move {
                    let start = st.root_path();
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Open repository")
                        .set_directory(&start)
                        .pick_folder()
                        .await;
                    if let Some(dir) = picked {
                        st.open_repo(dir.path().to_path_buf());
                    }
                },
                "📂"
            }
            span { class: "repopath", title: "{root_str}", "{root_str}" }
            input {
                class: "searchbox",
                r#type: "text",
                placeholder: "Search in files…  (Enter)",
                spellcheck: "false",
                value: "{search_text}",
                oninput: move |e| search_text.set(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        st.do_search();
                    }
                },
            }
            span { class: "idxstate", "{index_label}" }
            button {
                class: "iconbtn",
                title: "Reload git status & file tree",
                onclick: move |_| st.refresh(),
                "⟳"
            }
        }
    }
}
