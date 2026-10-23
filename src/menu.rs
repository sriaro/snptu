
use crate::{par, text::Text};
use std::{
    cell::RefCell,
    ffi::c_void,
    rc::Rc,
    sync::{Arc, Mutex, PoisonError, atomic::{AtomicBool, Ordering::SeqCst}},
    time::Instant,
};
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, GradientStop, IntSize, LineCap, LineJoin, LinearGradient, Paint, Pattern,
    PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt, CAPTUREBLT,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, GetMonitorInfoW,
            HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, ReleaseDC, SRCCOPY,
            SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_EXSTYLE, GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, IDC_ARROW,
                IsWindow, KillTimer, LoadCursorW, PostMessageW, RegisterClassExW, SM_CXDRAG, SM_CYDRAG, SW_SHOW, SW_SHOWNOACTIVATE,
                SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, ULW_ALPHA, USER_TIMER_MINIMUM,
                UpdateLayeredWindow, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
                WM_LBUTTONUP,
                WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::w,
};

#[derive(Clone)]
pub enum Entry {
    Item { id: String, label: String, hint: String, checked: bool, enabled: bool },
    Sub { label: String, items: Vec<Entry> },
    Sep,
}

impl Entry {
    pub fn item(id: impl Into<String>, label: impl Into<String>) -> Self {
        Entry::Item { id: id.into(), label: label.into(), hint: String::new(), checked: false, enabled: true }
    }

    pub fn sub(label: impl Into<String>, items: Vec<Entry>) -> Self {
        Entry::Sub { label: label.into(), items }
    }

    pub fn hint(mut self, text: impl Into<String>) -> Self {
        if let Entry::Item { hint, .. } = &mut self {
            *hint = text.into();
        }
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        if let Entry::Item { checked, .. } = &mut self {
            *checked = on;
        }
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        if let Entry::Item { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    fn selectable(&self) -> bool {
        match self {
            Entry::Item { enabled, .. } => *enabled,
            Entry::Sub { .. } => true,
            Entry::Sep => false,
        }
    }
}


const PAD: f32 = 6.0;
const ITEM_H: f32 = 30.0;
const SEP_H: f32 = 9.0;
const RADIUS: f32 = 12.0;
const MARGIN: f32 = 2.0;
const MIN_W: f32 = 190.0;
const TEXT_PX: f32 = 13.5;
pub(crate) const ANIM_MS: f32 = 460.0;
const STAGGER: f32 = 1.0;
const DROP_PX: f32 = 20.0;
const EASE_DROP: crate::settings::Ease = crate::settings::Ease::Bezier(0.25, 0.46, 0.45, 0.94);
pub(crate) const EASE_FADE: crate::settings::Ease = crate::settings::Ease::Bezier(0.3, 0.0, 0.6, 1.0);
pub(crate) const CLOSE_MS: f32 = 300.0;
const CLOSE_STAGGER: f32 = 0.6;
const CLOSE_RISE_PX: f32 = 10.0;
const EASE_CLOSE: crate::settings::Ease = crate::settings::Ease::Bezier(0.15, 0.2, 0.6, 1.0);
const FOLD_FAIL_LIMIT: u32 = 3;
const OPEN_FRAMES: usize = 0;
const CLOSE_FRAMES: usize = 1;
const WM_FRAME: u32 = WM_APP + 1;
const FRAME_TIMER: usize = 4;
const SUB_TIMER: usize = 2;
const SUB_DELAY_MS: u32 = 200;

pub(crate) struct MenuPal {
    pub(crate) ink: [u8; 3],
    pub(crate) dim: [u8; 3],
    pub(crate) hint: [u8; 3],
    pub(crate) tick: [u8; 3],
    pub(crate) arrow: [u8; 4],
    pub(crate) hover: [u8; 4],
    pub(crate) sep: [u8; 4],
    pub(crate) tint: [[u8; 4]; 2],
    pub(crate) solid: [[u8; 4]; 2],
    pub(crate) sheen: u8,
    pub(crate) edge: [[u8; 4]; 3],
}

pub(crate) fn pal(dark: bool) -> MenuPal {
    if dark {
        MenuPal {
            ink: [236, 240, 248],
            dim: [120, 132, 152],
            hint: [140, 152, 172],
            tick: [44, 178, 132],
            arrow: [255, 255, 255, 200],
            hover: [255, 255, 255, 34],
            sep: [255, 255, 255, 30],
            tint: [[26, 32, 50, 138], [12, 15, 27, 176]],
            solid: [[34, 40, 57, 244], [19, 23, 35, 247]],
            sheen: 30,
            edge: [[255, 255, 255, 90], [255, 255, 255, 34], [255, 255, 255, 48]],
        }
    } else {
        MenuPal {
            ink: [22, 32, 56],
            dim: [128, 140, 160],
            hint: [96, 108, 132],
            tick: [16, 158, 118],
            arrow: [22, 32, 56, 200],
            hover: [20, 32, 64, 26],
            sep: [20, 32, 64, 34],
            tint: [[250, 252, 255, 150], [238, 243, 252, 176]],
            solid: [[250, 252, 255, 246], [238, 243, 252, 248]],
            sheen: 60,
            edge: [[255, 255, 255, 230], [255, 255, 255, 170], [20, 32, 64, 40]],
        }
    }
}

struct Level {
    hwnd: HWND,
    entries: Vec<Entry>,
    rows: Vec<(f32, f32)>,
    body: [i32; 4],
    hover: Option<usize>,
    parent_row: Option<usize>,
    backdrop: Option<Pixmap>,
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut c_void,
    size: (i32, i32),
    born: Instant,
    from_bottom: bool,
    base: Option<Pixmap>,
}

struct State {
    levels: Vec<Level>,
    scale: f32,
    dark: bool,
    text: Arc<Text>,
    on_pick: Rc<dyn Fn(String)>,
    sub_due: Option<(usize, usize)>,
    passive: bool,
    armed: bool,
    origin: POINT,
}

#[derive(Clone, Copy, PartialEq)]
enum Open {
    Plain,
    Passive,
    Above,
}

struct Closing {
    levels: Vec<Level>,
    born: Instant,
    scale: f32,
}

unsafe impl Send for Closing {}

struct Fade {
    closing: Mutex<Option<Closing>>,
    over: AtomicBool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static CLOSING: RefCell<Option<Arc<Fade>>> = const { RefCell::new(None) };
}

fn tls<T: 'static, R>(key: &'static std::thread::LocalKey<T>, f: impl FnOnce(&T) -> R) -> Option<R> {
    key.try_with(f).ok()
}

fn take_state() -> Option<State> {
    tls(&STATE, |c| c.try_borrow_mut().ok().and_then(|mut g| g.take())).flatten()
}

fn take_closing() -> Option<Arc<Fade>> {
    tls(&CLOSING, |c| c.try_borrow_mut().ok().and_then(|mut g| g.take())).flatten()
}

fn end(fade: &Fade) -> bool {
    let closing = fade.closing.lock().unwrap_or_else(PoisonError::into_inner).take();
    let there = closing.is_some();
    drop(closing);
    there
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    tls(&STATE, |c| {
        let mut g = c.try_borrow_mut().ok()?;
        g.as_mut().map(f)
    })
    .flatten()
}

pub fn show(at: (i32, i32), entries: Vec<Entry>, scale: f32, dark: bool, text: Arc<Text>, on_pick: impl Fn(String) + 'static) {
    show_with(at, entries, scale, dark, text, on_pick, Open::Plain);
}

pub fn show_above(at: (i32, i32), entries: Vec<Entry>, scale: f32, dark: bool, text: Arc<Text>, on_pick: impl Fn(String) + 'static) {
    show_with(at, entries, scale, dark, text, on_pick, Open::Above);
}

pub(crate) fn height(entries: &[Entry], text: &Text, scale: f32) -> i32 {
    layout(entries, text, scale).2 as i32
}

pub fn show_passive(at: (i32, i32), entries: Vec<Entry>, scale: f32, dark: bool, text: Arc<Text>, on_pick: impl Fn(String) + 'static) {
    show_with(at, entries, scale, dark, text, on_pick, Open::Passive);
}

fn show_with(at: (i32, i32), entries: Vec<Entry>, scale: f32, dark: bool, text: Arc<Text>, on_pick: impl Fn(String) + 'static, how: Open) {
    let passive = how == Open::Passive;
    if dismiss() {
        unsafe {
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        }
    }
    crate::settings::refresh_motion();
    STATE.with(|c| {
        *c.borrow_mut() =
            Some(State { levels: Vec::new(), scale, dark, text, on_pick: Rc::new(on_pick), sub_due: None, passive, armed: false, origin: cursor() });
    });
    if !open_level(entries, at, at.0, None, false, how == Open::Above) {
        dismiss();
        return;
    }
    if let Some(hwnd) = with(|st| st.levels[0].hwnd) {
        unsafe {
            if !passive {
                let _ = SetForegroundWindow(hwnd);
            }
            SetCapture(hwnd);
        }
    }
}

pub fn is_open() -> bool {
    tls(&STATE, |c| c.try_borrow().is_ok_and(|g| g.is_some())).unwrap_or(false)
}

pub fn key(vk: u16) {
    on_key(vk);
}

pub fn close() {
    fade_out();
}

pub fn dismiss() -> bool {
    let fading = end_fade();
    let Some(st) = take_state() else { return fading };
    unsafe {
        let _ = ReleaseCapture();
    }
    let shown = st.levels.iter().any(|l| !l.hwnd.is_invalid());
    drop(st);
    fading || shown
}

fn end_fade() -> bool {
    take_closing().is_some_and(|fade| end(&fade))
}

pub fn finish_fade() -> bool {
    let Some(fade) = take_closing() else { return false };
    let until = Instant::now() + std::time::Duration::from_millis(CLOSE_MS as u64 + 200);
    while !fade.over.load(SeqCst) && Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    end(&fade)
}

fn snapshot(l: &Level) -> Option<Pixmap> {
    if l.bits.is_null() {
        return None;
    }
    let n = (l.size.0 * l.size.1) as usize * 4;
    let src = unsafe { std::slice::from_raw_parts(l.bits as *const u8, n) };
    let mut data = vec![0u8; n];
    rgba_to_bgra(&mut data, src);
    Pixmap::from_vec(data, IntSize::from_wh(l.size.0 as u32, l.size.1 as u32)?)
}

fn fade_out() {
    let Some((fade, root)) = hold_fade() else { return };
    play_close(fade, root);
}

fn hold_fade() -> Option<(Arc<Fade>, HWND)> {
    let st = take_state()?;
    unsafe {
        let _ = ReleaseCapture();
    }
    let State { levels, scale, .. } = st;
    let rooted = levels.first().is_some_and(|l| !l.hwnd.is_invalid());
    if !crate::settings::animations_on() || !rooted {
        drop(levels);
        return None;
    }
    let mut levels = levels;
    for l in &mut levels {
        l.base = snapshot(l);
    }
    if levels.iter().any(|l| l.base.is_none()) {
        drop(levels);
        return None;
    }
    if let Some(older) = take_closing() {
        end(&older);
    }
    let root = levels[0].hwnd;
    for l in &levels {
        unsafe {
            let ex = GetWindowLongPtrW(l.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(l.hwnd, GWL_EXSTYLE, ex | WS_EX_TRANSPARENT.0 as isize);
        }
    }
    let fade = Arc::new(Fade { closing: Mutex::new(Some(Closing { levels, born: Instant::now(), scale })), over: AtomicBool::new(false) });
    let held = fade.clone();
    if tls(&CLOSING, |c| c.try_borrow_mut().map(|mut g| *g = Some(held)).is_ok()) != Some(true) {
        end(&fade);
        return None;
    }
    Some((fade, root))
}

fn play_close(fade: Arc<Fade>, root: HWND) {
    let me = root.0 as isize;
    let theirs = fade.clone();
    if std::thread::Builder::new().name("menu-fold".into()).spawn(move || run_close(&theirs, me)).is_err() {
        let _ = end_fade();
    }
}

fn run_close(fade: &Fade, root: isize) {
    let mut last = None;
    run_close_with(fade, root, |l, place| push_fold_frame(l.hwnd, l.dc, place), || crate::settings::wait_frame(&mut last));
}

fn push_fold_frame(hwnd: HWND, dc: HDC, place: (POINT, SIZE)) -> bool {
    let mut rc = RECT::default();
    let there = unsafe { GetWindowRect(hwnd, &mut rc) }.is_ok() && rect_is(&rc, place);
    (there && push(hwnd, dc, 255, Some(place))) || push(hwnd, dc, 255, None)
}

fn rect_is(rc: &RECT, place: (POINT, SIZE)) -> bool {
    rc.left == place.0.x && rc.top == place.0.y && rc.right - rc.left == place.1.cx && rc.bottom - rc.top == place.1.cy
}

fn run_close_with(fade: &Fade, root: isize, mut show: impl FnMut(&Level, (POINT, SIZE)) -> bool, mut wait: impl FnMut()) {
    struct Over<'a>(&'a Fade, isize);
    impl Drop for Over<'_> {
        fn drop(&mut self) {
            self.0.over.store(true, SeqCst);
            let _ = unsafe { PostMessageW(Some(HWND(self.1 as *mut c_void)), WM_FRAME, WPARAM(CLOSE_FRAMES), LPARAM(0)) };
        }
    }
    let _over = Over(fade, root);
    let mut failed = 0;
    loop {
        match close_step(fade, &mut show) {
            Step::Playing => failed = 0,
            Step::Failed => {
                failed += 1;
                if failed >= FOLD_FAIL_LIMIT {
                    break;
                }
            }
            Step::Over | Step::Gone => break,
        }
        wait();
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Step {
    Playing,
    Failed,
    Over,
    Gone,
}

fn close_step(fade: &Fade, mut show: impl FnMut(&Level, (POINT, SIZE)) -> bool) -> Step {
    let g = fade.closing.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(cl) = g.as_ref() else { return Step::Gone };
    let t = (cl.born.elapsed().as_secs_f32() * 1000.0 / CLOSE_MS).clamp(0.0, 1.0);
    let mut shown = true;
    for l in &cl.levels {
        let Some(base) = l.base.as_ref() else { continue };
        if let Some(frame) = close_frame(base, t, l.from_bottom, cl.scale) {
            copy_to_dib(l, &frame);
            shown &= show(l, place_of(l, cl.scale));
        }
    }
    if t >= 1.0 {
        Step::Over
    } else if shown {
        Step::Playing
    } else {
        Step::Failed
    }
}

fn step_close() -> bool {
    let fade = tls(&CLOSING, |c| c.try_borrow().ok().and_then(|g| g.clone())).flatten();
    let Some(fade) = fade else { return true };
    if !fade.over.load(SeqCst) {
        return false;
    }
    let _ = take_closing();
    end(&fade);
    true
}

struct Ticker {
    hwnd: isize,
    stop: AtomicBool,
    waiting: AtomicBool,
}

thread_local! {
    static TICKERS: RefCell<Option<Arc<Ticker>>> = const { RefCell::new(None) };
}

fn start_frames(hwnd: HWND) {
    let me = hwnd.0 as isize;
    let started = tls(&TICKERS, |c| {
        let mut g = c.try_borrow_mut().ok()?;
        if let Some(old) = &*g {
            if old.hwnd == me && !old.stop.load(SeqCst) {
                return None;
            }
            old.stop.store(true, SeqCst);
        }
        let t = Arc::new(Ticker { hwnd: me, stop: AtomicBool::new(false), waiting: AtomicBool::new(false) });
        *g = Some(t.clone());
        Some(t)
    })
    .flatten();
    let Some(t) = started else { return };
    let thread = std::thread::Builder::new().name("menu-frames".into()).spawn(move || tick(&t));
    if thread.is_err() {
        unsafe {
            SetTimer(Some(hwnd), FRAME_TIMER, USER_TIMER_MINIMUM, None);
        }
    }
}

fn tick(t: &Ticker) {
    let mut last = None;
    while !t.stop.load(SeqCst) {
        crate::settings::wait_frame(&mut last);
        let hwnd = HWND(t.hwnd as *mut c_void);
        if t.stop.load(SeqCst) || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            break;
        }
        if t.waiting.swap(true, SeqCst) {
            continue;
        }
        if unsafe { PostMessageW(Some(hwnd), WM_FRAME, WPARAM(OPEN_FRAMES), LPARAM(0)) }.is_err() {
            t.waiting.store(false, SeqCst);
        }
    }
    t.stop.store(true, SeqCst);
}

fn on_frame(hwnd: HWND, kind: usize) {
    match kind {
        OPEN_FRAMES => {
            let ticker = tls(&TICKERS, |c| c.try_borrow().ok().and_then(|g| g.clone())).flatten();
            let done = step();
            if let Some(t) = &ticker {
                t.waiting.store(false, SeqCst);
            }
            if done {
                let old = tls(&TICKERS, |c| c.try_borrow_mut().ok().and_then(|mut g| g.take())).flatten();
                if let Some(t) = old {
                    t.stop.store(true, SeqCst);
                }
                let _ = unsafe { KillTimer(Some(hwnd), FRAME_TIMER) };
            }
        }
        CLOSE_FRAMES => {
            step_close();
        }
        _ => {}
    }
}

impl Drop for Level {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
        }
    }
}


fn layout(entries: &[Entry], text: &Text, s: f32) -> (Vec<(f32, f32)>, f32, f32) {
    let px = TEXT_PX * s;
    let mut y = PAD * s;
    let mut rows = Vec::with_capacity(entries.len());
    let (mut label_w, mut hint_w) = (0.0f32, 0.0f32);
    for e in entries {
        match e {
            Entry::Sep => {
                rows.push((y, SEP_H * s));
                y += SEP_H * s;
            }
            Entry::Item { label, hint, .. } => {
                rows.push((y, ITEM_H * s));
                y += ITEM_H * s;
                label_w = label_w.max(text.measure(label, px));
                hint_w = hint_w.max(text.measure(hint, px));
            }
            Entry::Sub { label, .. } => {
                rows.push((y, ITEM_H * s));
                y += ITEM_H * s;
                label_w = label_w.max(text.measure(label, px));
            }
        }
    }
    let h = y + PAD * s;
    let extra = if hint_w > 0.0 { hint_w + 28.0 * s } else { 0.0 };
    let w = (2.0 * PAD * s + 34.0 * s + label_w + extra + 34.0 * s).max(MIN_W * s);
    (rows, w.ceil(), h.ceil())
}

fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    crate::toolbar::rrect(x, y, w, h, r)
}

pub(crate) fn paint(c: [u8; 3], a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], a);
    p.anti_alias = true;
    p
}

pub(crate) fn gradient(y0: f32, y1: f32, stops: &[(f32, [u8; 4])]) -> Paint<'static> {
    let mut p = Paint { anti_alias: true, ..Default::default() };
    if let Some(g) = LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, y1),
        stops.iter().map(|(t, c)| GradientStop::new(*t, Color::from_rgba8(c[0], c[1], c[2], c[3]))).collect(),
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        p.shader = g;
    }
    p
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_glass(pix: &mut Pixmap, m: f32, bw: f32, bh: f32, radius: f32, s: f32, backdrop: Option<&Pixmap>, dark: bool) -> Option<()> {
    paint_glass_veiled(pix, m, bw, bh, radius, s, backdrop, dark, 0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_glass_veiled(
    pix: &mut Pixmap,
    m: f32,
    bw: f32,
    bh: f32,
    radius: f32,
    s: f32,
    backdrop: Option<&Pixmap>,
    dark: bool,
    veil: u8,
) -> Option<()> {
    let id = Transform::identity();
    let c = pal(dark);
    let body = crate::toolbar::frame(m, m, bw, bh, radius)?;
    let clear = Paint { blend_mode: BlendMode::Clear, ..Default::default() };
    pix.fill_path(&body, &clear, FillRule::Winding, id, None);
    let tint: [(f32, [u8; 4]); 2] = if let Some(bd) = backdrop {
        let p = Paint {
            shader: Pattern::new(bd.as_ref(), SpreadMode::Pad, FilterQuality::Nearest, 1.0, Transform::from_translate(m, m)),
            anti_alias: true,
            ..Default::default()
        };
        pix.fill_path(&body, &p, FillRule::Winding, id, None);
        if veil > 0 {
            pix.fill_path(&body, &paint([c.solid[1][0], c.solid[1][1], c.solid[1][2]], veil), FillRule::Winding, id, None);
        }
        [(0.0, c.tint[0]), (1.0, c.tint[1])]
    } else {
        [(0.0, c.solid[0]), (1.0, c.solid[1])]
    };
    pix.fill_path(&body, &gradient(m, m + bh, &tint), FillRule::Winding, id, None);
    pix.fill_path(
        &body,
        &gradient(m, m + bh * 0.45, &[(0.0, [255, 255, 255, c.sheen]), (1.0, [255, 255, 255, 0])]),
        FillRule::Winding,
        id,
        None,
    );
    let edge = Stroke { width: s.max(1.0), ..Default::default() };
    pix.stroke_path(&body, &gradient(m, m + bh, &[(0.0, c.edge[0]), (0.5, c.edge[1]), (1.0, c.edge[2])]), &edge, id, None);
    Some(())
}

fn draw(l: &Level, text: &Text, s: f32, dark: bool) -> Option<Pixmap> {
    let id = Transform::identity();
    let c = pal(dark);
    let m = MARGIN * s;
    let (bw, bh) = (l.body[2] as f32, l.body[3] as f32);
    let mut pix = Pixmap::new(l.size.0 as u32, l.size.1 as u32)?;
    paint_glass(&mut pix, m, bw, bh, RADIUS * s, s, l.backdrop.as_ref(), dark)?;

    let px = TEXT_PX * s;
    let line_h = text.line_height(px);
    for (i, (e, &(ry, rh))) in l.entries.iter().zip(&l.rows).enumerate() {
        let y = m + ry;
        match e {
            Entry::Sep => {
                if let Some(r) = tiny_skia::Rect::from_xywh(m + (PAD + 8.0) * s, y + (rh / 2.0).floor(), bw - 2.0 * (PAD + 8.0) * s, 1.0) {
                    pix.fill_rect(r, &paint([c.sep[0], c.sep[1], c.sep[2]], c.sep[3]), id, None);
                }
            }
            Entry::Item { label, hint, checked, enabled, .. } => {
                if l.hover == Some(i)
                    && *enabled
                    && let Some(p) = rrect(m + (PAD - 1.0) * s, y, bw - 2.0 * (PAD - 1.0) * s, rh, 8.0 * s)
                {
                    pix.fill_path(&p, &paint([c.hover[0], c.hover[1], c.hover[2]], c.hover[3]), FillRule::Winding, id, None);
                }
                let ink = if *enabled { c.ink } else { c.dim };
                text.draw(&mut pix, label, m + (PAD + 34.0) * s, y + (rh - line_h) / 2.0, px, ink);
                if !hint.is_empty() {
                    let hw = text.measure(hint, px);
                    text.draw(&mut pix, hint, m + bw - (PAD + 16.0) * s - hw, y + (rh - line_h) / 2.0, px, if *enabled { c.hint } else { c.dim });
                }
                if *checked {
                    let (cx, cy) = (m + (PAD + 17.0) * s, y + rh / 2.0);
                    let mut pb = PathBuilder::new();
                    pb.move_to(cx - 4.5 * s, cy + 0.2 * s);
                    pb.line_to(cx - 1.3 * s, cy + 3.6 * s);
                    pb.line_to(cx + 4.8 * s, cy - 3.6 * s);
                    if let Some(p) = pb.finish() {
                        let st = Stroke { width: 1.9 * s, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                        pix.stroke_path(&p, &paint(c.tick, if *enabled { 255 } else { 120 }), &st, id, None);
                    }
                }
            }
            Entry::Sub { label, .. } => {
                if l.hover == Some(i)
                    && let Some(p) = rrect(m + (PAD - 1.0) * s, y, bw - 2.0 * (PAD - 1.0) * s, rh, 8.0 * s)
                {
                    pix.fill_path(&p, &paint([c.hover[0], c.hover[1], c.hover[2]], c.hover[3]), FillRule::Winding, id, None);
                }
                text.draw(&mut pix, label, m + (PAD + 34.0) * s, y + (rh - line_h) / 2.0, px, c.ink);
                let (cx, cy) = (m + bw - (PAD + 17.0) * s, y + rh / 2.0);
                let mut pb = PathBuilder::new();
                pb.move_to(cx - 2.2 * s, cy - 4.2 * s);
                pb.line_to(cx + 2.2 * s, cy);
                pb.line_to(cx - 2.2 * s, cy + 4.2 * s);
                if let Some(p) = pb.finish() {
                    let st = Stroke { width: 1.6 * s, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
                    pix.stroke_path(&p, &paint([c.arrow[0], c.arrow[1], c.arrow[2]], c.arrow[3]), &st, id, None);
                }
            }
        }
    }
    Some(pix)
}

fn copy_to_dib(l: &Level, pix: &Pixmap) {
    let n = ((l.size.0 * l.size.1) as usize * 4).min(pix.data().len());
    let dst = unsafe { std::slice::from_raw_parts_mut(l.bits as *mut u8, n) };
    rgba_to_bgra(dst, pix.data());
}

pub(crate) fn rgba_to_bgra(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
        *d = [s[2], s[1], s[0], s[3]];
    }
}

fn place_of(l: &Level, scale: f32) -> (POINT, SIZE) {
    let m = (MARGIN * scale).round() as i32;
    (POINT { x: l.body[0] - m, y: l.body[1] - m }, SIZE { cx: l.size.0, cy: l.size.1 })
}

pub(crate) fn compose(base: &Pixmap, t: f32, anchor: (f32, f32), s: f32) -> Option<Pixmap> {
    use crate::settings::{EASE_OUT, EASE_PAGE, bezier};
    let e = bezier(EASE_PAGE, t.clamp(0.0, 1.0));
    let scale = 0.94 + 0.06 * e;
    let lift = -6.0 * s * (1.0 - e);
    let opacity = bezier(EASE_OUT, (t * 1.5).clamp(0.0, 1.0));
    let tf = Transform::from_translate(-anchor.0, -anchor.1).post_scale(scale, scale).post_translate(anchor.0, anchor.1 + lift);
    let mut out = Pixmap::new(base.width(), base.height())?;
    let paint = tiny_skia::PixmapPaint { opacity, quality: FilterQuality::Bilinear, ..Default::default() };
    out.draw_pixmap(0, 0, base.as_ref(), &paint, tf, None);
    Some(out)
}

pub(crate) fn unfold(base: &Pixmap, t: f32, anchor: (f32, f32), s: f32) -> Option<Pixmap> {
    unfold_rows(base, t, anchor.1 > 0.0, s)
}

fn unfold_rows(base: &Pixmap, t: f32, from_bottom: bool, s: f32) -> Option<Pixmap> {
    use crate::settings::bezier;
    let t = t.clamp(0.0, 1.0);
    let drop = DROP_PX * s;
    shift_rows(base, from_bottom, |d| {
        let p = (t * (1.0 + STAGGER) - STAGGER * d).clamp(0.0, 1.0);
        (bezier(EASE_FADE, p), drop * (1.0 - bezier(EASE_DROP, p)))
    })
}

fn shift_rows(base: &Pixmap, from_bottom: bool, row: impl Fn(f32) -> (f32, f32)) -> Option<Pixmap> {
    let (w, h) = (base.width() as usize, base.height() as usize);
    let mut out = Pixmap::new(base.width(), base.height())?;
    let row_len = w * 4;
    let src = base.data();
    for (y, dst) in out.data_mut().chunks_exact_mut(row_len).enumerate() {
        let d = (if from_bottom { h - 1 - y } else { y }) as f32 / h as f32;
        let (alpha, away) = row(d);
        if alpha <= 0.0 {
            continue;
        }
        if away <= 0.0 && alpha >= 1.0 {
            dst.copy_from_slice(&src[y * row_len..(y + 1) * row_len]);
            continue;
        }
        let from = y as f32 + if from_bottom { -away } else { away };
        let r0 = from.floor();
        let f = from - r0;
        let at = |r: i64| -> Option<&[u8]> { (r >= 0 && (r as usize) < h).then(|| &src[r as usize * row_len..(r as usize + 1) * row_len]) };
        let (a, b) = (at(r0 as i64), at(r0 as i64 + 1));
        for (i, px) in dst.iter_mut().enumerate() {
            let va = a.map_or(0.0, |r| r[i] as f32);
            let vb = b.map_or(0.0, |r| r[i] as f32);
            *px = ((va + (vb - va) * f) * alpha) as u8;
        }
    }
    Some(out)
}

pub(crate) fn close_frame(base: &Pixmap, t: f32, from_bottom: bool, s: f32) -> Option<Pixmap> {
    use crate::settings::bezier;
    let t = t.clamp(0.0, 1.0);
    let rise = CLOSE_RISE_PX * s;
    shift_rows(base, from_bottom, |d| {
        let c = (t * (1.0 + CLOSE_STAGGER) - CLOSE_STAGGER * (1.0 - d)).clamp(0.0, 1.0);
        (1.0 - bezier(EASE_CLOSE, c), rise * c)
    })
}

fn render(i: usize) {
    render_at(i, None);
}

fn render_at(i: usize, t0: Option<f32>) {
    let work = with(|st| {
        let scale = st.scale;
        let l = st.levels.get_mut(i)?;
        let pix = draw(l, &st.text, scale, st.dark)?;
        let t = if crate::settings::animations_on() { t0.unwrap_or_else(|| l.born.elapsed().as_secs_f32() * 1000.0 / ANIM_MS) } else { 1.0 };
        if t < 1.0 {
            let frame = unfold_rows(&pix, t, l.from_bottom, scale)?;
            copy_to_dib(l, &frame);
            l.base = Some(pix);
        } else {
            copy_to_dib(l, &pix);
            l.base = None;
        }
        Some((l.hwnd, l.dc, place_of(l, scale)))
    })
    .flatten();
    if let Some((hwnd, dc, place)) = work {
        push(hwnd, dc, 255, Some(place));
    }
}

pub(crate) fn push(hwnd: HWND, dc: HDC, alpha: u8, place: Option<(POINT, SIZE)>) -> bool {
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
    let src = POINT { x: 0, y: 0 };
    unsafe {
        let r = UpdateLayeredWindow(
            hwnd,
            None,
            place.as_ref().map(|p| &p.0 as *const POINT),
            place.as_ref().map(|p| &p.1 as *const SIZE),
            Some(dc),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        if let Err(e) = r {
            crate::log(&format!("menu: UpdateLayeredWindow(place={}): {e}", place.is_some()));
            return false;
        }
    }
    true
}

pub(crate) fn grab_backdrop(x: i32, y: i32, w: i32, h: i32, s: f32) -> Option<Pixmap> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut data = vec![0u8; w as usize * h as usize * 4];
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        let mut ok = false;
        if let Some((bmp, bits)) = make_dib(mem, w, h) {
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            if BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok() {
                let _ = GdiFlush();
                let n = data.len();
                data.copy_from_slice(std::slice::from_raw_parts(bits as *const u8, n));
                ok = true;
            }
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            return None;
        }
    }
    glass_from_bgra(data, w, h, s)
}

pub(crate) fn backdrop_from_frame(frame: &crate::capture::Frame, x: i32, y: i32, w: i32, h: i32, s: f32) -> Option<Pixmap> {
    let (fx, fy) = (x - frame.x, y - frame.y);
    if w <= 0 || h <= 0 || fx < 0 || fy < 0 || fx + w > frame.w as i32 || fy + h > frame.h as i32 {
        return None;
    }
    let (src, fw) = (frame.bgra(), frame.w as usize * 4);
    let mut data = vec![0u8; (w * h * 4) as usize];
    for row in 0..h as usize {
        let from = (fy as usize + row) * fw + fx as usize * 4;
        data[row * w as usize * 4..(row + 1) * w as usize * 4].copy_from_slice(&src[from..from + w as usize * 4]);
    }
    glass_from_bgra(data, w, h, s)
}

fn glass_from_bgra(mut data: Vec<u8>, w: i32, h: i32, s: f32) -> Option<Pixmap> {
    par::rows(&mut data, w as usize * 4, |_, row| {
        for px in row.as_chunks_mut::<4>().0 {
            let (b, g, r) = (px[0] as f32, px[1] as f32, px[2] as f32);
            let l = 0.299 * r + 0.587 * g + 0.114 * b;
            let boost = |c: f32| (l + (c - l) * 1.35).clamp(0.0, 255.0) as u8;
            px[0] = boost(r);
            px[1] = boost(g);
            px[2] = boost(b);
            px[3] = 255;
        }
    });
    const F: usize = 3;
    let (wu, hu) = (w as usize, h as usize);
    let (sw, sh) = (wu.div_ceil(F), hu.div_ceil(F));
    let mut small = vec![0u8; sw * sh * 4];
    par::rows(&mut small, sw * 4, |sy, row| {
        for sx in 0..sw {
            let (mut sum, mut n) = ([0u32; 3], 0u32);
            for y in (sy * F)..((sy + 1) * F).min(hu) {
                for x in (sx * F)..((sx + 1) * F).min(wu) {
                    let i = (y * wu + x) * 4;
                    for k in 0..3 {
                        sum[k] += data[i + k] as u32;
                    }
                    n += 1;
                }
            }
            for k in 0..3 {
                row[sx * 4 + k] = (sum[k] / n.max(1)) as u8;
            }
            row[sx * 4 + 3] = 255;
        }
    });
    par::blur_rgba(&mut small, sw, sh, ((6.0 * s / F as f32).round() as usize).max(1));
    par::rows(&mut data, wu * 4, |y, row| {
        let fy = ((y as f32 + 0.5) / F as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..wu {
            let fx = ((x as f32 + 0.5) / F as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            for k in 0..3 {
                let at = |xx: usize, yy: usize| small[(yy * sw + xx) * 4 + k] as f32;
                let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
                let bottom = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
                row[x * 4 + k] = (top * (1.0 - ty) + bottom * ty + 0.5) as u8;
            }
            row[x * 4 + 3] = 255;
        }
    });
    Pixmap::from_vec(data, IntSize::from_wh(w as u32, h as u32)?)
}


fn register_class() -> Option<HINSTANCE> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let hinst = unsafe { GetModuleHandleW(None) }.ok().map(|h| HINSTANCE(h.0))?;
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("SnptuMenu"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
    Some(hinst)
}

pub(crate) fn work_area(p: POINT) -> [i32; 4] {
    monitor_work(unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) })
}

pub(crate) fn monitor_work(mon: HMONITOR) -> [i32; 4] {
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool() {
        [mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom]
    } else {
        [0, 0, 1920, 1080]
    }
}

pub(crate) fn make_dib(dc: HDC, w: i32, h: i32) -> Option<(HBITMAP, *mut c_void)> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bmp = unsafe { CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
    if bits.is_null() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(bmp.0));
        }
        return None;
    }
    Some((bmp, bits))
}

pub(crate) fn signed(lp: LPARAM) -> (i32, i32) {
    (lp.0 as i16 as i32, (lp.0 >> 16) as i16 as i32)
}

fn window_flags(root: bool, passive: bool) -> (windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE, windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD) {
    if root && !passive { (Default::default(), SW_SHOW) } else { (WS_EX_NOACTIVATE, SW_SHOWNOACTIVATE) }
}

fn place_level(at: (i32, i32), x_left: i32, size: (i32, i32), wa: [i32; 4], root: bool, above: bool) -> (i32, i32, bool) {
    let (bw, bh) = size;
    let x = if at.0 + bw > wa[2] { x_left - bw } else { at.0 }.clamp(wa[0], (wa[2] - bw).max(wa[0]));
    let up = root && (above || at.1 + bh > wa[3]);
    let y = if up { at.1 - bh } else { at.1 }.min(wa[3] - bh).max(wa[1]);
    (x, y, up)
}

fn open_level(entries: Vec<Entry>, at: (i32, i32), x_left: i32, parent_row: Option<usize>, select_first: bool, above: bool) -> bool {
    let Some((text, s, passive)) = with(|st| (st.text.clone(), st.scale, st.passive)) else { return false };
    let (rows, bw, bh) = layout(&entries, &text, s);
    let (bw, bh) = (bw as i32, bh as i32);
    let root = parent_row.is_none();
    let (x, y, up) = place_level(at, x_left, (bw, bh), work_area(POINT { x: at.0, y: at.1 }), root, above);
    let m = (MARGIN * s).round() as i32;
    let (ww, wh) = (bw + 2 * m, bh + 2 * m);

    let Some(hinst) = register_class() else { return false };
    let (ex_activation, show_cmd) = window_flags(root, passive);
    let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | ex_activation;
    let Ok(hwnd) = (unsafe { CreateWindowExW(ex, w!("SnptuMenu"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None) }) else {
        return false;
    };
    let dc = unsafe { CreateCompatibleDC(None) };
    let Some((bmp, bits)) = make_dib(dc, ww, wh) else {
        unsafe {
            let _ = DeleteDC(dc);
            let _ = DestroyWindow(hwnd);
        }
        return false;
    };
    unsafe {
        SelectObject(dc, HGDIOBJ(bmp.0));
    }
    let first_hover = if select_first { entries.iter().position(Entry::selectable) } else { None };
    let level = Level {
        hwnd,
        entries,
        rows,
        body: [x, y, bw, bh],
        hover: first_hover,
        parent_row,
        backdrop: grab_backdrop(x, y, bw, bh, s),
        dc,
        bmp,
        bits,
        size: (ww, wh),
        born: Instant::now(),
        from_bottom: up,
        base: None,
    };
    let idx = with(|st| {
        st.levels.push(level);
        st.levels.len() - 1
    });
    let Some(idx) = idx else {
        return false;
    };
    render_at(idx, Some(0.0));
    unsafe {
        let _ = ShowWindow(hwnd, show_cmd);
    }
    let timer_on = with(|st| {
        let l = st.levels.get_mut(idx)?;
        l.born = Instant::now();
        l.base.as_ref()?;
        Some(st.levels[0].hwnd)
    })
    .flatten();
    if let Some(first) = timer_on {
        start_frames(first);
    }
    true
}

fn open_sub(li: usize, row: usize, select_first: bool) {
    let info = with(|st| {
        let l = st.levels.get(li)?;
        let Some(Entry::Sub { items, .. }) = l.entries.get(row) else { return None };
        let (ry, _) = l.rows[row];
        Some((items.clone(), l.body, ry, st.scale))
    })
    .flatten();
    let Some((items, body, ry, s)) = info else { return };
    arm_sub(None);
    close_from(li + 1);
    let at = (body[0] + body[2] - (4.0 * s) as i32, body[1] + ry as i32 - (PAD * s) as i32);
    open_level(items, at, body[0] + (4.0 * s) as i32, Some(row), select_first, false);
}

fn wanted_sub(st: &State, p: POINT) -> Option<(usize, usize)> {
    let (li, Some(row)) = hit(st, p)? else { return None };
    let open = st.levels.get(li + 1).is_some_and(|c| c.parent_row == Some(row));
    (matches!(st.levels[li].entries[row], Entry::Sub { .. }) && !open).then_some((li, row))
}

fn arm_sub(want: Option<(usize, usize)>) {
    let Some((first, was)) = with(|st| (st.levels.first().map(|l| l.hwnd), std::mem::replace(&mut st.sub_due, want))) else { return };
    let Some(first) = first.filter(|_| was != want) else { return };
    unsafe {
        if want.is_some() {
            SetTimer(Some(first), SUB_TIMER, SUB_DELAY_MS, None);
        } else {
            let _ = KillTimer(Some(first), SUB_TIMER);
        }
    }
}

fn on_sub_timer(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), SUB_TIMER);
    }
    let p = cursor();
    let want = with(|st| {
        st.sub_due = None;
        wanted_sub(st, p)
    })
    .flatten();
    if let Some((li, row)) = want {
        open_sub(li, row, false);
    }
}

fn close_from(from: usize) {
    let gone = with(|st| if st.levels.len() > from { st.levels.split_off(from) } else { Vec::new() }).unwrap_or_default();
    drop(gone);
}


fn hit(st: &State, p: POINT) -> Option<(usize, Option<usize>)> {
    for (li, l) in st.levels.iter().enumerate().rev() {
        let [x, y, w, h] = l.body;
        if p.x >= x && p.x < x + w && p.y >= y && p.y < y + h {
            let ry = (p.y - y) as f32;
            let row = l.rows.iter().position(|&(top, rh)| ry >= top && ry < top + rh);
            return Some((li, row));
        }
    }
    None
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

fn past_drag(from: POINT, to: POINT, limit: (i32, i32)) -> bool {
    (to.x - from.x).abs() > limit.0 || (to.y - from.y).abs() > limit.1
}

fn drag_limit() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) }
}

fn on_mouse_move() {
    let p = cursor();
    let act = with(|st| {
        st.armed |= past_drag(st.origin, p, drag_limit());
        let (li, row) = hit(st, p)?;
        let l = &mut st.levels[li];
        let row = row.filter(|&r| l.entries[r].selectable());
        let changed = l.hover != row;
        l.hover = row;
        let has_child = st.levels.get(li + 1).is_some_and(|c| c.parent_row == row);
        Some((li, changed, has_child, wanted_sub(st, p)))
    })
    .flatten();
    let Some((li, changed, has_child, want)) = act else { return };
    if !has_child {
        close_from(li + 1);
    }
    if changed {
        render(li);
    }
    arm_sub(want);
}

fn on_button_down(p: POINT, left: bool) {
    let inside = with(|st| {
        let inside = hit(st, p).is_some();
        st.armed |= inside && left;
        inside
    });
    if inside == Some(false) {
        fade_out();
    }
}

fn pick(id: String) {
    let cb = with(|st| st.on_pick.clone());
    fade_out();
    if let Some(cb) = cb {
        cb(id);
    }
}

fn on_button_up(p: POINT) {
    let id = with(|st| {
        if !st.armed {
            return None;
        }
        let (li, Some(row)) = hit(st, p)? else { return None };
        match &st.levels[li].entries[row] {
            Entry::Item { id, enabled: true, .. } => Some(id.clone()),
            _ => None,
        }
    })
    .flatten();
    if let Some(id) = id {
        pick(id);
    }
}

fn on_key(vk: u16) {
    enum Do {
        Pick(String),
        Render(usize),
        Open(usize, usize),
        CloseLast,
        Dismiss,
        Nothing,
    }
    let what = with(|st| {
        let last = st.levels.len().checked_sub(1)?;
        let l = &mut st.levels[last];
        let n = l.entries.len();
        let step = |from: Option<usize>, dir: i32| -> Option<usize> {
            let mut i = from.map_or(if dir > 0 { n as i32 - 1 } else { 0 }, |v| v as i32);
            for _ in 0..n {
                i = (i + dir).rem_euclid(n as i32);
                if l.entries[i as usize].selectable() {
                    return Some(i as usize);
                }
            }
            None
        };
        Some(match vk {
            v if v == VK_ESCAPE.0 => {
                if last > 0 { Do::CloseLast } else { Do::Dismiss }
            }
            v if v == VK_DOWN.0 || v == VK_UP.0 => {
                let next = step(l.hover, if v == VK_DOWN.0 { 1 } else { -1 });
                if next.is_some() {
                    l.hover = next;
                }
                Do::Render(last)
            }
            v if v == VK_LEFT.0 => {
                if last > 0 { Do::CloseLast } else { Do::Nothing }
            }
            v if v == VK_RIGHT.0 => match l.hover {
                Some(r) if matches!(l.entries[r], Entry::Sub { .. }) => Do::Open(last, r),
                _ => Do::Nothing,
            },
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => match l.hover.map(|r| (r, &l.entries[r])) {
                Some((_, Entry::Item { id, enabled: true, .. })) => Do::Pick(id.clone()),
                Some((r, Entry::Sub { .. })) => Do::Open(last, r),
                _ => Do::Nothing,
            },
            _ => Do::Nothing,
        })
    })
    .flatten();
    match what {
        Some(Do::Pick(id)) => pick(id),
        Some(Do::Render(i)) => render(i),
        Some(Do::Open(li, r)) => open_sub(li, r, true),
        Some(Do::CloseLast) => {
            let n = with(|st| st.levels.len()).unwrap_or(1);
            close_from(n.saturating_sub(1));
        }
        Some(Do::Dismiss) => fade_out(),
        Some(Do::Nothing) | None => {}
    }
}

fn step() -> bool {
    let (updates, done) = with(|st| {
        let scale = st.scale;
        let mut done = true;
        let mut ups = Vec::new();
        for l in &mut st.levels {
            let Some(base) = l.base.as_ref() else { continue };
            let t = (l.born.elapsed().as_secs_f32() * 1000.0 / ANIM_MS).clamp(0.0, 1.0);
            let finished = t >= 1.0;
            if finished {
                copy_to_dib(l, base);
            } else if let Some(frame) = unfold_rows(base, t, l.from_bottom, scale) {
                copy_to_dib(l, &frame);
            }
            ups.push((l.hwnd, l.dc, place_of(l, scale)));
            if finished {
                l.base = None;
            } else {
                done = false;
            }
        }
        (ups, done)
    })
    .unwrap_or((Vec::new(), true));
    for (h, dc, place) in updates {
        push(h, dc, 255, Some(place));
    }
    done
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(hwnd, msg, wparam, lparam)))
        .unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

fn handle(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => on_mouse_move(),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => on_button_down(cursor(), msg == WM_LBUTTONDOWN),
        WM_LBUTTONUP => on_button_up(cursor()),
        WM_KEYDOWN => on_key(wparam.0 as u16),
        WM_FRAME if wparam.0 <= CLOSE_FRAMES => on_frame(hwnd, wparam.0),
        WM_TIMER if wparam.0 == FRAME_TIMER => on_frame(hwnd, OPEN_FRAMES),
        WM_TIMER if wparam.0 == SUB_TIMER => on_sub_timer(hwnd),
        WM_ACTIVATE if wparam.0 & 0xFFFF == 0 => fade_out(),
        WM_CAPTURECHANGED if HWND(lparam.0 as *mut c_void) != hwnd => {
            if with(|st| st.levels.first().is_some_and(|l| l.hwnd == hwnd)) == Some(true) {
                fade_out();
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}
