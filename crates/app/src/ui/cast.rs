//! The player bar's 投放 menu and the Sonos settings (`CastState`).

use client_core::{CastHiRes, CastOpus, CastRoom, Command, Commander, Settings};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, CastRoomRow, CastState};

/// What the core last said about rooms and the output.
#[derive(Default)]
pub(super) struct CastView {
    rooms: Vec<CastRoom>,
    output: Option<CastRoom>,
    searching: bool,
    error: Option<String>,
}

fn row(room: &CastRoom) -> CastRoomRow {
    CastRoomRow {
        id: room.id.clone().into(),
        name: room.name.clone().into(),
        detail: room.detail.clone().into(),
    }
}

/// At most `max` characters, with an ellipsis.
fn short(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_owned();
    }
    let mut s: String = name.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

pub(super) fn install(window: &AppWindow, commander: &Commander, settings: &Settings) {
    let g = window.global::<CastState>();
    g.set_hires(match settings.cast.hires {
        CastHiRes::Mp3 => 0,
        CastHiRes::Flac48 => 1,
        CastHiRes::Original => 2,
    });
    g.set_opus(match settings.cast.opus {
        CastOpus::Mp3 => 0,
        CastOpus::Flac => 1,
    });
    g.set_hosts(settings.cast.hosts.clone().into());
    let c = commander.clone();
    g.on_refresh(move || c.send(Command::DiscoverCast));
    let c = commander.clone();
    g.on_select(move |id| {
        let id = (!id.is_empty()).then(|| id.to_string());
        c.send(Command::SetCastOutput(id));
    });
    let c = commander.clone();
    let weak = window.as_weak();
    g.on_options_changed(move || {
        let Some(w) = weak.upgrade() else { return };
        let g = w.global::<CastState>();
        c.send(Command::SetCastOptions {
            hires: match g.get_hires() {
                1 => CastHiRes::Flac48,
                2 => CastHiRes::Original,
                _ => CastHiRes::Mp3,
            },
            opus: if g.get_opus() == 1 {
                CastOpus::Flac
            } else {
                CastOpus::Mp3
            },
            hosts: g.get_hosts().to_string(),
        });
    });
}

impl CastView {
    pub(super) fn rooms(&mut self, searching: bool, rooms: Vec<CastRoom>, error: Option<String>) {
        self.searching = searching;
        self.rooms = rooms;
        self.error = error;
    }

    pub(super) fn output(&mut self, output: Option<CastRoom>) {
        self.output = output;
    }

    pub(super) fn show(&self, w: &AppWindow) {
        let g = w.global::<CastState>();
        let mut rows: Vec<CastRoomRow> = self.rooms.iter().map(row).collect();
        // The chosen room stays listed before a search found it.
        if let Some(out) = &self.output
            && !self.rooms.iter().any(|r| r.id == out.id)
        {
            rows.insert(0, row(out));
        }
        g.set_rooms(ModelRc::new(VecModel::from(rows)));
        g.set_searching(self.searching);
        g.set_output_id(
            self.output
                .as_ref()
                .map(|r| r.id.clone())
                .unwrap_or_default()
                .into(),
        );
        g.set_output_label(
            self.output
                .as_ref()
                .map(|r| short(&r.name, 8))
                .unwrap_or_default()
                .into(),
        );
        let hint = if !self.rooms.is_empty() {
            String::new()
        } else if self.searching {
            "正在搜索局域网中的 Sonos…".to_owned()
        } else if let Some(e) = &self.error {
            format!("搜索失败：{e}")
        } else {
            "未发现 Sonos 房间。请确认电脑和音箱在同一局域网；仍找不到时在设置中填写音箱地址。"
                .to_owned()
        };
        g.set_hint(hint.into());
    }
}

#[cfg(test)]
mod tests {
    use super::short;

    #[test]
    fn shortens_room_names() {
        assert_eq!(short("客厅", 8), "客厅");
        assert_eq!(short("Living Room + Kitchen", 8), "Living …");
    }
}
