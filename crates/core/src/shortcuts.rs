//! Window-local shortcut bindings, independent of the UI backend.
use serde::{Deserialize, Serialize};

pub const ACTIONS: &[(&str, &str, &str)] = &[
    ("playPause", "播放 / 暂停", "Space"),
    ("previous", "上一首", "Ctrl+Left"),
    ("next", "下一首", "Ctrl+Right"),
    ("seekBack", "后退 10 秒", "Left"),
    ("seekForward", "前进 10 秒", "Right"),
    ("volumeUp", "增加音量", "Ctrl+Up"),
    ("volumeDown", "降低音量", "Ctrl+Down"),
    ("shuffle", "切换随机播放", "Ctrl+S"),
    ("repeat", "切换循环模式", "Ctrl+R"),
    ("queue", "显示 / 隐藏队列", "Ctrl+Q"),
    ("search", "搜索曲库", "Ctrl+K"),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shortcuts {
    pub bindings: std::collections::BTreeMap<String, String>,
}
impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            bindings: ACTIONS
                .iter()
                .map(|(id, _, key)| ((*id).into(), (*key).into()))
                .collect(),
        }
    }
}
impl Shortcuts {
    pub fn binding(&self, id: &str) -> &str {
        self.bindings.get(id).map_or("", String::as_str)
    }
    pub fn set(&mut self, id: &str, key: String) -> bool {
        if !ACTIONS.iter().any(|(a, _, _)| *a == id)
            || (!key.is_empty() && self.bindings.iter().any(|(a, k)| a != id && *k == key))
        {
            return false;
        }
        self.bindings.insert(id.into(), key);
        true
    }
    pub fn action(&self, key: &str) -> Option<&str> {
        ACTIONS
            .iter()
            .find_map(|(id, _, _)| (!key.is_empty() && self.binding(id) == key).then_some(*id))
    }

    /// Populates defaults for actions introduced after an older `settings.json`
    /// was saved, without overwriting user overrides or intentional clears (`""`).
    pub fn merge_defaults(&mut self) {
        for &(id, _, default_key) in ACTIONS {
            if self.bindings.contains_key(id) {
                continue;
            }
            let conflict =
                !default_key.is_empty() && self.bindings.values().any(|k| k == default_key);
            let value = if conflict {
                String::new()
            } else {
                default_key.to_owned()
            };
            self.bindings.insert(id.to_owned(), value);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_conflicts_clear_and_roundtrip() {
        let mut keys = Shortcuts::default();
        assert_eq!(keys.action("Space"), Some("playPause"));
        assert!(!keys.set("next", "Space".into()));
        assert!(keys.set("playPause", String::new()));
        assert!(keys.set("next", "Space".into()));
        assert_eq!(keys.action("Space"), Some("next"));
        assert_eq!(keys.action(""), None);
        assert!(!keys.set("unknown", "X".into()));
        assert_eq!(
            serde_json::from_str::<Shortcuts>(&serde_json::to_string(&keys).unwrap()).unwrap(),
            keys
        );
        assert_eq!(
            serde_json::from_str::<Shortcuts>("{}").unwrap(),
            Shortcuts::default()
        );
    }

    #[test]
    fn merge_defaults_fills_missing_actions_and_respects_clears_and_conflicts() {
        let mut keys: Shortcuts =
            serde_json::from_str(r#"{"bindings":{"playPause":"","next":"Ctrl+K"}}"#).unwrap();
        keys.merge_defaults();
        // Explicitly cleared action stays empty.
        assert_eq!(keys.binding("playPause"), "");
        // Missing non-conflicting action receives its default.
        assert_eq!(keys.binding("previous"), "Ctrl+Left");
        // Missing action whose default is already taken by another action stays empty.
        assert_eq!(keys.binding("search"), "");
        assert_eq!(keys.binding("next"), "Ctrl+K");
    }
}
