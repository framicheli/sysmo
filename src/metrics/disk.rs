use std::io::Cursor;
use std::path::Path;
use std::process::Command;

use sysinfo::{DiskRefreshKind, Disks};

#[derive(Clone, Debug)]
pub struct DiskHealth {
    /// One line, e.g. "SMART Verified · 8% wear · 2560 h powered on".
    pub summary: String,
    pub ok: bool,
}

pub fn watch() -> Disks {
    Disks::new_with_refreshed_list_specifics(storage_only())
}

/// (total, available) bytes for the volume mounted at "/". Other mounts are
/// deliberately not shown.
///
/// ponytail: sysinfo counts purgeable space as available, like Finder does,
/// so this reads ~100 GB roomier than `df`. Matching `df` exactly would mean
/// an FFI `statfs` binding for a number macOS itself does not show.
pub fn root_usage(disks: &mut Disks) -> (u64, u64) {
    disks.refresh_specifics(false, storage_only());
    disks
        .list()
        .iter()
        .find(|d| d.mount_point() == Path::new("/"))
        .map_or((0, 0), |d| (d.total_space(), d.available_space()))
}

fn storage_only() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage()
}

/// SMART health of the boot disk, via `diskutil`. No public API exposes this,
/// and the values barely move, so it is read once at startup, not per tick.
pub fn health() -> Option<DiskHealth> {
    let out = Command::new("diskutil")
        .args(["info", "-plist", "/"])
        .output()
        .ok()?;
    let value = plist::Value::from_reader(Cursor::new(out.stdout)).ok()?;
    Some(summarize(value.as_dictionary()?))
}

fn summarize(info: &plist::Dictionary) -> DiskHealth {
    let status = info
        .get("SMARTStatus")
        .and_then(plist::Value::as_string)
        .unwrap_or("Unknown");
    let smart = info
        .get("SMARTDeviceSpecificKeysMayVaryNotGuaranteed")
        .and_then(plist::Value::as_dictionary);
    // ponytail: NVMe counters are split into _0 (low) / _1 (high) words; the
    // high word only matters past 2^64 units, so only _0 is read.
    let counter = |key: &str| {
        smart
            .and_then(|d| d.get(key))
            .and_then(plist::Value::as_signed_integer)
    };

    let mut parts = vec![format!("SMART {status}")];
    if let Some(used) = counter("PERCENTAGE_USED") {
        parts.push(format!("{used}% wear"));
    }
    if let Some(spare) = counter("AVAILABLE_SPARE") {
        parts.push(format!("{spare}% spare"));
    }
    if let Some(hours) = counter("POWER_ON_HOURS_0") {
        parts.push(format!("{hours} h powered on"));
    }
    if let Some(errors) = counter("MEDIA_ERRORS_0") {
        parts.push(format!("{errors} media errors"));
    }
    if let Some(shutdowns) = counter("UNSAFE_SHUTDOWNS_0") {
        parts.push(format!("{shutdowns} unsafe shutdowns"));
    }

    DiskHealth {
        // "Not Supported" (external/virtual disks) is not a failure, but it is
        // not a clean bill of health either — only Verified counts as ok.
        ok: status == "Verified",
        summary: parts.join(" · "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, plist::Value)>) -> plist::Dictionary {
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }

    #[test]
    fn summarize_full_nvme_info() {
        let smart = dict(vec![
            ("PERCENTAGE_USED", 8.into()),
            ("AVAILABLE_SPARE", 100.into()),
            ("POWER_ON_HOURS_0", 2560.into()),
            ("MEDIA_ERRORS_0", 0.into()),
            ("UNSAFE_SHUTDOWNS_0", 12.into()),
        ]);
        let info = dict(vec![
            ("SMARTStatus", "Verified".into()),
            (
                "SMARTDeviceSpecificKeysMayVaryNotGuaranteed",
                plist::Value::Dictionary(smart),
            ),
        ]);
        let health = summarize(&info);
        assert!(health.ok);
        assert_eq!(
            health.summary,
            "SMART Verified · 8% wear · 100% spare · 2560 h powered on · 0 media errors · 12 unsafe shutdowns"
        );
    }

    #[test]
    fn summarize_without_smart_details() {
        let health = summarize(&dict(vec![("SMARTStatus", "Not Supported".into())]));
        assert!(!health.ok);
        assert_eq!(health.summary, "SMART Not Supported");
    }

    #[test]
    fn summarize_empty_info() {
        let health = summarize(&plist::Dictionary::new());
        assert!(!health.ok);
        assert_eq!(health.summary, "SMART Unknown");
    }
}
