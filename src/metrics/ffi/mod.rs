// Binding approach derived from macmon (MIT): https://github.com/vladkens/macmon

#[cfg(target_os = "macos")]
mod gpu;
#[cfg(target_os = "macos")]
mod ioreport;
#[cfg(target_os = "macos")]
mod smc;

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::Metrics;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default)]
pub struct FfiSample {
    pub cpu_power_w: Option<f32>,
    pub gpu_power_w: Option<f32>,
    pub ane_power_w: Option<f32>,
    pub ecluster_freq_mhz: Option<Vec<f32>>,
    pub pcluster_freq_mhz: Option<Vec<f32>>,
    pub gpu_util_pct: Option<f32>,
    pub temps: Option<Vec<(String, f32)>>,
    pub fans: Option<Vec<(String, f32)>>,
}

impl FfiSample {
    pub fn merge_into(self, metrics: &mut Metrics) {
        metrics.cpu_power_w = self.cpu_power_w;
        metrics.gpu_power_w = self.gpu_power_w;
        metrics.ane_power_w = self.ane_power_w;
        metrics.ecluster_freq_mhz = self.ecluster_freq_mhz;
        metrics.pcluster_freq_mhz = self.pcluster_freq_mhz;
        metrics.gpu_util_pct = self.gpu_util_pct;
        metrics.temps = self.temps;
        metrics.fans = self.fans;
    }
}

pub struct FfiCollector {
    #[cfg(target_os = "macos")]
    ioreport: Option<ioreport::IoReport>,
    #[cfg(target_os = "macos")]
    gpu: Option<gpu::Gpu>,
    #[cfg(target_os = "macos")]
    smc: Option<smc::Smc>,
}

impl FfiCollector {
    pub fn new() -> Self {
        if std::env::var_os("SYSMO_NO_FFI").is_some() {
            return Self {
                #[cfg(target_os = "macos")]
                ioreport: None,
                #[cfg(target_os = "macos")]
                gpu: None,
                #[cfg(target_os = "macos")]
                smc: None,
            };
        }

        #[cfg(target_os = "macos")]
        let ioreport = ioreport::IoReport::new().ok();
        #[cfg(target_os = "macos")]
        let gpu = gpu::Gpu::new().ok();
        #[cfg(target_os = "macos")]
        let smc = smc::Smc::new().ok();

        Self {
            #[cfg(target_os = "macos")]
            ioreport,
            #[cfg(target_os = "macos")]
            gpu,
            #[cfg(target_os = "macos")]
            smc,
        }
    }

    pub fn poll(&mut self) -> FfiSample {
        let mut sample = FfiSample::default();
        #[cfg(target_os = "macos")]
        if let Some(ioreport) = &mut self.ioreport {
            ioreport.poll(&mut sample);
        }
        #[cfg(target_os = "macos")]
        if let Some(gpu) = &self.gpu {
            sample.gpu_util_pct = gpu.poll();
        }
        #[cfg(target_os = "macos")]
        if let Some(smc) = &mut self.smc {
            let (temperatures, fans) = smc.poll();
            sample.temps = Some(temperatures);
            sample.fans = Some(fans);
        }
        sample
    }
}

pub fn run(samples: &Sender<FfiSample>) {
    let mut collector = FfiCollector::new();
    let mut next_tick = Instant::now();
    loop {
        let now = Instant::now();
        if now < next_tick {
            std::thread::sleep(next_tick - now);
        }
        if samples.send(collector.poll()).is_err() {
            return;
        }
        next_tick += POLL_INTERVAL;
    }
}
