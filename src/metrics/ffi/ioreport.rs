// Binding approach derived from macmon (MIT): https://github.com/vladkens/macmon

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::time::Instant;

use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex};
use core_foundation::base::{CFAllocatorRef, CFRange, CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation::data::{CFDataGetBytes, CFDataGetLength, CFDataRef};
use core_foundation::dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryGetValue, CFDictionaryRef,
    CFMutableDictionaryRef,
};
use core_foundation::string::{
    CFStringCreateWithBytes, CFStringGetCString, CFStringRef, kCFStringEncodingUTF8,
};

use super::FfiSample;

type Result<T> = std::result::Result<T, String>;

#[repr(C)]
struct IOReportSubscription {
    _private: [u8; 0],
}

type IOReportSubscriptionRef = *const IOReportSubscription;

#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFStringRef,
        subgroup: CFStringRef,
        a: u64,
        b: u64,
        c: u64,
    ) -> CFDictionaryRef;
    fn IOReportMergeChannels(a: CFDictionaryRef, b: CFDictionaryRef, nil: CFTypeRef);
    fn IOReportCreateSubscription(
        a: *const c_void,
        channels: CFMutableDictionaryRef,
        subscribed: *mut CFMutableDictionaryRef,
        b: u64,
        c: CFTypeRef,
    ) -> IOReportSubscriptionRef;
    fn IOReportCreateSamples(
        subscription: IOReportSubscriptionRef,
        channels: CFMutableDictionaryRef,
        nil: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportCreateSamplesDelta(
        previous: CFDictionaryRef,
        current: CFDictionaryRef,
        nil: CFTypeRef,
    ) -> CFDictionaryRef;
    fn IOReportChannelGetGroup(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetChannelName(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(channel: CFDictionaryRef, index: i32) -> i64;
    fn IOReportStateGetCount(channel: CFDictionaryRef) -> i32;
    fn IOReportStateGetNameForIndex(channel: CFDictionaryRef, index: i32) -> CFStringRef;
    fn IOReportStateGetResidency(channel: CFDictionaryRef, index: i32) -> i64;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: CFDictionaryRef,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryGetName(entry: u32, name: *mut c_char) -> i32;
    fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut CFMutableDictionaryRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> i32;
    fn IOObjectRelease(object: u32) -> u32;
}

struct OwnedCf(*const c_void);

impl OwnedCf {
    fn new(value: *const c_void, what: &str) -> Result<Self> {
        if value.is_null() {
            Err(format!("{what} returned null"))
        } else {
            Ok(Self(value))
        }
    }

    fn borrowed(&self) -> BorrowedCf {
        BorrowedCf(self.0)
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: OwnedCf is only constructed for a non-null object returned at +1 ownership.
        unsafe { CFRelease(self.0) };
    }
}

#[derive(Clone, Copy)]
struct BorrowedCf(*const c_void);

struct IoObject(u32);

impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns a non-zero IOKit object returned by IOKit.
        unsafe { IOObjectRelease(self.0) };
    }
}

pub struct IoReport {
    source: SampleSource,
    cpu_source: Option<SampleSource>,
    e_freqs: Vec<f32>,
    p_freqs: Vec<f32>,
}

struct SampleSource {
    subscription: OwnedCf,
    channels: OwnedCf,
    previous: OwnedCf,
    previous_at: Instant,
}

impl IoReport {
    pub fn new() -> Result<Self> {
        let energy_group = cf_string("Energy Model")?;
        let cpu_group = cf_string("CPU Stats")?;
        let cpu_subgroup = cf_string("CPU Core Performance States")?;

        // SAFETY: the strings are live CF objects and reserved arguments are zero/null.
        let energy =
            unsafe { IOReportCopyChannelsInGroup(energy_group.0.cast(), ptr::null(), 0, 0, 0) };
        let energy = OwnedCf::new(energy.cast(), "Energy Model channels")?;
        // SAFETY: the strings are live CF objects and reserved arguments are zero.
        let cpu = unsafe {
            IOReportCopyChannelsInGroup(cpu_group.0.cast(), cpu_subgroup.0.cast(), 0, 0, 0)
        };
        let cpu = OwnedCf::new(cpu.cast(), "CPU Stats channels")?;
        // SAFETY: both dictionaries are live; IOReportMergeChannels mutates the first one.
        unsafe { IOReportMergeChannels(energy.0.cast(), cpu.0.cast(), ptr::null()) };

        // SAFETY: energy is a live dictionary; the returned mutable copy has +1 ownership.
        let channels = unsafe {
            CFDictionaryCreateMutableCopy(
                kCFAllocatorDefault,
                CFDictionaryGetCount(energy.0.cast()),
                energy.0.cast(),
            )
        };
        let channels = OwnedCf::new(channels.cast(), "channel dictionary copy")?;
        let (e_freqs, p_freqs) = cpu_frequency_tables()?;

        // The combined channel set is the fast path; chips that reject it get
        // energy and CPU stats as two separate subscriptions.
        if let Ok(source) = SampleSource::new(channels) {
            return Ok(Self {
                source,
                cpu_source: None,
                e_freqs,
                p_freqs,
            });
        }
        // SAFETY: the string is live and reserved arguments are zero/null.
        let energy =
            unsafe { IOReportCopyChannelsInGroup(energy_group.0.cast(), ptr::null(), 0, 0, 0) };
        let energy = OwnedCf::new(energy.cast(), "Energy Model channels")?;
        let energy = mutable_copy(energy.borrowed())?;
        let cpu = mutable_copy(cpu.borrowed())?;
        Ok(Self {
            source: SampleSource::new(energy)?,
            cpu_source: SampleSource::new(cpu).ok(),
            e_freqs,
            p_freqs,
        })
    }

    pub fn poll(&mut self, output: &mut FfiSample) {
        if let Some((delta, elapsed)) = self.source.delta() {
            read_delta(
                delta.borrowed(),
                elapsed,
                &self.e_freqs,
                &self.p_freqs,
                output,
            );
        }
        if let Some(source) = &mut self.cpu_source
            && let Some((delta, elapsed)) = source.delta()
        {
            read_delta(
                delta.borrowed(),
                elapsed,
                &self.e_freqs,
                &self.p_freqs,
                output,
            );
        }
    }
}

impl SampleSource {
    fn new(channels: OwnedCf) -> Result<Self> {
        let (subscription, channels) = create_subscription(channels)?;
        let previous = create_sample(subscription.borrowed(), channels.borrowed())?;
        Ok(Self {
            subscription,
            channels,
            previous,
            previous_at: Instant::now(),
        })
    }

    fn delta(&mut self) -> Option<(OwnedCf, f32)> {
        let current = create_sample(self.subscription.borrowed(), self.channels.borrowed()).ok()?;
        let elapsed = self.previous_at.elapsed().as_secs_f32().max(0.001);
        // SAFETY: both samples are live dictionaries from the same subscription.
        let delta = unsafe {
            IOReportCreateSamplesDelta(self.previous.0.cast(), current.0.cast(), ptr::null())
        };
        let delta = OwnedCf::new(delta.cast(), "IOReport delta").ok()?;
        self.previous = current;
        self.previous_at = Instant::now();
        Some((delta, elapsed))
    }
}

fn mutable_copy(source: BorrowedCf) -> Result<OwnedCf> {
    // SAFETY: source is a live dictionary; the returned mutable copy has +1 ownership.
    let channels = unsafe {
        CFDictionaryCreateMutableCopy(
            kCFAllocatorDefault,
            CFDictionaryGetCount(source.0.cast()),
            source.0.cast(),
        )
    };
    OwnedCf::new(channels.cast(), "channel dictionary copy")
}

fn create_subscription(channels: OwnedCf) -> Result<(OwnedCf, OwnedCf)> {
    let mut subscribed: CFMutableDictionaryRef = ptr::null_mut();
    // SAFETY: channels is live, subscribed points to writable storage, and reserved values
    // follow IOReport's contract.
    let subscription = unsafe {
        IOReportCreateSubscription(
            ptr::null(),
            channels.0.cast_mut().cast(),
            &raw mut subscribed,
            0,
            ptr::null(),
        )
    };
    let subscription = OwnedCf::new(subscription.cast(), "IOReport subscription")?;
    if !subscribed.is_null() {
        let _subscribed = OwnedCf::new(subscribed.cast(), "subscribed channel dictionary")?;
    }
    Ok((subscription, channels))
}

fn create_sample(subscription: BorrowedCf, channels: BorrowedCf) -> Result<OwnedCf> {
    // SAFETY: both handles remain live for the call and originate from one subscription.
    let sample = unsafe {
        IOReportCreateSamples(
            subscription.0.cast(),
            channels.0.cast_mut().cast(),
            ptr::null(),
        )
    };
    OwnedCf::new(sample.cast(), "IOReport sample")
}

fn read_delta(
    delta: BorrowedCf,
    elapsed: f32,
    e_freqs: &[f32],
    p_freqs: &[f32],
    output: &mut FfiSample,
) {
    let Some(channels) = dictionary_value(delta, "IOReportChannels") else {
        return;
    };
    // SAFETY: IOReportChannels is an array owned by the live delta dictionary.
    let count = unsafe { CFArrayGetCount(channels.0.cast()) };
    // None until a matching channel shows up: a chip without an ANE must read
    // as unavailable, not as a flat 0 W.
    let mut cpu_power: Option<f32> = None;
    let mut gpu_power: Option<f32> = None;
    let mut ane_power: Option<f32> = None;
    let accumulate = |slot: &mut Option<f32>, watts: f32| {
        *slot = Some(slot.unwrap_or(0.0) + watts);
    };
    let mut e_by_die: BTreeMap<usize, Vec<f32>> = BTreeMap::new();
    let mut p_by_die: BTreeMap<usize, Vec<f32>> = BTreeMap::new();

    for index in 0..count {
        // SAFETY: index is in bounds and the array remains owned by delta for this loop.
        let channel = unsafe { CFArrayGetValueAtIndex(channels.0.cast(), index) };
        if channel.is_null() {
            continue;
        }
        let channel = BorrowedCf(channel);
        let group = channel_string(channel, ChannelString::Group);
        let name = channel_string(channel, ChannelString::Name);
        if group == "Energy Model" {
            let unit = channel_string(channel, ChannelString::Unit);
            let Some(watts) = channel_watts(channel, &unit, elapsed) else {
                continue;
            };
            if name == "GPU Energy" {
                accumulate(&mut gpu_power, watts);
            } else if name.ends_with("CPU Energy") {
                accumulate(&mut cpu_power, watts);
            } else if name.starts_with("ANE") {
                accumulate(&mut ane_power, watts);
            }
            continue;
        }
        if group != "CPU Stats" {
            continue;
        }
        let die = die_id(&name);
        if name.contains("PCPU") {
            if let Some(freq) = effective_frequency(channel, p_freqs) {
                p_by_die.entry(die).or_default().push(freq);
            }
        } else if (name.contains("ECPU") || name.contains("MCPU"))
            && let Some(freq) = effective_frequency(channel, e_freqs)
        {
            e_by_die.entry(die).or_default().push(freq);
        }
    }

    output.cpu_power_w = cpu_power.or(output.cpu_power_w);
    output.gpu_power_w = gpu_power.or(output.gpu_power_w);
    output.ane_power_w = ane_power.or(output.ane_power_w);
    if let Some(frequencies) = cluster_averages(e_by_die) {
        output.ecluster_freq_mhz = Some(frequencies);
    }
    if let Some(frequencies) = cluster_averages(p_by_die) {
        output.pcluster_freq_mhz = Some(frequencies);
    }
}

fn cluster_averages(values: BTreeMap<usize, Vec<f32>>) -> Option<Vec<f32>> {
    let averages: Vec<f32> = values
        .into_values()
        .filter(|items| !items.is_empty())
        .map(|items| items.iter().sum::<f32>() / items.len() as f32)
        .collect();
    (!averages.is_empty()).then_some(averages)
}

fn channel_watts(channel: BorrowedCf, unit: &str, elapsed: f32) -> Option<f32> {
    // SAFETY: channel is a live IOReport channel dictionary.
    let energy = unsafe { IOReportSimpleGetIntegerValue(channel.0.cast(), 0) } as f32;
    let joules = match unit.trim() {
        "mJ" => energy / 1e3,
        "uJ" => energy / 1e6,
        "nJ" => energy / 1e9,
        _ => return None,
    };
    Some(joules / elapsed)
}

fn effective_frequency(channel: BorrowedCf, frequencies: &[f32]) -> Option<f32> {
    // SAFETY: channel is a live state-residency channel dictionary.
    let count = unsafe { IOReportStateGetCount(channel.0.cast()) };
    let mut active = Vec::new();
    for index in 0..count {
        // SAFETY: index is within IOReportStateGetCount for this live channel.
        let name = unsafe { IOReportStateGetNameForIndex(channel.0.cast(), index) };
        let name = cf_string_value(name);
        if matches!(name.as_str(), "IDLE" | "DOWN" | "OFF") {
            continue;
        }
        // SAFETY: index is within IOReportStateGetCount for this live channel.
        let residency = unsafe { IOReportStateGetResidency(channel.0.cast(), index) }.max(0) as f64;
        active.push(residency);
    }
    if active.len() != frequencies.len() {
        return None;
    }
    let total: f64 = active.iter().sum();
    if total <= 0.0 {
        return Some(0.0);
    }
    Some(
        active
            .iter()
            .zip(frequencies)
            .map(|(residency, frequency)| residency / total * f64::from(*frequency))
            .sum::<f64>() as f32,
    )
}

#[derive(Clone, Copy)]
enum ChannelString {
    Group,
    Name,
    Unit,
}

fn channel_string(channel: BorrowedCf, field: ChannelString) -> String {
    // SAFETY: channel is a live IOReport channel and the selected accessor returns borrowed data.
    let value = unsafe {
        match field {
            ChannelString::Group => IOReportChannelGetGroup(channel.0.cast()),
            ChannelString::Name => IOReportChannelGetChannelName(channel.0.cast()),
            ChannelString::Unit => IOReportChannelGetUnitLabel(channel.0.cast()),
        }
    };
    cf_string_value(value)
}

fn cf_string(value: &str) -> Result<OwnedCf> {
    // SAFETY: the byte slice remains valid for the call and CoreFoundation copies it.
    let string = unsafe {
        CFStringCreateWithBytes(
            kCFAllocatorDefault,
            value.as_ptr(),
            isize::try_from(value.len()).unwrap_or(isize::MAX),
            kCFStringEncodingUTF8,
            0,
        )
    };
    OwnedCf::new(string.cast(), "CFString creation")
}

fn cf_string_value(value: CFStringRef) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut buffer = [0 as c_char; 256];
    // SAFETY: value is a live CFString and buffer is writable for its full declared length.
    let copied = unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            isize::try_from(buffer.len()).unwrap_or(isize::MAX),
            kCFStringEncodingUTF8,
        )
    };
    if copied == 0 {
        return String::new();
    }
    // SAFETY: CFStringGetCString succeeded and therefore wrote a NUL-terminated C string.
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn dictionary_value(dictionary: BorrowedCf, key: &str) -> Option<BorrowedCf> {
    let key = cf_string(key).ok()?;
    // SAFETY: dictionary and key are live CF objects for this lookup.
    let value = unsafe { CFDictionaryGetValue(dictionary.0.cast(), key.0) };
    (!value.is_null()).then_some(BorrowedCf(value))
}

fn die_id(channel: &str) -> usize {
    channel
        .strip_prefix("DIE_")
        .and_then(|rest| rest.split_once('_'))
        .and_then(|(id, _)| id.parse().ok())
        .unwrap_or(0)
}

fn cpu_frequency_tables() -> Result<(Vec<f32>, Vec<f32>)> {
    let service_name = c"AppleARMIODevice";
    // SAFETY: service_name is a valid static C string.
    let matching = unsafe { IOServiceMatching(service_name.as_ptr()) };
    if matching.is_null() {
        return Err("AppleARMIODevice matching failed".into());
    }
    let mut iterator = 0;
    // SAFETY: matching is transferred to IOKit and iterator points to writable storage.
    let status = unsafe { IOServiceGetMatchingServices(0, matching, &raw mut iterator) };
    if status != 0 || iterator == 0 {
        return Err(format!("AppleARMIODevice lookup failed: {status}"));
    }
    let iterator = IoObject(iterator);
    loop {
        // SAFETY: iterator is a live IOKit iterator.
        let entry = unsafe { IOIteratorNext(iterator.0) };
        if entry == 0 {
            break;
        }
        let entry = IoObject(entry);
        let mut name = [0 as c_char; 128];
        // SAFETY: entry is live and name is a writable 128-byte buffer required by IOKit.
        let named = unsafe { IORegistryEntryGetName(entry.0, name.as_mut_ptr()) } == 0;
        if !named {
            continue;
        }
        // SAFETY: a successful IORegistryEntryGetName writes a NUL-terminated string.
        if unsafe { CStr::from_ptr(name.as_ptr()) }.to_bytes() != b"pmgr" {
            continue;
        }
        let mut properties: CFMutableDictionaryRef = ptr::null_mut();
        // SAFETY: entry is live, properties is writable, and the default allocator is valid.
        let status = unsafe {
            IORegistryEntryCreateCFProperties(entry.0, &raw mut properties, kCFAllocatorDefault, 0)
        };
        if status != 0 {
            return Err(format!("pmgr properties failed: {status}"));
        }
        let properties = OwnedCf::new(properties.cast(), "pmgr properties")?;
        let mut e_key = "voltage-states1-sram".to_string();
        let mut p_key = "voltage-states5-sram".to_string();
        if dictionary_data(properties.borrowed(), &e_key).is_none()
            && let Some((e, p)) = cluster_keys(properties.borrowed())
        {
            e_key = e;
            p_key = p;
        }
        let e = dvfs_frequencies(properties.borrowed(), &e_key)
            .ok_or_else(|| format!("missing {e_key}"))?;
        let p = dvfs_frequencies(properties.borrowed(), &p_key)
            .ok_or_else(|| format!("missing {p_key}"))?;
        return Ok((e, p));
    }
    Err("pmgr service not found".into())
}

fn cluster_keys(properties: BorrowedCf) -> Option<(String, String)> {
    let bytes = dictionary_data(properties, "acc-clusters")?;
    let mut clusters: Vec<(u8, u8)> = bytes
        .chunks_exact(8)
        .map(|chunk| (chunk[1], chunk[0]))
        .collect();
    clusters.sort_unstable();
    if clusters.len() < 2 {
        return None;
    }
    let e = clusters.get(clusters.len() - 2)?.1;
    let p = clusters.last()?.1;
    Some((
        format!("voltage-states{e}-sram"),
        format!("voltage-states{p}-sram"),
    ))
}

fn dvfs_frequencies(properties: BorrowedCf, key: &str) -> Option<Vec<f32>> {
    let bytes = dictionary_data(properties, key)?;
    let raw: Vec<u32> = bytes
        .chunks_exact(8)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    let scale = if raw.iter().copied().max()? > 100_000_000 {
        1_000_000.0
    } else {
        1_000.0
    };
    Some(
        raw.into_iter()
            .map(|frequency| frequency as f32 / scale)
            .collect(),
    )
}

fn dictionary_data(dictionary: BorrowedCf, key: &str) -> Option<Vec<u8>> {
    let data = dictionary_value(dictionary, key)?;
    // SAFETY: the dictionary value is CFData for the pmgr keys used here.
    let len = unsafe { CFDataGetLength(data.0.cast()) };
    if len <= 0 {
        return None;
    }
    let mut bytes = vec![0; len as usize];
    // SAFETY: data is live CFData and bytes has exactly len writable bytes.
    unsafe {
        CFDataGetBytes(
            data.0.cast::<c_void>().cast::<_>() as CFDataRef,
            CFRange::init(0, len),
            bytes.as_mut_ptr(),
        );
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::{cluster_averages, die_id};
    use std::collections::BTreeMap;

    #[test]
    fn groups_cluster_frequency_by_die() {
        let mut values = BTreeMap::new();
        values.insert(0, vec![1000.0, 1200.0]);
        assert_eq!(cluster_averages(values), Some(vec![1100.0]));
        assert_eq!(die_id("DIE_2_PCPU3"), 2);
        assert_eq!(die_id("ECPU0"), 0);
    }
}
