/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Read-only HID temperature events. Driver labels are not CPU/GPU assignments.
//! The undocumented event API is optional: absent events mean unavailable.
use core_foundation::{
    array::{CFArray, CFArrayRef},
    base::{CFType, CFTypeRef, TCFType},
    dictionary::{CFDictionary, CFDictionaryRef},
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use magpie_platform::fan::{Fan, FanCache, HwmonChip, TemperatureSensor};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> CFTypeRef;
    fn IOHIDEventSystemClientSetMatching(client: CFTypeRef, matching: CFDictionaryRef);
    fn IOHIDEventSystemClientCopyServices(client: CFTypeRef) -> CFArrayRef;
    fn IOHIDServiceClientGetRegistryID(service: CFTypeRef) -> u64;
    fn IOHIDServiceClientCopyProperty(service: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(
        service: CFTypeRef,
        event_type: i64,
        matching: i32,
        options: i64,
    ) -> CFTypeRef;
    fn IOHIDEventGetFloatValue(event: CFTypeRef, field: i64) -> f64;
}

fn owned(ptr: CFTypeRef) -> Option<CFType> {
    (!ptr.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(ptr) })
}

fn readings() -> Vec<(u64, String, Option<i32>)> {
    let Some(client) = owned(unsafe { IOHIDEventSystemClientCreate(std::ptr::null()) }) else {
        return vec![];
    };
    let matching = CFDictionary::from_CFType_pairs(&[
        (
            CFString::new("PrimaryUsagePage"),
            CFNumber::from(0xff00_i32),
        ),
        (CFString::new("PrimaryUsage"), CFNumber::from(5_i32)),
    ]);
    unsafe {
        IOHIDEventSystemClientSetMatching(client.as_CFTypeRef(), matching.as_concrete_TypeRef());
    }
    let ptr = unsafe { IOHIDEventSystemClientCopyServices(client.as_CFTypeRef()) };
    if ptr.is_null() {
        return vec![];
    }
    // Copy rule owns the array; the array keeps each borrowed service alive.
    let services = unsafe { CFArray::<CFTypeRef>::wrap_under_create_rule(ptr) };
    let product = CFString::new("Product");
    services
        .iter()
        .take(4096)
        .filter_map(|service| {
            let service = *service;
            let id = unsafe { IOHIDServiceClientGetRegistryID(service) };
            if id == 0 {
                return None;
            }
            let label = owned(unsafe {
                IOHIDServiceClientCopyProperty(service, product.as_concrete_TypeRef())
            })?
            .downcast::<CFString>()?
            .to_string();
            // Calibration constants are not live temperature measurements.
            if label.to_ascii_lowercase().contains("tcal") {
                return None;
            }
            let value = owned(unsafe { IOHIDServiceClientCopyEvent(service, 15, 0, 0) }).and_then(
                |event| {
                    let value = unsafe { IOHIDEventGetFloatValue(event.as_CFTypeRef(), 15 << 16) };
                    (value.is_finite() && value > 0. && value <= 150.)
                        .then_some((value * 1000.).round() as i32)
                },
            );
            Some((id, label, value))
        })
        .collect()
}

pub struct Sensors {
    temperatures: Vec<TemperatureSensor>,
    // Assign indices by registry identity, not enumeration order or duplicate labels.
    indices: HashMap<u64, u32>,
    next_index: u32,
    smc: Option<crate::smc::Smc>,
    smc_keys: Vec<u32>,
}

impl FanCache for Sensors {
    fn new() -> Self {
        let mut smc = crate::smc::Smc::open();
        let smc_keys = smc
            .as_mut()
            .map_or_else(Vec::new, |smc| smc.temperature_keys());
        Self {
            temperatures: vec![],
            indices: HashMap::new(),
            next_index: 1,
            smc,
            smc_keys,
        }
    }
    fn refresh(&mut self) {
        let mut seen = HashSet::new();
        self.temperatures = readings()
            .into_iter()
            .filter(|(id, _, _)| seen.insert(*id))
            .filter_map(|(id, label, value)| {
                if !self.indices.contains_key(&id) {
                    // Bound identity bookkeeping during unusual device hotplug churn.
                    if self.indices.len() >= 16384 {
                        return None;
                    }
                    self.indices.insert(id, self.next_index);
                    self.next_index += 1;
                }
                Some(TemperatureSensor {
                    temp_index: self.indices[&id],
                    chip: Some(HwmonChip {
                        hwmon_index: 0,
                        name: Some("Apple HID sensors".into()),
                        device_path: Some(format!("ioregistry:{id}")),
                        ..Default::default()
                    }),
                    label: Some(label),
                    input_millicelsius: value,
                    ..Default::default()
                })
            })
            .collect();
        self.temperatures.sort_by_key(|sensor| sensor.temp_index);
        if let Some(smc) = &mut self.smc {
            for &key in &self.smc_keys {
                self.temperatures.push(TemperatureSensor {
                    temp_index: key,
                    chip: Some(HwmonChip {
                        hwmon_index: 1,
                        name: Some("Apple SMC sensors".into()),
                        device_path: Some("AppleSMCKeysEndpoint".into()),
                        ..Default::default()
                    }),
                    label: Some(format!(
                        "SMC {}",
                        String::from_utf8_lossy(&key.to_be_bytes())
                    )),
                    input_millicelsius: smc.temperature(key),
                    ..Default::default()
                });
            }
        }
    }
    fn cached_entries(&self) -> &[Fan] {
        // No inferred fans: the user's MacBook Air is passively cooled.
        &[]
    }
    fn cached_temperatures(&self) -> &[TemperatureSensor] {
        &self.temperatures
    }
}
