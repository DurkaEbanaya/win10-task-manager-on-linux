/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Read-only IOKit registry queries. No private user-client calls or privileges.
use core_foundation::{
    base::{CFType, CFTypeRef, TCFType},
    dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionaryRef},
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use std::ffi::{c_char, c_void, CStr};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOObjectConformsTo(object: u32, class: *const c_char) -> u32;
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        port: u32,
        matching: CFDictionaryRef,
        iterator: *mut u32,
    ) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IORegistryEntrySearchCFProperty(
        entry: u32,
        plane: *const c_char,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
}

struct Object(u32);
impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

pub struct Entry(Object);
impl Entry {
    pub fn conforms(&self, class: &CStr) -> bool {
        unsafe { IOObjectConformsTo(self.0 .0, class.as_ptr()) != 0 }
    }
    pub fn number(&self, key: &str) -> Option<i64> {
        self.property(key)?.downcast::<CFNumber>()?.to_i64()
    }
    pub fn boolean(&self, key: &str) -> Option<bool> {
        self.property(key)?
            .downcast::<core_foundation::boolean::CFBoolean>()
            .map(|v| v.into())
    }
    pub fn id(&self) -> Option<u64> {
        let mut id = 0;
        (unsafe { IORegistryEntryGetRegistryEntryID(self.0 .0, &mut id) } == 0).then_some(id)
    }
    pub fn property(&self, key: &str) -> Option<CFType> {
        let key = CFString::new(key);
        // Create rule: returned CF property is owned by this caller.
        let ptr = unsafe {
            IORegistryEntryCreateCFProperty(
                self.0 .0,
                key.as_concrete_TypeRef(),
                std::ptr::null(),
                0,
            )
        };
        (!ptr.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(ptr) })
    }
    pub fn string(&self, key: &str) -> Option<String> {
        self.property(key)?
            .downcast::<CFString>()
            .map(|v| v.to_string())
    }
    pub fn dictionary(&self, key: &str) -> Option<CFDictionary> {
        self.property(key)?.downcast::<CFDictionary>()
    }
    pub fn child_property(&self, key: &str) -> Option<CFType> {
        let key = CFString::new(key);
        let ptr = unsafe {
            IORegistryEntrySearchCFProperty(
                self.0 .0,
                c"IOService".as_ptr(),
                key.as_concrete_TypeRef(),
                std::ptr::null(),
                1,
            )
        };
        (!ptr.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(ptr) })
    }
    pub fn child_string(&self, key: &str) -> Option<String> {
        self.child_property(key)?
            .downcast::<CFString>()
            .map(|v| v.to_string())
    }
    pub fn child_number(&self, key: &str) -> Option<u64> {
        u64::try_from(self.child_property(key)?.downcast::<CFNumber>()?.to_i64()?).ok()
    }
}

pub fn entries(class: &CStr) -> Vec<Entry> {
    // IOServiceGetMatchingServices consumes the matching dictionary even on error.
    let matching = unsafe { IOServiceMatching(class.as_ptr()) };
    if matching.is_null() {
        return vec![];
    }
    let mut iterator = 0;
    if unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) } != 0 {
        return vec![];
    }
    let iterator = Object(iterator);
    let mut entries = vec![];
    for _ in 0..4096 {
        let entry = unsafe { IOIteratorNext(iterator.0) };
        if entry == 0 {
            break;
        }
        entries.push(Entry(Object(entry)));
    }
    entries
}

pub fn number(dict: &CFDictionary, key: &str) -> Option<u64> {
    let key = CFString::new(key);
    let value = dict.find(key.as_CFTypeRef())?;
    // Dictionary owns the value; retain it before downcasting, never assume its type.
    let value = unsafe { CFType::wrap_under_get_rule(*value) };
    u64::try_from(value.downcast::<CFNumber>()?.to_i64()?).ok()
}
