// Binding approach derived from macmon (MIT): https://github.com/vladkens/macmon

use std::ffi::{c_char, c_void};

use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation::dictionary::{
    CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef, CFMutableDictionaryRef,
};
use core_foundation::number::{
    CFNumberGetTypeID, CFNumberGetValue, CFNumberRef, kCFNumberFloat64Type,
};
use core_foundation::string::{CFStringCreateWithBytes, kCFStringEncodingUTF8};

type Result<T> = std::result::Result<T, String>;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: CFDictionaryRef,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFTypeRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
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
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: OwnedCf is only constructed for a non-null object returned at +1 ownership.
        unsafe { CFRelease(self.0) };
    }
}

struct IoObject(u32);

impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns a non-zero IOKit object returned by IOKit.
        unsafe { IOObjectRelease(self.0) };
    }
}

pub struct Gpu;

impl Gpu {
    pub fn new() -> Result<Self> {
        query_utilization().map(|_| Self)
    }

    /// `Gpu` is a capability token: holding one means the query worked once,
    /// so `poll` needs no state of its own.
    #[allow(clippy::unused_self)]
    pub fn poll(&self) -> Option<f32> {
        query_utilization().ok().flatten()
    }
}

fn query_utilization() -> Result<Option<f32>> {
    let service_name = c"IOAccelerator";
    // SAFETY: service_name is a valid static C string.
    let matching = unsafe { IOServiceMatching(service_name.as_ptr()) };
    if matching.is_null() {
        return Err("IOAccelerator matching failed".into());
    }
    let mut iterator = 0;
    // SAFETY: matching is transferred to IOKit and iterator points to writable storage.
    let status = unsafe { IOServiceGetMatchingServices(0, matching, &raw mut iterator) };
    if status != 0 || iterator == 0 {
        return Err(format!("IOAccelerator lookup failed: {status}"));
    }
    let iterator = IoObject(iterator);
    let property_key = cf_string("PerformanceStatistics")?;
    let primary_key = cf_string("Device Utilization %")?;
    let fallback_key = cf_string("GPU Activity(%)")?;
    let mut maximum: Option<f32> = None;

    loop {
        // SAFETY: iterator is a live IOKit iterator.
        let service = unsafe { IOIteratorNext(iterator.0) };
        if service == 0 {
            break;
        }
        let service = IoObject(service);
        // SAFETY: service and property_key are live; a non-null result has +1 ownership.
        let statistics = unsafe {
            IORegistryEntryCreateCFProperty(
                service.0,
                property_key.0,
                kCFAllocatorDefault.cast(),
                0,
            )
        };
        let Ok(statistics) = OwnedCf::new(statistics, "PerformanceStatistics") else {
            continue;
        };
        // SAFETY: statistics is a live CF object, so querying its type ID is valid.
        if unsafe { CFGetTypeID(statistics.0) } != unsafe { CFDictionaryGetTypeID() } {
            continue;
        }
        let dictionary: CFDictionaryRef = statistics.0.cast();
        let value = dictionary_number(dictionary, primary_key.0)
            .or_else(|| dictionary_number(dictionary, fallback_key.0));
        if let Some(value) = value.filter(|value| value.is_finite()) {
            maximum = Some(maximum.map_or(value, |current| current.max(value)));
        }
    }
    Ok(maximum.map(|value| value.clamp(0.0, 100.0)))
}

fn dictionary_number(dictionary: CFDictionaryRef, key: *const c_void) -> Option<f32> {
    // SAFETY: dictionary and key are live CoreFoundation objects for this lookup.
    let value = unsafe { CFDictionaryGetValue(dictionary, key) };
    if value.is_null() {
        return None;
    }
    // SAFETY: value is a live borrowed CF object from dictionary.
    if unsafe { CFGetTypeID(value) } != unsafe { CFNumberGetTypeID() } {
        return None;
    }
    let mut number = 0.0f64;
    // SAFETY: value is a CFNumber and number points to writable f64 storage.
    unsafe {
        CFNumberGetValue(
            value.cast::<c_void>().cast::<_>() as CFNumberRef,
            kCFNumberFloat64Type,
            (&raw mut number).cast(),
        )
    }
    .then_some(number as f32)
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
