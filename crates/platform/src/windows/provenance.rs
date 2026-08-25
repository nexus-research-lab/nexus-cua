//! Best-effort Authenticode provenance for trusted-host discovery.

use std::ffi::c_void;

use nexus_cua_protocol::SignatureStatus;
use windows::Win32::Foundation::{HWND, TRUST_E_NOSIGNATURE};
use windows::Win32::Security::Cryptography::{CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
    WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTHelperGetProvCertFromChain,
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows::core::PCWSTR;

pub(super) fn inspect(executable_path: &str) -> (Option<String>, SignatureStatus) {
    let path = executable_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: u32::try_from(size_of::<WINTRUST_FILE_INFO>()).unwrap_or(u32::MAX),
        pcwszFilePath: PCWSTR(path.as_ptr()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: u32::try_from(size_of::<WINTRUST_DATA>()).unwrap_or(u32::MAX),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    data.Anonymous.pFile = &raw mut file;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: All WinTrust structures point to live stack/UTF-16 storage for
    // the duration of verification and are closed before returning.
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &raw mut action,
            (&raw mut data).cast::<c_void>(),
        )
    };
    let publisher = (status == 0)
        .then(|| unsafe { publisher_name(&data) })
        .flatten();
    let signature_status = if status == 0 {
        SignatureStatus::Verified
    } else if status == TRUST_E_NOSIGNATURE.0 {
        SignatureStatus::Unsigned
    } else {
        SignatureStatus::Invalid
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: This closes the state allocated by the preceding verify call.
    let _ = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &raw mut action,
            (&raw mut data).cast::<c_void>(),
        )
    };
    (publisher, signature_status)
}

unsafe fn publisher_name(data: &WINTRUST_DATA) -> Option<String> {
    // SAFETY: WinVerifyTrust initialized the state handle and keeps it live.
    let provider = unsafe { WTHelperProvDataFromStateData(data.hWVTStateData) };
    if provider.is_null() {
        return None;
    }
    // SAFETY: The provider chain is live until WTD_STATEACTION_CLOSE.
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) };
    if signer.is_null() {
        return None;
    }
    // SAFETY: The signer chain owns the returned certificate descriptor.
    let certificate = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
    if certificate.is_null() {
        return None;
    }
    // SAFETY: Null was checked and the provider owns `pCert` until close.
    let context = unsafe { (*certificate).pCert };
    if context.is_null() {
        return None;
    }
    // SAFETY: A null output slice requests the required UTF-16 length.
    let length =
        unsafe { CertGetNameStringW(context, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, None) };
    if length <= 1 {
        return None;
    }
    let mut buffer = vec![0_u16; usize::try_from(length).ok()?];
    // SAFETY: The buffer has the exact capacity returned above.
    let copied = unsafe {
        CertGetNameStringW(
            context,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            None,
            Some(&mut buffer),
        )
    };
    if copied <= 1 {
        return None;
    }
    buffer.truncate(usize::try_from(copied - 1).ok()?);
    Some(String::from_utf16_lossy(&buffer))
}
