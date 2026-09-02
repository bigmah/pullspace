# pullspace

An IDE-style code reviewer written entirely in Rust and compiled to WASM

These days reading + approving pull requests is the bottleneck. The stock
diff viewer on github leaves alot to be desired, especially when compared
to the ergonomics of a local editor. 

Of course one can just checkout a particular PR's branch, and view in their
local editor, but this is often cumbersome due to having local changes staged. 

The path of least resistance today ends up looking like a million browser tabs
open, one for each specific thing we want to investigate. The browser makes 
sense as the place to review things in this way, but we pay the price with 
ergonomics and a cluttered brower tab list.

pullspace aims to be a solution to this. A better browser based code review UX.

![pullspace showing a pull request diff side-by-side](diff.png)

Features:

* Open many reviews/sessions in a single browser tab
* IDE-style code review features (file tree, goto def, view refs, search, etc)
* Native HTML rendering 
* No sign-in needed (PAT required for private repos)
* Source code never leaves the browser. Like a local IDE but compiled to WASM. 

Upcoming:

* support writes (PR approval, comments, etc)
* better support as a browser exension (one click on a PR to open pullspace)


## Running it
```sh
cargo install dioxus-cli --version 0.6.3 --locked   # once, for `dx`

dx build --platform web --release
```

