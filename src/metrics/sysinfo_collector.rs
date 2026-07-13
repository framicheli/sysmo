use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, Pid, ProcessesToUpdate, System};

use super::{Command, Metrics, ProcessInfo, Update, ffi::FfiSample};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

pub fn run(
    commands: &Receiver<Command>,
    updates: &Sender<Update>,
    ffi_samples: &Receiver<FfiSample>,
) {
    let mut sys = System::new();
    // Show memory and processes immediately; CPU percentages settle after the
    // short baseline interval below.
    sys.refresh_cpu_usage();
    let mut next_tick = Instant::now();
    let mut first = true;

    loop {
        // Sleep until next tick, but service commands (and shutdown) immediately.
        loop {
            let now = Instant::now();
            if now >= next_tick {
                break;
            }
            match commands.recv_timeout(next_tick - now) {
                Ok(Command::Kill(pid)) => {
                    let error = kill(&sys, pid);
                    if updates.send(Update::KillResult { pid, error }).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut metrics = collect(&sys);
        if let Some(sample) = ffi_samples.try_iter().last() {
            sample.merge_into(&mut metrics);
        }
        if updates.send(Update::Metrics(Box::new(metrics))).is_err() {
            return;
        }
        next_tick += if first {
            first = false;
            MINIMUM_CPU_UPDATE_INTERVAL
        } else {
            REFRESH_INTERVAL
        };
    }
}

fn collect(sys: &System) -> Metrics {
    let load = System::load_average();
    Metrics {
        per_core_cpu: sys.cpus().iter().map(sysinfo::Cpu::cpu_usage).collect(),
        global_cpu: sys.global_cpu_usage(),
        mem_total: sys.total_memory(),
        mem_used: sys.used_memory(),
        swap_total: sys.total_swap(),
        swap_used: sys.used_swap(),
        load_avg: (load.one, load.five, load.fifteen),
        processes: sys
            .processes()
            .iter()
            .map(|(pid, p)| ProcessInfo {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage(),
                mem: p.memory(),
            })
            .collect(),
        timestamp: Instant::now(),
        cpu_power_w: None,
        gpu_power_w: None,
        ane_power_w: None,
        ecluster_freq_mhz: None,
        pcluster_freq_mhz: None,
        gpu_util_pct: None,
        temps: None,
        fans: None,
    }
}

fn kill(sys: &System, pid: u32) -> Option<String> {
    match sys.process(Pid::from_u32(pid)) {
        // kill() only reports success/failure; on macOS a failed SIGKILL on a
        // live process is effectively always EPERM.
        Some(p) if p.kill() => None,
        Some(_) => Some("permission denied".into()),
        None => Some("no such process".into()),
    }
}
