
use crate::{
    menu::{self, Entry},
    recfmt::{self, Settings},
    record::Border,
    text::Text,
};
use std::{
    cell::RefCell,
    ffi::c_void,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tiny_skia::{FillRule, LineCap, LineJoin, PathBuilder, Pixmap, Stroke, Transform};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateCompatibleDC, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
                VK_SPACE, VK_TAB,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, IDC_ARROW, IDC_SIZEALL, KillTimer, LoadCursorW, SetCursor, RegisterClassExW, SW_SHOW,
                SetForegroundWindow, SetTimer, SetWindowDisplayAffinity, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_CAPTURECHANGED, WM_ERASEBKGND,
                WM_CHAR, WM_KEYDOWN, WM_SETCURSOR, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_TIMER, WNDCLASSEXW,
                WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::w,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ev {
    Begin,
    TooSmall(u32, u32),
    Stop,
    Pause(bool),
    Discard,
    Cancel,
    Reselect,
    Area([i32; 4]),
    Change(Settings),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Ready,
    Countdown,
    Recording,
    Paused,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chip {
    Format,
    Fps,
    Size,
    Pointer,
    Delay,
    More,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    Record,
    Stop,
    Pause,
    Discard,
    Cancel,
    Chip(Chip),
    Size,
    Grip,
    Text,
    Dot,
    Sep,
}

impl Item {
    fn clickable(self) -> bool {
        !matches!(self, Item::Text | Item::Dot | Item::Sep)
    }
}

const HIT_L: u8 = 1;
const HIT_R: u8 = 2;
const HIT_T: u8 = 4;
const HIT_B: u8 = 8;
const HIT_MOVE: u8 = 16;

fn region_hit(area: [i32; 4], p: (i32, i32)) -> u8 {
    const GRAB: i32 = 12;
    let [x, y, w, h] = area;
    let (r, b) = (x + w, y + h);
    if p.0 < x - GRAB || p.0 > r + GRAB || p.1 < y - GRAB || p.1 > b + GRAB {
        return 0;
    }
    let mut hit = 0;
    if (p.0 - x).abs() <= GRAB && (p.0 - x).abs() <= (p.0 - r).abs() {
        hit |= HIT_L;
    } else if (p.0 - r).abs() <= GRAB {
        hit |= HIT_R;
    }
    if (p.1 - y).abs() <= GRAB && (p.1 - y).abs() <= (p.1 - b).abs() {
        hit |= HIT_T;
    } else if (p.1 - b).abs() <= GRAB {
        hit |= HIT_B;
    }
    if hit == 0 && p.0 >= x && p.0 < r && p.1 >= y && p.1 < b {
        hit = HIT_MOVE;
    }
    hit
}

struct Drag {
    hit: u8,
    last: std::cell::Cell<[i32; 4]>,
    cursor: (i32, i32),
    area: [i32; 4],
    body: [i32; 4],
}

struct SizeEdit {
    w: String,
    h: String,
    on_h: bool,
    fresh: bool,
}

struct Cell {
    item: Item,
    rect: [f32; 4],
    label: String,
}


const BAR_H: f32 = 48.0;
const BTN_H: f32 = 32.0;
const PAD: f32 = 10.0;
const GAP: f32 = 6.0;
const MARGIN: f32 = 2.0;
const TEXT_PX: f32 = 13.5;
const SPACE: f32 = 8.0;
const FRAME_GAP: i32 = 5;
const FADE_MS: f32 = 140.0;

const TEAL: [u8; 3] = [44, 178, 132];
const RED: [u8; 3] = [235, 64, 52];
const AMBER: [u8; 3] = [240, 170, 40];

struct St {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut c_void,
    size: (i32, i32),
    body: [i32; 4],
    backdrop: Option<(Pixmap, [i32; 4])>,
    area: [i32; 4],
    scale: f32,
    dark: bool,
    text: Arc<Text>,
    settings: Settings,
    phase: Phase,
    cells: Vec<Cell>,
    hover: Option<usize>,
    pressed: Option<usize>,
    body_y: i32,
    centre_x: i32,
    work: [i32; 4],
    started: Instant,
    paused_total: Duration,
    paused_since: Option<Instant>,
    countdown_end: Instant,
    shown: i64,
    border: Option<Border>,
    on_event: Rc<dyn Fn(Ev)>,
    alpha: u8,
    edit: Option<SizeEdit>,
    drag: Option<Drag>,
}

thread_local! {
    static STATE: RefCell<Option<St>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    STATE
        .try_with(|c| {
            let mut g = c.try_borrow_mut().ok()?;
            g.as_mut().map(f)
        })
        .ok()
        .flatten()
}

pub fn is_open() -> bool {
    STATE.with(|c| c.try_borrow().map_or(true, |g| g.is_some()))
}

pub fn open(
    area: [i32; 4],
    settings: Settings,
    scale: f32,
    dark: bool,
    text: Arc<Text>,
    shot: Option<&crate::capture::Frame>,
    on_event: impl Fn(Ev) + 'static,
) -> bool {
    close();
    let Some(st) = St::new(area, settings, scale, dark, text, shot, Rc::new(on_event)) else { return false };
    STATE.with(|c| *c.borrow_mut() = Some(st));
    let animate = crate::settings::refresh_motion();
    with(|st| {
        st.alpha = if animate { 0 } else { 255 };
        st.render();
        unsafe {
            let _ = ShowWindow(st.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(st.hwnd);
        }
    });
    with(St::fade_in);
    true
}

fn fade_alpha(ms: f32) -> (u8, bool) {
    let t = (ms / FADE_MS).clamp(0.0, 1.0);
    ((255.0 * (1.0 - (1.0 - t) * (1.0 - t))) as u8, t >= 1.0)
}

pub fn close() {
    let st = STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take()));
    drop(st);
}

pub fn shortcut() {
    let ev = with(|st| match st.phase {
        Phase::Ready => st.press_record(),
        Phase::Countdown => vec![Ev::Cancel],
        Phase::Recording | Phase::Paused => vec![Ev::Stop],
    });
    emit(ev);
}

fn emit(evs: Option<Vec<Ev>>) {
    let Some(evs) = evs.filter(|e| !e.is_empty()) else { return };
    let Some(f) = with(|st| st.on_event.clone()) else { return };
    for e in evs {
        f(e);
    }
}

fn same_items(a: &[Cell], b: &[Cell]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.item == y.item)
}

pub fn set_settings(s: Settings) {
    with(|st| {
        if matches!(st.phase, Phase::Ready | Phase::Countdown) && st.settings != s {
            st.settings = s;
            st.relayout();
            st.render();
        }
    });
}


fn apply_id(s: &mut Settings, id: &str) -> bool {
    let (key, val) = id.split_once(':').unwrap_or((id, ""));
    let n = val.parse::<u32>().ok();
    let byte = n.and_then(|n| u8::try_from(n).ok());
    match key {
        "fmt" => {
            s.webp = n == Some(1);
            s.mp4 = n == Some(2);
        }
        "fps" => match byte.filter(|n| recfmt::FPS.contains(n)) {
            Some(n) => s.fps = n,
            None => return false,
        },
        "scale" => match byte.filter(|n| recfmt::SCALE.contains(n)) {
            Some(n) => s.scale = n,
            None => return false,
        },
        "ptr" => match byte.filter(|n| *n <= 2) {
            Some(n) => s.pointer = n,
            None => return false,
        },
        "delay" => match byte.filter(|n| recfmt::COUNTDOWN.contains(n)) {
            Some(n) => s.countdown = n,
            None => return false,
        },
        "q" => match n.filter(|n| *n <= 2) {
            Some(n) => s.quality = n as u8,
            None => return false,
        },
        "f" => match n.filter(|n| *n <= 3 && s.uses_filter()) {
            Some(n) => s.filter = n as u8,
            None => return false,
        },
        "max" => match n.filter(|n| recfmt::MAX_SECS.contains(n)) {
            Some(n) => s.max_secs = n,
            None => return false,
        },
        "dither" if s.uses_dither() => s.dither = !s.dither,
        _ => return false,
    }
    true
}

fn pick(id: String) {
    if with(|st| matches!(st.phase, Phase::Ready | Phase::Countdown)) != Some(true) {
        return;
    }
    if id == "reselect" {
        emit(Some(vec![Ev::Reselect]));
        return;
    }
    let ev = with(|st| {
        let mut s = st.settings;
        if !apply_id(&mut s, &id) {
            return None;
        }
        st.settings = s;
        st.relayout();
        st.render();
        unsafe {
            let _ = SetForegroundWindow(st.hwnd);
        }
        Some(vec![Ev::Change(s)])
    })
    .flatten();
    emit(ev);
}

fn entries_for(chip: Chip, s: &Settings) -> Vec<Entry> {
    let one = |id: String, label: String, on: bool| Entry::item(id, label).checked(on);
    match chip {
        Chip::Format => vec![
            one("fmt:0".into(), "GIF  \u{2014}  plays everywhere".into(), !s.webp && !s.mp4),
            one("fmt:1".into(), "WebP  \u{2014}  full colour, smaller".into(), s.webp && !s.mp4),
            one("fmt:2".into(), "MP4  \u{2014}  video, smooth and small".into(), s.mp4),
        ],
        Chip::Fps => recfmt::FPS
            .iter()
            .map(|f| {
                let got = Settings { fps: *f, ..*s }.grab_fps();
                let label = if got == *f as u64 { format!("{f} frames per second") } else { format!("{got} frames per second  \u{2014}  the GIF maximum") };
                one(format!("fps:{f}"), label, s.fps == *f)
            })
            .collect(),
        Chip::Size => recfmt::SCALE
            .iter()
            .map(|v| one(format!("scale:{v}"), if *v == 100 { "100%  (original size)".into() } else { format!("{v}%") }, s.scale == *v))
            .collect(),
        Chip::Pointer => ["Hide the mouse pointer", "Show the mouse pointer", "Show it and highlight clicks"]
            .iter()
            .enumerate()
            .map(|(i, l)| one(format!("ptr:{i}"), (*l).into(), s.pointer as usize == i))
            .collect(),
        Chip::Delay => recfmt::COUNTDOWN
            .iter()
            .map(|d| one(format!("delay:{d}"), if *d == 0 { "Start at once".into() } else { format!("Count down {d} seconds") }, s.countdown == *d))
            .collect(),
        Chip::More => {
            vec![
                Entry::item("reselect", "Select another region\u{2026}"),
                Entry::Sep,
                Entry::sub(
                    "Quality",
                    ["Fast  \u{2014}  lightest on the CPU", "Balanced", "Best  \u{2014}  needs a fast PC"]
                        .iter()
                        .enumerate()
                        .map(|(i, l)| one(format!("q:{i}"), (*l).into(), s.quality as usize == i))
                        .collect(),
                ),
                Entry::sub(
                    "Noise filter",
                    ["Off", "Low", "Medium", "High"]
                        .iter()
                        .enumerate()
                        .map(|(i, l)| one(format!("f:{i}"), (*l).into(), s.filter as usize == i).enabled(s.uses_filter()))
                        .collect(),
                ),
                one("dither".into(), "Dithering (GIF)".into(), s.dither).enabled(s.uses_dither()),
                Entry::sub(
                    "Maximum length",
                    recfmt::MAX_SECS
                        .iter()
                        .map(|m| one(format!("max:{m}"), fmt_len(*m), s.max_secs == *m))
                        .collect(),
                ),
            ]
        }
    }
}

fn chip_label(chip: Chip, s: &Settings) -> String {
    match chip {
        Chip::Format => String::from(if s.mp4 { "MP4" } else if s.webp { "WebP" } else { "GIF" }),
        Chip::Fps => format!("{} fps", s.grab_fps()),
        Chip::Size => format!("{}%", s.scale),
        Chip::Pointer => ["No pointer", "Pointer", "Clicks"][s.pointer.min(2) as usize].to_string(),
        Chip::Delay => if s.countdown == 0 { "No delay".to_string() } else { format!("{} s delay", s.countdown) },
        Chip::More => "\u{2026}".to_string(),
    }
}

fn chip_labels(chip: Chip) -> Vec<String> {
    let s = Settings::default();
    let label = |s: Settings| chip_label(chip, &s);
    match chip {
        Chip::Format => (0..3).map(|i| label(Settings { webp: i == 1, mp4: i == 2, ..s })).collect(),
        Chip::Fps => [false, true].iter().flat_map(|webp| recfmt::FPS.iter().map(move |fps| Settings { webp: *webp, fps: *fps, ..s })).map(label).collect(),
        Chip::Size => recfmt::SCALE.iter().map(|scale| label(Settings { scale: *scale, ..s })).collect(),
        Chip::Pointer => (0..3).map(|pointer| label(Settings { pointer, ..s })).collect(),
        Chip::Delay => recfmt::COUNTDOWN.iter().map(|countdown| label(Settings { countdown: *countdown, ..s })).collect(),
        Chip::More => vec![label(s)],
    }
}

fn fmt_len(secs: u32) -> String {
    if secs < 60 { format!("{secs} seconds") } else { format!("{} min", secs / 60) }
}

fn fmt_time(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn menu_place(body: [i32; 4], x: i32, menu_h: i32, bottom: i32, scale: f32) -> ((i32, i32), bool) {
    let gap = (4.0 * scale) as i32;
    let below = body[1] + body[3] + gap;
    let down = below + menu_h <= bottom;
    ((body[0] + x, if down { below } else { body[1] - gap }), down)
}

fn open_chip(chip: Chip) {
    let info = with(|st| {
        let i = st.cells.iter().position(|c| c.item == Item::Chip(chip))?;
        let r = st.cells[i].rect;
        let entries = entries_for(chip, &st.settings);
        let (at, down) = menu_place(st.body, r[0] as i32, menu::height(&entries, &st.text, st.scale), st.work[3], st.scale);
        Some((entries, at, down, st.scale, st.dark, st.text.clone()))
    })
    .flatten();
    match info {
        Some((entries, at, true, scale, dark, text)) => menu::show(at, entries, scale, dark, text, pick),
        Some((entries, at, false, scale, dark, text)) => menu::show_above(at, entries, scale, dark, text, pick),
        None => {}
    }
}

impl St {
    fn new(area: [i32; 4], settings: Settings, scale: f32, dark: bool, text: Arc<Text>, shot: Option<&crate::capture::Frame>, on_event: Rc<dyn Fn(Ev)>) -> Option<Self> {
        let hinst = unsafe { GetModuleHandleW(None) }.ok().map(|h| HINSTANCE(h.0))?;
        {
            use std::sync::Once;
            static ONCE: Once = Once::new();
            ONCE.call_once(|| unsafe {
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinst,
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    lpszClassName: w!("SnptuRecBar"),
                    ..Default::default()
                };
                RegisterClassExW(&wc);
            });
        }
        let hwnd = unsafe {
            CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("SnptuRecBar"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)
        }
        .ok()?;
        let work = menu::work_area(POINT { x: area[0] + area[2] / 2, y: area[1] + area[3] / 2 });
        let mut st = St {
            hwnd,
            dc: HDC::default(),
            bmp: HBITMAP::default(),
            bits: std::ptr::null_mut(),
            size: (0, 0),
            body: [0; 4],
            backdrop: None,
            area,
            scale,
            dark,
            text,
            settings,
            phase: Phase::Ready,
            cells: Vec::new(),
            hover: None,
            pressed: None,
            body_y: 0,
            centre_x: area[0] + area[2] / 2,
            work,
            started: Instant::now(),
            paused_total: Duration::ZERO,
            paused_since: None,
            countdown_end: Instant::now(),
            shown: -1,
            border: None,
            on_event,
            alpha: 255,
            edit: None,
            drag: None,
        };
        let built = st.build();
        let (bw, bh) = (built.1.ceil() as i32, (BAR_H * scale).round() as i32);
        let (x, y, inside) = place(area, bw, bh, work, scale);
        st.body_y = y;
        st.centre_x = x + bw / 2;
        st.body = [x, y, bw, bh];
        st.cells = built.0;
        st.backdrop = shot
            .and_then(|f| menu::backdrop_from_frame(f, x, y, bw, bh, scale))
            .or_else(|| menu::grab_backdrop(x, y, bw, bh, scale))
            .map(|p| (p, [x, y, bw, bh]));
        if inside {
            unsafe {
                let _ = SetWindowDisplayAffinity(hwnd, windows::Win32::UI::WindowsAndMessaging::WDA_EXCLUDEFROMCAPTURE);
            }
        }
        crate::record::set_border_hook(border_mouse);
        st.border = Border::editable(area, TEAL);
        st.raise();
        st.resize_dib()?;
        Some(st)
    }

    fn resize_dib(&mut self) -> Option<()> {
        let m = (MARGIN * self.scale).round() as i32;
        let (ww, wh) = (self.body[2] + 2 * m, self.body[3] + 2 * m);
        if self.size == (ww, wh) && !self.bits.is_null() {
            return Some(());
        }
        unsafe {
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
                let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            }
            let dc = CreateCompatibleDC(None);
            let Some((bmp, bits)) = menu::make_dib(dc, ww, wh) else {
                let _ = DeleteDC(dc);
                self.dc = HDC::default();
                self.bits = std::ptr::null_mut();
                return None;
            };
            SelectObject(dc, HGDIOBJ(bmp.0));
            self.dc = dc;
            self.bmp = bmp;
            self.bits = bits;
            self.size = (ww, wh);
        }
        Some(())
    }


    fn build(&self) -> (Vec<Cell>, f32) {
        let s = self.scale;
        let px = TEXT_PX * s;
        let t = &*self.text;
        let (h, y) = (BTN_H * s, (BAR_H - BTN_H) * s / 2.0);
        let mut cells = Vec::new();
        let mut x = PAD * s;
        let add = |cells: &mut Vec<Cell>, x: &mut f32, item: Item, label: String, w: f32| {
            cells.push(Cell { item, rect: [*x, y, w, h], label });
            *x += w + GAP * s;
        };
        let chip_w = |label: &str| t.measure(label, px) + 34.0 * s;
        let s_ = &self.settings;
        let sep = |cells: &mut Vec<Cell>, x: &mut f32| {
            *x += 2.0 * s;
            cells.push(Cell { item: Item::Sep, rect: [*x, y + 4.0 * s, 1.0, h - 8.0 * s], label: String::new() });
            *x += 1.0 + (GAP + 2.0) * s;
        };
        match self.phase {
            Phase::Ready => {
                add(&mut cells, &mut x, Item::Grip, String::new(), 16.0 * s);
                let label = "Record".to_string();
                let w = t.measure(&label, px) + 52.0 * s;
                add(&mut cells, &mut x, Item::Record, label, w);
                let (ow, oh) = s_.out_size(self.area[2] as u32, self.area[3] as u32);
                let size = format!("{ow} \u{d7} {oh}");
                let w = t.measure(&size, px).max(t.measure("0000 \u{d7} 0000", px)) + 24.0 * s;
                add(&mut cells, &mut x, Item::Size, size, w);
                sep(&mut cells, &mut x);
                for c in [Chip::Format, Chip::Fps, Chip::Size, Chip::Pointer, Chip::Delay] {
                    let w = chip_labels(c).iter().map(|l| chip_w(l)).fold(0.0, f32::max);
                    add(&mut cells, &mut x, Item::Chip(c), chip_label(c, s_), w);
                }
                add(&mut cells, &mut x, Item::Chip(Chip::More), "\u{2026}".into(), 36.0 * s);
                sep(&mut cells, &mut x);
                add(&mut cells, &mut x, Item::Cancel, String::new(), BTN_H * s);
            }
            Phase::Countdown => {
                let left = self.countdown_end.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u32;
                let label = format!("Recording starts in {left}");
                let widest = format!("Recording starts in {}", s_.countdown);
                let w = t.measure(&label, px).max(t.measure(&widest, px)) + 14.0 * s;
                add(&mut cells, &mut x, Item::Text, label, w);
                let w = t.measure("Start now", px) + 28.0 * s;
                add(&mut cells, &mut x, Item::Record, "Start now".into(), w);
                let w = t.measure("Cancel", px) + 28.0 * s;
                add(&mut cells, &mut x, Item::Discard, "Cancel".into(), w);
            }
            Phase::Recording | Phase::Paused => {
                add(&mut cells, &mut x, Item::Dot, String::new(), 14.0 * s);
                let label = format!("{} / {}", fmt_time(self.elapsed().as_secs()), fmt_time(s_.max_secs as u64));
                let w = t.measure("00:00 / 00:00", px) + 8.0 * s;
                add(&mut cells, &mut x, Item::Text, label, w);
                sep(&mut cells, &mut x);
                let pause = if self.phase == Phase::Paused { "Resume" } else { "Pause" };
                let w = t.measure("Resume", px) + 44.0 * s;
                add(&mut cells, &mut x, Item::Pause, pause.into(), w);
                let w = t.measure("Stop", px) + 44.0 * s;
                add(&mut cells, &mut x, Item::Stop, "Stop".into(), w);
                let w = t.measure("Discard", px) + 24.0 * s;
                add(&mut cells, &mut x, Item::Discard, "Discard".into(), w);
            }
        }
        (cells, x - GAP * s + PAD * s)
    }

    fn elapsed(&self) -> Duration {
        let now = self.paused_since.unwrap_or_else(Instant::now);
        let limit = Duration::from_secs(self.settings.max_secs as u64);
        now.saturating_duration_since(self.started).saturating_sub(self.paused_total).min(limit)
    }

    fn relayout(&mut self) {
        let (cells, w) = self.build();
        let (full_w, bh) = (self.backdrop.as_ref().map_or(self.body[2], |b| b.1[2]), self.body[3]);
        let bw = (w.ceil() as i32).min(full_w).max(1);
        let x = (self.centre_x - bw / 2).clamp(self.work[0], (self.work[2] - bw).max(self.work[0]));
        if !same_items(&self.cells, &cells) {
            self.hover = None;
            self.pressed = None;
        }
        self.cells = cells;
        self.body = [x, self.body_y, bw, bh];
        let _ = self.resize_dib();
    }

    fn crop_backdrop(&self) -> Option<Pixmap> {
        let (full, r) = self.backdrop.as_ref()?;
        let (bw, bh) = (self.body[2], self.body[3]);
        if bw > r[2] || bh > r[3] {
            return None;
        }
        let bx = (self.body[0] - r[0]).clamp(0, r[2] - bw);
        let by = (self.body[1] - r[1]).clamp(0, r[3] - bh);
        let mut out = Pixmap::new(bw as u32, bh as u32)?;
        let (fw, ow) = (full.width() as usize * 4, bw as usize * 4);
        for y in 0..bh as usize {
            let src = (by as usize + y) * fw + bx as usize * 4;
            out.data_mut()[y * ow..(y + 1) * ow].copy_from_slice(&full.data()[src..src + ow]);
        }
        Some(out)
    }


    fn render(&mut self) {
        let Some(pix) = self.draw() else { return };
        if self.bits.is_null() {
            return;
        }
        let n = (self.size.0 * self.size.1) as usize * 4;
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits as *mut u8, n.min(pix.data().len())) };
        menu::rgba_to_bgra(dst, pix.data());
        self.present();
    }

    fn fade_in(&mut self) {
        let start = Instant::now();
        let mut frame = None;
        while self.alpha < 255 {
            let (a, done) = fade_alpha(start.elapsed().as_secs_f32() * 1000.0);
            self.alpha = if done { 255 } else { a };
            self.present();
            if !done {
                crate::settings::wait_frame(&mut frame);
            }
        }
    }

    fn present(&self) {
        if self.bits.is_null() {
            return;
        }
        let m = (MARGIN * self.scale).round() as i32;
        let pos = POINT { x: self.body[0] - m, y: self.body[1] - m };
        let size = SIZE { cx: self.size.0, cy: self.size.1 };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: self.alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe {
            let _ = UpdateLayeredWindow(self.hwnd, None, Some(&pos), Some(&size), Some(self.dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }

    fn draw(&self) -> Option<Pixmap> {
        let s = self.scale;
        let m = (MARGIN * s).round();
        let id = Transform::identity();
        let c = menu::pal(self.dark);
        let mut pix = Pixmap::new(self.size.0 as u32, self.size.1 as u32)?;
        let (bw, bh) = (self.body[2] as f32, self.body[3] as f32);
        let backdrop = self.crop_backdrop();
        menu::paint_glass(&mut pix, m, bw, bh, bh / 2.0, s, backdrop.as_ref(), self.dark)?;
        let px = TEXT_PX * s;
        let lh = self.text.line_height(px);
        for (i, cell) in self.cells.iter().enumerate() {
            let [rx, ry, rw, rh] = cell.rect;
            let (x, y) = (m + rx, m + ry);
            let hot = self.hover == Some(i) && cell.item.clickable();
            let down = hot && self.pressed == Some(i);
            let ghost = |pix: &mut Pixmap, always: bool| {
                let a = if down { c.hover[3] as u32 * 2 } else if hot { c.hover[3] as u32 } else if always { c.hover[3] as u32 / 3 } else { 0 };
                if a > 0
                    && let Some(p) = crate::toolbar::rrect(x, y, rw, rh, rh / 2.0)
                {
                    pix.fill_path(&p, &menu::paint([c.hover[0], c.hover[1], c.hover[2]], a.min(255) as u8), FillRule::Winding, id, None);
                }
            };
            let solid = |pix: &mut Pixmap, rgb: [u8; 3]| {
                let k = if down { 0.82 } else if hot { 1.08 } else { 1.0 };
                let col = [(rgb[0] as f32 * k).min(255.0) as u8, (rgb[1] as f32 * k).min(255.0) as u8, (rgb[2] as f32 * k).min(255.0) as u8];
                if let Some(p) = crate::toolbar::rrect(x, y, rw, rh, rh / 2.0) {
                    pix.fill_path(&p, &menu::paint(col, 255), FillRule::Winding, id, None);
                }
            };
            let label_at = |pix: &mut Pixmap, text: &str, tx: f32, ink: [u8; 3]| {
                self.text.draw(pix, text, tx, y + (rh - lh) / 2.0, px, ink);
            };
            let stroke = Stroke { width: 1.8 * s, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
            match cell.item {
                Item::Sep => {
                    if let Some(r) = tiny_skia::Rect::from_xywh(x, y, 1.0, rh) {
                        pix.fill_rect(r, &menu::paint([c.sep[0], c.sep[1], c.sep[2]], c.sep[3]), id, None);
                    }
                }
                Item::Text => label_at(&mut pix, &cell.label, x + 4.0 * s, c.ink),
                Item::Grip => {
                    let col = if hot || self.drag.is_some() { c.ink } else { c.hint };
                    for dx in [-2.5f32, 2.5] {
                        for dy in [-5.0f32, 0.0, 5.0] {
                            let mut pb = PathBuilder::new();
                            pb.push_circle(x + rw / 2.0 + dx * s, y + rh / 2.0 + dy * s, 1.3 * s);
                            if let Some(p) = pb.finish() {
                                pix.fill_path(&p, &menu::paint(col, 255), FillRule::Winding, id, None);
                            }
                        }
                    }
                }
                Item::Size => {
                    ghost(&mut pix, self.edit.is_some());
                    let tx = x + 12.0 * s;
                    if let Some(e) = &self.edit {
                        let text = format!("{} \u{d7} {}", e.w, e.h);
                        label_at(&mut pix, &text, tx, c.ink);
                        let upto = if e.on_h { text.as_str() } else { e.w.as_str() };
                        let cx = tx + self.text.measure(upto, px) + 1.0 * s;
                        if let Some(r) = tiny_skia::Rect::from_xywh(cx, y + (rh - lh) / 2.0, 1.5 * s, lh) {
                            pix.fill_rect(r, &menu::paint(TEAL, 255), id, None);
                        }
                    } else {
                        label_at(&mut pix, &cell.label, tx, c.ink);
                    }
                }
                Item::Dot => {
                    let col = if self.phase == Phase::Paused { AMBER } else { RED };
                    let mut pb = PathBuilder::new();
                    pb.push_circle(x + rw / 2.0, y + rh / 2.0, 5.0 * s);
                    if let Some(p) = pb.finish() {
                        pix.fill_path(&p, &menu::paint(col, 255), FillRule::Winding, id, None);
                    }
                }
                Item::Record => {
                    solid(&mut pix, RED);
                    let mut pb = PathBuilder::new();
                    let tw = self.text.measure(&cell.label, px);
                    let start = x + (rw - tw) / 2.0;
                    if cell.label == "Record" {
                        pb.push_circle(start - 12.0 * s, y + rh / 2.0, 4.5 * s);
                        if let Some(p) = pb.finish() {
                            pix.fill_path(&p, &menu::paint([255, 255, 255], 255), FillRule::Winding, id, None);
                        }
                    }
                    label_at(&mut pix, &cell.label, start + if cell.label == "Record" { 2.0 * s } else { 0.0 }, [255, 255, 255]);
                }
                Item::Stop => {
                    solid(&mut pix, RED);
                    let tw = self.text.measure(&cell.label, px);
                    let start = x + (rw - tw) / 2.0 + 8.0 * s;
                    if let Some(r) = tiny_skia::Rect::from_xywh(start - 18.0 * s, y + rh / 2.0 - 4.5 * s, 9.0 * s, 9.0 * s) {
                        pix.fill_rect(r, &menu::paint([255, 255, 255], 255), id, None);
                    }
                    label_at(&mut pix, &cell.label, start - 4.0 * s, [255, 255, 255]);
                }
                Item::Pause => {
                    ghost(&mut pix, true);
                    let tw = self.text.measure(&cell.label, px);
                    let start = x + (rw - tw) / 2.0 + 8.0 * s;
                    let (cx, cy) = (start - 14.0 * s, y + rh / 2.0);
                    let mut pb = PathBuilder::new();
                    if self.phase == Phase::Paused {
                        pb.move_to(cx - 3.0 * s, cy - 5.0 * s);
                        pb.line_to(cx + 5.0 * s, cy);
                        pb.line_to(cx - 3.0 * s, cy + 5.0 * s);
                        pb.close();
                        if let Some(p) = pb.finish() {
                            pix.fill_path(&p, &menu::paint(c.ink, 255), FillRule::Winding, id, None);
                        }
                    } else {
                        for dx in [-3.0f32, 3.0] {
                            if let Some(r) = tiny_skia::Rect::from_xywh(cx + dx * s - 1.4 * s, cy - 5.0 * s, 2.8 * s, 10.0 * s) {
                                pix.fill_rect(r, &menu::paint(c.ink, 255), id, None);
                            }
                        }
                    }
                    label_at(&mut pix, &cell.label, start - 4.0 * s, c.ink);
                }
                Item::Discard => {
                    ghost(&mut pix, false);
                    let tw = self.text.measure(&cell.label, px);
                    label_at(&mut pix, &cell.label, x + (rw - tw) / 2.0, c.hint);
                }
                Item::Cancel => {
                    ghost(&mut pix, false);
                    let (cx, cy, r) = (x + rw / 2.0, y + rh / 2.0, 4.2 * s);
                    let mut pb = PathBuilder::new();
                    pb.move_to(cx - r, cy - r);
                    pb.line_to(cx + r, cy + r);
                    pb.move_to(cx + r, cy - r);
                    pb.line_to(cx - r, cy + r);
                    if let Some(p) = pb.finish() {
                        pix.stroke_path(&p, &menu::paint(c.hint, 255), &stroke, id, None);
                    }
                }
                Item::Chip(chip) => {
                    ghost(&mut pix, true);
                    if chip == Chip::More {
                        for dx in [-6.0f32, 0.0, 6.0] {
                            let mut pb = PathBuilder::new();
                            pb.push_circle(x + rw / 2.0 + dx * s, y + rh / 2.0, 1.5 * s);
                            if let Some(p) = pb.finish() {
                                pix.fill_path(&p, &menu::paint(c.ink, 255), FillRule::Winding, id, None);
                            }
                        }
                    } else {
                        label_at(&mut pix, &cell.label, x + 12.0 * s, c.ink);
                        let (cx, cy) = (x + rw - 14.0 * s, y + rh / 2.0);
                        let mut pb = PathBuilder::new();
                        pb.move_to(cx - 3.6 * s, cy - 1.6 * s);
                        pb.line_to(cx, cy + 2.0 * s);
                        pb.line_to(cx + 3.6 * s, cy - 1.6 * s);
                        if let Some(p) = pb.finish() {
                            pix.stroke_path(&p, &menu::paint([c.arrow[0], c.arrow[1], c.arrow[2]], c.arrow[3]), &Stroke { width: 1.5 * s, ..stroke }, id, None);
                        }
                    }
                }
            }
        }
        Some(pix)
    }


    fn press_record(&mut self) -> Vec<Ev> {
        match self.phase {
            Phase::Ready => {
                let mut evs = self.settle();
                if let Some((w, h)) = self.too_small() {
                    evs.push(Ev::TooSmall(w, h));
                } else if self.settings.countdown > 0 {
                    menu::dismiss();
                    self.phase = Phase::Countdown;
                    self.countdown_end = Instant::now() + Duration::from_secs(self.settings.countdown as u64);
                    self.shown = -1;
                    self.border = Border::new(self.area, TEAL);
                    self.raise();
                    self.relayout();
                    self.render();
                    unsafe {
                        SetTimer(Some(self.hwnd), 1, 100, None);
                    }
                } else {
                    menu::dismiss();
                    evs.extend(self.begin());
                }
                evs
            }
            Phase::Countdown => self.begin(),
            _ => vec![],
        }
    }

    fn too_small(&self) -> Option<(u32, u32)> {
        let size = self.settings.out_size(self.area[2] as u32, self.area[3] as u32);
        (self.settings.mp4 && crate::mp4::too_small(size).is_some()).then_some(size)
    }

    fn settle(&mut self) -> Vec<Ev> {
        let mut evs = self.finish_edit();
        if self.drag.is_some() {
            unsafe {
                let _ = ReleaseCapture();
            }
            evs.extend(self.drop_drag());
        }
        evs
    }

    fn drop_drag(&mut self) -> Vec<Ev> {
        if self.drag.is_none() {
            return vec![];
        }
        self.pressed = None;
        let evs = self.end_drag();
        self.drag = None;
        evs
    }

    fn lose_capture(&mut self) -> Vec<Ev> {
        if self.drag.is_none() {
            return vec![];
        }
        let evs = self.drop_drag();
        self.render();
        evs
    }

    fn begin(&mut self) -> Vec<Ev> {
        self.phase = Phase::Recording;
        self.started = Instant::now();
        self.paused_total = Duration::ZERO;
        self.paused_since = None;
        self.shown = 0;
        self.border = Border::new(self.area, RED);
        self.relayout();
        self.render();
        unsafe {
            SetTimer(Some(self.hwnd), 1, 200, None);
        }
        vec![Ev::Begin]
    }

    fn click(&mut self, item: Item) -> Vec<Ev> {
        match item {
            Item::Record => self.press_record(),
            Item::Stop => vec![Ev::Stop],
            Item::Discard if self.phase == Phase::Countdown => vec![Ev::Cancel],
            Item::Discard => vec![Ev::Discard],
            Item::Cancel => vec![Ev::Cancel],
            Item::Pause => {
                if self.phase == Phase::Paused {
                    self.paused_total += self.paused_since.take().map_or(Duration::ZERO, |t| t.elapsed());
                    self.phase = Phase::Recording;
                    self.border = Border::new(self.area, RED);
                    self.relayout();
                    self.render();
                    unsafe {
                        SetTimer(Some(self.hwnd), 1, 200, None);
                    }
                    vec![Ev::Pause(false)]
                } else {
                    self.paused_since = Some(Instant::now());
                    self.phase = Phase::Paused;
                    self.border = Border::new(self.area, AMBER);
                    self.relayout();
                    self.render();
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), 1);
                    }
                    vec![Ev::Pause(true)]
                }
            }
            Item::Size => {
                if self.phase == Phase::Ready && self.edit.is_none() {
                    self.start_edit();
                }
                vec![]
            }
            Item::Grip | Item::Chip(_) | Item::Text | Item::Dot | Item::Sep => vec![],
        }
    }


    fn start_edit(&mut self) {
        let (w, h) = self.settings.out_size(self.area[2] as u32, self.area[3] as u32);
        self.edit = Some(SizeEdit { w: w.to_string(), h: h.to_string(), on_h: false, fresh: true });
        self.render();
    }

    fn typed(&mut self, ch: char) {
        let Some(e) = self.edit.as_mut() else { return };
        if ch.is_ascii_digit() {
            let f = if e.on_h { &mut e.h } else { &mut e.w };
            if e.fresh {
                f.clear();
                e.fresh = false;
            }
            if f.len() < 4 {
                f.push(ch);
            }
        } else if matches!(ch, 'x' | 'X' | '*' | ',' | '\u{d7}') {
            e.on_h = true;
            e.fresh = true;
        } else {
            return;
        }
        self.render();
    }

    fn edit_key(&mut self, k: u16) -> Vec<Ev> {
        let Some(e) = self.edit.as_mut() else { return vec![] };
        if k == VK_ESCAPE.0 {
            self.edit = None;
        } else if k == VK_RETURN.0 {
            return self.finish_edit();
        } else if k == VK_BACK.0 {
            let f = if e.on_h { &mut e.h } else { &mut e.w };
            if e.fresh {
                f.clear();
                e.fresh = false;
            } else {
                f.pop();
            }
        } else if k == VK_TAB.0 {
            e.on_h = !e.on_h;
            e.fresh = true;
        } else if k == VK_LEFT.0 || k == VK_RIGHT.0 {
            e.on_h = k == VK_RIGHT.0;
            e.fresh = true;
        } else {
            return vec![];
        }
        self.render();
        vec![]
    }

    fn finish_edit(&mut self) -> Vec<Ev> {
        let Some(e) = self.edit.take() else { return vec![] };
        let parse = |v: &str| v.parse::<u32>().ok().filter(|n| *n > 0);
        let (Some(ow), Some(oh)) = (parse(&e.w), parse(&e.h)) else {
            self.render();
            return vec![];
        };
        let pct = self.settings.scale.max(1) as u32;
        let region = |o: u32| if pct >= 100 { o } else { (o * 100).div_ceil(pct) };
        let (vx, vy, vw, vh) = virtual_screen();
        let w = (region(ow) as i32).clamp(16, vw.clamp(16, 4096));
        let h = (region(oh) as i32).clamp(16, vh.clamp(16, 4096));
        let x = self.area[0].min(vx + vw - w).max(vx);
        let y = self.area[1].min(vy + vh - h).max(vy);
        let area = [x, y, w, h];
        if area == self.area || crate::record::check_area(area).is_err() {
            self.render();
            return vec![];
        }
        self.move_to(area);
        vec![Ev::Area(area)]
    }

    fn raise(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos};
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    fn start_drag(&mut self, hit: u8) -> Vec<Ev> {
        let evs = self.finish_edit();
        let mut p = POINT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
        }
        self.drag = Some(Drag { hit, last: std::cell::Cell::new(self.area), cursor: (p.x, p.y), area: self.area, body: self.body });
        evs
    }

    fn dragged_area(&self) -> Option<[i32; 4]> {
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let d = self.drag.as_ref()?;
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let (vx, vy, vw, vh) = virtual_screen();
        let [x, y, w, h] = d.area;
        let (dx, dy) = (p.x - d.cursor.0, p.y - d.cursor.1);
        if d.hit == HIT_MOVE {
            let nx = (x + dx).clamp(vx, (vx + vw - w).max(vx));
            let ny = (y + dy).clamp(vy, (vy + vh - h).max(vy));
            return Some([nx, ny, w, h]);
        }
        let min = 16;
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
        if d.hit & HIT_L != 0 {
            x0 = (x + dx).max(vx).min(x1 - min);
        }
        if d.hit & HIT_R != 0 {
            x1 = (x + w + dx).min(vx + vw).max(x0 + min);
        }
        if d.hit & HIT_T != 0 {
            y0 = (y + dy).max(vy).min(y1 - min);
        }
        if d.hit & HIT_B != 0 {
            y1 = (y + h + dy).min(vy + vh).max(y0 + min);
        }
        let a = [x0, y0, x1 - x0, y1 - y0];
        if crate::record::check_area(a).is_err() {
            return Some(d.last.get());
        }
        d.last.set(a);
        Some(a)
    }

    fn drag_to(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
        let (Some(area), Some(d)) = (self.dragged_area(), self.drag.as_ref()) else { return };
        if d.hit != HIT_MOVE {
            if area == self.area {
                return;
            }
            if let Some(b) = &self.border {
                b.set_area(area);
            }
            self.follow(area);
            return;
        }
        let (dx, dy) = (area[0] - d.area[0], area[1] - d.area[1]);
        let m = (MARGIN * self.scale).round() as i32;
        if let Some(b) = &self.border {
            b.move_to(area);
        }
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, d.body[0] - m + dx, d.body[1] - m + dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    fn follow(&mut self, area: [i32; 4]) {
        use windows::Win32::UI::WindowsAndMessaging::{WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
        self.area = area;
        self.work = menu::work_area(POINT { x: area[0] + area[2] / 2, y: area[1] + area[3] / 2 });
        let (full_w, bh) = (self.backdrop.as_ref().map_or(self.body[2], |b| b.1[2]), self.body[3]);
        let (x, y, inside) = place(area, full_w, bh, self.work, self.scale);
        self.body_y = y;
        self.centre_x = x + full_w / 2;
        self.body = [x, y, self.body[2], bh];
        unsafe {
            let _ = SetWindowDisplayAffinity(self.hwnd, if inside { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
        }
        self.relayout();
        self.render();
    }

    fn end_drag(&mut self) -> Vec<Ev> {
        let Some(area) = self.dragged_area() else { return vec![] };
        let moved = self.drag.take().is_some_and(|d| d.area != area);
        if !moved {
            return vec![];
        }
        self.move_to(area);
        vec![Ev::Area(area)]
    }

    fn move_to(&mut self, area: [i32; 4]) {
        use windows::Win32::{
            Graphics::Dwm::DwmFlush,
            UI::WindowsAndMessaging::{SW_HIDE, WDA_EXCLUDEFROMCAPTURE, WDA_NONE},
        };
        self.area = area;
        self.work = menu::work_area(POINT { x: area[0] + area[2] / 2, y: area[1] + area[3] / 2 });
        let (bw, bh) = (self.backdrop.as_ref().map_or(self.body[2], |b| b.1[2]), self.body[3]);
        let (x, y, inside) = place(area, bw, bh, self.work, self.scale);
        if inside {
            self.border = None;
        } else {
            match &self.border {
                Some(b) => b.set_area(area),
                None => self.border = Border::editable(area, TEAL),
            }
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = DwmFlush();
        }
        self.body_y = y;
        self.centre_x = x + bw / 2;
        self.body = [x, y, bw, bh];
        unsafe {
            let _ = SetWindowDisplayAffinity(self.hwnd, if inside { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
        }
        self.backdrop = menu::grab_backdrop(x, y, bw, bh, self.scale).map(|p| (p, [x, y, bw, bh]));
        if self.border.is_none() {
            self.border = Border::editable(area, TEAL);
        }
        self.relayout();
        self.alpha = if crate::settings::animations_on() { 0 } else { 255 };
        self.render();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.raise();
        self.fade_in();
    }

    fn tick(&mut self) -> Vec<Ev> {
        match self.phase {
            Phase::Countdown => {
                if Instant::now() >= self.countdown_end {
                    return self.begin();
                }
                let left = self.countdown_end.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as i64;
                if left != self.shown {
                    self.shown = left;
                    self.relayout();
                    self.render();
                }
            }
            Phase::Recording => {
                let secs = self.elapsed().as_secs() as i64;
                if secs != self.shown {
                    self.shown = secs;
                    self.relayout();
                    self.render();
                }
            }
            _ => {}
        }
        vec![]
    }

    fn cell_at(&self, x: i32, y: i32) -> Option<usize> {
        let m = (MARGIN * self.scale).round();
        let (fx, fy) = (x as f32 - m, y as f32 - m);
        self.cells.iter().position(|c| c.item.clickable() && fx >= c.rect[0] && fx < c.rect[0] + c.rect[2] && fy >= c.rect[1] && fy < c.rect[1] + c.rect[3])
    }
}

impl Drop for St {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), 1);
            let _ = DestroyWindow(self.hwnd);
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
                let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            }
        }
    }
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};
    unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) }
}

fn border_mouse(msg: u32, hwnd: HWND) -> bool {
    use windows::Win32::UI::{
        Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
        WindowsAndMessaging::{GetCursorPos, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE},
    };
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    let at = (p.x, p.y);
    match msg {
        WM_SETCURSOR => with(|st| {
            let hit = match &st.drag {
                Some(d) => d.hit,
                None if st.phase == Phase::Ready => region_hit(st.area, at),
                None => 0,
            };
            let id = match hit {
                HIT_MOVE => IDC_SIZEALL,
                h if h == HIT_L || h == HIT_R => IDC_SIZEWE,
                h if h == HIT_T || h == HIT_B => IDC_SIZENS,
                h if h == HIT_L | HIT_T || h == HIT_R | HIT_B => IDC_SIZENWSE,
                h if h == HIT_R | HIT_T || h == HIT_L | HIT_B => IDC_SIZENESW,
                _ => return false,
            };
            unsafe {
                SetCursor(LoadCursorW(None, id).ok());
            }
            true
        })
        .unwrap_or(false),
        WM_LBUTTONDOWN => {
            let evs = with(|st| {
                if st.phase != Phase::Ready {
                    return None;
                }
                let hit = region_hit(st.area, at);
                if hit == 0 {
                    return None;
                }
                let evs = st.start_drag(hit);
                unsafe {
                    SetCapture(hwnd);
                }
                Some(evs)
            })
            .flatten();
            let handled = evs.is_some();
            emit(evs);
            handled
        }
        WM_MOUSEMOVE => with(|st| {
            if st.drag.is_none() {
                return false;
            }
            st.drag_to();
            true
        })
        .unwrap_or(false),
        WM_LBUTTONUP => {
            if !with(|st| st.drag.is_some()).unwrap_or(false) {
                return false;
            }
            unsafe {
                let _ = ReleaseCapture();
            }
            emit(with(|st| {
                let evs = st.end_drag();
                st.hover = None;
                st.render();
                evs
            }));
            true
        }
        WM_CAPTURECHANGED => {
            emit(with(St::lose_capture));
            false
        }
        _ => false,
    }
}


fn place(area: [i32; 4], bw: i32, bh: i32, wa: [i32; 4], s: f32) -> (i32, i32, bool) {
    let x = (area[0] + area[2] / 2 - bw / 2).clamp(wa[0], (wa[2] - bw).max(wa[0]));
    let gap = FRAME_GAP + (SPACE * s) as i32;
    let below = area[1] + area[3] + gap;
    if below + bh <= wa[3] {
        return (x, below, false);
    }
    let above = area[1] - gap - bh;
    if above >= wa[1] {
        return (x, above, false);
    }
    let inside = (area[1] + area[3] - bh - (12.0 * s) as i32).clamp(wa[1], (wa[3] - bh).max(wa[1]));
    (x, inside, true)
}


extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(hwnd, msg, wparam, lparam)))
        .unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

fn handle(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = menu::signed(lparam);
            if with(|st| {
                if st.drag.is_some() {
                    st.drag_to();
                }
                st.drag.is_some()
            })
            .unwrap_or(false)
            {
                return LRESULT(0);
            }
            with(|st| {
                let h = st.cell_at(x, y);
                if h != st.hover {
                    st.hover = h;
                    st.render();
                }
                let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: st.hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut t);
                }
            });
        }
        WM_MOUSELEAVE => {
            with(|st| {
                if st.hover.is_some() && st.pressed.is_none() {
                    st.hover = None;
                    st.render();
                }
            });
        }
        WM_LBUTTONDOWN => {
            let (x, y) = menu::signed(lparam);
            emit(with(|st| {
                st.pressed = st.cell_at(x, y);
                st.hover = st.pressed;
                let mut evs = vec![];
                if st.phase == Phase::Ready && st.pressed.is_some_and(|i| st.cells[i].item == Item::Grip) {
                    evs = st.start_drag(HIT_MOVE);
                }
                st.render();
                evs
            }));
            unsafe {
                SetCapture(hwnd);
            }
        }
        WM_CAPTURECHANGED if HWND(lparam.0 as *mut c_void) != hwnd => emit(with(St::lose_capture)),
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let (x, y) = menu::signed(lparam);
            if with(|st| st.drag.is_some()).unwrap_or(false) {
                emit(with(|st| {
                    st.pressed = None;
                    let evs = st.end_drag();
                    st.hover = st.cell_at(x, y);
                    st.render();
                    evs
                }));
                return LRESULT(0);
            }
            let hit = with(|st| {
                let (was, now) = (st.pressed.take(), st.cell_at(x, y));
                st.hover = now;
                st.render();
                if was.is_some() && was == now { now.map(|i| st.cells[i].item) } else { None }
            })
            .flatten();
            if !matches!(hit, Some(Item::Size)) {
                emit(with(|st| st.finish_edit()));
            }
            match hit {
                Some(Item::Chip(c)) => open_chip(c),
                Some(item) => emit(with(|st| st.click(item))),
                None => {}
            }
        }
        WM_KEYDOWN => {
            let k = wparam.0 as u16;
            if with(|st| st.edit.is_some()).unwrap_or(false) {
                emit(with(|st| st.edit_key(k)));
            } else if k == VK_ESCAPE.0 {
                emit(with(|st| if st.phase == Phase::Ready || st.phase == Phase::Countdown { vec![Ev::Cancel] } else { vec![] }));
            } else if k == VK_RETURN.0 || k == VK_SPACE.0 {
                emit(with(|st| if st.phase == Phase::Ready { st.press_record() } else { vec![] }));
            }
        }
        WM_CHAR => {
            if let Some(ch) = char::from_u32(wparam.0 as u32) {
                with(|st| st.typed(ch));
            }
        }
        WM_SETCURSOR => {
            let on_grip = with(|st| st.drag.is_some() || st.hover.is_some_and(|i| st.cells.get(i).is_some_and(|c| c.item == Item::Grip))).unwrap_or(false);
            if on_grip {
                unsafe {
                    SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
                }
                return LRESULT(1);
            }
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        WM_TIMER => emit(with(|st| st.tick())),
        WM_MOUSEACTIVATE => {
            let ready = with(|st| st.phase == Phase::Ready).unwrap_or(false);
            return LRESULT(if ready { 1 } else { 3 });
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}
