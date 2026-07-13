use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use super::{InventoryItem, ScanEvent, Source};

pub fn scan(tx: &Sender<ScanEvent>) {
    let mut dirs: Vec<PathBuf> = vec!["/Applications".into(), "/System/Applications".into()];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join("Applications"));
    }
    for dir in dirs {
        scan_dir(&dir, tx, true);
    }
}

/// Recurses one level only: some vendors nest (e.g. /Applications/Utilities).
fn scan_dir(dir: &Path, tx: &Sender<ScanEvent>, recurse: bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "app") {
            let _ = tx.send(ScanEvent::Item(app_item(&path)));
        } else if recurse && path.is_dir() {
            scan_dir(&path, tx, false);
        }
    }
}

// ponytail: a malformed Info.plist falls back to the bundle's folder name
// instead of feeding a separate scan-errors channel; the item still shows up,
// which is what the user actually cares about.
fn app_item(path: &Path) -> InventoryItem {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let info = plist::Value::from_file(path.join("Contents/Info.plist")).ok();
    let dict = info.as_ref().and_then(plist::Value::as_dictionary);
    let get = |key: &str| {
        dict.and_then(|d| d.get(key))
            .and_then(plist::Value::as_string)
            .map(str::to_string)
    };
    InventoryItem {
        name: get("CFBundleName").filter(|n| !n.is_empty()).unwrap_or(stem),
        version: get("CFBundleShortVersionString").or_else(|| get("CFBundleVersion")),
        source: Source::App,
        path: Some(path.to_path_buf()),
    }
}
