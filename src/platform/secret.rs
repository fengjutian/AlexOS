//! Platform credential storage. Secrets must never be persisted as plaintext
//! by callers; unavailable backends fail closed.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretStoreError {
    #[error("platform secret store is unavailable")]
    Unsupported,
    #[error("platform secret store failed: {0}")]
    Failed(String),
}

pub trait SecretStore: Send + Sync {
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), SecretStoreError>;
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    fn delete(&self, service: &str, account: &str) -> Result<bool, SecretStoreError>;
}

#[derive(Debug, Clone, Copy)]
pub struct NativeSecretStore;

#[cfg(target_os = "macos")]
impl SecretStore for NativeSecretStore {
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        validate_key(service, account)?;
        security_framework::passwords::set_generic_password(service, account, secret)
            .map_err(|error| SecretStoreError::Failed(error.to_string()))
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        validate_key(service, account)?;
        match security_framework::passwords::get_generic_password(service, account) {
            Ok(secret) => Ok(Some(secret)),
            Err(error) if error.code() == -25300 => Ok(None), // errSecItemNotFound
            Err(error) => Err(SecretStoreError::Failed(error.to_string())),
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, SecretStoreError> {
        validate_key(service, account)?;
        match security_framework::passwords::delete_generic_password(service, account) {
            Ok(()) => Ok(true),
            Err(error) if error.code() == -25300 => Ok(false),
            Err(error) => Err(SecretStoreError::Failed(error.to_string())),
        }
    }
}

#[cfg(target_os = "windows")]
impl SecretStore for NativeSecretStore {
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        use windows::{
            Win32::Security::Credentials::{
                CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
                CREDENTIALW, CredWriteW,
            },
            core::PWSTR,
        };
        validate_key(service, account)?;
        if secret.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err(SecretStoreError::Failed(
                "secret exceeds Windows Credential Manager limit".into(),
            ));
        }
        let mut target = wide(&target_name(service, account));
        let mut username = wide(account);
        let mut blob = secret.to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(username.as_mut_ptr()),
            ..Default::default()
        };
        // SAFETY: all pointers reference live buffers for the duration of the
        // synchronous call; CredWriteW copies the credential data.
        unsafe { CredWriteW(&credential, 0) }
            .map_err(|error| SecretStoreError::Failed(error.to_string()))
    }

    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        use windows::Win32::Security::Credentials::{
            CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
        };
        use windows::core::PCWSTR;
        validate_key(service, account)?;
        let target = wide(&target_name(service, account));
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: the output pointer is initialized by CredReadW and released
        // with CredFree on every successful call.
        match unsafe {
            CredReadW(
                PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                None,
                &mut credential,
            )
        } {
            Ok(()) => {
                if credential.is_null() {
                    return Err(SecretStoreError::Failed(
                        "Credential Manager returned a null credential".into(),
                    ));
                }
                // SAFETY: CredentialBlob is owned by the returned credential
                // and remains valid until CredFree below.
                let secret = unsafe {
                    let value = &*credential;
                    std::slice::from_raw_parts(
                        value.CredentialBlob,
                        value.CredentialBlobSize as usize,
                    )
                    .to_vec()
                };
                // SAFETY: credential was allocated by CredReadW.
                unsafe { CredFree(credential.cast()) };
                Ok(Some(secret))
            }
            Err(error) if error.code().0 == -2147023728 => Ok(None), // HRESULT_FROM_WIN32(ERROR_NOT_FOUND)
            Err(error) => Err(SecretStoreError::Failed(error.to_string())),
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, SecretStoreError> {
        use windows::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
        use windows::core::PCWSTR;
        validate_key(service, account)?;
        let target = wide(&target_name(service, account));
        // SAFETY: target is a live, NUL-terminated UTF-16 buffer.
        match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(true),
            Err(error) if error.code().0 == -2147023728 => Ok(false),
            Err(error) => Err(SecretStoreError::Failed(error.to_string())),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl SecretStore for NativeSecretStore {
    fn set(&self, _: &str, _: &str, _: &[u8]) -> Result<(), SecretStoreError> {
        Err(SecretStoreError::Unsupported)
    }
    fn get(&self, _: &str, _: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Err(SecretStoreError::Unsupported)
    }
    fn delete(&self, _: &str, _: &str) -> Result<bool, SecretStoreError> {
        Err(SecretStoreError::Unsupported)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn validate_key(service: &str, account: &str) -> Result<(), SecretStoreError> {
    let valid = |value: &str| !value.is_empty() && value.len() <= 255 && !value.contains('\0');
    if valid(service) && valid(account) {
        Ok(())
    } else {
        Err(SecretStoreError::Failed(
            "invalid service or account name".into(),
        ))
    }
}

#[cfg(target_os = "windows")]
fn target_name(service: &str, account: &str) -> String {
    format!("AlexOS:{service}:{account}")
}

#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn native() -> NativeSecretStore {
    NativeSecretStore
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secret_keys_are_bounded_and_cannot_contain_nul() {
        assert!(validate_key("com.alex.runtime", "app").is_ok());
        assert!(validate_key("", "app").is_err());
        assert!(validate_key("service", "bad\0account").is_err());
    }
}
