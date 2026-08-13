//! The Origin Private File System: a real filesystem, per origin, inside the
//! browser.
//!
//! It is what [`store`](super::store) is for settings, scaled up — same
//! per-origin isolation, no 5 MB ceiling, and bytes instead of strings. This is
//! where a repository's files are kept between visits, so opening a pull
//! request on a repository read last week costs almost nothing.
//!
//! Every operation here is best effort, for the same reason the settings store
//! is: a private window, a browser too old for OPFS, or a full disk each end at
//! the same place, and none of them are worth interrupting anyone over. A
//! `None` means "not cached", the caller goes to the network, and the app is
//! merely as slow as it was before there was a cache.
//!
//! The one deliberate omission is `createSyncAccessHandle`, which is faster and
//! only exists inside a worker — everything pullspace does runs on the UI
//! thread, and a worker to talk to would be a second copy of the app.

use js_sys::{IteratorNext, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions,
    FileSystemGetFileOptions, FileSystemRemoveOptions, FileSystemWritableFileStream,
};

/// Everything pullspace writes lives under one directory, so nothing else on
/// the origin is ever in reach of a sweep.
const ROOT: &str = "pullspace";

/// Await a JS promise, and treat a rejection as an absence.
///
/// Rejections here are ordinary: `getFileHandle` on a file that is not there
/// rejects with `NotFoundError`, which is a cache miss and not a fault.
async fn settle(promise: js_sys::Promise) -> Option<JsValue> {
    JsFuture::from(promise).await.ok()
}

/// The root of pullspace's own storage, created on first use.
///
/// `None` outside a browser — which is also what the native `cargo test` build
/// gets, since `window()` is `None` there and nothing below is ever reached.
pub async fn root() -> Option<FileSystemDirectoryHandle> {
    let storage = web_sys::window()?.navigator().storage();
    let handle: FileSystemDirectoryHandle = settle(storage.get_directory()).await?.dyn_into().ok()?;
    dir(&handle, ROOT).await
}

/// A subdirectory, created if it is not there yet.
pub async fn dir(parent: &FileSystemDirectoryHandle, name: &str) -> Option<FileSystemDirectoryHandle> {
    let opts = FileSystemGetDirectoryOptions::new();
    opts.set_create(true);
    settle(parent.get_directory_handle_with_options(name, &opts))
        .await?
        .dyn_into()
        .ok()
}

pub async fn read(parent: &FileSystemDirectoryHandle, name: &str) -> Option<Vec<u8>> {
    let handle: FileSystemFileHandle = settle(parent.get_file_handle(name)).await?.dyn_into().ok()?;
    let file: File = settle(handle.get_file()).await?.dyn_into().ok()?;
    let buffer = settle(file.array_buffer()).await?;
    Some(Uint8Array::new(&buffer).to_vec())
}

/// Write a file whole, replacing whatever was there.
///
/// `close` is what commits it: the writable stream is a staging copy until
/// then, so a tab shut mid-write leaves the old contents rather than half the
/// new ones.
///
/// The bytes are copied onto the JavaScript heap before the browser is handed
/// them, and that copy is not an oversight to be optimised away. Passing a
/// `&[u8]` straight through gives JavaScript a *view* into wasm's linear
/// memory, and `write` consumes what it is given some time after it is called —
/// by which point any allocation on this side may have grown that memory and
/// detached the view out from under it. What reaches the disk then is zeros, or
/// nothing, and a file of zeros reads back as a perfectly ordinary binary file.
pub async fn write(parent: &FileSystemDirectoryHandle, name: &str, bytes: &[u8]) -> bool {
    let opts = FileSystemGetFileOptions::new();
    opts.set_create(true);
    let Some(handle) = settle(parent.get_file_handle_with_options(name, &opts))
        .await
        .and_then(|h| h.dyn_into::<FileSystemFileHandle>().ok())
    else {
        return false;
    };
    let Some(stream) = settle(handle.create_writable())
        .await
        .and_then(|w| w.dyn_into::<FileSystemWritableFileStream>().ok())
    else {
        return false;
    };
    let owned = Uint8Array::new_with_length(bytes.len() as u32);
    owned.copy_from(bytes);
    let written = match stream.write_with_js_u8_array(&owned) {
        Ok(promise) => settle(promise).await.is_some(),
        Err(_) => false,
    };
    settle(stream.close()).await.is_some() && written
}

/// When a file was last written, as milliseconds since the epoch.
///
/// The store keeps no timestamps of its own: rewriting a repository's manifest
/// each time it is opened is what makes this the "last used" the sweep sorts
/// on.
pub async fn modified(parent: &FileSystemDirectoryHandle, name: &str) -> Option<f64> {
    let handle: FileSystemFileHandle = settle(parent.get_file_handle(name)).await?.dyn_into().ok()?;
    let file: File = settle(handle.get_file()).await?.dyn_into().ok()?;
    Some(file.last_modified())
}

/// The names in a directory. Empty for one that does not exist, which is the
/// same answer as one with nothing in it — and the same thing to a caller.
pub async fn list(parent: &FileSystemDirectoryHandle) -> Vec<String> {
    let mut names = Vec::new();
    let entries = parent.keys();
    loop {
        let Ok(promise) = entries.next() else { break };
        let Some(step) = settle(promise).await else { break };
        let step: IteratorNext = step.unchecked_into();
        if step.done() {
            break;
        }
        if let Some(name) = step.value().as_string() {
            names.push(name);
        }
    }
    names
}

pub async fn remove(parent: &FileSystemDirectoryHandle, name: &str) -> bool {
    settle(parent.remove_entry(name)).await.is_some()
}

/// Remove a directory and everything under it.
pub async fn remove_all(parent: &FileSystemDirectoryHandle, name: &str) -> bool {
    let opts = FileSystemRemoveOptions::new();
    opts.set_recursive(true);
    settle(parent.remove_entry_with_options(name, &opts))
        .await
        .is_some()
}

/// Bytes this origin is using, and what the browser will let it use.
///
/// Both come from `navigator.storage.estimate()`, which counts everything on
/// the origin rather than only what is below [`root`] — near enough, since
/// pullspace's other storage is a token and two pane widths.
pub async fn usage() -> Option<(f64, f64)> {
    let storage = web_sys::window()?.navigator().storage();
    let estimate = settle(storage.estimate().ok()?).await?;
    let read = |key: &str| {
        Reflect::get(&estimate, &JsValue::from_str(key))
            .ok()
            .and_then(|v| v.as_f64())
    };
    Some((read("usage")?, read("quota")?))
}

/// Ask the browser to keep this data when it is short of space.
///
/// Without it OPFS is evictable, and a browser under storage pressure may drop
/// the lot — which costs a re-clone rather than any data, so the answer is not
/// worth acting on. Chrome grants it on a site with any history of use;
/// Firefox asks; Safari decides for itself.
pub async fn persist() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Ok(promise) = window.navigator().storage().persist() else {
        return false;
    };
    settle(promise).await.and_then(|v| v.as_bool()).unwrap_or(false)
}
