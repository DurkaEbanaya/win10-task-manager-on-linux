/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Optional IOReport active-residency clocks. No sleeps, privileged sampling or
//! guessed energy channels. Resolve dynamically because this API is undocumented.
use core_foundation::{
    array::CFArray,
    base::{CFType, CFTypeRef, TCFType},
    dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef},
    string::{CFString, CFStringRef},
};
use std::sync::OnceLock;

struct Api {
    copy: unsafe extern "C" fn(u64, u64) -> CFDictionaryRef,
    subscribe: unsafe extern "C" fn(
        CFTypeRef,
        CFMutableDictionaryRef,
        *mut CFMutableDictionaryRef,
        u64,
        CFTypeRef,
    ) -> CFTypeRef,
    sample: unsafe extern "C" fn(CFTypeRef, CFMutableDictionaryRef, CFTypeRef) -> CFDictionaryRef,
    delta: unsafe extern "C" fn(CFDictionaryRef, CFDictionaryRef, CFTypeRef) -> CFDictionaryRef,
    subgroup: unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef,
    name: unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef,
    state_count: unsafe extern "C" fn(CFDictionaryRef) -> i32,
    state_name: unsafe extern "C" fn(CFDictionaryRef, i32) -> CFStringRef,
    residency: unsafe extern "C" fn(CFDictionaryRef, i32) -> i64,
}
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        // The successful library reference lives for the process lifetime.
        let library = libc::dlopen(
            c"/usr/lib/libIOReport.dylib".as_ptr(),
            libc::RTLD_LOCAL | libc::RTLD_LAZY,
        );
        if library.is_null() {
            return None;
        }
        macro_rules! symbol {
            ($name:literal, $signature:ty) => {{
                let ptr = libc::dlsym(library, concat!($name, "\0").as_ptr().cast());
                if ptr.is_null() {
                    libc::dlclose(library);
                    return None;
                }
                std::mem::transmute::<*mut libc::c_void, $signature>(ptr)
            }};
        }
        Some(Api {
            copy: symbol!(
                "IOReportCopyAllChannels",
                unsafe extern "C" fn(u64, u64) -> CFDictionaryRef
            ),
            subscribe: symbol!(
                "IOReportCreateSubscription",
                unsafe extern "C" fn(
                    CFTypeRef,
                    CFMutableDictionaryRef,
                    *mut CFMutableDictionaryRef,
                    u64,
                    CFTypeRef,
                ) -> CFTypeRef
            ),
            sample: symbol!(
                "IOReportCreateSamples",
                unsafe extern "C" fn(
                    CFTypeRef,
                    CFMutableDictionaryRef,
                    CFTypeRef,
                ) -> CFDictionaryRef
            ),
            delta: symbol!(
                "IOReportCreateSamplesDelta",
                unsafe extern "C" fn(
                    CFDictionaryRef,
                    CFDictionaryRef,
                    CFTypeRef,
                ) -> CFDictionaryRef
            ),
            subgroup: symbol!(
                "IOReportChannelGetSubGroup",
                unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef
            ),
            name: symbol!(
                "IOReportChannelGetChannelName",
                unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef
            ),
            state_count: symbol!(
                "IOReportStateGetCount",
                unsafe extern "C" fn(CFDictionaryRef) -> i32
            ),
            state_name: symbol!(
                "IOReportStateGetNameForIndex",
                unsafe extern "C" fn(CFDictionaryRef, i32) -> CFStringRef
            ),
            residency: symbol!(
                "IOReportStateGetResidency",
                unsafe extern "C" fn(CFDictionaryRef, i32) -> i64
            ),
        })
    })
    .as_ref()
}
fn owned(ptr: CFTypeRef) -> Option<CFType> {
    (!ptr.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(ptr) })
}
fn string(ptr: CFStringRef) -> Option<String> {
    (!ptr.is_null()).then(|| unsafe { CFString::wrap_under_get_rule(ptr) }.to_string())
}
fn channels(dict: &CFDictionary) -> Option<CFArray> {
    let key = CFString::new("IOReportChannels");
    let value = dict.find(key.as_CFTypeRef())?;
    unsafe { CFType::wrap_under_get_rule(*value) }.downcast()
}

pub struct Clocks {
    subscription: CFType,
    channels: CFDictionary,
    previous: Option<CFDictionary>,
    efficiency: Vec<f32>,
    performance: Vec<f32>,
    validated: (bool, bool),
}
type FrequencyRange = Option<(f32, f32)>;

fn frequency_bounds(table: &[f32]) -> FrequencyRange {
    if table.iter().any(|value| !value.is_finite() || *value <= 0.) {
        return None;
    }
    Some((
        table.iter().copied().reduce(f32::min)?,
        table.iter().copied().reduce(f32::max)?,
    ))
}
// IOReport/CF ownership is confined to one cache, accessed under the handler's
// mutex. Workers may move it, but never sample or release it concurrently.
unsafe impl Send for Clocks {}

fn frequency_table(key: &str, divisor: f32) -> Option<Vec<f32>> {
    crate::iokit::entries(c"AppleARMIODevice")
        .into_iter()
        .find_map(|entry| {
            if entry.name().as_deref() != Some("pmgr") {
                return None;
            }
            let data = entry.data(key)?;
            if data.is_empty() || data.len() > 4096 || data.len() % 8 != 0 {
                return None;
            }
            let values: Vec<_> = data
                .chunks_exact(8)
                .map(|bytes| u32::from_le_bytes(bytes[..4].try_into().unwrap()) as f32 / divisor)
                .collect();
            values
                .iter()
                .all(|value| *value > 0. && *value < 10000.)
                .then_some(values)
        })
}

impl Clocks {
    pub fn ranges(&self) -> (FrequencyRange, FrequencyRange) {
        (
            self.validated
                .0
                .then(|| frequency_bounds(&self.efficiency))
                .flatten(),
            self.validated
                .1
                .then(|| frequency_bounds(&self.performance))
                .flatten(),
        )
    }

    pub fn new(chip: &str) -> Option<Self> {
        // Restrict the state/table mapping to families with this known ABI.
        // Unknown chips/VMs remain unavailable rather than using nominal clocks.
        let divisor = if chip.starts_with("Apple M5") || chip.starts_with("Apple M4") {
            1000.
        } else if ["Apple M1", "Apple M2", "Apple M3"]
            .iter()
            .any(|prefix| chip.starts_with(prefix))
        {
            1_000_000.
        } else {
            return None;
        };
        let efficiency = frequency_table("voltage-states1-sram", divisor)?;
        let performance = frequency_table("voltage-states5-sram", divisor)?;
        let api = api()?;
        let all = owned(unsafe { (api.copy)(0, 0) }.cast())?.downcast::<CFDictionary>()?;
        let selected: Vec<_> = channels(&all)?
            .iter()
            .filter_map(|item| {
                let dict =
                    unsafe { CFType::wrap_under_get_rule(*item) }.downcast::<CFDictionary>()?;
                let subgroup = string(unsafe { (api.subgroup)(dict.as_concrete_TypeRef()) })?;
                let name = string(unsafe { (api.name)(dict.as_concrete_TypeRef()) })?;
                (subgroup == "CPU Complex Performance States"
                    && matches!(name.as_str(), "ECPU" | "PCPU"))
                .then_some(dict)
            })
            .collect();
        if selected.len() != 2 {
            return None;
        }
        let selected = CFArray::from_CFTypes(&selected);
        let dictionary =
            CFDictionary::from_CFType_pairs(&[(CFString::new("IOReportChannels"), selected)]);
        // IOReport may cache channel metadata, so pass an actual mutable dictionary.
        let mutable = unsafe {
            core_foundation::dictionary::CFDictionaryCreateMutableCopy(
                std::ptr::null(),
                0,
                dictionary.as_concrete_TypeRef(),
            )
        };
        let mutable = owned(mutable.cast())?;
        let mut subscribed = std::ptr::null_mut();
        let subscription = owned(unsafe {
            (api.subscribe)(
                std::ptr::null(),
                mutable.as_CFTypeRef() as _,
                &mut subscribed,
                0,
                std::ptr::null(),
            )
        });
        let subscribed = owned(subscribed.cast());
        Some(Self {
            subscription: subscription?,
            channels: subscribed?.downcast()?,
            previous: None,
            efficiency,
            performance,
            validated: (false, false),
        })
    }
    pub fn sample(&mut self) -> (Option<f32>, Option<f32>) {
        self.validated = (false, false);
        let Some(api) = api() else {
            return (None, None);
        };
        let current = owned(
            unsafe {
                (api.sample)(
                    self.subscription.as_CFTypeRef(),
                    self.channels.as_concrete_TypeRef() as _,
                    std::ptr::null(),
                )
            }
            .cast(),
        )
        .and_then(|value| value.downcast::<CFDictionary>());
        let previous = std::mem::replace(&mut self.previous, current.clone());
        let Some((previous, current)) = previous.zip(current) else {
            return (None, None);
        };
        let Some(delta) = owned(
            unsafe {
                (api.delta)(
                    previous.as_concrete_TypeRef(),
                    current.as_concrete_TypeRef(),
                    std::ptr::null(),
                )
            }
            .cast(),
        )
        .and_then(|value| value.downcast::<CFDictionary>()) else {
            return (None, None);
        };
        let Some(channels) = channels(&delta) else {
            return (None, None);
        };
        let mut result = (None, None);
        for item in channels.iter().take(16) {
            let Some(dict) =
                (unsafe { CFType::wrap_under_get_rule(*item) }).downcast::<CFDictionary>()
            else {
                continue;
            };
            let ptr = dict.as_concrete_TypeRef();
            let name = string(unsafe { (api.name)(ptr) });
            let table = match name.as_deref() {
                Some("ECPU") => &self.efficiency,
                Some("PCPU") => &self.performance,
                _ => continue,
            };
            // No truncation or guessed offsets: exactly IDLE + one state per table entry.
            if unsafe { (api.state_count)(ptr) } != table.len() as i32 + 1
                || string(unsafe { (api.state_name)(ptr, 0) }).as_deref() != Some("IDLE")
            {
                continue;
            }
            let mut active = 0_f64;
            let mut weighted = 0_f64;
            let mut valid = true;
            for (index, mhz) in table.iter().enumerate() {
                let expected = format!("V{index}P{}", table.len() - index - 1);
                if string(unsafe { (api.state_name)(ptr, index as i32 + 1) }).as_deref()
                    != Some(expected.as_str())
                {
                    valid = false;
                    break;
                }
                let ticks = unsafe { (api.residency)(ptr, index as i32 + 1) };
                if ticks < 0 {
                    valid = false;
                    break;
                }
                active += ticks as f64;
                weighted += ticks as f64 * f64::from(*mhz);
            }
            let value = (valid && active > 0.).then_some((weighted / active) as f32);
            match name.as_deref() {
                Some("ECPU") => {
                    result.0 = value;
                    self.validated.0 = valid;
                }
                Some("PCPU") => {
                    result.1 = value;
                    self.validated.1 = valid;
                }
                _ => {}
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_tables_are_validated_from_samples_not_channel_metadata() {
        // VMs/unsupported hosts may not expose pmgr tables. A physical supported
        // host with both tables must not lose all clocks during construction.
        let output = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .unwrap();
        let chip = String::from_utf8_lossy(&output.stdout);
        let chip = chip.trim();
        let divisor = if chip.starts_with("Apple M4") || chip.starts_with("Apple M5") {
            1000.
        } else {
            1_000_000.
        };
        if !chip.starts_with("Apple M")
            || super::frequency_table("voltage-states1-sram", divisor).is_none()
            || super::frequency_table("voltage-states5-sram", divisor).is_none()
        {
            return;
        }
        let mut clocks = super::Clocks::new(chip).expect("native tables must not disable clocks");
        assert_eq!(clocks.ranges(), (None, None));
        clocks.sample();
        std::thread::sleep(std::time::Duration::from_millis(50));
        clocks.sample();
        let (efficiency, performance) = clocks.ranges();
        assert!(efficiency.is_some_and(|(min, max)| min > 0. && max >= min));
        assert!(performance.is_some_and(|(min, max)| min > 0. && max >= min));
    }

    #[test]
    fn bounds_do_not_assume_monotonic_dvfs_tables() {
        use super::frequency_bounds;
        assert_eq!(
            frequency_bounds(&[1308., 4464., 4428., 4416.]),
            Some((1308., 4464.))
        );
        assert_eq!(frequency_bounds(&[]), None);
        assert_eq!(frequency_bounds(&[0., 972.]), None);
        assert_eq!(frequency_bounds(&[972., f32::NAN]), None);
    }
}
