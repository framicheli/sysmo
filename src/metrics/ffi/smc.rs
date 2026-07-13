// Binding approach derived from macmon (MIT): https://github.com/vladkens/macmon

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, c_char, c_void};
use std::mem::size_of;

use core_foundation::dictionary::{CFDictionaryRef, CFMutableDictionaryRef};

type Result<T> = std::result::Result<T, String>;
type Readings = Vec<(String, f32)>;

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
    fn IOObjectRelease(object: u32) -> u32;
    fn IOServiceOpen(device: u32, task: u32, kind: u32, connection: *mut u32) -> i32;
    fn IOServiceClose(connection: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
    fn mach_task_self() -> u32;
}

#[repr(C)]
#[derive(Default)]
struct KeyDataVersion {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default)]
struct PowerLimitData {
    version: u16,
    length: u16,
    cpu_limit: u32,
    gpu_limit: u32,
    memory_limit: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KeyInfo {
    data_size: u32,
    data_type: u32,
    attributes: u8,
}

#[repr(C)]
#[derive(Default)]
struct KeyData {
    key: u32,
    version: KeyDataVersion,
    power_limit: PowerLimitData,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: [(); 6] = [(); size_of::<KeyDataVersion>()];
const _: [(); 16] = [(); size_of::<PowerLimitData>()];
const _: [(); 12] = [(); size_of::<KeyInfo>()];
const _: [(); 80] = [(); size_of::<KeyData>()];

struct IoObject(u32);

impl Drop for IoObject {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns a non-zero IOKit object returned by IOKit.
        unsafe { IOObjectRelease(self.0) };
    }
}

struct Connection(u32);

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns a successfully opened IOKit connection.
        unsafe { IOServiceClose(self.0) };
    }
}

#[derive(Clone)]
struct Sensor {
    key: String,
    label: String,
}

pub struct Smc {
    connection: Connection,
    key_info: HashMap<u32, KeyInfo>,
    temperatures: Vec<Sensor>,
    fans: Vec<Sensor>,
}

impl Smc {
    pub fn new() -> Result<Self> {
        let connection = open_connection()?;
        let mut smc = Self {
            connection,
            key_info: HashMap::new(),
            temperatures: Vec::new(),
            fans: Vec::new(),
        };
        let keys = smc.all_keys()?;
        let key_set: HashSet<&str> = keys.iter().map(String::as_str).collect();
        for key in &keys {
            if is_temperature_key(key)
                && smc
                    .read_numeric(key)
                    .is_some_and(|value| (0.0..=130.0).contains(&value))
            {
                smc.temperatures.push(Sensor {
                    key: key.clone(),
                    label: key.clone(),
                });
            } else if key.len() == 4 && key.starts_with('F') && key.ends_with("Ac") {
                let prefix = &key[..2];
                let min = format!("{prefix}Mn");
                let max = format!("{prefix}Mx");
                let min_rpm = key_set
                    .contains(min.as_str())
                    .then(|| smc.read_numeric(&min))
                    .flatten();
                let max_rpm = key_set
                    .contains(max.as_str())
                    .then(|| smc.read_numeric(&max))
                    .flatten();
                let label = match (min_rpm, max_rpm) {
                    (Some(min), Some(max)) => format!("{prefix} {min:.0}–{max:.0}"),
                    _ => prefix.to_string(),
                };
                smc.fans.push(Sensor {
                    key: key.clone(),
                    label,
                });
            }
        }
        Ok(smc)
    }

    pub fn poll(&mut self) -> (Readings, Readings) {
        let temperature_sensors = self.temperatures.clone();
        let fan_sensors = self.fans.clone();
        let temperatures = temperature_sensors
            .into_iter()
            .filter_map(|sensor| {
                let value = self.read_numeric(&sensor.key)?;
                (0.0..=130.0)
                    .contains(&value)
                    .then_some((sensor.label, value))
            })
            .collect();
        let fans = fan_sensors
            .into_iter()
            .filter_map(|sensor| {
                let value = self.read_numeric(&sensor.key)?;
                value.is_finite().then_some((sensor.label, value.max(0.0)))
            })
            .collect();
        (temperatures, fans)
    }

    fn all_keys(&mut self) -> Result<Vec<String>> {
        let count = self
            .read_numeric_raw("#KEY")
            .and_then(|(bytes, _)| bytes.get(..4).and_then(|value| value.try_into().ok()))
            .map(u32::from_be_bytes)
            .ok_or_else(|| "SMC #KEY read failed".to_string())?;
        let mut keys = Vec::new();
        for index in 0..count {
            let input = KeyData {
                data8: 8,
                data32: index,
                ..Default::default()
            };
            if let Ok(output) = self.call(&input)
                && let Ok(key) = std::str::from_utf8(&output.key.to_be_bytes())
            {
                keys.push(key.to_string());
            }
        }
        Ok(keys)
    }

    fn read_numeric(&mut self, key: &str) -> Option<f32> {
        let (bytes, kind) = self.read_numeric_raw(key)?;
        decode_numeric(&bytes, &kind)
    }

    fn read_numeric_raw(&mut self, key: &str) -> Option<(Vec<u8>, String)> {
        let key = key_id(key)?;
        let info = self.key_info(key).ok()?;
        let size = usize::try_from(info.data_size).ok()?;
        if size > 32 {
            return None;
        }
        let input = KeyData {
            key,
            key_info: info,
            data8: 5,
            ..Default::default()
        };
        let output = self.call(&input).ok()?;
        let kind = std::str::from_utf8(&info.data_type.to_be_bytes())
            .ok()?
            .to_string();
        Some((output.bytes[..size].to_vec(), kind))
    }

    fn key_info(&mut self, key: u32) -> Result<KeyInfo> {
        if let Some(info) = self.key_info.get(&key) {
            return Ok(*info);
        }
        let output = self.call(&KeyData {
            key,
            data8: 9,
            ..Default::default()
        })?;
        self.key_info.insert(key, output.key_info);
        Ok(output.key_info)
    }

    fn call(&self, input: &KeyData) -> Result<KeyData> {
        let mut output = KeyData::default();
        let mut output_size = size_of::<KeyData>();
        // SAFETY: both payload pointers reference correctly aligned 80-byte repr(C) KeyData
        // values for the duration of the synchronous selector-2 call.
        let status = unsafe {
            IOConnectCallStructMethod(
                self.connection.0,
                2,
                (input as *const KeyData).cast(),
                size_of::<KeyData>(),
                (&mut output as *mut KeyData).cast(),
                &mut output_size,
            )
        };
        if status != 0 {
            return Err(format!("SMC call failed: {status}"));
        }
        if output.result != 0 {
            return Err(format!("SMC result: {}", output.result));
        }
        if output_size != size_of::<KeyData>() {
            return Err(format!("SMC returned {output_size} bytes"));
        }
        Ok(output)
    }
}

fn open_connection() -> Result<Connection> {
    let service_name = c"AppleSMC";
    // SAFETY: service_name is a valid static C string.
    let matching = unsafe { IOServiceMatching(service_name.as_ptr()) };
    if matching.is_null() {
        return Err("AppleSMC matching failed".into());
    }
    let mut iterator = 0;
    // SAFETY: matching is transferred to IOKit and iterator points to writable storage.
    let status = unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) };
    if status != 0 || iterator == 0 {
        return Err(format!("AppleSMC lookup failed: {status}"));
    }
    let iterator = IoObject(iterator);
    loop {
        // SAFETY: iterator is a live IOKit iterator.
        let service = unsafe { IOIteratorNext(iterator.0) };
        if service == 0 {
            return Err("AppleSMCKeysEndpoint not found".into());
        }
        let service = IoObject(service);
        let mut name = [0 as c_char; 128];
        // SAFETY: service is live and name is the 128-byte writable buffer IOKit requires.
        if unsafe { IORegistryEntryGetName(service.0, name.as_mut_ptr()) } != 0 {
            continue;
        }
        // SAFETY: a successful IORegistryEntryGetName writes a NUL-terminated string.
        if unsafe { CStr::from_ptr(name.as_ptr()) }.to_bytes() != b"AppleSMCKeysEndpoint" {
            continue;
        }
        let mut connection = 0;
        // SAFETY: service is live, connection is writable, and mach_task_self is a borrowed port.
        let status = unsafe { IOServiceOpen(service.0, mach_task_self(), 0, &mut connection) };
        if status == 0 && connection != 0 {
            return Ok(Connection(connection));
        }
        return Err(format!("IOServiceOpen AppleSMC failed: {status}"));
    }
}

fn key_id(key: &str) -> Option<u32> {
    let bytes: [u8; 4] = key.as_bytes().try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

fn is_temperature_key(key: &str) -> bool {
    ["Tp", "Te", "Ts", "Tg", "TC", "TG"]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

fn decode_numeric(bytes: &[u8], kind: &str) -> Option<f32> {
    match kind {
        "flt " if bytes.len() == 4 => Some(f32::from_le_bytes(bytes.try_into().ok()?)),
        "fpe2" if bytes.len() >= 2 => {
            Some(u16::from_be_bytes(bytes[..2].try_into().ok()?) as f32 / 4.0)
        }
        "sp78" if bytes.len() >= 2 => {
            Some(i16::from_be_bytes(bytes[..2].try_into().ok()?) as f32 / 256.0)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyData, decode_numeric};
    use std::mem::size_of;

    #[test]
    fn smc_layout_and_numeric_decoding() {
        assert_eq!(size_of::<KeyData>(), 80);
        assert_eq!(decode_numeric(&42.5f32.to_le_bytes(), "flt "), Some(42.5));
        assert_eq!(decode_numeric(&[0x13, 0x88], "fpe2"), Some(1250.0));
        assert_eq!(decode_numeric(&[0x2a, 0x80], "sp78"), Some(42.5));
    }
}
