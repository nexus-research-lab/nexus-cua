//! Platform-protected registration secret storage.

use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;

use thiserror::Error;
use zeroize::Zeroizing;

/// Registration secret storage that never exposes values through formatting.
pub trait SecretStore: Send + Sync {
    /// Stores one new 32-byte registration secret without overwriting an existing value.
    ///
    /// # Errors
    ///
    /// Fails for invalid secret size, collisions, unsupported platforms, or
    /// protected credential-storage errors.
    fn put(&self, registration_id: &str, secret: &[u8]) -> Result<(), SecretStoreError>;
    /// Loads one exact registration secret into zeroizing memory.
    ///
    /// # Errors
    ///
    /// Fails when the value is missing, malformed, unavailable, or cannot be
    /// decrypted by the current OS user.
    fn get(&self, registration_id: &str) -> Result<Zeroizing<Vec<u8>>, SecretStoreError>;
    /// Deletes one registration secret; missing values are treated as already removed.
    ///
    /// # Errors
    ///
    /// Fails when protected credential storage cannot remove the value.
    fn delete(&self, registration_id: &str) -> Result<(), SecretStoreError>;
}

/// OS-backed secret store used by the standalone local provider.
pub struct SystemSecretStore {
    #[cfg(target_os = "windows")]
    root: PathBuf,
}

impl SystemSecretStore {
    /// Creates a platform secret store rooted beside owner-private registrations.
    ///
    /// # Errors
    ///
    /// Fails when platform-protected storage cannot be initialized.
    pub fn new(state_root: &Path) -> Result<Self, SecretStoreError> {
        #[cfg(target_os = "windows")]
        {
            let root = state_root.join("secrets");
            crate::private_fs::secure_directory(&root).map_err(SecretStoreError::Io)?;
            Ok(Self { root })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = state_root;
            Ok(Self {})
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ptr;

    use core_foundation::base::{CFType, TCFType as _};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use security_framework_sys::base::{errSecDuplicateItem, errSecItemNotFound, errSecSuccess};
    use security_framework_sys::item::{
        kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecReturnData,
        kSecValueData,
    };
    use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemDelete};
    use zeroize::Zeroizing;

    use super::{SecretStoreError, SystemSecretStore};

    const SERVICE: &str = "com.nexusresearch.nexuscua.authority.v1";

    impl SystemSecretStore {
        pub(super) fn put_platform(
            &self,
            registration_id: &str,
            secret: &[u8],
        ) -> Result<(), SecretStoreError> {
            let _ = self;
            let class_key = unsafe { CFString::wrap_under_get_rule(kSecClass) };
            let class_value = unsafe { CFString::wrap_under_get_rule(kSecClassGenericPassword) };
            let service_key = unsafe { CFString::wrap_under_get_rule(kSecAttrService) };
            let account_key = unsafe { CFString::wrap_under_get_rule(kSecAttrAccount) };
            let data_key = unsafe { CFString::wrap_under_get_rule(kSecValueData) };
            let service = CFString::new(SERVICE);
            let account = CFString::new(registration_id);
            let data = CFData::from_buffer(secret);
            let query = CFDictionary::from_CFType_pairs(&[
                (class_key.as_CFType(), class_value.as_CFType()),
                (service_key.as_CFType(), service.as_CFType()),
                (account_key.as_CFType(), account.as_CFType()),
                (data_key.as_CFType(), data.as_CFType()),
            ]);
            let status = unsafe { SecItemAdd(query.as_concrete_TypeRef(), ptr::null_mut()) };
            if status == errSecSuccess {
                Ok(())
            } else if status == errSecDuplicateItem {
                Err(SecretStoreError::AlreadyExists)
            } else {
                Err(SecretStoreError::Platform(status))
            }
        }

        pub(super) fn get_platform(
            &self,
            registration_id: &str,
        ) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
            let _ = self;
            let class_key = unsafe { CFString::wrap_under_get_rule(kSecClass) };
            let class_value = unsafe { CFString::wrap_under_get_rule(kSecClassGenericPassword) };
            let service_key = unsafe { CFString::wrap_under_get_rule(kSecAttrService) };
            let account_key = unsafe { CFString::wrap_under_get_rule(kSecAttrAccount) };
            let return_data_key = unsafe { CFString::wrap_under_get_rule(kSecReturnData) };
            let service = CFString::new(SERVICE);
            let account = CFString::new(registration_id);
            let query = CFDictionary::from_CFType_pairs(&[
                (class_key.as_CFType(), class_value.as_CFType()),
                (service_key.as_CFType(), service.as_CFType()),
                (account_key.as_CFType(), account.as_CFType()),
                (
                    return_data_key.as_CFType(),
                    CFBoolean::true_value().as_CFType(),
                ),
            ]);
            let mut result = ptr::null();
            let status =
                unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &raw mut result) };
            if status == errSecItemNotFound {
                return Err(SecretStoreError::NotFound);
            }
            if status != errSecSuccess || result.is_null() {
                return Err(SecretStoreError::Platform(status));
            }
            let value = unsafe { CFType::wrap_under_create_rule(result) };
            let data = value
                .downcast::<CFData>()
                .ok_or(SecretStoreError::InvalidSecret)?;
            Ok(Zeroizing::new(data.bytes().to_vec()))
        }

        pub(super) fn delete_platform(
            &self,
            registration_id: &str,
        ) -> Result<(), SecretStoreError> {
            let _ = self;
            let class_key = unsafe { CFString::wrap_under_get_rule(kSecClass) };
            let class_value = unsafe { CFString::wrap_under_get_rule(kSecClassGenericPassword) };
            let service_key = unsafe { CFString::wrap_under_get_rule(kSecAttrService) };
            let account_key = unsafe { CFString::wrap_under_get_rule(kSecAttrAccount) };
            let service = CFString::new(SERVICE);
            let account = CFString::new(registration_id);
            let query = CFDictionary::from_CFType_pairs(&[
                (class_key.as_CFType(), class_value.as_CFType()),
                (service_key.as_CFType(), service.as_CFType()),
                (account_key.as_CFType(), account.as_CFType()),
            ]);
            let status = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
            if status == errSecSuccess || status == errSecItemNotFound {
                Ok(())
            } else {
                Err(SecretStoreError::Platform(status))
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::slice;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;
    use zeroize::{Zeroize as _, Zeroizing};

    use super::{SecretStoreError, SystemSecretStore};

    const MAX_PROTECTED_SECRET_BYTES: u64 = 16 * 1024;

    impl SystemSecretStore {
        fn path(&self, registration_id: &str) -> std::path::PathBuf {
            self.root.join(format!("{registration_id}.dpapi"))
        }

        pub(super) fn put_platform(
            &self,
            registration_id: &str,
            secret: &[u8],
        ) -> Result<(), SecretStoreError> {
            let protected = protect(secret, registration_id.as_bytes())?;
            let path = self.path(registration_id);
            let mut file = crate::private_fs::create_private_file(&path).map_err(|error| {
                match error.kind() {
                    std::io::ErrorKind::AlreadyExists => SecretStoreError::AlreadyExists,
                    _ => SecretStoreError::Io(error),
                }
            })?;
            std::io::Write::write_all(&mut file, &protected).map_err(SecretStoreError::Io)?;
            file.sync_all().map_err(SecretStoreError::Io)
        }

        pub(super) fn get_platform(
            &self,
            registration_id: &str,
        ) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
            let path = self.path(registration_id);
            crate::private_fs::validate_private_regular_file(&path, MAX_PROTECTED_SECRET_BYTES)
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => SecretStoreError::NotFound,
                    _ => SecretStoreError::Io(error),
                })?;
            let protected = std::fs::read(path).map_err(SecretStoreError::Io)?;
            unprotect(&protected, registration_id.as_bytes())
        }

        pub(super) fn delete_platform(
            &self,
            registration_id: &str,
        ) -> Result<(), SecretStoreError> {
            crate::private_fs::remove_private_file(&self.path(registration_id))
                .map_err(SecretStoreError::Io)
        }
    }

    fn protect(secret: &[u8], entropy: &[u8]) -> Result<Vec<u8>, SecretStoreError> {
        let input = blob(secret)?;
        let entropy = blob(entropy)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &raw const input,
                PCWSTR::null(),
                Some(&raw const entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut output,
            )
        }
        .map_err(|_| SecretStoreError::Platform(windows::core::Error::from_thread().code().0))?;
        copy_and_free(output)
    }

    fn unprotect(protected: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
        let input = blob(protected)?;
        let entropy = blob(entropy)?;
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(
                &raw const input,
                None,
                Some(&raw const entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &raw mut output,
            )
        }
        .map_err(|_| SecretStoreError::Platform(windows::core::Error::from_thread().code().0))?;
        let mut copied = copy_and_free(output)?;
        if copied.len() != 32 {
            copied.zeroize();
            return Err(SecretStoreError::InvalidSecret);
        }
        Ok(Zeroizing::new(copied))
    }

    fn blob(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, SecretStoreError> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(bytes.len()).map_err(|_| SecretStoreError::InvalidSecret)?,
            pbData: bytes.as_ptr().cast_mut(),
        })
    }

    fn copy_and_free(output: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, SecretStoreError> {
        if output.pbData.is_null() || output.cbData == 0 {
            return Err(SecretStoreError::InvalidSecret);
        }
        let bytes = unsafe {
            slice::from_raw_parts(output.pbData, usize::try_from(output.cbData).unwrap_or(0))
        }
        .to_vec();
        let _ = unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
        Ok(bytes)
    }
}

impl SecretStore for SystemSecretStore {
    fn put(&self, registration_id: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        if secret.len() != 32 {
            return Err(SecretStoreError::InvalidSecret);
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.put_platform(registration_id, secret)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = registration_id;
            Err(SecretStoreError::UnsupportedPlatform)
        }
    }

    fn get(&self, registration_id: &str) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let secret = self.get_platform(registration_id)?;
            if secret.len() != 32 {
                return Err(SecretStoreError::InvalidSecret);
            }
            Ok(secret)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = registration_id;
            Err(SecretStoreError::UnsupportedPlatform)
        }
    }

    fn delete(&self, registration_id: &str) -> Result<(), SecretStoreError> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.delete_platform(registration_id)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = registration_id;
            Err(SecretStoreError::UnsupportedPlatform)
        }
    }
}

/// Protected secret-store failure without secret contents or storage paths.
#[derive(Debug, Error)]
pub enum SecretStoreError {
    /// A value already exists and was not overwritten.
    #[error("registration secret already exists")]
    AlreadyExists,
    /// No protected value exists for the registration.
    #[error("registration secret was not found")]
    NotFound,
    /// Protected data was malformed or had an unexpected size.
    #[error("registration secret is invalid")]
    InvalidSecret,
    /// Native credential storage failed with a non-sensitive status code.
    #[error("platform credential storage failed with status {0}")]
    Platform(i32),
    /// Credential storage is not available on this build target.
    #[error("platform credential storage is unsupported")]
    UnsupportedPlatform,
    /// Owner-private filesystem operation failed.
    #[error("credential storage filesystem operation failed: {0}")]
    Io(#[source] std::io::Error),
}
