//! Current-layout character lookup; called only on the serialized input actor.

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::data::CFData;
use core_foundation::string::CFStringRef;
use std::ffi::c_void;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> CFTypeRef;
    fn TISGetInputSourceProperty(source: CFTypeRef, key: CFStringRef) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        key: u16,
        action: u16,
        modifiers: u32,
        keyboard: u32,
        options: u32,
        dead_state: *mut u32,
        maximum: usize,
        length: *mut usize,
        output: *mut u16,
    ) -> i32;
}

pub(super) fn character_key(character: u16) -> Option<(u16, bool)> {
    // SAFETY: Copy returns an owned CF object, retained until all property reads finish.
    let raw = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
    if raw.is_null() {
        return None;
    }
    let source = unsafe { CFType::wrap_under_create_rule(raw) };
    let property = unsafe {
        TISGetInputSourceProperty(source.as_CFTypeRef(), kTISPropertyUnicodeKeyLayoutData)
    };
    if property.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_get_rule(property) };
    let data = value.downcast::<CFData>()?;
    if data.bytes().is_empty() {
        return None;
    }
    let keyboard = u32::from(unsafe { LMGetKbdType() });
    // Bounded lookup with dead-key processing disabled. No input-source mutation.
    for shift in [false, true] {
        for key in 0..128_u16 {
            let mut output = [0_u16; 4];
            let mut length = 0_usize;
            let mut dead = 0_u32;
            let status = unsafe {
                UCKeyTranslate(
                    data.bytes().as_ptr().cast(),
                    key,
                    0,
                    if shift { 2 } else { 0 },
                    keyboard,
                    1,
                    &raw mut dead,
                    output.len(),
                    &raw mut length,
                    output.as_mut_ptr(),
                )
            };
            if status == 0 && length == 1 && output[0] == character {
                return Some((key, shift));
            }
        }
    }
    None
}
