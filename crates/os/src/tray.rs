//! Tray icon with the Electron client's menu (Windows; Linux needs gtk and
//! is not done yet). Menu and icon events arrive on the UI thread's
//! message loop, which the Slint (winit) event loop pumps.

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Left click on the icon or "打开主窗口".
    Show,
    TogglePause,
    Next,
    Previous,
    Stop,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
}

impl Tray {
    /// `rgba` is the icon (`width` x `height`). `on_action` may run on any thread.
    pub fn new(
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        on_action: impl Fn(TrayAction) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let items = [
            (
                MenuItem::new("播放 / 暂停", true, None),
                TrayAction::TogglePause,
            ),
            (MenuItem::new("下一首", true, None), TrayAction::Next),
            (MenuItem::new("上一首", true, None), TrayAction::Previous),
            (MenuItem::new("停止", true, None), TrayAction::Stop),
            (MenuItem::new("打开主窗口", true, None), TrayAction::Show),
            (MenuItem::new("退出", true, None), TrayAction::Quit),
        ];
        let separator = PredefinedMenuItem::separator();
        let menu = Menu::with_items(&[
            &items[0].0,
            &items[1].0,
            &items[2].0,
            &items[3].0,
            &separator,
            &items[4].0,
            &items[5].0,
        ])
        .map_err(|e| e.to_string())?;
        let ids: Vec<(MenuId, TrayAction)> = items
            .iter()
            .map(|(item, action)| (item.id().clone(), *action))
            .collect();

        let icon = Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("032player")
            .with_icon(icon)
            .build()
            .map_err(|e| e.to_string())?;

        let on_action = std::sync::Arc::new(on_action);
        let menu_action = on_action.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = ids.iter().find(|(id, _)| *id == event.id) {
                menu_action(*action);
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                on_action(TrayAction::Show);
            }
        }));
        Ok(Self { icon: tray })
    }

    /// Hover text, e.g. the current track.
    pub fn set_tooltip(&self, text: &str) {
        let _ = self.icon.set_tooltip(Some(text));
    }
}
