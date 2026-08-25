//! Owner-only Windows named-pipe security descriptor.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

use crate::TransportError;

// Protected DACL: full access for LocalSystem and the object owner only.
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

pub(crate) struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// SAFETY: The descriptor is self-relative LocalAlloc memory, all access is
// serialized through `&mut self`, and CreateNamedPipeW copies its contents.
unsafe impl Send for PipeSecurity {}

impl PipeSecurity {
    pub(crate) fn owner_only() -> Result<Self, TransportError> {
        let wide: Vec<u16> = PIPE_SDDL.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and remains alive for the call;
        // Windows allocates the returned self-relative descriptor with LocalAlloc.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(TransportError::Io(io::Error::last_os_error()));
        }
        let length = u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(|_| {
            TransportError::InvalidConfiguration(
                "Windows security attribute size does not fit u32".to_owned(),
            )
        })?;
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: length,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }

    pub(crate) fn attributes_ptr(&mut self) -> *mut c_void {
        ptr::from_mut(&mut self.attributes).cast()
    }

    pub(crate) fn create_pipe(
        &mut self,
        endpoint: &str,
        first_instance: bool,
        max_instances: usize,
    ) -> io::Result<NamedPipeServer> {
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first_instance)
            .reject_remote_clients(true)
            .max_instances(max_instances);
        // SAFETY: `attributes_ptr` points to this value's live security
        // descriptor for the duration of CreateNamedPipeW.
        unsafe { options.create_with_security_attributes_raw(endpoint, self.attributes_ptr()) }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: This pointer is the exact LocalAlloc result owned by
            // this value and is freed once, after all pipe creations finish.
            unsafe {
                let _ = LocalFree(self.descriptor);
            }
        }
    }
}
