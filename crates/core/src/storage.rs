//! Small JSON key-value file (playback resume snapshot, client id).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use session::KeyValueStorage;

pub struct FileStorage {
    path: PathBuf,
    map: BTreeMap<String, String>,
}

impl FileStorage {
    /// Loads `path`; a missing or corrupt file starts empty.
    pub fn open(path: PathBuf) -> Self {
        let map = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, map }
    }

    fn save(&self) {
        let Ok(bytes) = serde_json::to_vec_pretty(&self.map) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("tmp");
        if fs::write(&tmp, bytes).is_ok() && fs::rename(&tmp, &self.path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

impl KeyValueStorage for FileStorage {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    fn set(&mut self, key: &str, value: &str) {
        if self.map.get(key).map(String::as_str) != Some(value) {
            self.map.insert(key.to_owned(), value.to_owned());
            self.save();
        }
    }

    fn remove(&mut self, key: &str) {
        if self.map.remove(key).is_some() {
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_across_instances() {
        let dir = std::env::temp_dir().join(format!("032player-storage-{}", std::process::id()));
        let path = dir.join("playback.json");
        let _ = fs::remove_dir_all(&dir);

        let mut s = FileStorage::open(path.clone());
        assert_eq!(s.get("a"), None);
        s.set("a", "1");
        s.set("b", "2");
        s.remove("b");

        let s2 = FileStorage::open(path.clone());
        assert_eq!(s2.get("a").as_deref(), Some("1"));
        assert_eq!(s2.get("b"), None);

        fs::write(&path, b"not json").unwrap();
        assert_eq!(FileStorage::open(path).get("a"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
