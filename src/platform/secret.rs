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

#[cfg(not(target_os = "macos"))]
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
