//! The branch panel: which branch the working tree is on, and which two to
//! hold against each other.
//!
//! It only ever reads. Nothing here checks anything out, moves a ref or touches
//! the index — comparing two branches in pullspace leaves the repository
//! exactly as it was found, the same promise borrowing a clone for a pull
//! request makes.

use dioxus::prelude::*;

use crate::backend::branches::{self, Branch, Head};

use super::app::{St, Workspace};

/// What the compare button is doing, if anything.
#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    Working(String),
    Failed(String),
}

#[component]
pub fn BranchPanel() -> Element {
    let st = use_context::<St>();
    let mut open = st.branch_open;

    // Read once per opening: the panel is mounted when it is shown and dropped
    // when it is closed, so this is where "as of now" is settled. `⟳` bumps the
    // tick, which is how a branch created since gets into the list.
    let repo = use_memo(move || {
        st.refresh_tick.read();
        let root = st.root_path();
        (
            branches::head_of(&root),
            branches::list(&root),
            branches::default_pair(&root),
        )
    });
    let (head, all, pair) = repo();

    let open_compare = st.workspace.read().compare().cloned();
    // The pair already on screen is the one to come back to, so reopening the
    // panel while comparing does not offer to start something else.
    let (start_base, start_head) = match (&open_compare, pair) {
        (Some(view), _) => (view.base.clone(), view.head.clone()),
        (None, Some(pair)) => pair,
        (None, None) => (String::new(), String::new()),
    };
    let mut base = use_signal(|| start_base);
    let mut target = use_signal(|| start_head);
    let mut status = use_signal(|| Status::Idle);

    let names: Vec<Branch> = all;
    let working = matches!(*status.read(), Status::Working(_));

    let compare = move |_| {
        let from = base.peek().clone();
        let to = target.peek().clone();
        if from == to {
            status.set(Status::Failed(
                "Those are the same branch — pick two different ones.".to_string(),
            ));
            return;
        }
        status.set(Status::Working(format!("Comparing {from} … {to}…")));
        // This component's scope, deliberately: closing the panel mid-compare
        // drops the work, and nothing writes back into a panel that is gone.
        spawn(async move {
            let root = st.root_path();
            let done =
                tokio::task::spawn_blocking(move || branches::compare(&root, &from, &to)).await;
            match done {
                // Which closes the panel — the comparison *is* the answer.
                Ok(Ok(view)) => st.enter_compare(view),
                Ok(Err(e)) => status.set(Status::Failed(format!("{e:#}"))),
                Err(e) => status.set(Status::Failed(e.to_string())),
            }
        });
    };

    rsx! {
        div {
            class: "ghoverlay",
            onclick: move |_| open.set(false),
            tabindex: "-1",
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    open.set(false);
                }
            },
            div {
                class: "ghpanel",
                onclick: move |e| e.stop_propagation(),
                div { class: "ghhdr",
                    span { class: "ghtitle", "Branches" }
                    span { class: "spacer" }
                    button {
                        class: "iconbtn",
                        title: "Close",
                        onclick: move |_| open.set(false),
                        "✕"
                    }
                }
                div { class: "ghbody",
                    div { class: "ghsection",
                        div { class: "ghlabel", "Working tree" }
                        match head {
                            Some(Head::Branch(name)) => rsx! {
                                div { class: "branchnow",
                                    span { class: "branchglyph", "⎇" }
                                    span { class: "branchname", "{name}" }
                                    span { class: "ghnote", "is checked out" }
                                }
                            },
                            Some(Head::Detached(sha)) => rsx! {
                                div { class: "branchnow",
                                    span { class: "branchglyph", "⎇" }
                                    span { class: "branchname", "detached at {sha}" }
                                    span { class: "ghnote", "not on a branch" }
                                }
                            },
                            None => rsx! {
                                div { class: "ghnote", "This directory is not a git repository." }
                            },
                        }
                    }
                    if names.is_empty() {
                        div { class: "ghsection",
                            div { class: "ghnote",
                                "There is nothing to compare yet — a repository needs a commit on at least one branch."
                            }
                        }
                    } else {
                        div { class: "ghsection",
                            div { class: "ghlabel", "Compare" }
                            div { class: "ghrow branchrow",
                                BranchSelect { value: base, options: names.clone(), what: "base" }
                                button {
                                    class: "iconbtn",
                                    title: "Swap the two",
                                    onclick: move |_| {
                                        let (from, to) = (base.peek().clone(), target.peek().clone());
                                        base.set(to);
                                        target.set(from);
                                    },
                                    "⇄"
                                }
                                BranchSelect { value: target, options: names.clone(), what: "head" }
                                button {
                                    class: "primarybtn",
                                    disabled: working,
                                    onclick: compare,
                                    "Compare"
                                }
                            }
                            div { class: "ghhelp",
                                "Diffed against the "
                                b { "merge base" }
                                " of the two, so what you see is what the head branch adds — not everything that has landed on the base since it forked. Nothing is checked out: the working tree is left exactly as it is."
                            }
                        }
                    }
                    match &*status.read() {
                        Status::Working(note) => rsx! {
                            div { class: "ghsection", div { class: "ghnote", "{note}" } }
                        },
                        Status::Failed(e) => rsx! {
                            div { class: "ghsection", div { class: "gherror", "{e}" } }
                        },
                        Status::Idle => rsx! {},
                    }
                    if let Some(view) = open_compare {
                        div { class: "ghsection ghcache",
                            span { class: "ghnote",
                                "Showing {view.label()} — {view.files.len()} file"
                                if view.files.len() != 1 {
                                    "s"
                                }
                                " changed"
                            }
                            span { class: "spacer" }
                            button {
                                class: "linkbtn",
                                onclick: move |_| {
                                    open.set(false);
                                    st.back_to_worktree();
                                },
                                "Stop comparing"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One end of the comparison. A select rather than a list, because a
/// repository can have three branches or three hundred and this is the control
/// that reads the same either way.
#[component]
fn BranchSelect(value: Signal<String>, options: Vec<Branch>, what: String) -> Element {
    let mut value = value;
    let chosen = value.read().clone();
    // A comparison can name a branch that has since been deleted, or a commit
    // that was never a branch at all — it still has to be selectable, or the
    // control would silently change what is being compared.
    let missing = !chosen.is_empty() && !options.iter().any(|b| b.name == chosen);
    rsx! {
        // Both `value` here and `selected` below, and they are not redundant:
        // the property is what moves the selection when the swap button
        // rewrites it, and the attribute is what has it right on the first
        // paint — when the options may not be in the document yet for the
        // property to find.
        select {
            class: "branchsel",
            title: "The {what} of the comparison",
            value: "{chosen}",
            onchange: move |e| value.set(e.value()),
            if missing {
                option { value: "{chosen}", selected: true, "{chosen}" }
            }
            for b in options {
                option {
                    value: "{b.name}",
                    selected: b.name == chosen,
                    if b.current {
                        "{b.name}  ·  current"
                    } else {
                        "{b.name}"
                    }
                }
            }
        }
    }
}

/// The top bar's branch chip: what is checked out, or what is being compared.
#[component]
pub fn BranchChip() -> Element {
    let st = use_context::<St>();
    let mut open = st.branch_open;

    // Cheap — one `Repository::discover` and a ref read — and re-read on every
    // refresh, so switching branches outside the app and pressing `⟳` shows the
    // branch you are actually on.
    let head = use_memo(move || {
        st.refresh_tick.read();
        st.root.read();
        branches::head_of(&st.root_path())
    });

    let comparing = matches!(&*st.workspace.read(), Workspace::Compare(_));
    let Some(head) = head() else {
        return rsx! {};
    };
    let detached = matches!(head, Head::Detached(_));
    let label = head.label();
    let cls = if detached {
        "branchchip loose"
    } else {
        "branchchip"
    };
    let title = if comparing {
        format!("{label} is checked out · compare two branches")
    } else {
        format!("On {label} · click to compare two branches")
    };

    rsx! {
        button {
            class: "{cls}",
            title: "{title}",
            onclick: move |_| open.set(true),
            span { class: "branchglyph", "⎇" }
            span { class: "branchlabel", "{label}" }
        }
    }
}
