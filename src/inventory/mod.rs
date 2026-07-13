mod apps;
mod brew;
mod tools;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    App,
    Brew,
    Cask,
    Tool,
    Language,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::App => "App",
            Source::Brew => "Brew",
            Source::Cask => "Cask",
            Source::Tool => "Tool",
            Source::Language => "Language",
        }
    }
}

#[derive(Clone, Debug)]
pub struct InventoryItem {
    pub name: String,
    pub version: Option<String>,
    pub source: Source,
    pub path: Option<PathBuf>,
}

pub enum ScanEvent {
    Item(InventoryItem),
    CategoryDone(Source),
    AllDone { duration: Duration },
}

/// One-shot scan on a detached background thread; events stream to the
/// returned receiver. Fast filesystem scans run first so the UI fills
/// immediately; version probes stream in behind.
pub fn start_scan() -> Receiver<ScanEvent> {
    let (tx, rx) = channel();
    std::thread::spawn(move || run(&tx));
    rx
}

fn run(tx: &Sender<ScanEvent>) {
    let start = Instant::now();
    apps::scan(tx);
    let _ = tx.send(ScanEvent::CategoryDone(Source::App));
    brew::scan(tx);
    let _ = tx.send(ScanEvent::CategoryDone(Source::Brew));
    let _ = tx.send(ScanEvent::CategoryDone(Source::Cask));
    tools::scan(tx);
    let _ = tx.send(ScanEvent::CategoryDone(Source::Tool));
    let _ = tx.send(ScanEvent::CategoryDone(Source::Language));
    let _ = tx.send(ScanEvent::AllDone {
        duration: start.elapsed(),
    });
}

#[cfg(test)]
mod smoke {
    use super::*;

    /// Real-system smoke check: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn full_scan_completes() {
        let rx = start_scan();
        let mut items = 0;
        for event in rx {
            match event {
                ScanEvent::Item(_) => items += 1,
                ScanEvent::CategoryDone(_) => {}
                ScanEvent::AllDone { duration } => {
                    println!("scanned {items} items in {duration:?}");
                    return;
                }
            }
        }
        panic!("channel closed without AllDone");
    }
}
