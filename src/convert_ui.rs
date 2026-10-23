
use crate::{
    convert::{IfExists, Kind, Output, Params, PngLevel, Progress, ResizeFilter, ResizeMode, Source},
    settings::{
        EASE_OUT, EASE_PAGE, EASE_PILL, EASE_SPRING, HOVER_MS, KNOB_MS, PILL_MS, Presses, Tween, accent_paint, accent_paint_a, bezier, center_on_cursor_monitor, enable_acrylic, glass_card, glass_chip, mix,
        paint, paint4, press_wash, pressed_rect, render_background, round_corners, set_dark_mode, strip_caption_buttons,
    },
    text::Text,
    theme::Pal,
    toolbar::rrect,
};
use anyhow::Result;
use std::{
    collections::HashSet,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Instant,
};
use tiny_skia::{Color, FillRule, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};
use unicode_segmentation::UnicodeSegmentation;
use winit::{
    dpi::LogicalSize,
    event_loop::ActiveEventLoop,
    keyboard::ModifiersState,
    platform::windows::WindowAttributesExtWindows,
    window::{Window, WindowId},
};

pub const W: f32 = 820.0;
pub const H: f32 = 720.0;
const TITLE_H: f32 = 40.0;
const TOP: f32 = 52.0;
const LEFT_X: f32 = 16.0;
const LEFT_W: f32 = 296.0;
const RIGHT_X: f32 = 328.0;
const RIGHT_W: f32 = W - RIGHT_X - 16.0;
const ROW_H: f32 = 40.0;
const FILE_ROW_H: f32 = 42.0;
const BOTTOM_Y: f32 = H - 62.0;
const LIST_TOP: f32 = 74.0;
pub const MAX_FILES: usize = 10_000;
const SLIDER_LABEL_W: f32 = 44.0;
const SLIDER_GAP: f32 = 10.0;
const SLIDER_MARGIN: f32 = 6.0;
const SLIDE_MS: f32 = 300.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wd {
    Close,
    AddFiles,
    Clear,
    Remove(usize),
    Page,
    Format,
    Lossless,
    Quality,
    Effort,
    Png,
    Deep,
    Profile,
    GifSpeed,
    Resize,
    RMode,
    RFilter,
    FPercent,
    FW,
    FH,
    Folder,
    Choose,
    FTemplate,
    Exists,
    DelOrig,
    Threads,
    Convert,
    Cancel,
    OpenFolder,
    Defaults,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Effect {
    AddFiles,
    ChooseFolder,
    FormatMenu([f32; 4]),
    FilterMenu([f32; 4]),
    Convert,
    Cancel,
    OpenFolder,
    Close,
}

#[derive(Clone, Debug)]
pub enum Status {
    Waiting,
    Done(PathBuf),
    Kept(PathBuf, &'static str),
    Skipped,
    Failed(String),
}

pub struct FileRow {
    pub src: Source,
    pub name: String,
    pub info: String,
    pub status: Status,
    pub slot: Option<usize>,
}

#[derive(Clone, Default)]
pub struct LineEdit {
    pub text: String,
    pub caret: usize,
    pub all: bool,
}

impl LineEdit {
    pub fn new(s: &str) -> Self {
        Self { text: s.to_string(), caret: s.len(), all: false }
    }

    pub fn insert(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if self.all {
            self.text.clear();
            self.caret = 0;
            self.all = false;
        }
        if self.text.chars().count() < 120 {
            self.text.insert(self.caret, c);
            self.caret += c.len_utf8();
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| !c.is_control()) {
            if !self.all && self.text.chars().count() >= 120 {
                break;
            }
            self.insert(c);
        }
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.caret].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.caret..].graphemes(true).next().map_or(self.caret, |g| self.caret + g.len())
    }

    pub fn backspace(&mut self) {
        if self.all {
            self.text.clear();
            self.caret = 0;
            self.all = false;
        } else if self.caret > 0 {
            let p = self.prev_boundary();
            self.text.replace_range(p..self.caret, "");
            self.caret = p;
        }
    }

    pub fn delete(&mut self) {
        if self.all {
            return self.backspace();
        }
        let n = self.next_boundary();
        if n > self.caret {
            self.text.replace_range(self.caret..n, "");
        }
    }

    pub fn left(&mut self) {
        self.all = false;
        self.caret = self.prev_boundary();
    }

    pub fn right(&mut self) {
        self.all = false;
        self.caret = self.next_boundary();
    }

    pub fn home(&mut self) {
        self.all = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.all = false;
        self.caret = self.text.len();
    }
}

#[derive(Clone)]
pub enum Ctl {
    Chip(String),
    Drop(String),
    Switch(bool),
    Seg(Vec<String>, usize),
    Slider(f32, String, bool),
    Field,
    Button { label: &'static str, primary: bool, enabled: bool },
}

fn seg(labels: &[&str], sel: usize) -> Ctl {
    Ctl::Seg(labels.iter().map(|l| l.to_string()).collect(), sel)
}

pub struct Placed {
    pub id: Wd,
    pub rect: [f32; 4],
    pub ctl: Ctl,
}

pub struct RowText {
    pub x: f32,
    pub y: f32,
    pub h: f32,
    pub title: &'static str,
    pub desc: String,
}

type OptionRow = (&'static str, String, Vec<(Wd, Ctl, f32)>);

pub struct Layout {
    pub cards: Vec<[f32; 4]>,
    pub rows: Vec<RowText>,
    pub items: Vec<Placed>,
    pub heads: Vec<(f32, f32, String)>,
    pub list: [f32; 4],
}

#[derive(Default)]
pub struct Estimating {
    key: Option<(String, Params)>,
    value: Option<Result<crate::convert::Estimate, String>>,
    round: u64,
    busy: bool,
}

impl Estimating {
    fn begin(&mut self, key: (String, Params)) -> u64 {
        static ROUND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        self.key = Some(key);
        self.value = None;
        self.round = ROUND.fetch_add(1, Ordering::Relaxed) + 1;
        self.busy = true;
        self.round
    }

    fn end(&mut self, round: u64, res: Result<crate::convert::Estimate, String>) {
        if round == self.round {
            self.busy = false;
            self.value = Some(res);
        }
    }
}

struct Page {
    files: Vec<FileRow>,
    params: Params,
    scroll: f32,
    progress: Option<Arc<Progress>>,
    status: Option<(String, bool)>,
}

impl Page {
    fn new(animation: bool) -> Self {
        Self { files: Vec::new(), params: Params { animation, ..Params::new(Kind::Webp) }, scroll: 0.0, progress: None, status: None }
    }
}

pub struct ConvertWindow {
    pub window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    pal: Pal,
    acrylic: bool,
    semibold: Option<Arc<Text>>,
    bg: Option<Pixmap>,
    canvas: Option<Pixmap>,
    list_layer: Option<Pixmap>,
    pub(crate) frames: crate::settings::FrameClock,
    pub(crate) opening: Option<crate::settings::Opening>,
    icon: Option<(u32, Pixmap)>,
    pub animation: bool,
    other: Page,
    pub files: Vec<FileRow>,
    listed: HashSet<Vec<u16>>,
    pub params: Params,
    pub out_dir: Option<PathBuf>,
    pub template: LineEdit,
    pub exists: IfExists,
    pub delete_original: bool,
    pub threads: usize,
    pub resize_on: bool,
    pub resize_mode: u8,
    pub f_percent: LineEdit,
    pub f_w: LineEdit,
    pub f_h: LineEdit,
    pub filter: ResizeFilter,
    pub scroll: f32,
    pub hover: Option<Wd>,
    pub drag: Option<Wd>,
    pub focus: Option<Wd>,
    pub mods: ModifiersState,
    pub cursor: (f32, f32),
    pub progress: Option<Arc<Progress>>,
    pub status: Option<(String, bool)>,
    pub dropped: Vec<PathBuf>,
    hovers: Vec<(Wd, Tween, f32)>,
    anims: Vec<(Wd, Tween, f32)>,
    pub(crate) presses: Presses<Wd>,
    bar: Option<(usize, Tween, f32)>,
    est: Estimating,
    body: Option<Pixmap>,
    fixed: Option<Pixmap>,
    xfade: Option<Xfade>,
    page_alpha: f32,
    desc_fx: crate::settings::DescFx<&'static str>,
    desc_page: Option<bool>,
}

pub fn kind_blurb(k: Kind, animation: bool) -> &'static str {
    match k {
        Kind::Png => "Lossless; universal compatibility",
        Kind::Jpeg => "Lossy; best suited to photographs",
        Kind::Webp if animation => "Animated; far smaller than GIF, with full colour and transparency",
        Kind::Webp => "Lossy or lossless; efficient for the web",
        Kind::Gif => "Plays everywhere; up to 256 colours per frame",
        Kind::Ico => "Windows icon, up to 256 \u{d7} 256 px",
    }
}

pub fn filter_label(f: ResizeFilter) -> &'static str {
    match f {
        ResizeFilter::Lanczos => "Lanczos-3",
        ResizeFilter::CatmullRom => "Catmull-Rom",
        ResizeFilter::Bilinear => "Bilinear",
        ResizeFilter::Nearest => "Nearest neighbour",
    }
}

fn human_size(bytes: u64) -> String {
    if bytes >= 10 << 20 {
        format!("{:.0} MB", bytes as f64 / (1 << 20) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1 << 20) as f64)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

type Facts = (u64, bool, Option<(u32, u32)>);

fn file_facts(path: &Path) -> Option<Facts> {
    use std::io::Seek;
    let len = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return None,
        Err(_) => return Some((0, false, None)),
    };
    let Ok(f) = std::fs::File::open(path) else { return Some((len, false, None)) };
    let mut r = std::io::BufReader::new(f);
    let animated = crate::anim::animated_in(&mut r);
    let back = r.stream_position().ok().and_then(|at| r.seek_relative(-(at as i64)).ok());
    Some((len, animated, back.and_then(|()| picture_size(r, path))))
}

fn look_at(paths: Vec<PathBuf>, listed: &mut HashSet<Vec<u16>>, mut room: usize) -> (Vec<(PathBuf, Facts)>, bool) {
    let mut found = Vec::new();
    for p in paths {
        let key = list_key(&p);
        if listed.contains(&key) {
            continue;
        }
        if room == 0 {
            return (found, true);
        }
        if let Some(facts) = file_facts(&p) {
            listed.insert(key);
            found.push((p, facts));
            room -= 1;
        }
    }
    (found, false)
}

fn list_key(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut key = Vec::new();
    for c in char::decode_utf16(p.as_os_str().encode_wide()) {
        match c {
            Ok(c) => {
                let mut up = c.to_uppercase();
                let c = match (up.next(), up.next()) {
                    (Some(u), None) => u,
                    _ => c,
                };
                key.extend_from_slice(c.encode_utf16(&mut [0; 2]));
            }
            Err(e) => key.push(e.unpaired_surrogate()),
        }
    }
    let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let (unc, verbatim) = (units(r"\\?\UNC\"), units(r"\\?\"));
    if key.starts_with(&unc) {
        key.splice(..unc.len(), units(r"\\"));
    } else if key.starts_with(&verbatim) {
        key.drain(..verbatim.len());
    }
    key
}

fn picture_size<R: std::io::BufRead + std::io::Seek>(mut r: R, path: &Path) -> Option<(u32, u32)> {
    use image::{ImageFormat, ImageReader};
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let named = ImageFormat::from_path(path).ok();
        let head = r.fill_buf().ok()?;
        let guessed = image::guess_format(&head[..head.len().min(16)]).ok();
        if head.starts_with(&[0xFF, 0xD8]) && (named == Some(ImageFormat::Jpeg) || guessed == Some(ImageFormat::Jpeg)) {
            return jpeg_size(&mut r);
        }
        if guessed == Some(ImageFormat::Ico) || head.starts_with(&[0, 0, 1, 0]) {
            return crate::convert::ico_size(&mut r);
        }
        if head.len() >= 30 && head.starts_with(b"RIFF") && &head[8..16] == b"WEBPVP8X" {
            let w = 1 + u32::from_le_bytes([head[24], head[25], head[26], 0]);
            let h = 1 + u32::from_le_bytes([head[27], head[28], head[29], 0]);
            return w.checked_mul(h).map(|_| (w, h));
        }
        if head.len() >= 24 && head.starts_with(b"\x89PNG\r\n\x1a\n") && &head[12..16] == b"IHDR" {
            let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
            let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
            return (w > 0 && h > 0).then_some((w, h));
        }
        let format = guessed.or(named)?;
        crate::convert::image_format_allowed(Some(format)).then(|| ImageReader::with_format(r, format).into_dimensions().ok()).flatten()
    }))
    .ok()
    .flatten()
}

fn jpeg_size<R: std::io::Read + std::io::Seek>(r: &mut R) -> Option<(u32, u32)> {
    let mut left: u32 = 64 * 1024;
    let mut reach: u64 = 16 << 20;
    let mut byte = |r: &mut R| {
        left = left.checked_sub(1)?;
        let mut b = [0u8];
        r.read_exact(&mut b).ok().map(|()| b[0])
    };
    if (byte(r)?, byte(r)?) != (0xFF, 0xD8) {
        return None;
    }
    let mut last = 0;
    loop {
        let mut m = byte(r)?;
        if (m == 0xFF || m == 0) && last == 0xFF {
            while m == 0xFF || m == 0 {
                last = m;
                m = byte(r)?;
            }
        }
        if last == 0xFF {
            match m {
                0xC0..=0xC2 => {
                    let mut f = [0u8; 8];
                    r.read_exact(&mut f).ok()?;
                    let [l0, l1, bits, h0, h1, w0, w1, parts] = f;
                    let (h, w) = (u16::from_be_bytes([h0, h1]), u16::from_be_bytes([w0, w1]));
                    let fits = parts > 0 && u16::from_be_bytes([l0, l1]) == 8 + 3 * u16::from(parts);
                    return (bits == 8 && w > 0 && h > 0 && fits).then_some((w.into(), h.into()));
                }
                0xCC | 0xD9 | 0xDA | 0xDC => return None,
                _ => {
                    let len = u16::from_be_bytes([byte(r)?, byte(r)?]);
                    if len < 2 {
                        return None;
                    }
                    reach = reach.checked_sub(u64::from(len) - 2)?;
                    r.seek_relative(i64::from(len) - 2).ok()?;
                }
            }
        }
        last = m;
    }
}

fn slider_track(rect: [f32; 4], s: f32) -> (f32, f32) {
    (rect[0] + (SLIDER_LABEL_W + SLIDER_GAP) * s, rect[0] + rect[2] - SLIDER_MARGIN * s)
}

fn slider_pos(rect: [f32; 4], s: f32, x: f32) -> f32 {
    let (t0, t1) = slider_track(rect, s);
    ((x - t0) / (t1 - t0)).clamp(0.0, 1.0)
}

fn seg_at(x: f32, rect: [f32; 4], n: usize) -> usize {
    ((x - rect[0]) / rect[2].max(1.0) * n as f32).floor().clamp(0.0, n.saturating_sub(1) as f32) as usize
}

fn remove_rect(list: [f32; 4], i: usize, scroll: f32) -> Option<[f32; 4]> {
    let y = list[1] + i as f32 * FILE_ROW_H - scroll + 9.0;
    let (top, bottom) = (y.max(list[1]), (y + 24.0).min(list[1] + list[3]));
    (bottom > top).then_some([list[0] + list[2] - 30.0, top, 24.0, bottom - top])
}

fn adopt(rows: &mut [FileRow], outcomes: &[Option<crate::convert::Outcome>]) {
    for row in rows.iter_mut().filter(|r| matches!(r.status, Status::Waiting)) {
        let Some(Some(o)) = row.slot.and_then(|s| outcomes.get(s)) else { continue };
        row.status = match o {
            crate::convert::Outcome::Done(p) => Status::Done(p.clone()),
            crate::convert::Outcome::Kept(p, why) => Status::Kept(p.clone(), why),
            crate::convert::Outcome::Skipped => Status::Skipped,
            crate::convert::Outcome::Failed(e) => Status::Failed(crate::failmsg::plain(e)),
        };
    }
}

fn converted(st: &Status) -> bool {
    matches!(st, Status::Done(_) | Status::Kept(..))
}

fn tally(rows: &[FileRow]) -> (usize, usize, usize) {
    let count = |is: fn(&Status) -> bool| rows.iter().filter(|f| is(&f.status)).count();
    (count(converted), count(|st| matches!(st, Status::Skipped)), count(|st| matches!(st, Status::Failed(_))))
}

fn batch_line(total: usize, finished: usize, ended: bool, cancelling: bool, (done, skipped, failed): (usize, usize, usize)) -> String {
    if !ended {
        return if cancelling { "Cancelling\u{2026}".to_string() } else { format!("Converting {} of {total}\u{2026}", (finished + 1).min(total)) };
    }
    if finished < total {
        return format!("Cancelled: {done} of {total} converted");
    }
    let mut parts = Vec::new();
    if done > 0 || skipped == 0 {
        parts.push(format!("{done} converted"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    format!("Done: {}", parts.join(", "))
}

const EFFORT_RANGE: (i32, i32) = (0, 6);

impl ConvertWindow {
    pub fn new(el: &ActiveEventLoop, dark: bool) -> Result<Self> {
        let window = Arc::new(el.create_window(
            crate::app_window_attributes()
                .with_title("Snptu convert")
                .with_inner_size(LogicalSize::new(W as f64, H as f64))
                .with_resizable(false)
                .with_decorations(false)
                .with_undecorated_shadow(true)
                .with_maximized(false)
                .with_visible(false),
        )?);
        crate::set_window_class_icon(&window);
        round_corners(&window);
        let acrylic = enable_acrylic(&window, dark);
        center_on_cursor_monitor(el, &window);
        let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Self {
            window,
            surface,
            pal: Pal::new(dark),
            acrylic,
            semibold: Text::load_semibold(),
            bg: None,
            canvas: None,
            list_layer: None,
            frames: Default::default(),
            opening: None,
            icon: None,
            animation: false,
            other: Page::new(true),
            files: Vec::new(),
            listed: HashSet::new(),
            params: Params::new(Kind::Webp),
            out_dir: None,
            template: LineEdit::new("{name}"),
            exists: IfExists::Rename,
            delete_original: false,
            threads: crate::par::workers(),
            resize_on: false,
            resize_mode: 0,
            f_percent: LineEdit::new("50"),
            f_w: LineEdit::new("1920"),
            f_h: LineEdit::new("1080"),
            filter: ResizeFilter::Lanczos,
            scroll: 0.0,
            hover: None,
            drag: None,
            focus: None,
            mods: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            progress: None,
            status: None,
            dropped: Vec::new(),
            hovers: Vec::new(),
            anims: Vec::new(),
            presses: Presses::new(),
            bar: None,
            body: None,
            fixed: None,
            xfade: None,
            page_alpha: 1.0,
            desc_fx: crate::settings::DescFx::new(),
            desc_page: None,
            est: Estimating::default(),
        })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn show(&self) {
        self.window.set_visible(true);
        strip_caption_buttons(&self.window);
    }

    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    pub fn running(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| !p.ended.load(Ordering::Relaxed))
    }

    pub fn add_files(&mut self, paths: Vec<PathBuf>) {
        let (mut on_show, mut on_other) = (0, 0);
        let room = MAX_FILES.saturating_sub(self.files.len() + self.other.files.len());
        let (found, full) = look_at(paths, &mut self.listed, room);
        for (p, (size, animated, dims)) in found {
            let mut info = match dims {
                Some((w, h)) => format!("{w} x {h}  \u{b7}  {}", human_size(size)),
                None => human_size(size),
            };
            if animated {
                info += "  \u{b7}  animated";
            }
            let name = p.file_name().map_or_else(|| "image".into(), |n| n.to_string_lossy().into_owned());
            let row = FileRow { src: Source::File(p), name, info, status: Status::Waiting, slot: None };
            if animated == self.animation {
                self.files.push(row);
                on_show += 1;
            } else {
                self.other.files.push(row);
                on_other += 1;
            }
        }
        if on_other > 0 && on_show == 0 && !self.running() {
            self.settle();
            self.swap_page();
        }
        if full {
            self.status = Some((format!("The list holds {MAX_FILES} files at most"), true));
        }
    }

    pub fn swap_page(&mut self) {
        if self.running() {
            return;
        }
        self.animation = !self.animation;
        std::mem::swap(&mut self.files, &mut self.other.files);
        std::mem::swap(&mut self.params, &mut self.other.params);
        std::mem::swap(&mut self.scroll, &mut self.other.scroll);
        std::mem::swap(&mut self.progress, &mut self.other.progress);
        std::mem::swap(&mut self.status, &mut self.other.status);
        self.focus = None;
        self.drag = None;
        self.anims.retain(|a| a.0 == Wd::Page);
        let seen = self.page_alpha;
        let now = Instant::now();
        let (to, s) = (usize::from(self.animation), self.scale());
        if let Some(xf) = self.xfade.as_mut().filter(|xf| xf.cache.is_some() && !xf.over(now) && crate::settings::animations_on()) {
            xf.restart(now, s, to);
            if let Some(page) = xf.cache.as_mut().and_then(|c| c.page.as_mut()) {
                page.2 = to;
            }
        } else {
            self.xfade = self.body.clone().filter(|_| crate::settings::animations_on()).map(|old| Xfade::new(old, now, seen, self.list_layer.clone()));
        }
        self.window.request_redraw();
    }

    fn settle(&mut self) {
        self.xfade = None;
    }

    fn counts(&self) -> (usize, usize) {
        if self.animation { (self.other.files.len(), self.files.len()) } else { (self.files.len(), self.other.files.len()) }
    }

    pub fn add_pixels(&mut self, rgba: Arc<Vec<u8>>, w: u32, h: u32, name: String, dir: Option<PathBuf>) {
        let info = format!("{w} x {h}  \u{b7}  pinned image");
        let row = FileRow { src: Source::Pixels { rgba, w, h, name: name.clone(), dir }, name, info, status: Status::Waiting, slot: None };
        if self.animation {
            self.other.files.push(row);
            self.settle();
            self.swap_page();
        } else {
            self.files.push(row);
        }
    }


    fn range_of(&self, wd: Wd) -> (i32, i32) {
        match wd {
            Wd::Quality => (1, 100),
            Wd::Effort => EFFORT_RANGE,
            Wd::GifSpeed => (1, 30),
            Wd::Threads => (1, std::thread::available_parallelism().map_or(4, |n| n.get()) as i32),
            _ => (0, 1),
        }
    }

    pub fn slider_value(&self, wd: Wd) -> i32 {
        match wd {
            Wd::Quality => self.params.quality as i32,
            Wd::Effort => self.params.effort as i32,
            Wd::GifSpeed => self.params.gif_speed as i32,
            Wd::Threads => self.threads as i32,
            _ => 0,
        }
    }

    pub fn set_slider(&mut self, wd: Wd, v: i32) {
        self.settle();
        let (lo, hi) = self.range_of(wd);
        let v = v.clamp(lo, hi);
        match wd {
            Wd::Quality => self.params.quality = v as u8,
            Wd::Effort => self.params.effort = v as u8,
            Wd::GifSpeed => self.params.gif_speed = v as u8,
            Wd::Threads => self.threads = v as usize,
            _ => {}
        }
    }

    pub fn slider_from_x(&mut self, wd: Wd, rect: [f32; 4], x: f32) {
        let s = self.scale();
        let (lo, hi) = self.range_of(wd);
        let t = slider_pos(rect, 1.0, x / s);
        self.set_slider(wd, lo + (t * (hi - lo) as f32).round() as i32);
    }

    pub fn estimate_wanted(&mut self) -> Option<(u64, Source, Params)> {
        let first = self.files.first()?;
        if self.animation || self.drag.is_some() || self.running() || self.est.busy || !(self.params.kind.has_quality() || self.params.kind.has_lossless_switch()) {
            return None;
        }
        let (params, _) = self.build().ok()?;
        let id = match &first.src {
            Source::File(p) => p.to_string_lossy().into_owned(),
            Source::Pixels { rgba, .. } => format!("pixels {:p}", Arc::as_ptr(rgba)),
        };
        if self.est.key.as_ref().is_some_and(|(i, p)| *i == id && *p == params) {
            return None;
        }
        let round = self.est.begin((id, params.clone()));
        self.window.request_redraw();
        Some((round, first.src.clone(), params))
    }

    pub fn set_estimate(&mut self, round: u64, res: Result<crate::convert::Estimate, String>) {
        self.est.end(round, res);
        self.window.request_redraw();
    }

    fn estimate_text(&self) -> Option<String> {
        if self.animation {
            return None;
        }
        self.est.key.as_ref()?;
        Some(match &self.est.value {
            None => "Measuring on the first picture\u{2026}".to_string(),
            Some(Err(_)) => "Couldn't measure this picture".to_string(),
            Some(Ok(e)) => {
                let mut s = human_size(e.bytes);
                if let Some(o) = e.original.filter(|o| *o > 0) {
                    s += &format!(" ({}% of the original)", (e.bytes * 100 + o / 2) / o);
                }
                match e.psnr {
                    None => s += " \u{b7} identical pixels",
                    Some(db) => s += &format!(" \u{b7} {db:.1} dB"),
                }
                s
            }
        })
    }

    fn slider_ctl(&self, wd: Wd, label: String) -> Ctl {
        let (lo, hi) = self.range_of(wd);
        let t = (self.slider_value(wd) - lo) as f32 / (hi - lo).max(1) as f32;
        Ctl::Slider(t, label, false)
    }

    pub fn set_kind(&mut self, k: Kind) {
        self.settle();
        let old = self.params.clone();
        self.params = Params::new(k);
        self.params.resize = old.resize;
        self.params.background = old.background;
        self.params.animation = old.animation;
    }

    pub fn layout(&self, t: &Text) -> Layout {
        let mut lay = Layout { cards: Vec::new(), rows: Vec::new(), items: Vec::new(), heads: Vec::new(), list: [0.0; 4] };
        lay.items.push(Placed { id: Wd::Close, rect: [W - 40.0, 6.0, 28.0, 28.0], ctl: Ctl::Chip(String::new()) });
        let px = 12.0;
        let card_h = BOTTOM_Y - 14.0 - TOP;
        lay.cards.push([LEFT_X, TOP, LEFT_W, card_h]);
        lay.heads.push((LEFT_X + 14.0, TOP + 11.0, "Files".to_string()));
        let (add_w, clear_w) = (t.measure("Add\u{2026}", px) + 22.0, 54.0);
        let btn_y = TOP + 8.0;
        lay.items.push(Placed { id: Wd::Clear, rect: [LEFT_X + LEFT_W - 12.0 - clear_w, btn_y, clear_w, 24.0], ctl: Ctl::Chip("Clear".into()) });
        lay.items.push(Placed { id: Wd::AddFiles, rect: [LEFT_X + LEFT_W - 12.0 - clear_w - 8.0 - add_w, btn_y, add_w, 24.0], ctl: Ctl::Chip("Add\u{2026}".into()) });
        let (pics, anims) = self.counts();
        let tab = |n: usize, name: &str| if n > 0 { format!("{name}  {n}") } else { name.to_string() };
        lay.items.push(Placed { id: Wd::Page, rect: [LEFT_X + 12.0, TOP + 40.0, LEFT_W - 24.0, 26.0], ctl: Ctl::Seg(vec![tab(pics, "Pictures"), tab(anims, "Animations")], usize::from(self.animation)) });
        lay.list = [LEFT_X + 6.0, TOP + LIST_TOP, LEFT_W - 12.0, card_h - LIST_TOP - 6.0];
        let running = self.running();
        if !running {
            for i in 0..self.files.len() {
                if let Some(rect) = remove_rect(lay.list, i, self.scroll) {
                    lay.items.push(Placed { id: Wd::Remove(i), rect, ctl: Ctl::Chip("x".into()) });
                }
            }
        }

        let mut y = TOP;
        let p = &self.params;
        let k = p.kind;
        let section = |lay: &mut Layout, y: &mut f32, rows: Vec<OptionRow>| {
            let h = rows.len() as f32 * ROW_H;
            lay.cards.push([RIGHT_X, *y, RIGHT_W, h]);
            for (i, (title, desc, ctls)) in rows.into_iter().enumerate() {
                let ry = *y + i as f32 * ROW_H;
                lay.rows.push(RowText { x: RIGHT_X + 14.0, y: ry, h: ROW_H, title, desc });
                let mut right = RIGHT_X + RIGHT_W - 14.0;
                for (id, ctl, w) in ctls {
                    let (ch, cy) = match &ctl {
                        Ctl::Switch(_) => (20.0, ry + (ROW_H - 20.0) / 2.0),
                        Ctl::Field => (26.0, ry + (ROW_H - 26.0) / 2.0),
                        _ => (24.0, ry + (ROW_H - 24.0) / 2.0),
                    };
                    lay.items.push(Placed { id, rect: [right - w, cy, w, ch], ctl });
                    right -= w + 8.0;
                }
            }
            *y += h + 12.0;
        };
        let chip_w = |s: &str| t.measure(s, px) + 30.0;

        let mut fmt: Vec<OptionRow> = Vec::new();
        fmt.push(("Format", kind_blurb(k, self.animation).to_string(), vec![(Wd::Format, Ctl::Drop(k.label().to_string()), chip_w(k.label()) + 16.0)]));
        if k.has_lossless_switch() {
            let desc = if p.lossless { "Identical pixels, larger file" } else { "Smaller file, a little loss" };
            fmt.push(("Lossless", desc.to_string(), vec![(Wd::Lossless, Ctl::Switch(p.lossless), 34.0)]));
        }
        if k.has_quality() {
            let locked = k.has_lossless_switch() && p.lossless;
            let sweet = if locked {
                "Locked at Max while Lossless is on"
            } else if k == Kind::Jpeg {
                "Recommended: 85"
            } else {
                "Recommended: 80"
            };
            let sweet = match (self.files.is_empty(), self.estimate_text()) {
                (false, Some(t)) => t,
                _ => sweet.to_string(),
            };
            let ctl = if locked {
                Ctl::Slider(1.0, "Max".to_string(), true)
            } else {
                self.slider_ctl(Wd::Quality, p.quality.to_string())
            };
            fmt.push(("Quality", sweet, vec![(Wd::Quality, ctl, 200.0)]));
        }
        if k.has_lossless_switch() {
            let desc = "Higher values compress better but take longer";
            fmt.push(("Effort", desc.to_string(), vec![(Wd::Effort, self.slider_ctl(Wd::Effort, p.effort.to_string()), 200.0)]));
        }
        if k == Kind::Png {
            fmt.push(("Compression", "Higher levels reduce size at the cost of encoding time".to_string(), vec![
                (Wd::Png, seg(&["Fast", "Default", "Best"], match p.png_level {
                    PngLevel::Fast => 0,
                    PngLevel::Default => 1,
                    PngLevel::Best => 2,
                }), 168.0),
            ]));
        }
        if crate::color::embeds(k) && !self.animation {
            let desc = if p.keep_profile { "Keeps wide-gamut colours (Display P3, Adobe RGB)" } else { "Converts colours to sRGB" };
            fmt.push(("Keep colour profile", desc.to_string(), vec![(Wd::Profile, Ctl::Switch(p.keep_profile), 34.0)]));
        }
        if crate::deep::can_hold(k) && !self.animation {
            let desc = if p.deep { "Keeps 10/16-bit depth. HDR is converted to SDR" } else { "Reduces to 8-bit. HDR is converted to SDR" };
            fmt.push(("High bit depth", desc.to_string(), vec![(Wd::Deep, Ctl::Switch(p.deep), 34.0)]));
        }
        if k == Kind::Gif {
            fmt.push(("Palette search", "A slower search improves colour accuracy".to_string(), vec![(Wd::GifSpeed, self.slider_ctl(Wd::GifSpeed, p.gif_speed.to_string()), 200.0)]));
        }
        section(&mut lay, &mut y, fmt);

        let resize_desc = if self.resize_on { "Scales each output image" } else { "Keeps the original size" };
        let mut size_rows: Vec<OptionRow> = vec![("Resize", resize_desc.to_string(), vec![(Wd::Resize, Ctl::Switch(self.resize_on), 34.0)])];
        if self.resize_on {
            size_rows.push(("Mode", String::new(), vec![(Wd::RMode, seg(&["Percent", "Fit", "Exact"], self.resize_mode as usize), 186.0)]));
            if self.resize_mode == 0 {
                size_rows.push(("Scale", "Percentage of the original dimensions".to_string(), vec![(Wd::FPercent, Ctl::Field, 84.0)]));
            } else {
                let desc = if self.resize_mode == 1 { "Fit within these dimensions, preserving aspect ratio" } else { "Output exactly these dimensions" };
                size_rows.push(("Size", desc.to_string(), vec![(Wd::FH, Ctl::Field, 76.0), (Wd::FW, Ctl::Field, 76.0)]));
            }
            size_rows.push(("Filter", String::new(), vec![(Wd::RFilter, Ctl::Drop(filter_label(self.filter).to_string()), chip_w(filter_label(self.filter)) + 16.0)]));
        }
        section(&mut lay, &mut y, size_rows);

        let folder_desc = match &self.out_dir {
            Some(d) => d.display().to_string(),
            _ => "Same folder as each source file".to_string(),
        };
        let mut folder_ctls = vec![(Wd::Folder, seg(&["Source", "Custom"], usize::from(self.out_dir.is_some())), 120.0)];
        if self.out_dir.is_some() {
            folder_ctls.push((Wd::Choose, Ctl::Chip("Choose\u{2026}".into()), 76.0));
        }
        let delete_desc = if self.delete_original { "Moves the source file to the Recycle Bin" } else { "Keeps the source file" };
        let out_rows: Vec<OptionRow> = vec![
            ("Folder", folder_desc, folder_ctls),
            ("File name", "Tokens: {name} {n} {w} {h} {date} {time}".to_string(), vec![(Wd::FTemplate, Ctl::Field, 170.0)]),
            ("If it exists", String::new(), vec![(Wd::Exists, seg(&["Rename", "Overwrite", "Skip"], match self.exists {
                IfExists::Rename => 0,
                IfExists::Overwrite => 1,
                IfExists::Skip => 2,
            }), 204.0)]),
            ("Delete original", delete_desc.to_string(), vec![(Wd::DelOrig, Ctl::Switch(self.delete_original), 34.0)]),
            ("Threads", "Number of files converted concurrently".to_string(), vec![(Wd::Threads, self.slider_ctl(Wd::Threads, self.threads.to_string()), 200.0)]),
        ];
        section(&mut lay, &mut y, out_rows);

        lay.items.push(Placed { id: Wd::Defaults, rect: [16.0, BOTTOM_Y + 14.0, 128.0, 28.0], ctl: Ctl::Button { label: "Restore defaults", primary: false, enabled: true } });
        let go = Placed { id: if running { Wd::Cancel } else { Wd::Convert }, rect: [W - 16.0 - 130.0, BOTTOM_Y + 8.0, 130.0, 36.0], ctl: Ctl::Button { label: if running { "Cancel" } else { "Convert" }, primary: !running, enabled: running || !self.files.is_empty() } };
        lay.items.push(go);
        if !running && self.files.iter().any(|f| converted(&f.status)) {
            lay.items.push(Placed { id: Wd::OpenFolder, rect: [W - 16.0 - 130.0 - 8.0 - 104.0, BOTTOM_Y + 11.0, 104.0, 30.0], ctl: Ctl::Button { label: "Open folder", primary: false, enabled: true } });
        }
        lay
    }

    pub fn hit(&self, t: &Text, x: f32, y: f32) -> Option<(Wd, [f32; 4])> {
        let s = self.scale();
        let (lx, ly) = (x / s, y / s);
        self.layout(t).items.into_iter().rev().find(|p| lx >= p.rect[0] && lx < p.rect[0] + p.rect[2] && ly >= p.rect[1] && ly < p.rect[1] + p.rect[3]).map(|p| (p.id, [p.rect[0] * s, p.rect[1] * s, p.rect[2] * s, p.rect[3] * s]))
    }

    pub fn in_title_bar(&self, y: f32) -> bool {
        y < TITLE_H * self.scale() && self.hover != Some(Wd::Close)
    }

    pub fn click(&mut self, wd: Wd, rect: [f32; 4]) -> Option<Effect> {
        self.focus = None;
        if wd != Wd::Page {
            self.settle();
        }
        match wd {
            Wd::Close => return Some(Effect::Close),
            Wd::AddFiles => return Some(Effect::AddFiles),
            Wd::Clear if !self.running() => {
                self.presses.clear();
                for f in self.files.drain(..) {
                    if let Source::File(p) = &f.src {
                        self.listed.remove(&list_key(p));
                    }
                }
                self.scroll = 0.0;
                self.progress = None;
                self.status = None;
            }
            Wd::Remove(i) if i < self.files.len() && !self.running() => {
                self.presses.clear();
                if let Source::File(p) = &self.files.remove(i).src {
                    self.listed.remove(&list_key(p));
                }
                self.scroll_by(0.0);
                if self.files.is_empty() {
                    self.progress = None;
                }
            }
            Wd::Clear | Wd::Remove(_) => {}
            Wd::Page => {
                if (seg_at(self.cursor.0, rect, 2) == 1) != self.animation {
                    self.swap_page();
                }
            }
            Wd::Format => return Some(Effect::FormatMenu(rect)),
            Wd::RFilter => return Some(Effect::FilterMenu(rect)),
            Wd::Lossless => self.params.lossless = !self.params.lossless,
            Wd::Deep => self.params.deep = !self.params.deep,
            Wd::Profile => self.params.keep_profile = !self.params.keep_profile,
            Wd::Png => self.params.png_level = [PngLevel::Fast, PngLevel::Default, PngLevel::Best][seg_at(self.cursor.0, rect, 3)],
            Wd::Resize => self.resize_on = !self.resize_on,
            Wd::RMode => self.resize_mode = seg_at(self.cursor.0, rect, 3) as u8,
            Wd::Folder => {
                if seg_at(self.cursor.0, rect, 2) == 0 {
                    self.out_dir = None;
                } else if self.out_dir.is_none() {
                    return Some(Effect::ChooseFolder);
                }
            }
            Wd::Choose => return Some(Effect::ChooseFolder),
            Wd::Exists => self.exists = [IfExists::Rename, IfExists::Overwrite, IfExists::Skip][seg_at(self.cursor.0, rect, 3)],
            Wd::DelOrig => self.delete_original = !self.delete_original,
            Wd::FPercent | Wd::FW | Wd::FH | Wd::FTemplate => {
                self.focus = Some(wd);
                if let Some(e) = self.editor_mut() {
                    e.all = true;
                    e.caret = e.text.len();
                }
            }
            Wd::Quality if self.params.kind.has_lossless_switch() && self.params.lossless => {}
            Wd::Quality | Wd::Effort | Wd::GifSpeed | Wd::Threads => {
                self.drag = Some(wd);
                self.slider_from_x(wd, [rect[0] / self.scale(), rect[1] / self.scale(), rect[2] / self.scale(), rect[3] / self.scale()], self.cursor.0);
            }
            Wd::Convert => return Some(Effect::Convert),
            Wd::Cancel => return Some(Effect::Cancel),
            Wd::OpenFolder => return Some(Effect::OpenFolder),
            Wd::Defaults => self.reset_defaults(),
        }
        None
    }

    pub fn prefs(&self) -> crate::convert_prefs::ConvertPrefs {
        let (pics, anims) = if self.animation { (&self.other.params, &self.params) } else { (&self.params, &self.other.params) };
        crate::convert_prefs::ConvertPrefs {
            params: pics.clone(),
            anim: anims.clone(),
            threads: self.threads,
            template: self.template.text.clone(),
            exists: self.exists,
            resize_on: self.resize_on,
            resize_mode: self.resize_mode,
            percent: self.f_percent.text.clone(),
            width: self.f_w.text.clone(),
            height: self.f_h.text.clone(),
            filter: self.filter,
            out_dir: self.out_dir.clone(),
        }
    }

    pub fn apply_prefs(&mut self, p: &crate::convert_prefs::ConvertPrefs) {
        let (pics, anims) = (p.params.clone(), p.anim.clone());
        (self.params, self.other.params) = if self.animation { (anims, pics) } else { (pics, anims) };
        self.threads = p.threads;
        self.template = LineEdit::new(&p.template);
        self.exists = p.exists;
        self.resize_on = p.resize_on;
        self.resize_mode = p.resize_mode;
        self.f_percent = LineEdit::new(&p.percent);
        self.f_w = LineEdit::new(&p.width);
        self.f_h = LineEdit::new(&p.height);
        self.filter = p.filter;
        self.out_dir = p.out_dir.clone();
    }

    pub fn reset_defaults(&mut self) {
        self.params = Params { animation: self.animation, ..Params::new(Kind::Webp) };
        self.other.params = Params { animation: !self.animation, ..Params::new(Kind::Webp) };
        self.out_dir = None;
        self.template = LineEdit::new("{name}");
        self.exists = IfExists::Rename;
        self.delete_original = false;
        self.threads = crate::par::workers();
        self.resize_on = false;
        self.resize_mode = 0;
        self.f_percent = LineEdit::new("50");
        self.f_w = LineEdit::new("1920");
        self.f_h = LineEdit::new("1080");
        self.filter = ResizeFilter::Lanczos;
        self.status = None;
        self.focus = None;
    }

    pub fn editor_mut(&mut self) -> Option<&mut LineEdit> {
        match self.focus? {
            Wd::FPercent => Some(&mut self.f_percent),
            Wd::FW => Some(&mut self.f_w),
            Wd::FH => Some(&mut self.f_h),
            Wd::FTemplate => Some(&mut self.template),
            _ => None,
        }
    }

    pub fn build(&self) -> Result<(Params, Output), String> {
        let mut p = self.params.clone();
        if self.resize_on {
            let num = |e: &LineEdit, what: &str| e.text.trim().parse::<f32>().ok().filter(|v| *v >= 1.0 && *v <= 65535.0).ok_or_else(|| format!("Enter a {what} between 1 and 65535"));
            let mode = match self.resize_mode {
                0 => ResizeMode::Percent(num(&self.f_percent, "percentage")?),
                1 => ResizeMode::Fit(num(&self.f_w, "width")? as u32, num(&self.f_h, "height")? as u32),
                _ => ResizeMode::Exact(num(&self.f_w, "width")? as u32, num(&self.f_h, "height")? as u32),
            };
            p.resize = Some((mode, self.filter));
        } else {
            p.resize = None;
        }
        let template = self.template.text.trim();
        if template.is_empty() {
            return Err("The file name can't be empty".into());
        }
        let out = Output { dir: self.out_dir.clone(), template: template.to_string(), if_exists: self.exists, delete_original: self.delete_original };
        Ok((p, out))
    }

    pub fn sync_progress(&mut self) {
        let Some(pr) = &self.progress else { return };
        let Ok(outcomes) = pr.outcomes.lock() else { return };
        adopt(&mut self.files, &outcomes);
    }

    fn hv(&self, wd: Wd, now: Instant) -> f32 {
        self.hovers.iter().find(|a| a.0 == wd).map_or(0.0, |a| a.1.value(now, HOVER_MS, EASE_OUT))
    }


    pub fn draw(&mut self, text: Option<&Text>, dark: bool) {
        if self.draw_opening() {
            return;
        }
        let Some(t) = text else { return };
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        let s = self.scale();
        let id = Transform::identity();
        let now = Instant::now();
        if self.pal.dark != dark {
            self.pal = Pal::new(dark);
            self.bg = None;
            set_dark_mode(&self.window, dark);
        }
        let pal = self.pal;
        crate::theme::set_ui_dark(pal.dark);
        let pressing = self.presses.step(now, self.drag);
        let mut animating = update_hovers(&mut self.hovers, self.hover, now) | pressing;
        self.sync_progress();
        if self.draw_switch_frame(t, now, w, h) {
            if pressing {
                self.window.request_redraw();
            }
            return;
        }

        let stale = |p: &Option<Pixmap>| p.as_ref().is_none_or(|p| p.width() != size.width || p.height() != size.height);
        if stale(&self.bg) {
            self.bg = render_background(size.width, size.height, self.acrylic, &pal);
        }
        if stale(&self.canvas) {
            self.canvas = Pixmap::new(size.width, size.height);
        }
        if stale(&self.body) {
            self.body = Pixmap::new(size.width, size.height);
        }
        if stale(&self.fixed) {
            self.fixed = Pixmap::new(size.width, size.height);
        }
        let icon_px = (16.0 * s).round() as u32;
        if self.icon.as_ref().is_none_or(|(px, _)| *px != icon_px) {
            self.icon = Some((icon_px, crate::icon::render(icon_px)));
        }
        let lay = self.layout(t);
        {
            let rows: Vec<(&'static str, &str, bool)> = lay.rows.iter().map(|r| (r.title, r.desc.as_str(), !(r.title == "Quality" && !self.files.is_empty()))).collect();
            self.desc_fx.update(&rows, now, self.desc_page != Some(self.animation));
            self.desc_page = Some(self.animation);
            animating |= self.desc_fx.active(now);
        }
        forget_unplaced(&mut self.anims, &lay.items);
        self.presses.retain(|k| lay.items.iter().any(|p| p.id == *k));
        let list_px = [(lay.list[0] * s) as u32, (lay.list[1] * s) as u32, (lay.list[2] * s).ceil() as u32, (lay.list[3] * s).ceil() as u32];
        if self.list_layer.as_ref().is_none_or(|p| p.width() != list_px[2] || p.height() != list_px[3]) {
            self.list_layer = Pixmap::new(list_px[2].max(1), list_px[3].max(1));
        }
        let (Some(bg), Some(mut pix), Some(mut body), Some(mut fixed), Some(mut list)) =
            (self.bg.as_ref(), self.canvas.take(), self.body.take(), self.fixed.take(), self.list_layer.take())
        else {
            return;
        };
        pix.data_mut().copy_from_slice(bg.data());
        body.fill(Color::TRANSPARENT);
        fixed.fill(Color::TRANSPARENT);
        let semibold = self.semibold.as_deref().unwrap_or(t);
        let running = self.running();

        if let Some((_, icon)) = &self.icon {
            pix.draw_pixmap((14.0 * s).round() as i32, ((TITLE_H - 16.0) / 2.0 * s).round() as i32, icon.as_ref(), &PixmapPaint::default(), id, None);
        }
        semibold.draw(&mut pix, "Convert", 38.0 * s, (TITLE_H / 2.0 - 8.5) * s, 12.5 * s, pal.dim);
        {
            let hv = self.hv(Wd::Close, now);
            let pr = self.presses.value(&Wd::Close, now);
            let r = [(W - 40.0) * s, 6.0 * s, 28.0 * s, 28.0 * s];
            let glow = hv + 0.8 * pr.clamp(0.0, 1.0);
            if glow > 0.0
                && let Some(p) = PathBuilder::from_circle(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, r[2] / 2.0 * (1.0 - 0.1 * pr))
            {
                pix.fill_path(&p, &paint([pal.lift[0], pal.lift[1], pal.lift[2]], (pal.lift[3] as f32 * glow).min(255.0) as u8), FillRule::Winding, id, None);
            }
            let hv = hv.max(pr.clamp(0.0, 1.0));
            let (cx, cy, k) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 4.5 * s * (1.0 - 0.15 * pr));
            let mut pb = PathBuilder::new();
            pb.move_to(cx - k, cy - k);
            pb.line_to(cx + k, cy + k);
            pb.move_to(cx + k, cy - k);
            pb.line_to(cx - k, cy + k);
            if let Some(p) = pb.finish() {
                pix.stroke_path(&p, &paint(mix(pal.dim, pal.text, hv), 255), &Stroke { width: 1.4 * s, line_cap: tiny_skia::LineCap::Round, ..Default::default() }, id, None);
            }
        }

        for c in &lay.cards {
            glass_card(&mut body, c[0] * s, c[1] * s, c[2] * s, c[3] * s, 14.0 * s, s, &pal);
        }
        for (x, y, title) in &lay.heads {
            if !title.is_empty() {
                semibold.draw(&mut body, title, x * s, y * s, 12.5 * s, pal.text);
            }
        }
        for r in &lay.rows {
            let ctl_left = lay.items.iter().filter(|p| p.rect[0] >= RIGHT_X && p.rect[1] >= r.y && p.rect[1] < r.y + r.h).map(|p| p.rect[0]).fold(f32::MAX, f32::min);
            let room = ((ctl_left.min(RIGHT_X + RIGHT_W) - r.x - 10.0) * s).max(20.0);
            if r.desc.is_empty() {
                semibold.draw(&mut body, r.title, r.x * s, (r.y + (r.h - 12.0 * 1.3) / 2.0) * s, 12.5 * s, pal.text);
            } else {
                semibold.draw(&mut body, r.title, r.x * s, (r.y + 6.0) * s, 12.5 * s, pal.text);
                let d = crate::settings::fit_text(t, &r.desc, 10.5 * s, room);
                match self.desc_fx.fading(r.title, now) {
                    Some((from, p)) => {
                        let from = crate::settings::fit_text(t, from, 10.5 * s, room);
                        crate::settings::fade_text(t, &mut body, &from, &d, p, r.x * s, (r.y + 22.0) * s, 10.5 * s, room, pal.dim, s);
                    }
                    None => t.draw(&mut body, &d, r.x * s, (r.y + 22.0) * s, 10.5 * s, pal.dim),
                }
            }
        }

        list.fill(Color::TRANSPARENT);
        if self.files.is_empty() {
            let title = if self.animation { "Drop animations here" } else { "Drop pictures here" };
            let (cx, cy) = (list_px[2] as f32 / 2.0, list_px[3] as f32 / 2.0);
            semibold.draw_centered(&mut list, title, cx, cy, 13.0 * s, mix(pal.dim, pal.text, 0.35));
        }
        for (i, f) in self.files.iter().enumerate() {
            let y = i as f32 * FILE_ROW_H - self.scroll;
            if y + FILE_ROW_H < 0.0 || y > lay.list[3] {
                continue;
            }
            let rm = self.hv(Wd::Remove(i), now);
            let rp = self.presses.value(&Wd::Remove(i), now);
            if i % 2 == 0
                && let Some(p) = rrect(0.0, y * s, list_px[2] as f32, (FILE_ROW_H - 4.0) * s, 9.0 * s)
            {
                list.fill_path(&p, &paint([pal.text[0], pal.text[1], pal.text[2]], if pal.dark { 8 } else { 10 }), FillRule::Winding, id, None);
            }
            let name_room = (lay.list[2] - 52.0) * s;
            let name = crate::text::normalize_display(&f.name);
            if !semibold.draw_system(&mut list, &name, 10.0 * s, (y + 5.0) * s, 12.0 * s, name_room, pal.text) {
                let n = crate::settings::fit_text(t, &name, 12.0 * s, name_room);
                semibold.draw(&mut list, &n, 10.0 * s, (y + 5.0) * s, 12.0 * s, pal.text);
            }
            let (sub, col) = match &f.status {
                Status::Waiting => (f.info.clone(), pal.dim),
                Status::Done(p) => (format!("\u{2192} {}", p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())), [52, 190, 140]),
                Status::Kept(p, why) => (format!("\u{2192} {} (original kept: {why})", p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())), pal.dim),
                Status::Skipped => ("Skipped: the output file already exists".to_string(), pal.dim),
                Status::Failed(e) => (e.clone(), pal.danger),
            };
            let sub = crate::text::normalize_display(&sub);
            if !t.draw_system(&mut list, &sub, 10.0 * s, (y + 22.0) * s, 10.5 * s, name_room, col) {
                let sub = crate::settings::fit_text(t, &sub, 10.5 * s, name_room);
                t.draw(&mut list, &sub, 10.0 * s, (y + 22.0) * s, 10.5 * s, col);
            }
            if running {
                continue;
            }
            let (cx, cy, k) = ((lay.list[2] - 18.0) * s, (y + 21.0 - 2.0) * s, 3.6 * s * (1.0 - 0.18 * rp));
            if rp > 0.0
                && let Some(p) = PathBuilder::from_circle(cx, cy, 11.0 * s * (1.0 - 0.1 * rp))
            {
                list.fill_path(&p, &paint([pal.lift[0], pal.lift[1], pal.lift[2]], (pal.lift[3] as f32 * rp.min(1.0)) as u8), FillRule::Winding, id, None);
            }
            let rm = (rm + rp.clamp(0.0, 1.0)).min(1.0);
            let mut pb = PathBuilder::new();
            pb.move_to(cx - k, cy - k);
            pb.line_to(cx + k, cy + k);
            pb.move_to(cx + k, cy - k);
            pb.line_to(cx - k, cy + k);
            if let Some(p) = pb.finish() {
                list.stroke_path(&p, &paint(mix(pal.dim, pal.text, rm), 255), &Stroke { width: 1.3 * s, line_cap: tiny_skia::LineCap::Round, ..Default::default() }, id, None);
            }
        }
        let mut fixed_rect: Option<tiny_skia::IntRect> = None;
        let mut page_from: Option<f32> = None;
        for it in &lay.items {
            let r = [it.rect[0] * s, it.rect[1] * s, it.rect[2] * s, it.rect[3] * s];
            let hv = self.hv(it.id, now);
            let pr = self.presses.value(&it.id, now);
            let lit = |hv: f32| hv.max(pr.clamp(0.0, 1.0));
            match &it.ctl {
                Ctl::Chip(label) => {
                    if matches!(it.id, Wd::Remove(_) | Wd::Close) {
                        continue;
                    }
                    let a = if running && it.id == Wd::Clear { 0.45 } else { 1.0 };
                    let rr = pressed_rect(r, pr);
                    glass_chip(&mut body, rr, lit(hv) * a, false, s, &pal);
                    press_wash(&mut body, rr, rr[3] / 2.0, pr * a, pal.dark);
                    semibold.draw_centered(&mut body, label, r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 11.5 * s, mix(pal.dim, pal.text, a));
                }
                Ctl::Drop(label) => {
                    let rr = pressed_rect(r, pr);
                    glass_chip(&mut body, rr, lit(hv), false, s, &pal);
                    press_wash(&mut body, rr, rr[3] / 2.0, pr, pal.dark);
                    let (px, arrow, gap) = (11.5 * s, 7.0 * s, 7.0 * s);
                    let lw = semibold.measure(label, px);
                    let left = r[0] + (r[2] - lw - gap - arrow) / 2.0;
                    let cy = r[1] + r[3] / 2.0;
                    semibold.draw_centered(&mut body, label, left + lw / 2.0, cy, px, pal.text);
                    let ax = left + lw + gap;
                    let mut pb = PathBuilder::new();
                    pb.move_to(ax, cy - 1.8 * s);
                    pb.line_to(ax + arrow, cy - 1.8 * s);
                    pb.line_to(ax + arrow / 2.0, cy + 2.2 * s);
                    pb.close();
                    if let Some(p) = pb.finish() {
                        body.fill_path(&p, &paint(pal.text, 255), FillRule::Winding, id, None);
                    }
                }
                Ctl::Button { label, primary, enabled } => {
                    let a = if *enabled { 1.0 } else { 0.45 };
                    let rr = pressed_rect(r, pr);
                    if *primary {
                        if let Some(p) = rrect(rr[0], rr[1], rr[2], rr[3], rr[3] / 2.0) {
                            body.fill_path(&p, &accent_paint_a(rr, (255.0 * a * (0.9 + 0.1 * hv)) as u8), FillRule::Winding, id, None);
                        }
                        press_wash(&mut body, rr, rr[3] / 2.0, pr * a, false);
                        semibold.draw_centered(&mut body, label, r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 13.0 * s, [255, 255, 255]);
                    } else {
                        glass_chip(&mut body, rr, lit(hv) * a, false, s, &pal);
                        press_wash(&mut body, rr, rr[3] / 2.0, pr * a, pal.dark);
                        semibold.draw_centered(&mut body, label, r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 12.5 * s, mix(pal.dim, pal.text, a));
                    }
                }
                Ctl::Switch(on) => {
                    let (v, moving) = anim(&mut self.anims, it.id, if *on { 1.0 } else { 0.0 }, KNOB_MS, EASE_SPRING, now);
                    animating |= moving;
                    if let Some(p) = rrect(r[0], r[1], r[2], r[3], r[3] / 2.0) {
                        body.fill_path(&p, &paint(pal.switch_off, 235), FillRule::Winding, id, None);
                        if v > 0.0 {
                            body.fill_path(&p, &accent_paint_a(r, (v.clamp(0.0, 1.0) * 255.0) as u8), FillRule::Winding, id, None);
                        }
                        if hv > 0.0 {
                            body.stroke_path(&p, &accent_paint_a(r, (150.0 * hv) as u8), &Stroke { width: 1.5 * s, ..Default::default() }, id, None);
                        }
                    }
                    let knob = r[3] / 2.0 - 2.5 * s;
                    let (x_off, x_on) = (r[0] + r[3] / 2.0, r[0] + r[2] - r[3] / 2.0);
                    let (rx, ry) = (knob * (1.0 + 0.34 * pr), knob * (1.0 - 0.05 * pr));
                    let kx = x_off + (x_on - x_off) * v + (rx - knob) * (1.0 - 2.0 * v.clamp(0.0, 1.0));
                    let ky = r[1] + r[3] / 2.0;
                    if let Some(p) = Rect::from_xywh(kx - rx, ky + 0.8 * s - ry, 2.0 * rx, 2.0 * ry).and_then(PathBuilder::from_oval) {
                        body.fill_path(&p, &paint([0, 6, 20], 55), FillRule::Winding, id, None);
                    }
                    if let Some(p) = Rect::from_xywh(kx - rx, ky - ry, 2.0 * rx, 2.0 * ry).and_then(PathBuilder::from_oval) {
                        body.fill_path(&p, &paint([255, 255, 255], 255), FillRule::Winding, id, None);
                    }
                }
                Ctl::Seg(labels, sel) => {
                    let starts = self.anims.iter().any(|a| a.0 == it.id && (a.2 - *sel as f32).abs() > f32::EPSILON);
                    let (v, moving) = anim(&mut self.anims, it.id, *sel as f32, PILL_MS, EASE_PILL, now);
                    animating |= moving;
                    if it.id == Wd::Page && starts {
                        page_from = Some(v);
                    }
                    let target = if it.id == Wd::Page {
                        fixed_rect = tiny_skia::IntRect::from_xywh(r[0] as i32 - 3, r[1] as i32 - 3, r[2].ceil() as u32 + 6, r[3].ceil() as u32 + 6);
                        &mut fixed
                    } else {
                        &mut body
                    };
                    draw_seg(target, r, labels, v, hv, pr, self.cursor.0, &pal, semibold, s);
                }
                Ctl::Slider(pos, label, locked) => {
                    let (pos, moving) = anim(&mut self.anims, it.id, *pos, SLIDE_MS, EASE_PAGE, now);
                    animating |= moving;
                    let pos = pos.clamp(0.0, 1.0);
                    let label_w = SLIDER_LABEL_W * s;
                    t.draw(&mut body, label, r[0] + (label_w - t.measure(label, 11.5 * s)).max(0.0), r[1] + (r[3] - t.line_height(11.5 * s)) / 2.0, 11.5 * s, pal.text);
                    let (t0, t1) = slider_track(r, s);
                    let cy = r[1] + r[3] / 2.0;
                    let track = Stroke { width: 3.0 * s, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
                    let mut pb = PathBuilder::new();
                    pb.move_to(t0, cy);
                    pb.line_to(t1, cy);
                    if let Some(p) = pb.finish() {
                        body.stroke_path(&p, &paint(if pal.dark { [255, 255, 255] } else { [20, 32, 64] }, if pal.dark { 55 } else { 45 }), &track, id, None);
                    }
                    let kx = t0 + (t1 - t0) * pos;
                    let mut pb = PathBuilder::new();
                    pb.move_to(t0, cy);
                    pb.line_to(kx.max(t0 + 0.1), cy);
                    if let Some(p) = pb.finish() {
                        body.stroke_path(&p, &accent_paint([t0, cy, (kx - t0).max(1.0), 1.0]), &track, id, None);
                    }
                    if !*locked {
                        let kr = (5.5 + 1.0 * hv + 1.5 * pr) * s;
                        if pr > 0.0
                            && let Some(p) = PathBuilder::from_circle(kx, cy, kr + 4.0 * s * pr)
                        {
                            body.fill_path(&p, &accent_paint_a([kx - kr, cy - kr, 2.0 * kr, 2.0 * kr], (70.0 * pr.min(1.0)) as u8), FillRule::Winding, id, None);
                        }
                        if let Some(p) = PathBuilder::from_circle(kx, cy + 0.8 * s, kr) {
                            body.fill_path(&p, &paint([0, 6, 20], 60), FillRule::Winding, id, None);
                        }
                        if let Some(p) = PathBuilder::from_circle(kx, cy, kr) {
                            body.fill_path(&p, &paint([255, 255, 255], 255), FillRule::Winding, id, None);
                        }
                    }
                }
                Ctl::Field => {
                    let editing = self.focus == Some(it.id);
                    let ed = match it.id {
                        Wd::FPercent => &self.f_percent,
                        Wd::FW => &self.f_w,
                        Wd::FH => &self.f_h,
                        _ => &self.template,
                    };
                    if let Some(p) = rrect(r[0], r[1], r[2], r[3], 8.0 * s) {
                        body.fill_path(&p, &paint4(if editing { pal.field_edit } else { pal.field }), FillRule::Winding, id, None);
                        let (border, width) = if editing {
                            (accent_paint(r), 1.5 * s)
                        } else {
                            let b = if pal.dark { paint([255, 255, 255], (36.0 + 34.0 * hv) as u8) } else { paint([20, 32, 64], (60.0 + 50.0 * hv) as u8) };
                            (b, 1.0_f32.max(s * 0.75))
                        };
                        body.stroke_path(&p, &border, &Stroke { width, ..Default::default() }, id, None);
                    }
                    let px = 12.0 * s;
                    let ty = r[1] + r[3] / 2.0 - t.line_height(px) / 2.0;
                    let room = r[2] - 18.0 * s;
                    let fitted = crate::settings::fit_text_left(t, &ed.text, px, room);
                    if editing && ed.all && !ed.text.is_empty() {
                        let sel = [r[0] + 8.0 * s, r[1] + 4.0 * s, t.measure(&fitted, px).min(room), r[3] - 8.0 * s];
                        if let Some(p) = rrect(sel[0], sel[1], sel[2] + 2.0 * s, sel[3], 4.0 * s) {
                            body.fill_path(&p, &paint([59, 120, 245], 70), FillRule::Winding, id, None);
                        }
                    }
                    t.draw(&mut body, &fitted, r[0] + 9.0 * s, ty, px, pal.text);
                    if it.id == Wd::FPercent {
                        let tw = t.measure(&fitted, px);
                        t.draw(&mut body, "%", r[0] + 9.0 * s + tw + 3.0 * s, ty, px, pal.dim);
                    }
                    if editing && !ed.all {
                        let before = &ed.text[..ed.caret.min(ed.text.len())];
                        let cx = (r[0] + 9.0 * s + t.measure(&crate::settings::fit_text_left(t, before, px, room), px) + 1.0 * s).round();
                        if let Some(c) = Rect::from_xywh(cx, ty + 1.0 * s, 1.5 * s, t.line_height(px) - 2.0 * s) {
                            body.fill_rect(c, &paint(pal.text, 255), id, None);
                        }
                    }
                }
            }
        }

        let (bar_x, bar_w) = (162.0 * s, (W - 162.0 - 16.0 - 130.0 - 8.0 - 112.0 - 12.0) * s);
        let mut status_line: Option<(String, [u8; 3])> = self.status.as_ref().map(|(m, err)| (m.clone(), if *err { pal.danger } else { pal.dim }));
        if self.progress.is_none() {
            self.bar = None;
        }
        if let Some(pr) = &self.progress {
            let fin = pr.finished.load(Ordering::Relaxed);
            let frac = if pr.total == 0 { 0.0 } else { fin as f32 / pr.total as f32 };
            if let Some(p) = rrect(bar_x, (BOTTOM_Y + 30.0) * s, bar_w, 5.0 * s, 2.5 * s) {
                body.fill_path(&p, &paint4(pal.field), FillRule::Winding, id, None);
            }
            let (shown, moving) = bar_fill(&mut self.bar, Arc::as_ptr(pr) as usize, frac, now, crate::settings::animations_on());
            animating |= moving;
            if shown > 0.0 {
                let fw = (bar_w * shown).max(5.0 * s);
                if let Some(p) = rrect(bar_x, (BOTTOM_Y + 30.0) * s, fw, 5.0 * s, 2.5 * s) {
                    body.fill_path(&p, &accent_paint([bar_x, 0.0, fw, 1.0]), FillRule::Winding, id, None);
                }
            }
            if status_line.is_none() {
                let counts = tally(&self.files);
                let msg = batch_line(pr.total, fin, !running, pr.cancel.load(Ordering::Relaxed), counts);
                status_line = Some((msg, if counts.2 > 0 && !running { pal.danger } else { pal.dim }));
            }
        }
        if let Some((msg, col)) = status_line {
            let fitted = crate::settings::fit_text(t, &msg, 11.5 * s, bar_w);
            t.draw(&mut body, &fitted, bar_x, (BOTTOM_Y + 10.0) * s, 11.5 * s, col);
        }

        let bare = self.xfade.as_ref().filter(|xf| xf.cache.is_none()).map(|_| body.clone());
        body.draw_pixmap(list_px[0] as i32, list_px[1] as i32, list.as_ref(), &PixmapPaint::default(), id, None);

        let mut began = false;
        if let Some(mut xf) = self.xfade.take() {
            let ms = now.saturating_duration_since(xf.t0).as_secs_f32() * 1000.0;
            let over = ms >= crate::settings::PAGE_RISE_MS.max(crate::settings::PAGE_FADE_MS) || xf.old.width() != pix.width() || xf.old.height() != pix.height();
            if !over && xf.cache.is_none() {
                animating = true;
                let (fade_in, rise) = xf.look(now);
                self.page_alpha = through(fade_in).1;
                let to = usize::from(self.animation);
                let contents = match (bare.as_ref(), xf.old_list.take()) {
                    (Some(bare), Some(old_list)) if old_list.width() == list.width() && old_list.height() == list.height() => {
                        copy_area(&mut xf.old, bare, list_px);
                        Some(Lists { at: (list_px[0] as i32, list_px[1] as i32), pics: by_page(old_list, list.clone(), to) })
                    }
                    _ => None,
                };
                let shown = if contents.is_some() { bare.as_ref().unwrap_or(&body) } else { &body };
                let (differs, any_same) = strip_unchanged(&mut xf.old, shown, &page_bands(&lay), s);
                let (Some(mut same), Some(mut moving)) = (Pixmap::new(body.width(), body.height()), Pixmap::new(body.width(), body.height())) else { return };
                split_by(shown, &differs, &mut same, &mut moving);
                if any_same {
                    pix.draw_pixmap(0, 0, same.as_ref(), &PixmapPaint::default(), id, None);
                }
                let page = lay.items.iter().find(|it| it.id == Wd::Page).and_then(|it| match &it.ctl {
                    Ctl::Seg(labels, sel) => Some((it.rect, labels.clone(), *sel)),
                    _ => None,
                });
                let cache = Switch { base: pix.clone(), pages: by_page(crop_opaque(&xf.old), crop_opaque(&moving), to), lists: contents, to, from: None, page, dark: pal.dark };
                cache.lay_over(&mut pix, xf.seen, fade_in, rise, s);
                xf.cache = Some(cache);
                began = true;
                self.xfade = Some(xf);
            }
        }
        if !began {
            self.page_alpha = 1.0;
            pix.draw_pixmap(0, 0, body.as_ref(), &PixmapPaint::default(), id, None);
        }
        if let Some(rc) = fixed_rect
            && let Some(part) = fixed.as_ref().clone_rect(rc)
        {
            pix.draw_pixmap(rc.x(), rc.y(), part.as_ref(), &PixmapPaint::default(), id, None);
        }

        self.present(&pix, w, h);
        if began {
            let start = Instant::now();
            if let Some(xf) = self.xfade.as_mut() {
                xf.t0 = start;
                xf.fade = Xfade::fade_from(start);
            }
            if let Some(from) = page_from {
                restart(&mut self.anims, Wd::Page, from, start, PILL_MS, EASE_PILL);
            }
        }
        self.canvas = Some(pix);
        self.body = Some(body);
        self.fixed = Some(fixed);
        self.list_layer = Some(list);
        if animating {
            self.window.request_redraw();
        }
    }

    pub(crate) fn begin_opening(&mut self) {
        self.finish_opening();
        self.opening = crate::settings::Opening::begin(self.window.clone(), |t| self.present_opening(t));
    }

    pub(crate) fn finish_opening(&mut self) {
        self.opening = None;
    }

    fn draw_opening(&mut self) -> bool {
        let Some(opening) = self.opening.take() else { return false };
        let size = self.window.inner_size();
        if self.canvas.as_ref().is_none_or(|p| p.width() != size.width || p.height() != size.height) {
            return false;
        }
        if opening.step(|t| self.present_opening(t)) {
            self.opening = Some(opening);
        }
        true
    }

    pub(crate) fn present_opening(&mut self, t: f32) {
        let Some(pix) = self.canvas.take() else { return };
        if let (Some(w), Some(h)) = (NonZeroU32::new(pix.width()), NonZeroU32::new(pix.height())) {
            let frame = crate::settings::opening_frame(&pix, t, self.scale());
            self.present(frame.as_ref().unwrap_or(&pix), w, h);
        }
        self.canvas = Some(pix);
    }

    pub(crate) fn present_closing(&mut self, t: f32) {
        let Some(pix) = self.canvas.take() else { return };
        if let (Some(w), Some(h), Some(frame)) = (NonZeroU32::new(pix.width()), NonZeroU32::new(pix.height()), crate::settings::closing_frame(&pix, self.bg.as_ref(), t, self.scale())) {
            self.present(&frame, w, h);
        }
        self.canvas = Some(pix);
    }

    fn present(&mut self, pix: &Pixmap, w: NonZeroU32, h: NonZeroU32) {
        if self.surface.resize(w, h).is_ok()
            && let Ok(mut buf) = self.surface.buffer_mut()
        {
            for (dst, src) in buf.iter_mut().zip(pix.data().as_chunks::<4>().0) {
                *dst = (src[3] as u32) << 24 | (src[0] as u32) << 16 | (src[1] as u32) << 8 | src[2] as u32;
            }
            let _ = buf.present();
        }
    }

    fn draw_switch_frame(&mut self, t: &Text, now: Instant, w: NonZeroU32, h: NonZeroU32) -> bool {
        let Some(mut pix) = self.canvas.take() else { return false };
        let s = self.scale();
        let (fade_in, mut animating) = {
            let Some((xf, sw)) = self.xfade.as_ref().and_then(|xf| xf.cache.as_ref().map(|c| (xf, c))) else {
                self.canvas = Some(pix);
                return false;
            };
            if xf.over(now)
                || sw.dark != self.pal.dark
                || sw.base.width() != pix.width()
                || sw.base.height() != pix.height()
            {
                self.canvas = Some(pix);
                return false;
            }
            let (fade_in, rise) = xf.look(now);
            sw.lay_over(&mut pix, xf.seen, fade_in, rise, s);
            let mut animating = true;
            if let Some((rect, labels, sel)) = &sw.page {
                let (v, moving) = anim(&mut self.anims, Wd::Page, *sel as f32, PILL_MS, EASE_PILL, now);
                animating |= moving;
                let r = [rect[0] * s, rect[1] * s, rect[2] * s, rect[3] * s];
                let semibold = self.semibold.as_deref().unwrap_or(t);
                draw_seg(&mut pix, r, labels, v, self.hv(Wd::Page, now), self.presses.value(&Wd::Page, now), self.cursor.0, &self.pal, semibold, s);
            }
            (fade_in, animating)
        };
        self.page_alpha = through(fade_in).1;
        self.present(&pix, w, h);
        self.canvas = Some(pix);
        animating |= fade_in < 1.0;
        if animating {
            self.window.request_redraw();
        }
        true
    }

    pub fn frame_wake(&mut self) -> Option<Instant> {
        crate::settings::frame_wake(&mut self.frames, &self.window)
    }

    pub fn frame_revive(&mut self) {
        if self.frames.take_due() {
            self.window.request_redraw();
        }
    }

    pub fn scroll_by(&mut self, dy: f32) {
        let list_h = BOTTOM_Y - 14.0 - TOP - LIST_TOP - 6.0;
        let max = (self.files.len() as f32 * FILE_ROW_H - list_h).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
    }

    pub fn over_list(&self, x: f32, y: f32) -> bool {
        let s = self.scale();
        let (lx, ly) = (x / s, y / s);
        (LEFT_X..LEFT_X + LEFT_W).contains(&lx) && (TOP + LIST_TOP..BOTTOM_Y - 14.0).contains(&ly)
    }
}

struct Xfade {
    old: Pixmap,
    t0: Instant,
    fade: Tween,
    seen: f32,
    cache: Option<Switch>,
    old_list: Option<Pixmap>,
}

impl Xfade {
    fn new(old: Pixmap, now: Instant, seen: f32, old_list: Option<Pixmap>) -> Self {
        Self { old, t0: now, fade: Self::fade_from(now), seen, cache: None, old_list }
    }

    fn fade_from(now: Instant) -> Tween {
        let mut fade = Tween::at(0.0);
        fade.retarget(1.0, now, crate::settings::PAGE_FADE_MS, EASE_OUT);
        fade
    }

    fn look(&self, now: Instant) -> (f32, f32) {
        let ms = now.saturating_duration_since(self.t0).as_secs_f32() * 1000.0;
        let rise = crate::settings::PAGE_RISE_PX * (1.0 - bezier(EASE_PAGE, ms / crate::settings::PAGE_RISE_MS));
        (self.fade.value(now, crate::settings::PAGE_FADE_MS, EASE_OUT), rise)
    }

    fn restart(&mut self, now: Instant, s: f32, to: usize) {
        let (fade_in, rise) = self.look(now);
        let Some(sw) = self.cache.as_mut() else { return };
        if sw.restart(self.seen, fade_in, rise, s, to) {
            self.t0 = now;
            self.fade = Self::fade_from(now);
            self.seen = 1.0;
        }
    }

    fn over(&self, now: Instant) -> bool {
        let ms = now.saturating_duration_since(self.t0).as_secs_f32() * 1000.0;
        self.fade.done(now, crate::settings::PAGE_FADE_MS) && ms >= crate::settings::PAGE_RISE_MS.max(crate::settings::PAGE_FADE_MS)
    }
}

struct Switch {
    base: Pixmap,
    pages: [((i32, i32), Pixmap); 2],
    lists: Option<Lists>,
    to: usize,
    from: Option<Seen>,
    page: Option<([f32; 4], Vec<String>, usize)>,
    dark: bool,
}

struct Lists {
    at: (i32, i32),
    pics: [Pixmap; 2],
}

struct Seen {
    pic: ((i32, i32), Pixmap),
    list: Option<Pixmap>,
}

fn by_page<T>(gone: T, came: T, to: usize) -> [T; 2] {
    if to == 0 { [came, gone] } else { [gone, came] }
}

impl Switch {
    fn lay_over(&self, pix: &mut Pixmap, seen: f32, fade_in: f32, rise: f32, s: f32) {
        pix.data_mut().copy_from_slice(self.base.data());
        let (out, inn) = through(fade_in);
        let low = (rise * s).round() as i32;
        let ((x, y), old) = self.leaving();
        lay_over(pix, old, x, y, seen * out);
        let ((x, y), new) = &self.pages[self.to];
        lay_over(pix, new, *x, *y + low, inn);
        if let Some(lists) = &self.lists {
            let (old, new) = (self.leaving_list(lists), &lists.pics[self.to]);
            let (x, y) = lists.at;
            let clip = [x, y, x + new.width() as i32, y + new.height() as i32];
            lay_over_in(pix, old, x, y, seen * out, clip);
            lay_over_in(pix, new, x, y + low, inn, clip);
        }
    }

    fn leaving(&self) -> ((i32, i32), &Pixmap) {
        let (at, pic) = self.from.as_ref().map_or_else(|| &self.pages[1 - self.to], |f| &f.pic);
        (*at, pic)
    }

    fn leaving_list<'a>(&'a self, lists: &'a Lists) -> &'a Pixmap {
        self.from.as_ref().and_then(|f| f.list.as_ref()).unwrap_or(&lists.pics[1 - self.to])
    }

    fn restart(&mut self, seen: f32, fade_in: f32, rise: f32, s: f32, to: usize) -> bool {
        let (out, inn) = through(fade_in);
        let low = (rise * s).round() as i32;
        let ((lx, ly), old) = self.leaving();
        let ((ax, ay), new) = &self.pages[self.to];
        let ay = ay + low;
        let (ux, uy) = (lx.min(*ax), ly.min(ay));
        let (uw, uh) = ((lx + old.width() as i32).max(ax + new.width() as i32) - ux, (ly + old.height() as i32).max(ay + new.height() as i32) - uy);
        let Some(mut shown) = Pixmap::new(uw.max(1) as u32, uh.max(1) as u32) else { return false };
        lay_over(&mut shown, old, lx - ux, ly - uy, seen * out);
        lay_over(&mut shown, new, ax - ux, ay - uy, inn);
        let ((cx, cy), pic) = crop_opaque(&shown);
        let list = self.lists.as_ref().and_then(|lists| {
            let (old, new) = (self.leaving_list(lists), &lists.pics[self.to]);
            let mut shown = Pixmap::new(new.width(), new.height())?;
            let clip = [0, 0, new.width() as i32, new.height() as i32];
            lay_over_in(&mut shown, old, 0, 0, seen * out, clip);
            lay_over_in(&mut shown, new, 0, low, inn, clip);
            Some(shown)
        });
        self.from = Some(Seen { pic: ((ux + cx, uy + cy), pic), list });
        self.to = to;
        true
    }
}

const OUT_BY: f32 = 0.45;
const IN_FROM: f32 = 0.35;

fn through(fade_in: f32) -> (f32, f32) {
    let f = fade_in.clamp(0.0, 1.0);
    (1.0 - (f / OUT_BY).min(1.0), ((f - IN_FROM) / (1.0 - IN_FROM)).max(0.0))
}

fn lay_over(dst: &mut Pixmap, src: &Pixmap, x: i32, y: i32, alpha: f32) {
    lay_over_in(dst, src, x, y, alpha, [0, 0, dst.width() as i32, dst.height() as i32]);
}

fn lay_over_in(dst: &mut Pixmap, src: &Pixmap, x: i32, y: i32, alpha: f32, clip: [i32; 4]) {
    let scale = |px: u32, a: u32| ((((px & 0x00FF_00FF) * a) >> 8) & 0x00FF_00FF) | ((((px >> 8) & 0x00FF_00FF) * a) & 0xFF00_FF00);
    let a = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let (x0, x1, y0, y1) = (x.max(0).max(clip[0]), (x + sw).min(dw).min(clip[2]), y.max(0).max(clip[1]), (y + sh).min(dh).min(clip[3]));
    if a == 0 || x0 >= x1 || y0 >= y1 {
        return;
    }
    let n = (x1 - x0) as usize * 4;
    let dst_data = dst.data_mut();
    for dy in y0..y1 {
        let from = (((dy - y) * sw + (x0 - x)) * 4) as usize;
        let to = ((dy * dw + x0) * 4) as usize;
        for (sp, dp) in src.data()[from..from + n].as_chunks::<4>().0.iter().zip(dst_data[to..to + n].as_chunks_mut::<4>().0.iter_mut()) {
            let sp = u32::from_le_bytes([sp[0], sp[1], sp[2], sp[3]]);
            if sp == 0 {
                continue;
            }
            let covered = ((sp >> 24) * a) >> 8;
            let under = u32::from_le_bytes([dp[0], dp[1], dp[2], dp[3]]);
            dp.copy_from_slice(&(scale(sp, a) + scale(under, 256 - covered)).to_le_bytes());
        }
    }
}

fn copy_area(dst: &mut Pixmap, src: &Pixmap, area: [u32; 4]) {
    if dst.width() != src.width() || dst.height() != src.height() {
        return;
    }
    let w = dst.width() as usize;
    let (x0, x1) = ((area[0] as usize).min(w), ((area[0] + area[2]) as usize).min(w));
    let (y0, y1) = ((area[1] as usize).min(dst.height() as usize), ((area[1] + area[3]) as usize).min(dst.height() as usize));
    for y in y0..y1 {
        let r = (y * w + x0) * 4..(y * w + x1) * 4;
        dst.data_mut()[r.clone()].copy_from_slice(&src.data()[r]);
    }
}

fn crop_opaque(pix: &Pixmap) -> ((i32, i32), Pixmap) {
    let w = pix.width() as usize;
    let (mut x0, mut x1, mut y0, mut y1) = (w, 0, usize::MAX, 0);
    for (y, row) in pix.data().chunks_exact(w * 4).enumerate() {
        let first = row.as_chunks::<4>().0.iter().position(|p| p[3] != 0);
        if let Some(first) = first {
            let last = row.as_chunks::<4>().0.iter().rposition(|p| p[3] != 0).unwrap_or(first);
            x0 = x0.min(first);
            x1 = x1.max(last + 1);
            y0 = y0.min(y);
            y1 = y + 1;
        }
    }
    if x0 >= x1 {
        return ((0, 0), Pixmap::new(1, 1).unwrap());
    }
    let rect = tiny_skia::IntRect::from_xywh(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32);
    match rect.and_then(|r| pix.clone_rect(r)) {
        Some(part) => ((x0 as i32, y0 as i32), part),
        None => ((0, 0), Pixmap::new(1, 1).unwrap()),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_seg(body: &mut Pixmap, r: [f32; 4], labels: &[String], v: f32, hv: f32, press: f32, cursor_x: f32, pal: &Pal, semibold: &Text, s: f32) {
    let id = Transform::identity();
    if let Some(p) = rrect(r[0], r[1], r[2], r[3], r[3] / 2.0) {
        body.fill_path(&p, &paint4(pal.field), FillRule::Winding, id, None);
        let border = if pal.dark { paint([255, 255, 255], 30) } else { paint([20, 32, 64], 34) };
        body.stroke_path(&p, &border, &Stroke { width: 1.0_f32.max(s * 0.75), ..Default::default() }, id, None);
    }
    let seg_w = r[2] / labels.len() as f32;
    let hl = pressed_rect([r[0] + v * seg_w + 2.0 * s, r[1] + 2.0 * s, seg_w - 4.0 * s, r[3] - 4.0 * s], press);
    if let Some(p) = rrect(hl[0], hl[1], hl[2], hl[3], hl[3] / 2.0) {
        body.fill_path(&p, &accent_paint(hl), FillRule::Winding, id, None);
    }
    for (i, l) in labels.iter().enumerate() {
        let seg = [r[0] + i as f32 * seg_w + 2.0 * s, r[1] + 2.0 * s, seg_w - 4.0 * s, r[3] - 4.0 * s];
        let near = (1.0 - (v - i as f32).abs()).clamp(0.0, 1.0);
        let hot = hv > 0.0 && ((cursor_x - r[0]) / seg_w).floor().clamp(0.0, (labels.len() - 1) as f32) as usize == i;
        if hot
            && near < 1.0
            && let Some(p) = rrect(seg[0], seg[1], seg[2], seg[3], seg[3] / 2.0)
        {
            body.fill_path(&p, &paint([pal.lift[0], pal.lift[1], pal.lift[2]], (pal.lift[3] as f32 * hv * (1.0 - near)) as u8), FillRule::Winding, id, None);
        }
        if hot && press > 0.0 && near < 0.5 {
            press_wash(body, seg, seg[3] / 2.0, press, pal.dark);
        }
        let idle = mix(pal.dim, pal.text, if hot { hv * 0.6 } else { 0.0 });
        let c = mix(idle, [255, 255, 255], near);
        semibold.draw_centered(body, l, seg[0] + seg[2] / 2.0, seg[1] + seg[3] / 2.0, 11.0 * s, c);
    }
}

fn page_bands(lay: &Layout) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    for c in &lay.cards {
        if c[0] < RIGHT_X - 1.0 {
            out.push([c[0] - 2.0, c[1] - 2.0, c[2] + 4.0, lay.list[1] - c[1] + 2.0]);
            out.push([c[0] - 2.0, lay.list[1], c[2] + 4.0, c[1] + c[3] + 2.0 - lay.list[1]]);
            continue;
        }
        let mut rows: Vec<&RowText> = lay.rows.iter().filter(|r| r.x >= c[0] && r.x < c[0] + c[2] && r.y >= c[1] && r.y < c[1] + c[3]).collect();
        rows.sort_by(|a, b| a.y.total_cmp(&b.y));
        for (i, r) in rows.iter().enumerate() {
            let y0 = if i == 0 { c[1] - 2.0 } else { r.y };
            let y1 = if i + 1 == rows.len() { c[1] + c[3] + 2.0 } else { rows[i + 1].y };
            out.push([c[0] - 2.0, y0, c[2] + 4.0, y1 - y0]);
        }
    }
    let below = lay.cards.iter().map(|c| c[1] + c[3]).fold(TOP, f32::max) + 2.0;
    out.push([0.0, below, W, H - below]);
    out
}

fn strip_unchanged(old: &mut Pixmap, new: &Pixmap, bands: &[[f32; 4]], s: f32) -> (Vec<u8>, bool) {
    let (w, h) = (old.width() as usize, old.height() as usize);
    let mut differs = vec![1u8; w * h];
    let mut any_same = false;
    for b in bands {
        let (x0, x1) = (((b[0] * s).floor().max(0.0) as usize).min(w), (((b[0] + b[2]) * s).ceil().max(0.0) as usize).min(w));
        let (y0, y1) = (((b[1] * s).floor().max(0.0) as usize).min(h), (((b[1] + b[3]) * s).ceil().max(0.0) as usize).min(h));
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let same = (y0..y1).all(|y| old.data()[(y * w + x0) * 4..(y * w + x1) * 4] == new.data()[(y * w + x0) * 4..(y * w + x1) * 4]);
        if same {
            any_same = true;
            for y in y0..y1 {
                differs[y * w + x0..y * w + x1].fill(0);
                old.data_mut()[(y * w + x0) * 4..(y * w + x1) * 4].fill(0);
            }
        }
    }
    (differs, any_same)
}

fn split_by(body: &Pixmap, differs: &[u8], same: &mut Pixmap, moving: &mut Pixmap) {
    let dst = same.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(moving.data_mut().as_chunks_mut::<4>().0.iter_mut());
    for ((px, d), (keep, mv)) in body.data().as_chunks::<4>().0.iter().zip(differs).zip(dst) {
        if *d == 0 {
            keep.copy_from_slice(px);
            mv.fill(0);
        } else {
            keep.fill(0);
            mv.copy_from_slice(px);
        }
    }
}

fn anim(anims: &mut Vec<(Wd, Tween, f32)>, id: Wd, target: f32, ms: f32, ease: crate::settings::Ease, now: Instant) -> (f32, bool) {
    let i = match anims.iter().position(|a| a.0 == id) {
        Some(i) => i,
        None => {
            anims.push((id, Tween::at(target), target));
            anims.len() - 1
        }
    };
    let e = &mut anims[i];
    if (e.2 - target).abs() > f32::EPSILON {
        e.1.retarget(target, now, ms, ease);
        e.2 = target;
    }
    (e.1.value(now, ms, ease), !e.1.done(now, ms))
}

fn restart(anims: &mut [(Wd, Tween, f32)], id: Wd, from: f32, now: Instant, ms: f32, ease: crate::settings::Ease) {
    if let Some(e) = anims.iter_mut().find(|a| a.0 == id) {
        e.1 = Tween::at(from);
        e.1.retarget(e.2, now, ms, ease);
    }
}

fn forget_unplaced(anims: &mut Vec<(Wd, Tween, f32)>, items: &[Placed]) {
    anims.retain(|a| items.iter().any(|p| p.id == a.0));
}

fn update_hovers(hovers: &mut Vec<(Wd, Tween, f32)>, hover: Option<Wd>, now: Instant) -> bool {
    if let Some(h) = hover.filter(|h| !hovers.iter().any(|a| a.0 == *h)) {
        hovers.push((h, Tween::at(0.0), 0.0));
    }
    let mut moving = false;
    hovers.retain_mut(|e| {
        let target = if hover == Some(e.0) { 1.0 } else { 0.0 };
        if (e.2 - target).abs() > f32::EPSILON {
            e.1.retarget(target, now, HOVER_MS, EASE_OUT);
            e.2 = target;
        }
        let done = e.1.done(now, HOVER_MS);
        moving |= !done;
        !(done && target == 0.0)
    });
    moving
}

fn bar_fill(bar: &mut Option<(usize, Tween, f32)>, batch: usize, frac: f32, now: Instant, motion: bool) -> (f32, bool) {
    const MS: f32 = crate::settings::PAGE_RISE_MS;
    let e = match bar {
        Some(e) if e.0 == batch => e,
        _ => bar.insert((batch, Tween::at(frac), frac)),
    };
    if !motion {
        *e = (batch, Tween::at(frac), frac);
        return (frac.clamp(0.0, 1.0), false);
    }
    if (e.2 - frac).abs() > f32::EPSILON {
        e.1.retarget(frac, now, MS, EASE_PAGE);
        e.2 = frac;
    }
    (e.1.value(now, MS, EASE_PAGE).clamp(0.0, 1.0), !e.1.done(now, MS))
}
