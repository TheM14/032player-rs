//! Secret storage for the sign-in tokens. The app injects the system
//! credential store; tests use [`MemorySecrets`].

use std::collections::HashMap;
use std::sync::Mutex;

/// A small string key-value store for secrets. Calls may block (D-Bus,
/// Credential Manager): the core only calls them on blocking threads.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` when the key has no value.
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    /// Deleting a missing key is not an error.
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// In-memory store: tests, and the fallback when no system store exists.
#[derive(Debug, Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

impl MemorySecrets {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.map().get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.map().insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.map().remove(key);
        Ok(())
    }
}

/// Which of the two tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Api,
    Media,
}

/// Secret key of a token for one server (normalized base URL).
pub(crate) fn token_key(server: &str, kind: TokenKind) -> String {
    let name = match kind {
        TokenKind::Api => "api-token",
        TokenKind::Media => "media-token",
    };
    format!("{name}@{server}")
}
