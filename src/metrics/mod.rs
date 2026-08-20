mod battery;
mod disk;
mod ffi;
mod network;
mod sysinfo_collector;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::Instant;

pub use battery::Battery;
pub use disk::DiskHealth;
pub use network::NetworkStatus;

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
}

#[derive(Clone, Debug)]
pub struct Metrics {
    pub per_core_cpu: Vec<f32>,
    pub global_cpu: f32,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub load_avg: (f64, f64, f64),
    pub disk_total: u64,
    pub disk_available: u64,
    pub disk_health: Option<DiskHealth>,
    pub battery: Option<Battery>,
    pub network: NetworkStatus,
    pub processes: Vec<ProcessInfo>,
    pub timestamp: Instant,
    pub cpu_power_w: Option<f32>,
    pub gpu_power_w: Option<f32>,
    pub ane_power_w: Option<f32>,
    pub ecluster_freq_mhz: Option<Vec<f32>>,
    pub pcluster_freq_mhz: Option<Vec<f32>>,
    pub gpu_util_pct: Option<f32>,
    pub temps: Option<Vec<(String, f32)>>,
    pub fans: Option<Vec<(String, f32)>>,
}

pub enum Command {
    Kill(u32),
}

pub enum Update {
    Metrics(Box<Metrics>),
    KillResult { pid: u32, error: Option<String> },
}

pub struct Collector {
    pub updates: Receiver<Update>,
    commands: Sender<Command>,
    handle: JoinHandle<()>,
    ffi_handle: JoinHandle<()>,
}

impl Collector {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let (upd_tx, upd_rx) = channel();
        let (ffi_tx, ffi_rx) = channel();
        // FFI init runs on its own thread, so starting it immediately costs
        // the first frame nothing and gets temps/GPU into the second tick.
        let ffi_handle = std::thread::spawn(move || ffi::run(&ffi_tx));
        let handle = std::thread::spawn(move || sysinfo_collector::run(&cmd_rx, &upd_tx, &ffi_rx));
        Collector {
            updates: upd_rx,
            commands: cmd_tx,
            handle,
            ffi_handle,
        }
    }

    pub fn kill(&self, pid: u32) {
        let _ = self.commands.send(Command::Kill(pid));
    }

    /// Dropping the command sender wakes the collector's `recv_timeout`
    /// immediately, so the join returns within one refresh (~ms), not a tick.
    pub fn shutdown(self) {
        drop(self.commands);
        let _ = self.handle.join();
        let _ = self.ffi_handle.join();
    }
}
