//! Secrets in the system credential store: Windows Credential Manager,
//! macOS Keychain, Secret Service on Linux. Calls can block.

const SERVICE: &str = "032player";

/// Credential store entries under the `032player` service.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keyring;

impl Keyring {
    fn entry(key: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, key).map_err(|e| e.to_string())
    }

    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        match Self::entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        Self::entry(key)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    pub fn delete(&self, key: &str) -> Result<(), String> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
