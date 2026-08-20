use std::io::Cursor;
use std::process::Command;
use std::time::{Duration, Instant};

/// ponytail: battery state moves in minutes, not seconds, so the `ioreg` fork
/// runs on this interval instead of every tick.
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Below this fraction of design capacity macOS starts recommending service.
const HEALTHY_CAPACITY_PCT: f32 = 80.0;

#[derive(Clone, Debug)]
pub struct Battery {
    pub charge_pct: f32,
    pub state: &'static str,
    pub cycles: i64,
    pub design_cycles: Option<i64>,
    /// Remaining full-charge capacity as a percentage of the design capacity.
    pub health_pct: Option<f32>,
    pub good: bool,
}

impl Battery {
    pub fn health_label(&self) -> String {
        let verdict = if self.good { "Good" } else { "Bad" };
        let capacity = self
            .health_pct
            .map_or_else(String::new, |pct| format!(" {pct:.0}%"));
        let cycles = match self.design_cycles {
            Some(design) => format!("{}/{design} cycles", self.cycles),
            None => format!("{} cycles", self.cycles),
        };
        format!("health {verdict}{capacity} · {cycles}")
    }
}

pub struct BatteryWatch {
    cached: Option<Battery>,
    next_read: Instant,
}

impl BatteryWatch {
    pub fn new() -> Self {
        Self {
            cached: None,
            next_read: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> Option<Battery> {
        let now = Instant::now();
        if now >= self.next_read {
            self.cached = read();
            self.next_read = now + REFRESH_INTERVAL;
        }
        self.cached.clone()
    }
}

/// `AppleSmartBattery` holds charge, cycles and health in one `IORegistry`
/// entry; `-a` prints it as a plist, which is already a dependency here.
fn read() -> Option<Battery> {
    let out = Command::new("ioreg")
        .args(["-arc", "AppleSmartBattery"])
        .output()
        .ok()?;
    let value = plist::Value::from_reader(Cursor::new(out.stdout)).ok()?;
    // Desktop Macs match nothing, so the array is empty.
    let entry = value.as_array()?.first()?.as_dictionary()?;
    derive(entry)
}

fn derive(entry: &plist::Dictionary) -> Option<Battery> {
    let number = |key: &str| entry.get(key).and_then(plist::Value::as_signed_integer);
    let flag = |key: &str| entry.get(key).and_then(plist::Value::as_boolean);
    if flag("BatteryInstalled") == Some(false) {
        return None;
    }

    // Apple silicon reports MaxCapacity as 100 (a percentage); Intel reports
    // mAh. The ratio is the charge level on both.
    let current = number("CurrentCapacity")?;
    let max = number("MaxCapacity").filter(|max| *max > 0)?;
    let design = number("DesignCapacity").filter(|design| *design > 0);
    let health_pct = number("NominalChargeCapacity")
        .zip(design)
        .map(|(nominal, design)| 100.0 * nominal as f32 / design as f32);

    Some(Battery {
        charge_pct: (100.0 * current as f32 / max as f32).clamp(0.0, 100.0),
        state: if flag("IsCharging") == Some(true) {
            "charging"
        } else if flag("ExternalConnected") == Some(true) {
            "plugged in"
        } else {
            "on battery"
        },
        cycles: number("CycleCount").unwrap_or(0),
        design_cycles: number("DesignCycleCount9C").filter(|count| *count > 0),
        health_pct,
        // ponytail: cycle count alone is not a failure — Apple rates batteries
        // to hold 80% *at* their design cycle count — so only a reported
        // failure or worn-down capacity counts as bad.
        good: number("PermanentFailureStatus").unwrap_or(0) == 0
            && health_pct.is_none_or(|pct| pct >= HEALTHY_CAPACITY_PCT),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pairs: Vec<(&str, plist::Value)>) -> plist::Dictionary {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn laptop() -> Vec<(&'static str, plist::Value)> {
        vec![
            ("BatteryInstalled", true.into()),
            ("CurrentCapacity", 50.into()),
            ("MaxCapacity", 100.into()),
            ("DesignCapacity", 8694.into()),
            ("NominalChargeCapacity", 7639.into()),
            ("CycleCount", 940.into()),
            ("DesignCycleCount9C", 1000.into()),
            ("PermanentFailureStatus", 0.into()),
            ("IsCharging", false.into()),
            ("ExternalConnected", false.into()),
        ]
    }

    #[test]
    fn healthy_laptop_battery() {
        let b = derive(&entry(laptop())).unwrap();
        assert!((b.charge_pct - 50.0).abs() < f32::EPSILON);
        assert_eq!(b.state, "on battery");
        assert!(b.good);
        assert_eq!(b.health_label(), "health Good 88% · 940/1000 cycles");
    }

    #[test]
    fn worn_capacity_is_bad() {
        let mut pairs = laptop();
        pairs.push(("NominalChargeCapacity", 6000.into())); // 69% of design
        let b = derive(&entry(pairs)).unwrap();
        assert!(!b.good);
        assert_eq!(b.health_label(), "health Bad 69% · 940/1000 cycles");
    }

    #[test]
    fn permanent_failure_is_bad_despite_full_capacity() {
        let mut pairs = laptop();
        pairs.push(("PermanentFailureStatus", 1.into()));
        assert!(!derive(&entry(pairs)).unwrap().good);
    }

    #[test]
    fn intel_style_milliamp_hours_give_a_percentage() {
        let b = derive(&entry(vec![
            ("CurrentCapacity", 3000.into()),
            ("MaxCapacity", 5000.into()),
            ("IsCharging", true.into()),
            ("ExternalConnected", true.into()),
        ]))
        .unwrap();
        assert!((b.charge_pct - 60.0).abs() < f32::EPSILON);
        assert_eq!(b.state, "charging");
        assert!(b.good); // no capacity data: nothing says it is failing
        assert_eq!(b.health_label(), "health Good · 0 cycles");
    }

    #[test]
    fn no_battery_installed() {
        let mut pairs = laptop();
        pairs.push(("BatteryInstalled", false.into()));
        assert!(derive(&entry(pairs)).is_none());
        assert!(derive(&plist::Dictionary::new()).is_none());
    }
}
