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
}
