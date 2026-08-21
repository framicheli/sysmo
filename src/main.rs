// Pedantic clippy is on (see Cargo.toml). These three fire on nearly every
// conversion this program makes — byte counts and sensor readings turned into
// floats for display, then back into terminal cells — where the lost bits are
// far below one pixel of a gauge.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod app;
mod inventory;
mod metrics;
mod ui;

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use crate::app::App;
use crate::metrics::{Collector, Update};

const TICK_RATE: Duration = Duration::from_secs(1);

fn main() -> std::io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("sysmo {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help" | "-h") => {
            println!(
                "sysmo {}\nA terminal system monitor and software inventory for macOS.\n\nUsage: sysmo [--version] [--help]",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        _ => {}
    }

    let collector = Collector::spawn();
    // ratatui::init installs the terminal-restoring panic hook and enters
    // raw mode + alternate screen; restore runs on every exit path of run().
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &collector);
    ratatui::restore();
    collector.shutdown();
    result
}

fn run(terminal: &mut DefaultTerminal, collector: &Collector) -> std::io::Result<()> {
    let mut app = App::default();
    let mut scan_events: Option<std::sync::mpsc::Receiver<inventory::ScanEvent>> = None;
    let mut optional_started = false;
    let mut last_tick = Instant::now();
    while !app.should_quit {
        if let Some(events) = &scan_events {
            while let Ok(event) = events.try_recv() {
                app.apply_scan_event(event);
            }
        }
        if app.take_rescan() {
            scan_events = Some(inventory::start_scan());
        }
        while let Ok(update) = collector.updates.try_recv() {
            match update {
                Update::Metrics(m) => {
                    app.store_metrics(*m);
                }
                Update::KillResult { pid, error: None } => app.set_status(format!("killed {pid}")),
                Update::KillResult {
                    pid,
                    error: Some(e),
                } => app.set_status(format!("kill {pid} failed: {e}")),
            }
        }
        if let Some(pid) = app.pending_kill.take() {
            collector.kill(pid);
        }

        terminal.draw(|frame| ui::render(frame, &app))?;
        if !optional_started && app.metrics.is_some() {
            scan_events = Some(inventory::start_scan());
            optional_started = true;
        }
        let mut timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        // Keep the first useful frame responsive; normal rendering remains event-driven.
        if app.metrics.is_none() {
            timeout = timeout.min(Duration::from_millis(16));
        } else if app.scanning {
            timeout = timeout.min(Duration::from_millis(100));
        }
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }
        if last_tick.elapsed() >= TICK_RATE {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
    Ok(())
}
