use std::path::Path;
use std::sync::mpsc::Sender;

use super::{InventoryItem, ScanEvent, Source};

pub fn scan(tx: &Sender<ScanEvent>) {
    // /usr/local covers Intel-era Homebrew leftovers.
    for prefix in ["/opt/homebrew", "/usr/local"] {
        scan_versions_dir(&Path::new(prefix).join("Cellar"), Source::Brew, tx);
        scan_versions_dir(&Path::new(prefix).join("Caskroom"), Source::Cask, tx);
    }
}

/// Layout is <dir>/<name>/<version>/. A missing dir means Homebrew isn't
/// installed there — silently fine.
fn scan_versions_dir(dir: &Path, source: Source, tx: &Sender<ScanEvent>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with('.') || !path.is_dir() {
            continue;
        }
        let _ = tx.send(ScanEvent::Item(InventoryItem {
            name,
            version: latest_version(&path),
            source,
            path: Some(path),
        }));
    }
}

/// Lexically greatest version subdirectory.
fn latest_version(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_lexically_greatest_version() {
        let base = std::env::temp_dir().join(format!("msm-brew-test-{}", std::process::id()));
        let formula = base.join("foo");
        std::fs::create_dir_all(formula.join("1.2.0")).unwrap();
        std::fs::create_dir_all(formula.join("1.10.0")).unwrap();
        std::fs::write(formula.join("INSTALL_RECEIPT.json"), b"{}").unwrap();

        // lexical, per spec: "1.2.0" > "1.10.0"
        assert_eq!(latest_version(&formula), Some("1.2.0".to_string()));

        let empty = base.join("bar");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(latest_version(&empty), None);
        assert_eq!(latest_version(&base.join("missing")), None);

        std::fs::remove_dir_all(&base).unwrap();
    }
}
