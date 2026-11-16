/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Optional AppleSMC read-only commands (5, 8, 9), never writes or fan control.
//! ABI cross-checked against macmon (MIT); keys remain labelled by their FourCC.
use std::{collections::HashMap, ffi::c_void, mem::size_of};

#[repr(C)]
#[derive(Default)]
struct Version {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
    padding: [u8; 2],
}
#[repr(C)]
#[derive(Default)]
struct Limit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    memory: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Info {
    size: u32,
    kind: u32,
    attributes: u8,
    padding: [u8; 3],
}
#[repr(C)]
#[derive(Default)]
struct Payload {
    key: u32,
    version: Version,
    limit: Limit,
    info: Info,
    result: u8,
    status: u8,
    command: u8,
    padding: u8,
    index: u32,
    bytes: [u8; 32],
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceOpen(service: u32, task: u32, kind: u32, connection: *mut u32) -> i32;
    fn IOServiceClose(connection: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
}

pub struct Smc {
    connection: u32,
    keys: HashMap<u32, Info>,
}
impl Drop for Smc {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.connection) };
    }
}
impl Smc {
    pub fn open() -> Option<Self> {
        for entry in crate::iokit::entries(c"AppleSMC") {
            let mut connection = 0;
            let result = unsafe {
                IOServiceOpen(
                    entry.raw(),
                    mach2::traps::mach_task_self(),
                    0,
                    &mut connection,
                )
            };
            if result == 0 && connection != 0 {
                return Some(Self {
                    connection,
                    keys: HashMap::new(),
                });
            }
        }
        None
    }
    fn request(&self, input: Payload) -> Option<Payload> {
        let mut output = Payload::default();
        let mut size = size_of::<Payload>();
        // Both buffers have the C ABI layout and bounded sizes; reserved fields are zero.
        let result = unsafe {
            IOConnectCallStructMethod(
                self.connection,
                2,
                (&input as *const Payload).cast(),
                size_of::<Payload>(),
                (&mut output as *mut Payload).cast(),
                &mut size,
            )
        };
        (result == 0 && output.result == 0 && size == size_of::<Payload>()).then_some(output)
    }
    fn read(&mut self, key: u32) -> Option<(u32, Vec<u8>)> {
        let info = if let Some(info) = self.keys.get(&key) {
            *info
        } else {
            let info = self
                .request(Payload {
                    key,
                    command: 9,
                    ..Default::default()
                })?
                .info;
            if info.size > 32 || self.keys.len() >= 4096 {
                return None;
            }
            self.keys.insert(key, info);
            info
        };
        let output = self.request(Payload {
            key,
            info,
            command: 5,
            ..Default::default()
        })?;
        Some((info.kind, output.bytes[..info.size as usize].to_vec()))
    }
    pub fn temperature_keys(&mut self) -> Vec<u32> {
        let Some((kind, bytes)) = self.read(u32::from_be_bytes(*b"#KEY")) else {
            return vec![];
        };
        if kind != u32::from_be_bytes(*b"ui32") {
            return vec![];
        }
        let Ok(bytes) = <[u8; 4]>::try_from(bytes) else {
            return vec![];
        };
        (0..u32::from_be_bytes(bytes).min(4096))
            .filter_map(|index| {
                let key = self
                    .request(Payload {
                        command: 8,
                        index,
                        ..Default::default()
                    })?
                    .key;
                let bytes = key.to_be_bytes();
                // Avoid calibration/threshold/control keys (e.g. TVmS, Tz*, Tcal).
                // These prefixes are sensor channels, but not a verified CPU/GPU mapping.
                (matches!(&bytes[..2], b"Tp" | b"Te" | b"Tg" | b"Ts")
                    || matches!(&bytes, b"TB0T" | b"Tm0B" | b"TW0P"))
                .then_some(key)
            })
            .collect()
    }
    pub fn temperature(&mut self, key: u32) -> Option<i32> {
        let (kind, bytes) = self.read(key)?;
        temperature_value(kind, &bytes)
    }
}

fn temperature_value(kind: u32, bytes: &[u8]) -> Option<i32> {
    let value = if kind == u32::from_be_bytes(*b"flt ") {
        f32::from_le_bytes(bytes.try_into().ok()?)
    } else if kind == u32::from_be_bytes(*b"sp78") {
        i16::from_be_bytes(bytes.try_into().ok()?) as f32 / 256.
    } else {
        return None;
    };
    (value.is_finite() && value > 0. && value <= 150.).then_some((value * 1000.).round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn smc_abi_matches_the_driver_structure() {
        assert_eq!(size_of::<Payload>(), 80);
        assert_eq!(std::mem::offset_of!(Payload, bytes), 48);
        assert_eq!(std::mem::offset_of!(Payload, command), 42);
    }

    #[test]
    fn temperature_formats_reject_invalid_or_non_sensor_values() {
        let float = u32::from_be_bytes(*b"flt ");
        assert_eq!(
            temperature_value(float, &28.25_f32.to_le_bytes()),
            Some(28_250)
        );
        assert_eq!(
            temperature_value(u32::from_be_bytes(*b"sp78"), &0x1980_i16.to_be_bytes()),
            Some(25_500)
        );
        for value in [0., -1., 151., f32::NAN, f32::INFINITY] {
            assert_eq!(temperature_value(float, &value.to_le_bytes()), None);
        }
        assert_eq!(temperature_value(float, &[0, 1]), None);
        assert_eq!(
            temperature_value(u32::from_be_bytes(*b"ui32"), &[0; 4]),
            None
        );
    }
}
