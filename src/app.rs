use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use crate::inventory::{InventoryItem, ScanEvent, Source};
use crate::metrics::{Metrics, ProcessInfo};

const STATUS_TTL: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Monitor,
    Inventory,
}

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Monitor => "Monitor",
            Tab::Inventory => "Inventory",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortColumn {
    #[default]
    Cpu,
    Mem,
}

/// One flag per toggle the keymap exposes; grouping them into sub-structs
/// would only add a path to type in front of each one.
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    pub active_tab: Tab,
    pub should_quit: bool,
    pub tick: u64,
    pub metrics: Option<Metrics>,
    pub paused: bool,
    pub sort_col: SortColumn,
    pub sort_desc: bool,
    pub selected: usize,
    pub pending_kill: Option<u32>,
    status: Option<(String, Instant)>,
    pub inventory: Vec<InventoryItem>,
    pub scanning: bool,
    pub scan_duration: Option<Duration>,
    pub inv_selected: usize,
    pub inv_filter: String,
    pub inv_filter_mode: bool,
    pub inv_source_filter: Option<Source>,
    pub pending_rescan: bool,
}

impl Default for App {
    fn default() -> Self {
        App {
            active_tab: Tab::default(),
            should_quit: false,
            tick: 0,
            metrics: None,
            paused: false,
            sort_col: SortColumn::Cpu,
            sort_desc: true,
            selected: 0,
            pending_kill: None,
            status: None,
            inventory: Vec::new(),
            scanning: true, // the startup scan is already running
            scan_duration: None,
            inv_selected: 0,
            inv_filter: String::new(),
            inv_filter_mode: false,
            inv_source_filter: None,
            pending_rescan: false,
        }
    }
}

impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        // Filter mode swallows everything, so typed chars never hit global keys.
        if self.active_tab == Tab::Inventory && self.inv_filter_mode {
            match key.code {
                KeyCode::Esc => {
                    self.inv_filter.clear();
                    self.inv_filter_mode = false;
                }
                KeyCode::Enter => self.inv_filter_mode = false,
                KeyCode::Backspace => {
                    self.inv_filter.pop();
                    self.inv_selected = 0;
                }
                KeyCode::Char(c) => {
                    self.inv_filter.push(c);
                    self.inv_selected = 0;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            // ponytail: with exactly two tabs, next and prev are both a toggle
            KeyCode::Tab | KeyCode::Right | KeyCode::BackTab | KeyCode::Left => self.toggle_tab(),
            KeyCode::Char('1') => self.active_tab = Tab::Monitor,
            KeyCode::Char('2') => self.active_tab = Tab::Inventory,
            KeyCode::Char('p') => self.paused = !self.paused,
            code if self.active_tab == Tab::Monitor => self.on_monitor_key(code),
            code if self.active_tab == Tab::Inventory => self.on_inventory_key(code),
            _ => {}
        }
    }

    fn on_inventory_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.inv_selected = self.inv_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.inv_selected =
                    (self.inv_selected + 1).min(self.filtered_inventory().len().saturating_sub(1));
            }
            KeyCode::Char('/') => self.inv_filter_mode = true,
            KeyCode::Char('s') => {
                self.inv_source_filter = cycle_source(self.inv_source_filter);
                self.inv_selected = 0;
            }
            // r during a scan is ignored: no overlapping scans.
            KeyCode::Char('r') if !self.scanning => self.pending_rescan = true,
            _ => {}
        }
    }

    fn on_monitor_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('c') => self.toggle_sort(SortColumn::Cpu),
            KeyCode::Char('m') => self.toggle_sort(SortColumn::Mem),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.process_count().saturating_sub(1));
            }
            KeyCode::Char('x') => {
                self.pending_kill = self.sorted_processes().get(self.selected).map(|p| p.pid);
            }
            _ => {}
        }
    }

    fn toggle_sort(&mut self, col: SortColumn) {
        if self.sort_col == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_col = col;
            self.sort_desc = true;
        }
    }

    pub fn on_tick(&mut self) {
        self.tick += 1;
    }

    pub fn store_metrics(&mut self, metrics: Metrics) {
        if !self.paused {
            self.metrics = Some(metrics);
            self.selected = self.selected.min(self.process_count().saturating_sub(1));
        }
    }

    pub fn set_status(&mut self, message: String) {
        self.status = Some((message, Instant::now()));
    }

    pub fn status(&self) -> Option<&str> {
        match &self.status {
            Some((msg, at)) if at.elapsed() < STATUS_TTL => Some(msg),
            _ => None,
        }
    }

    pub fn sorted_processes(&self) -> Vec<ProcessInfo> {
        let mut procs = self
            .metrics
            .as_ref()
            .map(|m| m.processes.clone())
            .unwrap_or_default();
        sort_processes(&mut procs, self.sort_col, self.sort_desc);
        procs
    }

    fn process_count(&self) -> usize {
        self.metrics.as_ref().map_or(0, |m| m.processes.len())
    }

    pub fn apply_scan_event(&mut self, event: ScanEvent) {
        match event {
            ScanEvent::Item(item) => self.inventory.push(item),
            ScanEvent::CategoryDone(source) => {
                self.set_status(format!("{} scan done", source.label()));
            }
            ScanEvent::AllDone { duration } => {
                self.scanning = false;
                self.scan_duration = Some(duration);
            }
        }
    }

    /// True when the main loop should start a fresh scan.
    pub fn take_rescan(&mut self) -> bool {
        if !self.pending_rescan {
            return false;
        }
        self.pending_rescan = false;
        self.inventory.clear();
        self.scan_duration = None;
        self.inv_selected = 0;
        self.scanning = true;
        true
    }

    /// Source-filtered, name-filtered, alphabetical (stable).
    pub fn filtered_inventory(&self) -> Vec<InventoryItem> {
        let needle = self.inv_filter.to_lowercase();
        let mut items: Vec<InventoryItem> = self
            .inventory
            .iter()
            .filter(|i| inventory_matches(i, self.inv_source_filter, &needle))
            .cloned()
            .collect();
        items.sort_by_key(|i| i.name.to_lowercase());
        items
    }

    fn toggle_tab(&mut self) {
        self.active_tab = match self.active_tab {
            Tab::Monitor => Tab::Inventory,
            Tab::Inventory => Tab::Monitor,
        };
    }
}

pub fn inventory_matches(item: &InventoryItem, source: Option<Source>, needle_lower: &str) -> bool {
    source.is_none_or(|s| item.source == s)
        && (needle_lower.is_empty() || item.name.to_lowercase().contains(needle_lower))
}

fn cycle_source(current: Option<Source>) -> Option<Source> {
    match current {
        None => Some(Source::App),
        Some(Source::App) => Some(Source::Brew),
        Some(Source::Brew) => Some(Source::Cask),
        Some(Source::Cask) => Some(Source::Tool),
        Some(Source::Tool) => Some(Source::Language),
        Some(Source::Language) => None,
    }
}

pub fn sort_processes(procs: &mut [ProcessInfo], col: SortColumn, desc: bool) {
    procs.sort_by(|a, b| {
        let ord = match col {
            SortColumn::Cpu => a.cpu.total_cmp(&b.cpu),
            SortColumn::Mem => a.mem.cmp(&b.mem),
        };
        if desc { ord.reverse() } else { ord }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn proc(pid: u32, cpu: f32, mem: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: format!("p{pid}"),
            cpu,
            mem,
        }
    }

    #[test]
    fn keys_drive_state() {
        let mut app = App::default();
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.active_tab, Tab::Inventory);
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.active_tab, Tab::Monitor);
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.active_tab, Tab::Inventory);
        app.on_key(key(KeyCode::Char('1')));
        assert_eq!(app.active_tab, Tab::Monitor);
        app.on_key(key(KeyCode::Char('p')));
        assert!(app.paused);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn sort_by_cpu_and_mem_with_toggle() {
        let mut procs = vec![proc(1, 5.0, 300), proc(2, 50.0, 100), proc(3, 20.0, 200)];
        sort_processes(&mut procs, SortColumn::Cpu, true);
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), [2, 3, 1]);
        sort_processes(&mut procs, SortColumn::Cpu, false);
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), [1, 3, 2]);
        sort_processes(&mut procs, SortColumn::Mem, true);
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), [1, 3, 2]);
    }

    #[test]
    fn sort_key_toggles_direction() {
        let mut app = App::default();
        assert!(app.sort_desc);
        app.on_key(key(KeyCode::Char('c')));
        assert!(!app.sort_desc); // same column: flip
        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.sort_col, SortColumn::Mem);
        assert!(app.sort_desc); // new column: back to descending
    }

    fn item(name: &str, source: Source) -> InventoryItem {
        InventoryItem {
            name: name.to_string(),
            version: None,
            source,
            path: None,
        }
    }

    #[test]
    fn inventory_filter_matching() {
        let firefox = item("Firefox", Source::App);
        let ripgrep = item("ripgrep", Source::Brew);
        // case-insensitive name match
        assert!(inventory_matches(&firefox, None, "fire"));
        assert!(inventory_matches(
            &firefox,
            None,
            "FOX".to_lowercase().as_str()
        ));
        assert!(!inventory_matches(&firefox, None, "chrome"));
        // empty needle matches everything
        assert!(inventory_matches(&ripgrep, None, ""));
        // source filter
        assert!(inventory_matches(&firefox, Some(Source::App), "fire"));
        assert!(!inventory_matches(&firefox, Some(Source::Brew), "fire"));
        assert!(!inventory_matches(&ripgrep, Some(Source::App), ""));
    }

    #[test]
    fn filter_mode_captures_global_keys_and_rescan_guard_holds() {
        let mut app = App {
            active_tab: Tab::Inventory,
            scanning: false,
            ..App::default()
        };
        app.on_key(key(KeyCode::Char('/')));
        assert!(app.inv_filter_mode);
        for c in ['q', '1', 'p'] {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.inv_filter, "q1p");
        assert!(!app.should_quit && !app.paused);
        app.on_key(key(KeyCode::Esc));
        assert!(app.inv_filter.is_empty() && !app.inv_filter_mode);

        app.on_key(key(KeyCode::Char('r')));
        assert!(app.take_rescan());
        assert!(app.scanning);
        app.on_key(key(KeyCode::Char('r'))); // scan in flight: ignored
        assert!(!app.take_rescan());
    }

    #[test]
    fn sort_handles_nan_cpu_without_panic() {
        let mut procs = vec![proc(1, f32::NAN, 0), proc(2, 1.0, 0)];
        sort_processes(&mut procs, SortColumn::Cpu, true);
        assert_eq!(procs.len(), 2);
    }
}
