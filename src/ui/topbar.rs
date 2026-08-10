use dioxus::prelude::*;

use super::app::St;

#[component]
pub fn TopBar() -> Element {
    let st = use_context::<St>();
    let mut search_text = st.search_text;
    let root = st.root_path();
    let root_str = root.to_string_lossy().into_owned();
    let index_label = match st.index.read().as_ref() {
        None => "indexing symbols…".to_string(),
        Some(idx) => format!("{} symbols", idx.len()),
    };

    rsx! {
        div { class: "topbar",
            span { class: "brand", "pullspace" }
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
