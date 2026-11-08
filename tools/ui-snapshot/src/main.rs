//! Renders `crates/app/ui/app.slint` with fake data to PNG files.
//!
//! cargo run --manifest-path tools/ui-snapshot/Cargo.toml -- <out-dir>

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{
    ComponentHandle, Image, Model, ModelRc, PhysicalSize, Rgba8Pixel, SharedPixelBuffer,
    SharedString, VecModel,
};
use slint_interpreter::{Compiler, ComponentInstance, Struct, Value};

mod wide;

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

thread_local! {
    /// Real covers from `UI_SNAPSHOT_COVERS` (local only, never committed):
    /// name -> RGBA, at most 640px.
    static COVERS: std::cell::RefCell<Option<Vec<(String, image::RgbaImage)>>> =
        const { std::cell::RefCell::new(None) };
}

fn covers_dir() -> Vec<(String, image::RgbaImage)> {
    COVERS.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| {
                let Some(dir) = std::env::var_os("UI_SNAPSHOT_COVERS") else {
                    return Vec::new();
                };
                let mut files: Vec<_> = std::fs::read_dir(dir)
                    .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
                    .unwrap_or_default();
                files.sort();
                files
                    .into_iter()
                    .filter_map(|path| {
                        let img = image::open(&path).ok()?.to_rgba8();
                        let img = if img.width() > 640 {
                            let h = img.height() * 640 / img.width();
                            image::imageops::resize(&img, 640, h, image::imageops::FilterType::Triangle)
                        } else {
                            img
                        };
                        Some((path.file_stem()?.to_string_lossy().into_owned(), img))
                    })
                    .collect()
            })
            .clone()
    })
}

fn to_slint(img: &image::RgbaImage) -> Image {
    Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
        img.as_raw(),
        img.width(),
        img.height(),
    ))
}

/// A named cover: the real file when `UI_SNAPSHOT_COVERS` has it, else a
/// gradient seeded by the name.
fn art(name: &str) -> Image {
    if let Some((_, img)) = covers_dir().iter().find(|(n, _)| n == name) {
        return to_slint(img);
    }
    gradient(name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b))) % 97)
}

/// A 2:3 work poster cut from the middle of a named cover.
fn poster(name: &str) -> Image {
    if let Some((_, img)) = covers_dir().iter().find(|(n, _)| n == name) {
        let w = img.height() * 2 / 3;
        let x = (img.width().saturating_sub(w)) / 2;
        let crop = image::imageops::crop_imm(img, x, 0, w.min(img.width()), img.height()).to_image();
        return to_slint(&crop);
    }
    art(name)
}

fn cover(seed: u32) -> Image {
    let real = covers_dir();
    if !real.is_empty() {
        return to_slint(&real[seed as usize % real.len()].1);
    }
    gradient(seed)
}

fn gradient(seed: u32) -> Image {
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

/// 最近添加: no year, "入库于 …" last line.
fn recent_rows(columns: usize, count: usize) -> Value {
    let rows = album_rows_n(columns, count);
    if let Value::Model(rows) = &rows {
        for r in 0..rows.row_count() {
            if let Some(Value::Struct(row)) = rows.row_data(r)
                && let Some(Value::Model(items)) = row.get_field("items")
            {
                for i in 0..items.row_count() {
                    if let Some(Value::Struct(mut card)) = items.row_data(i) {
                        card.set_field("year".into(), s(""));
                        card.set_field(
                            "added".into(),
                            s(["入库于 5 小时前", "入库于 1 天前", "入库于 3 天前"][i % 3]),
                        );
                        items.set_row_data(i, Value::Struct(card));
                    }
                }
            }
        }
    }
    rows
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
            card.set_field(
                "badge".into(),
                s(if i % 7 == 2 { "原声集 OST" } else { "" }),
            );
            card.set_field(
                "note".into(),
                s(if i % 4 == 1 { "OP 1 · ED 2" } else { "" }),
            );
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
    d.set_field("kicker".into(), s(""));
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
                row.set_field("number".into(), Value::Number(i as f64 + 1.0));
                row.set_field("cover".into(), Value::Image(cover(i as u32 % 4 + 2)));
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
        (
            "夜に駆ける",
            "YOASOBI · THE BOOK",
            "OP",
            "进击的巨人 The Final Season",
        ),
        (
            "春日影",
            "MyGO!!!!! · 迷跡波",
            "ED",
            "BanG Dream! It's MyGO!!!!!",
        ),
        (
            "only my railgun",
            "fripSide · infinite synthesis",
            "插曲",
            "某科学的超电磁炮",
        ),
        (
            "だんご大家族",
            "茶太 · CLANNAD ORIGINAL SOUNDTRACK",
            "主题曲",
            "CLANNAD",
        ),
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
                    ("covers", model(Vec::new())),
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
                        // Mosaic except on the last row (fewer than four covers).
                        if i + 1 < rows.len() {
                            let tiles = (0..4).map(|k| Value::Image(cover(i as u32 * 4 + k)));
                            f.push(("covers", model(tiles.collect())));
                        }
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

// Same font stack as the app (`crates/app/src/fonts.rs`).
#[path = "../../../crates/app/src/fonts.rs"]
mod fonts;

fn create(def: &slint_interpreter::ComponentDefinition) -> ComponentInstance {
    let inst = def.create().unwrap();
    inst.set_global_callback("Fonts", "family", |args| {
        let text = match args.first() {
            Some(Value::String(s)) => s.to_string(),
            _ => String::new(),
        };
        Value::String(fonts::family_for(&text).into())
    })
    .unwrap();
    inst
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "/tmp".into());
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/app/ui/app.slint");

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(SnapshotPlatform(window.clone()))).unwrap();
    fonts::register();

    let mut compiler = Compiler::default();
    compiler.set_style("fluent".into());
    let result = spin_on::spin_on(compiler.build_from_path(root));
    for d in result.diagnostics() {
        eprintln!("{d}");
    }
    let def = result.component("AppWindow").expect("AppWindow");
    let size = PhysicalSize::new(1280, 800);
    // UI_SNAPSHOT_ONLY=wide: only the redesign canvases (faster).
    if std::env::var("UI_SNAPSHOT_ONLY").as_deref() == Ok("wide") {
        wide::run(&def, &window, &out);
        return;
    }

    // Login.
    let inst = create(&def);
    inst.show().unwrap();
    set(&inst, "login-server", s("https://music.example.com"));
    set(&inst, "login-error", s("API Token 已失效，请重新输入"));
    set(&inst, "login-media-token-saved", Value::Bool(true));
    render(&window, size, &format!("{out}/login.png"));
    inst.hide().unwrap();

    // Main views.
    let inst = create(&def);
    inst.show().unwrap();
    set(&inst, "logged-in", Value::Bool(true));
    if std::env::var_os("CLICK_TEST").is_some() {
        let hits = std::rc::Rc::new(std::cell::Cell::new(0));
        let h = hits.clone();
        inst.set_callback("shortcut-key", move |_| {
            h.set(h.get() + 1);
            Value::Bool(true)
        })
        .unwrap();
        let key = || {
            window.dispatch_event(slint::platform::WindowEvent::KeyPressed { text: " ".into() });
            window.dispatch_event(slint::platform::WindowEvent::KeyReleased { text: " ".into() });
        };
        render(&window, size, &format!("{out}/keyboard-test.png"));
        key();
        assert_eq!(hits.get(), 1, "Space must reach the window shortcut scope");
        set(&inst, "page", n(6.0));
        render(&window, size, &format!("{out}/keyboard-search-test.png"));
        key();
        assert_eq!(
            hits.get(),
            1,
            "Typing Space in search must not trigger playback"
        );
        let captures = std::rc::Rc::new(std::cell::Cell::new(0));
        let c = captures.clone();
        inst.set_callback("capture-shortcut", move |_| {
            c.set(c.get() + 1);
            Value::Bool(true)
        })
        .unwrap();
        set(&inst, "recording-shortcut", n(0.0));
        key();
        assert_eq!(
            captures.get(),
            1,
            "Recording must intercept keys before focused editors"
        );
        assert_eq!(hits.get(), 1);
        set(&inst, "recording-shortcut", n(-1.0));
        inst.set_callback("shortcut-key", |_| Value::Bool(false))
            .unwrap();
        set(&inst, "search-text", s(""));
        set(&inst, "page", n(0.0));
        println!("keyboard focus checks passed");
    }
    set(&inst, "server-label", s("https://music.example.com"));
    set(&inst, "server-status", s("music.example.com · 10,342 首"));
    set(&inst, "search-shortcut", s("Ctrl+K"));
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
    set(&inst, "np-cover-large", Value::Image(cover(3)));
    set(
        &inst,
        "side-playlists",
        model(
            [
                ("HOSHIMACHI SUISEI ARCHIVE", "20 首 · 1 小时 14 分"),
                ("音乃瀬奏", "1 首 · 3 分 12 秒"),
                ("通勤", "86 首 · 5 小时 2 分"),
            ]
            .iter()
            .enumerate()
            .map(|(i, (t, m))| {
                st(&[
                    ("id", n(i as f64 + 1.0)),
                    ("title", s(t)),
                    ("meta", s(m)),
                    ("year", s("")),
                    (
                        "cover",
                        Value::Image(if i == 2 {
                            Image::default()
                        } else {
                            cover(30 + i as u32)
                        }),
                    ),
                ])
            })
            .collect(),
        ),
    );
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
    set(&inst, "panel-queue", Value::Bool(false));
    render(&window, size, &format!("{out}/albums.png"));

    set(&inst, "page", Value::Number(1.0));
    set(&inst, "notice", s("播放失败：loading failed"));
    render(&window, size, &format!("{out}/tracks.png"));

    set(&inst, "notice", s(""));
    set(&inst, "queue", queue());
    set(&inst, "queue-total", Value::Number(12.0));
    set(&inst, "queue-current", Value::Number(1.0));
    // Queue and lyrics stacked in the right panel.
    set(&inst, "panel-queue", Value::Bool(true));
    set(&inst, "shuffle", Value::Bool(true));
    set(&inst, "repeat", Value::Number(2.0));
    set(&inst, "sleep-active", Value::Bool(true));
    set(&inst, "sleep-label", s("29:41"));
    render(&window, size, &format!("{out}/queue.png"));
    set(&inst, "panel-lyrics-first", Value::Bool(true));
    set(&inst, "panel-split", Value::Number(0.35));
    render(&window, size, &format!("{out}/queue-lyrics-first.png"));
    set(&inst, "panel-lyrics-first", Value::Bool(false));
    set(&inst, "panel-split", Value::Number(0.5));
    set(&inst, "panel-queue", Value::Bool(false));

    // Library pages.
    set(&inst, "panel-open", Value::Bool(false));
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

    // Album with linked works, credits and the inspector.
    let album_tracks = tracks();
    if let Value::Model(m) = &album_tracks {
        for i in 0..m.row_count() {
            if let Some(Value::Struct(mut row)) = m.row_data(i) {
                let parts: &[(&str, f64)] = if i % 3 == 2 {
                    &[]
                } else {
                    &[
                        ("作词 ", 0.0),
                        ("Paris Alexa, Sabrina Claudio, 星街すいせい", 22.0),
                        (" · ", 0.0),
                        ("作曲 ", 0.0),
                        ("Paris Alexa, Sabrina Claudio, Sad Money, Xansei", 23.0),
                        (" · ", 0.0),
                        ("Ayase", 20.0),
                        (" · ", 0.0),
                        ("作曲 ", 0.0),
                        ("Ayase", 20.0),
                        (" · ", 0.0),
                        ("编曲 ", 0.0),
                        ("Ayase", 0.0),
                        (", ", 0.0),
                        ("Giga", 21.0),
                    ]
                };
                row.set_field(
                    "credits".into(),
                    model(
                        parts
                            .iter()
                            .map(|(t, id)| st(&[("text", s(t)), ("id", n(*id))]))
                            .collect(),
                    ),
                );
                row.set_field(
                    "tieup".into(),
                    s(match i % 4 {
                        0 => "OP",
                        1 => "ED",
                        _ => "",
                    }),
                );
                row.set_field("tieup-count".into(), n(if i == 0 { 2.0 } else { 1.0 }));
                row.set_field("tieup-works".into(), model((0..if i == 0 {2} else {1}).map(|j| st(&[
                    ("id", n(90.0 + f64::from(j))),
                    ("title", s(if j == 0 { "CLANNAD" } else { "CLANNAD ～AFTER STORY～" })),
                    ("translated", s("团子大家族")), ("meta", s("TV 动画 · 2007")),
                    ("usage", s(if i % 2 == 0 { "OP" } else { "插曲" })),
                    ("poster", Value::Image(cover(8 + j))),
                ])).collect()));
                row.set_field(
                    "tieup-tooltip".into(),
                    s("作品标题 / 作品譯名\nOP · TV 动画 · 2024\n\n第二部作品\nED · 剧场版 · 2025"),
                );
                m.set_row_data(i, Value::Struct(row));
            }
        }
    }
    set(&inst, "detail-tracks", album_tracks);
    let uses = |items: &[(&str, &str)]| {
        model(
            items
                .iter()
                .map(|(l, t)| st(&[("label", s(l)), ("title", s(t))]))
                .collect(),
        )
    };
    set(
        &inst,
        "album-works",
        model(vec![
            st(&[
                ("id", n(1.0)),
                ("title", s("CLANNAD")),
                ("translated", s("团子大家族")),
                ("meta", s("动画 · 2007")),
                ("relation", s("OST · OP · ED")),
                ("usage", s("OST · OP · ED")),
                ("ost", Value::Bool(false)),
                ("poster", Value::Image(cover(11))),
                ("chip", Value::Image(cover(11))),
                (
                    "uses",
                    uses(&[
                        ("OP", "メグメル"),
                        ("ED", "だんご大家族"),
                        ("插曲", "小さなてのひら"),
                        ("ED", "TORCH"),
                    ]),
                ),
            ]),
            st(&[
                ("id", n(2.0)),
                ("title", s("CLANNAD ～AFTER STORY～")),
                ("translated", s("")),
                ("meta", s("动画 · 2008")),
                ("relation", s("原声集 OST")),
                ("usage", s("OST")),
                ("ost", Value::Bool(true)),
                ("poster", Value::Image(cover(12))),
                ("chip", Value::Image(cover(12))),
                ("uses", uses(&[])),
            ]),
        ]),
    );
    let facts = |items: &[(&str, &str)]| {
        model(
            items
                .iter()
                .map(|(l, v)| st(&[("label", s(l)), ("value", s(v))]))
                .collect(),
        )
    };
    set(
        &inst,
        "album-release",
        facts(&[
            ("类型", "原声"),
            ("演出者", "麻枝准、戸越まごめ"),
            ("发行日期", "2004-08-13"),
            ("厂牌", "Key Sounds Label"),
            ("目录号", "KSLA-0009~11"),
            ("流派", "Soundtrack"),
        ]),
    );
    set(
        &inst,
        "album-media",
        facts(&[("格式", "FLAC / 16–24bit / 44.1–96kHz"), ("容量", "1.2 GB")]),
    );
    set(
        &inst,
        "album-people",
        model(
            [
                ("麻枝准", "作曲 · 作词 · 64 首"),
                ("戸越まごめ", "作曲 · 32 首"),
                ("茶太", "演唱 · 2 首"),
                ("Lia", "演唱 · 1 首"),
            ]
            .iter()
            .enumerate()
            .map(|(i, (name, roles))| {
                st(&[
                    ("id", n(i as f64 + 1.0)),
                    ("name", s(name)),
                    ("roles", s(roles)),
                    (
                        "avatar",
                        Value::Image(if i == 2 {
                            Image::default()
                        } else {
                            cover(20 + i as u32)
                        }),
                    ),
                ])
            })
            .collect(),
        ),
    );
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
    render(
        &window,
        PhysicalSize::new(1600, 900),
        &format!("{out}/album-works.png"),
    );
    set(
        &inst,
        "detail",
        detail(
            2.0,
            "HOSHIMACHI SUISEI ARENA TOUR 2026 “Once Upon a Stellar”",
            "",
            "20 首 · 1 小时 14 分钟",
            "",
            "SETLIST",
        ),
    );
    render(
        &window,
        PhysicalSize::new(1600, 900),
        &format!("{out}/playlist-credits.png"),
    );
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
    if std::env::var_os("CLICK_TEST").is_some() {
        let queue_adds = Rc::new(std::cell::Cell::new(0));
        let adds = queue_adds.clone();
        inst.set_callback("track-action", move |args| {
            if args.get(1) == Some(&s("last")) {
                assert_eq!(args.get(2), Some(&n(0.0)), "Only the right-clicked track is added");
                adds.set(adds.get() + 1);
            }
            println!("track-action {args:?}");
            Value::Void
        })
        .unwrap();
        // First album row (1600x900): credits link, artist, empty title area.
        for (x, y) in [(300.0, 558.0), (846.0, 543.0), (600.0, 533.0)] {
            let position = slint::LogicalPosition::new(x, y);
            let button = slint::platform::PointerEventButton::Left;
            window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
            window
                .dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
            window
                .dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
            println!("clicked {x},{y}");
        }
        let position = slint::LogicalPosition::new(190.0, 543.0);
        let button = slint::platform::PointerEventButton::Right;
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
        render(&window, PhysicalSize::new(1600, 900), &format!("{out}/track-context-menu.png"));
        // "添加到播放队列末尾", the third menu item.
        let position = slint::LogicalPosition::new(290.0, 634.0);
        let button = slint::platform::PointerEventButton::Left;
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
        assert_eq!(queue_adds.get(), 1, "Right click must open a working queue menu");
        println!("track right-click queue action passed");
        render(&window, PhysicalSize::new(1600, 900), &format!("{out}/track-menu-closed.png"));
        // The "◆ OP 2" chip.
        let position = slint::LogicalPosition::new(283.0, 533.0);
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        slint::platform::update_timers_and_animations();
        std::thread::sleep(std::time::Duration::from_millis(900));
        slint::platform::update_timers_and_animations();
        render(&window, PhysicalSize::new(1600, 900), &format!("{out}/track-tieup-tooltip.png"));
        let button = slint::platform::PointerEventButton::Left;
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
        render(&window, PhysicalSize::new(1600, 900), &format!("{out}/track-tieup-popover.png"));
        window.dispatch_event(slint::platform::WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
        window.dispatch_event(slint::platform::WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(190.0, 480.0) });
    }
    set(&inst, "panel-open", Value::Bool(true));
    set(&inst, "sidebar-width", Value::Number(300.0));
    render(&window, size, &format!("{out}/album-works-narrow.png"));
    set(&inst, "sidebar-width", Value::Number(210.0));
    set(&inst, "panel-open", Value::Bool(false));
    set(&inst, "album-works", model(Vec::new()));
    set(&inst, "album-release", model(Vec::new()));
    set(&inst, "album-people", model(Vec::new()));
    set(&inst, "album-media", model(Vec::new()));
    set(&inst, "detail-tracks", tracks());
    set(
        &inst,
        "artist-groups",
        model(
            [
                ("专辑", 6usize),
                ("合辑与现场", 3),
                ("参与作品", 2),
                ("幕后参与", 4),
            ]
            .iter()
            .enumerate()
            .map(|(i, (title, count))| {
                st(&[
                    ("title", s(title)),
                    ("count", n(*count as f64)),
                    ("grid", n(i as f64)),
                    ("rows", album_rows_n(5, *count)),
                ])
            })
            .collect(),
        ),
    );
    set(
        &inst,
        "artist-about",
        s(
            "YOASOBI 是日本的音乐组合，由作曲家 Ayase 与歌手 ikura 组成，以\u{201c}将小说化为音乐\u{201d}为概念，2019 年以《夜に駆ける》出道。\n\n来源：Last.fm",
        ),
    );
    set(
        &inst,
        "detail",
        detail(
            1.0,
            "YOASOBI",
            "",
            "组合 · 日本 · 12 张专辑 · 140 首",
            "",
            "别名：ヨアソビ",
        ),
    );
    set(&inst, "artist-favs", tracks());
    render(
        &window,
        PhysicalSize::new(1280, 2200),
        &format!("{out}/artist.png"),
    );
    set(&inst, "artist-favs", Value::Model(ModelRc::default()));
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
    set(&inst, "fade-on-pause", Value::Bool(true));
    set(&inst, "update-version", s("v0.1.7"));
    set(
        &inst,
        "shortcuts",
        model(
            [
                ("播放 / 暂停", "Space"),
                ("上一首", "Ctrl+Left"),
                ("下一首", "Ctrl+Right"),
                ("后退 10 秒", "Left"),
                ("前进 10 秒", "Right"),
                ("增加音量", "Ctrl+Up"),
                ("降低音量", "Ctrl+Down"),
                ("切换随机播放", "Ctrl+S"),
                ("切换循环模式", "Ctrl+R"),
                ("显示 / 隐藏队列", "Ctrl+Q"),
                ("搜索曲库", "Ctrl+K"),
            ]
            .into_iter()
            .map(|(title, binding)| st(&[("title", s(title)), ("binding", s(binding))]))
            .collect(),
        ),
    );
    render(&window, size, &format!("{out}/settings.png"));
    render(&window, PhysicalSize::new(1280, 1500), &format!("{out}/settings-tall.png"));
    set(
        &inst,
        "info-rows",
        model(
            [
                ("标题", "夜に駆ける"),
                ("艺人", "YOASOBI"),
                ("专辑", "THE BOOK"),
                ("曲目", "第 1 碟 · 第 3 首"),
                ("时长", "4:21"),
                ("作词", "Ayase"),
                ("作曲", "Ayase"),
                ("音质", "FLAC / 24bit / 48kHz"),
                ("文件大小", "31.2 MB"),
                ("路径", "YOASOBI/THE BOOK/03 夜に駆ける (A very long file name to check wrapping).flac"),
                ("入库时间", "2026年10月12日 09:15"),
            ]
            .iter()
            .map(|(l, v)| st(&[("label", s(l)), ("value", s(v))]))
            .collect(),
        ),
    );
    set(&inst, "dialog", n(6.0));
    render(&window, size, &format!("{out}/track-info.png"));
    set(&inst, "dialog", n(0.0));

    // 032 home.
    set(&inst, "has-home", Value::Bool(true));
    set(&inst, "panel-open", Value::Bool(false));
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
    set_g(&inst, "Home", "recent-albums", recent_rows(3, 6));
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
    render(
        &window,
        PhysicalSize::new(1280, 2500),
        &format!("{out}/home.png"),
    );
    set_g(&inst, "Home", "recent-albums", album_rows_n(3, 6));

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
        model(vec![
            item("MyGO!!!!!", "动画 · 2023"),
            item("Ave Mujica", "动画 · 2025"),
        ]),
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
    render(
        &window,
        PhysicalSize::new(1280, 900),
        &format!("{out}/home-series.png"),
    );

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
                row.set_field(
                    "tag".into(),
                    s(if i % 3 == 0 {
                        "已中断"
                    } else {
                        "播放完成"
                    }),
                );
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
    render(
        &window,
        PhysicalSize::new(1280, 1200),
        &format!("{out}/work.png"),
    );

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
    render(
        &window,
        PhysicalSize::new(1280, 1300),
        &format!("{out}/series-detail.png"),
    );

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
            (
                "disliked-note",
                s("已记录：太重复、风格太跳。再点一次可撤销。"),
            ),
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
    set(
        &inst,
        "detail",
        detail(6.0, "深夜的钢琴与弦乐", "", "", "", ""),
    );
    render(&window, size, &format!("{out}/mix.png"));

    // Lyrics with translation and the search dialog.
    set(&inst, "page", n(0.0));
    set(&inst, "panel-open", Value::Bool(true));
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

    // Full-screen player: lyrics, up next and related tabs.
    set(&inst, "full-cover", Value::Image(cover(3)));
    set(&inst, "full-open", Value::Bool(true));
    set(&inst, "full-tab", n(2.0));
    render(&window, size, &format!("{out}/full-lyrics.png"));
    set(&inst, "full-tab", n(0.0));
    render(&window, size, &format!("{out}/full-queue.png"));
    set(&inst, "full-related", tracks());
    set(&inst, "full-tab", n(1.0));
    render(&window, PhysicalSize::new(1000, 640), &format!("{out}/full-related-small.png"));
    set(&inst, "full-open", Value::Bool(false));
    set_g(&inst, "LyricsSearch", "name", s("夜に駆ける"));
    set_g(&inst, "LyricsSearch", "artist", s("YOASOBI"));
    set_g(
        &inst,
        "LyricsSearch",
        "results",
        model(
            [
                (
                    "夜に駆ける",
                    "YOASOBI",
                    "NetEase — 1409311773 — 同步",
                    "98.20%",
                ),
                ("夜に駆ける", "YOASOBI", "LRCLIB — 2045 — 同步", "95.00%"),
                (
                    "夜に駆ける (Live)",
                    "YOASOBI",
                    "NetEase — 77 — 非同步",
                    "61.50%",
                ),
            ]
            .iter()
            .map(|(t, a, m, sc)| {
                st(&[
                    ("title", s(t)),
                    ("artist", s(a)),
                    ("meta", s(m)),
                    ("score", s(sc)),
                ])
            })
            .collect(),
        ),
    );
    set_g(&inst, "LyricsSearch", "selected", n(0.0));
    set_g(&inst, "LyricsSearch", "preview", lyrics_strings());
    set(&inst, "dialog", n(5.0));
    render(&window, size, &format!("{out}/lyrics-search.png"));

    // Sonos output: the chip in the player bar, then the 投放 menu.
    set(&inst, "dialog", n(0.0));
    set(&inst, "page", n(0.0));
    let room = |id: &str, name: &str, detail: &str| {
        st(&[("id", s(id)), ("name", s(name)), ("detail", s(detail))])
    };
    set_g(
        &inst,
        "CastState",
        "rooms",
        Value::Model(slint::ModelRc::new(slint::VecModel::from(vec![
            room("RINCON_A", "客厅 + 卧室", "Sonos · 2 个房间"),
            room("RINCON_B", "书房", "Sonos One"),
        ]))),
    );
    set_g(&inst, "CastState", "output-id", s("RINCON_A"));
    set_g(&inst, "CastState", "output-label", s("客厅 + 卧室"));
    render(&window, size, &format!("{out}/cast.png"));
    let (x, y) = std::env::var("CAST_BUTTON")
        .ok()
        .and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .unwrap_or((0.0f32, 0.0f32));
    if x > 0.0 {
        let position = slint::LogicalPosition::new(x, y);
        let button = slint::platform::PointerEventButton::Left;
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
        render(&window, size, &format!("{out}/cast-menu.png"));
        // Close the menu.
        let position = slint::LogicalPosition::new(600.0, 300.0);
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
    }
    set_g(&inst, "CastState", "output-id", s(""));
    set_g(&inst, "CastState", "output-label", s(""));

    // 032 Light.
    set(&inst, "dialog", n(0.0));
    set(&inst, "theme-mode", n(2.0));
    set(&inst, "page", n(0.0));
    set(&inst, "panel-open", Value::Bool(true));
    set(&inst, "panel-queue", Value::Bool(true));
    render(&window, size, &format!("{out}/light-albums.png"));
    set(&inst, "page", n(2.0));
    render(&window, size, &format!("{out}/light-settings.png"));
    set(&inst, "full-open", Value::Bool(true));
    set(&inst, "full-tab", n(2.0));
    render(&window, size, &format!("{out}/light-full.png"));
    set(&inst, "full-open", Value::Bool(false));
    set(&inst, "theme-mode", n(1.0));
    inst.hide().unwrap();

    wide::run(&def, &window, &out);
}

fn lyrics_strings() -> Value {
    strings(&[
        "沈むように溶けてゆくように",
        "二人だけの空が広がる夜に",
        "「さよなら」だけだった",
        "その一言で全てが分かった",
    ])
}
