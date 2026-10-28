//! Renders `crates/app/ui/app.slint` with fake data to PNG files.
//!
//! cargo run --manifest-path tools/ui-snapshot/Cargo.toml -- <out-dir>

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{
    Model,
    ComponentHandle, Image, ModelRc, PhysicalSize, Rgba8Pixel, SharedPixelBuffer, SharedString,
    VecModel,
};
use slint_interpreter::{Compiler, ComponentInstance, Struct, Value};

#[derive(Clone, Copy, Default)]
struct Px(Rgba8Pixel);

impl TargetPixel for Px {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = u16::from(255 - c.alpha);
        let mix = |dst: u8, src: u8| (u16::from(src) + u16::from(dst) * a / 255) as u8;
        self.0.r = mix(self.0.r, c.red);
        self.0.g = mix(self.0.g, c.green);
        self.0.b = mix(self.0.b, c.blue);
        self.0.a = 255;
    }

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Px(Rgba8Pixel { r, g, b, a: 255 })
    }
}

struct SnapshotPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for SnapshotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn s(v: &str) -> Value {
    Value::String(SharedString::from(v))
}

fn cover(seed: u32) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(64, 64);
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i % 64) as u32, (i / 64) as u32);
        *p = Rgba8Pixel {
            r: ((x * 3 + seed * 40) % 256) as u8,
            g: ((y * 2 + seed * 70) % 256) as u8,
            b: (160 + seed * 13 % 90) as u8,
            a: 255,
        };
    }
    Image::from_rgba8(buf)
}

fn model(values: Vec<Value>) -> Value {
    Value::Model(ModelRc::new(VecModel::from(values)))
}

fn album_rows(columns: usize) -> Value {
    album_rows_n(columns, 24)
}

fn album_rows_n(columns: usize, count: usize) -> Value {
    let titles = [
        ("ハルジオン", "YOASOBI", "2020"),
        ("我的歌声里", "曲婉婷", "2012"),
        (
            "Night Flight with a Very Long Album Title",
            "Various Artists",
            "1999",
        ),
        ("君の名は。", "RADWIMPS", "2016"),
        ("Random Access Memories", "Daft Punk", "2013"),
        ("十一月的萧邦", "周杰伦", "2005"),
        ("Untitled", "", ""),
        ("CLANNAD ORIGINAL SOUNDTRACK", "麻枝准 / 戸越まごめ", "2004"),
    ];
    let cards: Vec<Value> = (0..count)
        .map(|i| {
            let (title, artist, year) = titles[i % titles.len()];
            let mut card = Struct::default();
            card.set_field("id".into(), Value::Number(i as f64));
            card.set_field("title".into(), s(title));
            card.set_field("artist".into(), s(artist));
            card.set_field("year".into(), s(year));
            card.set_field("artist-id".into(), Value::Number(5.0));
            card.set_field("favorite".into(), Value::Bool(i % 3 == 0));
            card.set_field("badge".into(), s(if i % 7 == 2 { "原声集 OST" } else { "" }));
            card.set_field("note".into(), s(if i % 4 == 1 { "OP 1 · ED 2" } else { "" }));
            card.set_field("playing".into(), Value::Bool(i == 1));
            card.set_field(
                "cover".into(),
                Value::Image(if i % 5 == 4 {
                    Image::default()
                } else {
                    cover(i as u32)
                }),
            );
            Value::Struct(card)
        })
        .collect();
    model(
        cards
            .chunks(columns)
            .map(|chunk| {
                let mut row = Struct::default();
                row.set_field("items".into(), model(chunk.to_vec()));
                Value::Struct(row)
            })
            .collect(),
    )
}

fn tracks() -> Value {
    let rows = [
        ("夜に駆ける", "YOASOBI", "THE BOOK", "4:21"),
        ("晴天", "周杰伦", "叶惠美", "4:29"),
        (
            "Get Lucky (feat. Pharrell Williams & Nile Rodgers)",
            "Daft Punk",
            "Random Access Memories",
            "6:09",
        ),
        (
            "だんご大家族",
            "茶太",
            "CLANNAD ORIGINAL SOUNDTRACK",
            "3:38",
        ),
    ];
    model(
        (0..40)
            .map(|i| {
                let (t, a, al, d) = rows[i % rows.len()];
                let mut row = Struct::default();
                row.set_field("id".into(), Value::Number(i as f64 + 100.0));
                row.set_field("index".into(), Value::Number(i as f64));
                row.set_field("title".into(), s(t));
                row.set_field("artist".into(), s(a));
                row.set_field("album".into(), s(al));
                row.set_field("duration".into(), s(d));
                row.set_field("favorite".into(), Value::Bool(i % 4 == 1));
                row.set_field("number".into(), s(&format!("1-{:02}", i + 1)));
                row.set_field("album-id".into(), Value::Number(3.0));
                row.set_field("artist-id".into(), Value::Number(5.0));
                row.set_field("tag".into(), s(""));
                row.set_field("tag-accent".into(), Value::Bool(false));
                row.set_field("note".into(), s(""));
                row.set_field("disabled".into(), Value::Bool(false));
                row.set_field("blocked".into(), Value::Bool(false));
                Value::Struct(row)
            })
            .collect(),
    )
}

fn strings(values: &[&str]) -> Value {
    model(values.iter().map(|v| s(v)).collect())
}

fn bools(values: &[bool]) -> Value {
    model(values.iter().map(|v| Value::Bool(*v)).collect())
}

fn list_query(sort: f64, desc: bool) -> Value {
    let mut q = Struct::default();
    q.set_field("sort".into(), Value::Number(sort));
    q.set_field("desc".into(), Value::Bool(desc));
    q.set_field("search".into(), s(""));
    q.set_field("year-from".into(), s(""));
    q.set_field("year-to".into(), s(""));
    Value::Struct(q)
}

fn detail(
    kind: f64,
    title: &str,
    subtitle: &str,
    meta: &str,
    quality: &str,
    description: &str,
) -> Value {
    let mut d = Struct::default();
    d.set_field("kind".into(), Value::Number(kind));
    d.set_field("id".into(), Value::Number(1.0));
    d.set_field("title".into(), s(title));
    d.set_field("subtitle".into(), s(subtitle));
    d.set_field("subtitle-id".into(), Value::Number(5.0));
    d.set_field("meta".into(), s(meta));
    d.set_field("quality".into(), s(quality));
    d.set_field("description".into(), s(description));
    d.set_field("favorite".into(), Value::Bool(kind == 0.0));
    d.set_field("custom-artwork".into(), Value::Bool(false));
    Value::Struct(d)
}

fn queue() -> Value {
    let rows = [
        ("夜に駆ける", "YOASOBI", "4:21"),
        ("晴天", "周杰伦", "4:29"),
        (
            "Get Lucky (feat. Pharrell Williams & Nile Rodgers)",
            "Daft Punk",
            "6:09",
        ),
        ("だんご大家族", "茶太", "3:38"),
    ];
    model(
        (0..12)
            .map(|i| {
                let (t, a, d) = rows[i % rows.len()];
                let mut row = Struct::default();
                row.set_field("uid".into(), Value::Number(i as f64 + 1.0));
                row.set_field("title".into(), s(t));
                row.set_field("artist".into(), s(a));
                row.set_field("duration".into(), s(d));
                Value::Struct(row)
            })
            .collect(),
    )
}

fn lyrics() -> Value {
    let lines = [
        "沈むように溶けてゆくように",
        "二人だけの空が広がる夜に",
        "「さよなら」だけだった",
        "その一言で全てが分かった",
        "日が沈み出した空と君の姿",
        "フェンス越しに重なっていた",
        "初めて会った日から",
        "僕の心の全てを奪った",
    ];
    model(
        lines
            .iter()
            .map(|l| {
                let mut row = Struct::default();
                row.set_field("text".into(), s(l));
                row.set_field("translation".into(), s(""));
                row.set_field("ruby".into(), model(vec![]));
                row.set_field("romaji".into(), s(""));
                Value::Struct(row)
            })
            .collect(),
    )
}

fn st(fields: &[(&str, Value)]) -> Value {
    let mut v = Struct::default();
    for (k, val) in fields {
        v.set_field((*k).into(), val.clone());
    }
    Value::Struct(v)
}

fn n(v: f64) -> Value {
    Value::Number(v)
}

fn home_rows(kind: &str) -> Value {
    let rows = [
        ("夜に駆ける", "YOASOBI · THE BOOK", "OP", "进击的巨人 The Final Season"),
        ("春日影", "MyGO!!!!! · 迷跡波", "ED", "BanG Dream! It's MyGO!!!!!"),
        ("only my railgun", "fripSide · infinite synthesis", "插曲", "某科学的超电磁炮"),
        ("だんご大家族", "茶太 · CLANNAD ORIGINAL SOUNDTRACK", "主题曲", "CLANNAD"),
        ("晴天", "周杰伦 · 叶惠美", "OP", "不能说的秘密"),
    ];
    model(
        rows.iter()
            .enumerate()
            .map(|(i, (t, sub, role, work))| {
                let mut f = vec![
                    ("album-id", n(3.0)),
                    ("artist-id", n(5.0)),
                    ("favorite", Value::Bool(false)),
                    ("tag", s("")),
                    ("tag-accent", Value::Bool(false)),
                    ("link", s("")),
                    ("link-id", n(0.0)),
                    ("note", s("")),
                    ("dimmed", Value::Bool(false)),
                    ("index", n(i as f64)),
                    ("id", n(200.0 + i as f64)),
                    ("title", s(t)),
                    ("subtitle", s(sub)),
                    ("cover", Value::Image(cover(i as u32 + 2))),
                    ("percent", n(-1.0)),
                    ("duration", s("4:21")),
                ];
                match kind {
                    "resume" => {
                        f.push(("percent", n(35.0 + 20.0 * i as f64)));
                        f.push(("duration", s("1:35 / 4:00")));
                        f.push(("note", s("2026年10月27日 21:14")));
                    }
                    "songs" => {
                        f.push(("tag", s(role)));
                        f.push(("tag-accent", Value::Bool(*role == "OP" || *role == "ED")));
                        f.push(("link", s(work)));
                        f.push(("link-id", n(3.0)));
                    }
                    "mix" => {
                        f.push(("subtitle", s("安静 · 流行 · 30 首 · 1 小时 52 分")));
                        f.push(("dimmed", Value::Bool(i == 2)));
                    }
                    _ => f.push(("note", s("2026年10月28日 09:15"))),
                }
                st(&f)
            })
            .collect(),
    )
}

fn set_g(inst: &ComponentInstance, global: &str, name: &str, value: Value) {
    inst.set_global_property(global, name, value)
        .unwrap_or_else(|e| panic!("set {global}.{name}: {e:?}"));
}

fn set(inst: &ComponentInstance, name: &str, value: Value) {
    inst.set_property(name, value)
        .unwrap_or_else(|e| panic!("set {name}: {e:?}"));
}

fn render(window: &MinimalSoftwareWindow, size: PhysicalSize, path: &str) {
    window.set_size(size);
    // Let layouts, `init` and animations settle.
    for _ in 0..3 {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|_| {});
    }
    std::thread::sleep(std::time::Duration::from_millis(450));
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut buf = vec![Px::default(); w * h];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buf, w);
    });
    let bytes: Vec<u8> = buf
        .iter()
        .flat_map(|p| [p.0.r, p.0.g, p.0.b, 255])
        .collect();
    image::RgbaImage::from_raw(size.width, size.height, bytes)
        .unwrap()
        .save(path)
        .unwrap();
    println!("wrote {path}");
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp".into());
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/app/ui/app.slint");

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform(window.clone()))).unwrap();

    let mut compiler = Compiler::default();
    compiler.set_style("fluent-dark".into());
    let result = spin_on::spin_on(compiler.build_from_path(root));
    for d in result.diagnostics() {
        eprintln!("{d}");
    }
    let def = result.component("AppWindow").expect("AppWindow");
    let size = PhysicalSize::new(1280, 800);

    // Login.
    let inst = def.create().unwrap();
    inst.show().unwrap();
    set(&inst, "login-server", s("https://music.example.com"));
    set(&inst, "login-error", s("API Token 已失效，请重新输入"));
    set(&inst, "login-media-token-saved", Value::Bool(true));
    render(&window, size, &format!("{out}/login.png"));
    inst.hide().unwrap();

    // Main views.
    let inst = def.create().unwrap();
    inst.show().unwrap();
    set(&inst, "logged-in", Value::Bool(true));
    set(&inst, "server-label", s("https://music.example.com"));
    set(&inst, "browse", Value::Bool(true));
    set(
        &inst,
        "album-sorts",
        strings(&[
            "名称",
            "最近添加",
            "最近播放",
            "发行日期",
            "发行年份",
            "随机",
        ]),
    );
    set(
        &inst,
        "album-sort-desc",
        bools(&[false, true, true, true, true, false]),
    );
    set(
        &inst,
        "track-sorts",
        strings(&[
            "专辑",
            "名称",
            "播放次数",
            "最近播放",
            "发行日期",
            "发行年份",
            "最近添加",
            "时长",
            "随机",
        ]),
    );
    set(
        &inst,
        "track-sort-desc",
        bools(&[false, false, true, true, true, true, true, false, false]),
    );
    set(
        &inst,
        "artist-sorts",
        strings(&["名称", "专辑数", "曲目数", "随机"]),
    );
    set(
        &inst,
        "artist-sort-desc",
        bools(&[false, true, true, false]),
    );
    set(
        &inst,
        "playlist-sorts",
        strings(&["名称", "时长", "曲目数"]),
    );
    set(&inst, "playlist-sort-desc", bools(&[false, true, true]));
    set(&inst, "q-albums", list_query(1.0, true));
    set(&inst, "q-tracks", list_query(6.0, true));
    set(&inst, "q-artists", list_query(0.0, false));
    set(&inst, "q-playlists", list_query(0.0, false));
    set(&inst, "q-fav-tracks", list_query(1.0, false));
    set(&inst, "album-filter", s("艺人：YOASOBI"));
    // Columns for 1280 - 200 sidebar - 320 lyrics = 760px.
    set(&inst, "album-rows", album_rows(3));
    set(&inst, "album-loaded", Value::Number(500.0));
    set(&inst, "album-total", Value::Number(1234.0));
    set(&inst, "tracks", tracks());
    set(&inst, "track-total", Value::Number(10000.0));
    set(&inst, "has-track", Value::Bool(true));
    set(&inst, "playing-id", Value::Number(101.0));
    set(&inst, "np-title", s("夜に駆ける"));
    set(&inst, "np-artist", s("YOASOBI"));
    set(&inst, "np-quality", s("FLAC / 24bit / 48kHz"));
    set(&inst, "np-favorite", Value::Bool(true));
    set(&inst, "np-cover", Value::Image(cover(3)));
    set(&inst, "position-ms", Value::Number(83_000.0));
    set(&inst, "duration-ms", Value::Number(261_000.0));
    set(&inst, "position-text", s("1:23"));
    set(&inst, "duration-text", s("4:21"));
    set(&inst, "paused", Value::Bool(false));
    set(&inst, "volume", Value::Number(70.0));
    set(&inst, "lyrics", lyrics());
    set(&inst, "lyrics-synced", Value::Bool(true));
    set(&inst, "lyrics-active", Value::Number(2.0));
    set(&inst, "lyrics-source", s("LRCLIB"));
    render(&window, size, &format!("{out}/albums.png"));

    set(&inst, "page", Value::Number(1.0));
    set(&inst, "notice", s("播放失败：loading failed"));
    render(&window, size, &format!("{out}/tracks.png"));

    set(&inst, "notice", s(""));
    set(&inst, "queue", queue());
    set(&inst, "queue-current", Value::Number(1.0));
    set(&inst, "show-queue", Value::Bool(true));
    set(&inst, "shuffle", Value::Bool(true));
    set(&inst, "repeat", Value::Number(2.0));
    set(&inst, "sleep-active", Value::Bool(true));
    set(&inst, "sleep-label", s("29:41"));
    render(&window, size, &format!("{out}/queue.png"));
    set(&inst, "show-queue", Value::Bool(false));

    // Library pages.
    set(&inst, "show-lyrics", Value::Bool(false));
    set(&inst, "artist-rows", album_rows(5));
    set(&inst, "artist-total", Value::Number(321.0));
    set(&inst, "page", Value::Number(3.0));
    render(&window, size, &format!("{out}/artists.png"));
    set(&inst, "playlist-rows", album_rows(5));
    set(&inst, "playlist-count", Value::Number(24.0));
    set(&inst, "page", Value::Number(4.0));
    render(&window, size, &format!("{out}/playlists.png"));
    set(&inst, "fav-tracks", tracks());
    set(&inst, "page", Value::Number(5.0));
    render(&window, size, &format!("{out}/favorites.png"));
    set(&inst, "search-text", s("夜"));
    set(&inst, "search-tracks", tracks());
    set(&inst, "search-album-count", Value::Number(3.0));
    set(&inst, "page", Value::Number(6.0));
    render(&window, size, &format!("{out}/search.png"));

    // Details.
    set(&inst, "detail-tracks", tracks());
    set(&inst, "detail-cover", Value::Image(cover(5)));
    set(&inst, "can-go-back", Value::Bool(true));
    set(
        &inst,
        "detail",
        detail(
            0.0,
            "CLANNAD ORIGINAL SOUNDTRACK",
            "麻枝准 / 戸越まごめ",
            "2004 · 64 首 · 3 小时 2 分钟 · Soundtrack",
            "最高 FLAC / 24bit / 96kHz",
            "",
        ),
    );
    set(&inst, "page", Value::Number(10.0));
    render(&window, size, &format!("{out}/album.png"));
    set(&inst, "detail-releases", album_rows(5));
    set(
        &inst,
        "detail",
        detail(
            1.0,
            "YOASOBI",
            "",
            "12 张专辑 · 140 首",
            "",
            "日本的音乐组合，由作曲家 Ayase 与歌手 ikura 组成。",
        ),
    );
    render(&window, size, &format!("{out}/artist.png"));
    set(
        &inst,
        "detail",
        detail(2.0, "夜间播放", "", "40 首 · 2 小时 41 分钟", "", "客厅"),
    );
    set(&inst, "dialog", Value::Number(2.0));
    set(&inst, "dialog-name", s("夜间播放"));
    set(&inst, "dialog-description", s("客厅"));
    render(&window, size, &format!("{out}/playlist.png"));
    set(&inst, "dialog", Value::Number(0.0));

    set(&inst, "page", Value::Number(2.0));
    set(&inst, "notice", s(""));
    let devices: Vec<Value> = [
        "系统默认",
        "扬声器 (Realtek(R) Audio)",
        "Topping E30 II",
        "wasapi/{1234}（未连接）",
    ]
    .into_iter()
    .map(s)
    .collect();
    set(
        &inst,
        "audio-devices",
        Value::Model(ModelRc::new(VecModel::from(devices))),
    );
    set(&inst, "audio-device-index", Value::Number(2.0));
    set(&inst, "exclusive", Value::Bool(true));
    set(&inst, "tray-available", Value::Bool(true));
    set(
        &inst,
        "transcode-options",
        strings(&[
            "原始文件（默认，无损）",
            "FLAC（最高 48 kHz）",
            "MP3 320 kbps",
        ]),
    );
    set(&inst, "lyrics-translation-available", Value::Bool(true));
    render(&window, size, &format!("{out}/settings.png"));

    // 032 home.
    set(&inst, "has-home", Value::Bool(true));
    set(&inst, "show-lyrics", Value::Bool(false));
    set(&inst, "playing-id", n(201.0));
    set_g(&inst, "Home", "loading", Value::Bool(false));
    set_g(
        &inst,
        "Home",
        "tab-labels",
        strings(&["最新入库", "最近在听", "作品聚焦", "随机重温"]),
    );
    set_g(
        &inst,
        "Home",
        "tab-ids",
        model(vec![n(0.0), n(1.0), n(2.0), n(3.0)]),
    );
    set_g(&inst, "Home", "tab", n(1.0));
    set_g(
        &inst,
        "Home",
        "pane",
        st(&[
            ("tab", n(1.0)),
            ("list", n(5.0)),
            ("album-id", n(3.0)),
            ("title", s("BanG Dream! It's MyGO!!!!! ORIGINAL SOUNDTRACK")),
            ("artist", s("MyGO!!!!!")),
            ("artist-id", n(5.0)),
            ("facts", s("2023 · 24 首 · 2 Disc · 1:20:20")),
            ("spec", s("FLAC / 24bit / 96kHz")),
            ("note", s("上次播放 3 天前")),
            ("resume", s("从 1:35 继续")),
            ("more", s("查看全部 24 首")),
            ("favorite", Value::Bool(true)),
            ("cover", Value::Image(cover(7))),
            ("track-count", n(24.0)),
        ]),
    );
    set_g(&inst, "Home", "pane-tracks", tracks());
    set_g(&inst, "Home", "resume", home_rows("resume"));
    set_g(&inst, "Home", "recent", home_rows("recent"));
    set_g(&inst, "Home", "songs", home_rows("songs"));
    set_g(&inst, "Home", "songs-count", n(112.0));
    set_g(&inst, "Home", "songs-pages", n(6.0));
    set_g(&inst, "Home", "recent-albums", album_rows_n(3, 12));
    set_g(&inst, "Home", "works", album_rows(24));
    set_g(&inst, "Home", "works-count", n(87.0));
    set_g(&inst, "Home", "works-pages", n(4.0));
    set_g(
        &inst,
        "Home",
        "years",
        model(
            (0..24)
                .map(|i| {
                    st(&[
                        ("year", n(2001.0 + i as f64)),
                        ("count", n(((i * 7) % 40 + 1) as f64)),
                        ("height", n((((i * 7) % 40 + 1) * 100 / 40).max(3) as f64)),
                    ])
                })
                .collect(),
        ),
    );
    set_g(&inst, "Home", "year", n(2023.0));
    set_g(&inst, "Home", "year-total", n(40.0));
    set_g(&inst, "Home", "chronicle", album_rows_n(24, 12));
    set_g(&inst, "Home", "playlists", album_rows_n(24, 8));
    set_g(
        &inst,
        "Home",
        "stats",
        st(&[
            ("albums", n(812.0)),
            ("tracks", n(10342.0)),
            ("works", n(87.0)),
            ("last-scan", s("上次扫描 2026年10月27日 20:00")),
            ("pending", n(3.0)),
        ]),
    );
    set(&inst, "page", n(20.0));
    render(&window, PhysicalSize::new(1280, 2500), &format!("{out}/home.png"));

    // Works spotlight tab and daily mixes.
    set_g(&inst, "Home", "tab", n(2.0));
    set_g(
        &inst,
        "Home",
        "series",
        st(&[
            ("is-series", Value::Bool(true)),
            ("series-id", n(7.0)),
            ("first-work-id", n(31.0)),
            ("title", s("BanG Dream!")),
            ("by", s("系列 · 2 部作品")),
            ("facts", s("9 张关联专辑 · 112 首关联歌曲 · 2017-2025")),
            ("album-count", n(9.0)),
        ]),
    );
    set_g(
        &inst,
        "Home",
        "series-fan",
        model(vec![
            Value::Image(cover(1)),
            Value::Image(cover(2)),
            Value::Image(cover(3)),
        ]),
    );
    let item = |title: &str, meta: &str| {
        st(&[
            ("id", n(1.0)),
            ("title", s(title)),
            ("meta", s(meta)),
            ("year", s("2023")),
            ("cover", Value::Image(cover(4))),
        ])
    };
    set_g(
        &inst,
        "Home",
        "series-works",
        model(vec![item("MyGO!!!!!", "动画 · 2023"), item("Ave Mujica", "动画 · 2025")]),
    );
    set_g(
        &inst,
        "Home",
        "series-albums",
        model(vec![
            item("迷跡波", "MyGO!!!!! · 12 首"),
            item("Ave Mujica OST", "Ave Mujica · 30 首"),
        ]),
    );
    set_g(&inst, "Home", "mixes", home_rows("mix"));
    render(&window, PhysicalSize::new(1280, 900), &format!("{out}/home-series.png"));

    // Works, series, history.
    set_g(&inst, "Lib032", "works", album_rows(5));
    set_g(&inst, "Lib032", "works-total", n(87.0));
    set(&inst, "page", n(21.0));
    render(&window, size, &format!("{out}/works.png"));
    set_g(&inst, "Lib032", "series", album_rows(5));
    set_g(&inst, "Lib032", "series-total", n(31.0));
    set(&inst, "page", n(22.0));
    render(&window, size, &format!("{out}/series.png"));
    let history = tracks();
    if let Value::Model(m) = &history {
        for i in 0..m.row_count() {
            if let Some(Value::Struct(mut row)) = m.row_data(i) {
                row.set_field("tag".into(), s(if i % 3 == 0 { "已中断" } else { "播放完成" }));
                row.set_field(
                    "note".into(),
                    s("2026年10月28日 09:15 · 1:04 / 4:21 · 播放 3 次"),
                );
                m.set_row_data(i, Value::Struct(row));
            }
        }
    }
    set_g(&inst, "Lib032", "history", history);
    set_g(&inst, "Lib032", "history-total", n(1234.0));
    set(&inst, "page", n(23.0));
    render(&window, size, &format!("{out}/history.png"));

    // Work detail.
    set_g(
        &inst,
        "Lib032",
        "work",
        st(&[
            ("id", n(15.0)),
            ("title", s("Fate/Zero")),
            ("translated", s("命运之零")),
            ("reading", s("フェイト/ゼロ")),
            ("facts", s("动画 · 2011 · 3 张整张关联专辑 · 9 首曲目")),
            ("bangumi-url", s("https://bgm.tv/subject/10380")),
            ("series-id", n(7.0)),
            ("series-title", s("Fate系列")),
            ("series-meta", s("共 4 部作品 · 当前：第 2 部")),
            ("album-count", n(3.0)),
            ("collected", n(2.0)),
        ]),
    );
    set_g(&inst, "Lib032", "work-poster", Value::Image(cover(6)));
    set_g(&inst, "Lib032", "work-albums", album_rows(5));
    let collected = tracks();
    if let Value::Model(m) = &collected {
        for i in 0..m.row_count() {
            if let Some(Value::Struct(mut row)) = m.row_data(i) {
                row.set_field("tag".into(), s(if i % 2 == 0 { "OP1" } else { "插曲" }));
                m.set_row_data(i, Value::Struct(row));
            }
        }
    }
    set(&inst, "detail-tracks", collected);
    set(&inst, "detail", detail(4.0, "Fate/Zero", "", "", "", ""));
    set(&inst, "page", n(10.0));
    render(&window, PhysicalSize::new(1280, 1200), &format!("{out}/work.png"));

    // Series detail.
    set_g(
        &inst,
        "Lib032",
        "series-info",
        st(&[
            ("id", n(7.0)),
            ("title", s("Fate系列")),
            ("renamed", Value::Bool(true)),
            ("facts", s("4 部作品 · 2004 - 2011 · 5 张专辑 · 26 首曲目")),
            ("member-count", n(4.0)),
            ("album-count", n(5.0)),
            ("playing", Value::Bool(true)),
        ]),
    );
    set_g(&inst, "Lib032", "series-poster", Value::Image(cover(8)));
    set_g(&inst, "Lib032", "series-members", album_rows(5));
    set_g(&inst, "Lib032", "series-albums", album_rows(5));
    set(&inst, "detail", detail(5.0, "Fate系列", "", "", "", ""));
    render(&window, PhysicalSize::new(1280, 1300), &format!("{out}/series-detail.png"));

    // Daily mix.
    set_g(
        &inst,
        "Lib032",
        "mix",
        st(&[
            ("id", n(4.0)),
            ("title", s("深夜的钢琴与弦乐")),
            ("description", s("适合夜里安静听的器乐和慢歌。")),
            ("kicker", s("情绪 · 今日推荐")),
            ("subtitle", s("安静 · 器乐 · 30 首 · 1 小时 52 分")),
            ("rating", n(-1.0)),
            ("disliked-note", s("已记录：太重复、风格太跳。再点一次可撤销。")),
            ("current", Value::Bool(true)),
            ("replacements-left", n(2.0)),
            ("saved-playlist-id", n(9.0)),
            ("track-count", n(30.0)),
        ]),
    );
    set_g(
        &inst,
        "Lib032",
        "mix-covers",
        model((0..4).map(|i| Value::Image(cover(i))).collect()),
    );
    set_g(
        &inst,
        "Lib032",
        "mix-tags",
        model(vec![
            st(&[("label", s("安静")), ("style", Value::Bool(false))]),
            st(&[("label", s("器乐")), ("style", Value::Bool(true))]),
        ]),
    );
    set_g(&inst, "Lib032", "mix-others", home_rows("mix"));
    let mix_tracks = tracks();
    if let Value::Model(m) = &mix_tracks {
        for i in 0..m.row_count() {
            if let Some(Value::Struct(mut row)) = m.row_data(i) {
                if i % 4 == 1 {
                    row.set_field("tag".into(), s("新发现"));
                    row.set_field("tag-accent".into(), Value::Bool(true));
                }
                row.set_field("blocked".into(), Value::Bool(i == 2));
                row.set_field("disabled".into(), Value::Bool(i == 3));
                m.set_row_data(i, Value::Struct(row));
            }
        }
    }
    set(&inst, "detail-tracks", mix_tracks);
    set(&inst, "detail", detail(6.0, "深夜的钢琴与弦乐", "", "", "", ""));
    render(&window, size, &format!("{out}/mix.png"));

    // Lyrics with translation and the search dialog.
    set(&inst, "page", n(0.0));
    set(&inst, "show-lyrics", Value::Bool(true));
    let translated = lyrics();
    if let Value::Model(m) = &translated {
        for i in 0..m.row_count() {
            if let Some(Value::Struct(mut row)) = m.row_data(i) {
                row.set_field("translation".into(), s("像是沉没、像是融化一般"));
                m.set_row_data(i, Value::Struct(row));
            }
        }
    }
    set(&inst, "lyrics", translated);
    set(&inst, "lyrics-translated-lines", Value::Bool(true));
    set(&inst, "lyrics-can-search", Value::Bool(true));
    set(&inst, "lyrics-can-translate", Value::Bool(true));
    set(&inst, "lyrics-translation-shown", Value::Bool(true));
    set(&inst, "lyrics-offset", n(150.0));
    render(&window, size, &format!("{out}/lyrics.png"));
    // Furigana and romaji.
    let seg = |base: &str, reading: &str| st(&[("base", s(base)), ("reading", s(reading))]);
    let ruby_lines: Vec<Value> = (0..8)
        .map(|_| {
            st(&[
                ("text", s("日が沈み出した空と君の姿")),
                ("translation", s("")),
                (
                    "ruby",
                    model(vec![
                        seg("日", "ひ"),
                        seg("が", ""),
                        seg("沈", "しず"),
                        seg("み", ""),
                        seg("出", "だ"),
                        seg("した", ""),
                        seg("空", "そら"),
                        seg("と", ""),
                        seg("君", "きみ"),
                        seg("の", ""),
                        seg("姿", "すがた"),
                    ]),
                ),
                ("romaji", s("hi ga shizumi dashi ta sora to kimi no sugata")),
            ])
        })
        .collect();
    set(&inst, "lyrics", model(ruby_lines));
    set(&inst, "lyrics-translated-lines", Value::Bool(false));
    set(&inst, "lyrics-ruby-lines", Value::Bool(true));
    set(&inst, "lyrics-romaji-lines", Value::Bool(true));
    render(&window, size, &format!("{out}/lyrics-ruby.png"));
    set_g(&inst, "LyricsSearch", "name", s("夜に駆ける"));
    set_g(&inst, "LyricsSearch", "artist", s("YOASOBI"));
    set_g(
        &inst,
        "LyricsSearch",
        "results",
        model(
            [
                ("夜に駆ける", "YOASOBI", "NetEase — 1409311773 — 同步", "98.20%"),
                ("夜に駆ける", "YOASOBI", "LRCLIB — 2045 — 同步", "95.00%"),
                ("夜に駆ける (Live)", "YOASOBI", "NetEase — 77 — 非同步", "61.50%"),
            ]
            .iter()
            .map(|(t, a, m, sc)| {
                st(&[("title", s(t)), ("artist", s(a)), ("meta", s(m)), ("score", s(sc))])
            })
            .collect(),
        ),
    );
    set_g(&inst, "LyricsSearch", "selected", n(0.0));
    set_g(&inst, "LyricsSearch", "preview", lyrics_strings());
    set(&inst, "dialog", n(5.0));
    render(&window, size, &format!("{out}/lyrics-search.png"));
}

fn lyrics_strings() -> Value {
    strings(&[
        "沈むように溶けてゆくように",
        "二人だけの空が広がる夜に",
        "「さよなら」だけだった",
        "その一言で全てが分かった",
    ])
}
