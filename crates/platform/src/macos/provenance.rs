//! Best-effort macOS code-signing provenance for trusted-host discovery.

use std::ffi::c_void;
use std::path::Path;
use std::ptr;

use core_foundation::base::{CFGetTypeID, CFRelease, TCFType};
use core_foundation::dictionary::{CFDictionaryGetValueIfPresent, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringGetTypeID, CFStringRef};
use core_foundation::url::{CFURL, CFURLRef};

enum OpaqueSecRequirement {}
type SecRequirementRef = *mut OpaqueSecRequirement;
enum OpaqueSecStaticCode {}
type SecStaticCodeRef = *mut OpaqueSecStaticCode;
type SecCSFlags = u32;

const SEC_CS_BASIC_VALIDATE_ONLY: SecCSFlags = (1 << 1) | (1 << 2);
const SEC_CS_NO_NETWORK_ACCESS: SecCSFlags = 1 << 29;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoTeamIdentifier: CFStringRef;

    fn SecStaticCodeCreateWithPath(
        path: CFURLRef,
        flags: SecCSFlags,
        code: *mut SecStaticCodeRef,
    ) -> i32;
    fn SecStaticCodeCheckValidity(
        code: SecStaticCodeRef,
        flags: SecCSFlags,
        requirement: SecRequirementRef,
    ) -> i32;
    fn SecCodeCopySigningInformation(
        code: SecStaticCodeRef,
        flags: SecCSFlags,
        information: *mut CFDictionaryRef,
    ) -> i32;
    fn SecCodeCopyDesignatedRequirement(
        code: SecStaticCodeRef,
        flags: SecCSFlags,
        requirement: *mut SecRequirementRef,
    ) -> i32;
    fn SecRequirementCopyString(
        requirement: SecRequirementRef,
        flags: SecCSFlags,
        text: *mut CFStringRef,
    ) -> i32;
}

#[derive(Default)]
pub(super) struct SigningIdentity {
    pub(super) team_id: Option<String>,
    pub(super) designated_requirement: Option<String>,
}

pub(super) fn inspect(executable_path: &str) -> SigningIdentity {
    let Some(url) = CFURL::from_path(Path::new(executable_path), false) else {
        return SigningIdentity::default();
    };
    let mut code = ptr::null_mut();
    // SAFETY: Security.framework returns retained Core Foundation objects. All
    // outputs are checked and released on this function's thread.
    unsafe {
        if SecStaticCodeCreateWithPath(url.as_concrete_TypeRef(), 0, &raw mut code) != 0
            || code.is_null()
        {
            return SigningIdentity::default();
        }
        let valid = SecStaticCodeCheckValidity(
            code,
            SEC_CS_BASIC_VALIDATE_ONLY | SEC_CS_NO_NETWORK_ACCESS,
            ptr::null_mut(),
        ) == 0;
        let identity = if valid {
            SigningIdentity {
                team_id: signing_team_id(code),
                designated_requirement: designated_requirement(code),
            }
        } else {
            SigningIdentity::default()
        };
        CFRelease(code.cast());
        identity
    }
}

unsafe fn signing_team_id(code: SecStaticCodeRef) -> Option<String> {
    let mut information = ptr::null();
    // SAFETY: The caller supplies a valid retained static-code object.
    if unsafe { SecCodeCopySigningInformation(code, 0, &raw mut information) } != 0
        || information.is_null()
    {
        return None;
    }
    let mut value = ptr::null();
    // SAFETY: The dictionary and exported key are valid for this call.
    let present = unsafe {
        CFDictionaryGetValueIfPresent(
            information,
            kSecCodeInfoTeamIdentifier.cast(),
            &raw mut value,
        ) != 0
    };
    let team_id = present.then(|| unsafe { copied_string(value) }).flatten();
    // SAFETY: `information` follows the create rule.
    unsafe { CFRelease(information.cast()) };
    team_id
}

unsafe fn designated_requirement(code: SecStaticCodeRef) -> Option<String> {
    let mut requirement = ptr::null_mut();
    // SAFETY: The caller supplies a valid retained static-code object.
    if unsafe { SecCodeCopyDesignatedRequirement(code, 0, &raw mut requirement) } != 0
        || requirement.is_null()
    {
        return None;
    }
    let mut text = ptr::null();
    // SAFETY: The requirement is retained until after the string is copied.
    let copied = unsafe { SecRequirementCopyString(requirement, 0, &raw mut text) } == 0;
    let output = copied
        .then(|| unsafe { copied_string(text.cast()) })
        .flatten();
    if !text.is_null() {
        // SAFETY: `text` follows the create rule.
        unsafe { CFRelease(text.cast()) };
    }
    // SAFETY: `requirement` follows the create rule.
    unsafe { CFRelease(requirement.cast()) };
    output
}

unsafe fn copied_string(value: *const c_void) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: The value came from a CF dictionary or Security.framework output.
    if unsafe { CFGetTypeID(value.cast()) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: Type identity was checked above and the owner remains live.
    let value = unsafe { CFString::wrap_under_get_rule(value.cast()) };
    Some(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_test_executable_is_inspected_without_prompting() {
        let executable = std::env::current_exe().expect("current test executable");
        let identity = inspect(executable.to_str().expect("UTF-8 test executable path"));
        assert!(
            identity
                .team_id
                .as_ref()
                .is_none_or(|value| !value.is_empty())
        );
        assert!(
            identity
                .designated_requirement
                .as_ref()
                .is_none_or(|value| !value.is_empty())
        );
    }
}
