
use crate::{
    config::Action,
    text::{Text, needs_shaping, normalize_display},
    theme::{Pal, ThemeMode},
    toolbar::rrect,
};
use anyhow::Result;
use std::{borrow::Cow, num::NonZeroU32, sync::Arc, time::Instant};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, PixmapPaint, Point, RadialGradient, Rect,
    SpreadMode, Stroke, Transform,
};
use unicode_segmentation::UnicodeSegmentation;
use winit::{
    dpi::LogicalSize,
    event_loop::ActiveEventLoop,
    keyboard::ModifiersState,
    platform::windows::WindowAttributesExtWindows,
    window::{Window, WindowId},
};

const W: f32 = 420.0;
const MAX_ROWS: usize = 10;
const H: f32 = CARD_Y + MAX_ROWS as f32 * ROW_H + 48.0;
const TITLE_H: f32 = 40.0;
const TABS_Y: f32 = 46.0;
const TAB_H: f32 = 26.0;
const TAB_STEP: f32 = 88.0;
const CARD_X: f32 = 16.0;
const CARD_W: f32 = W - 2.0 * CARD_X;
const CARD_Y: f32 = 84.0;
const ROW_H: f32 = 46.0;
const CARD_PAD: f32 = 16.0;
const TABS: [&str; 4] = ["General", "Output", "Recording", "Hotkeys"];
const RESTORE_ROW: (&str, &str) = ("Restore defaults", "Return this page to its original values");
const THEME_SEG_W: f32 = 50.0;
const FORMAT_SEG_W: f32 = 40.0;
const QUALITY_LABEL_W: f32 = 80.0;
const QUALITY_W: f32 = 206.0;

fn seg_labels(g: usize) -> &'static [&'static str] {
    match g {
        0 => &["System", "Dark", "Light"],
        1 => &["PNG", "JPG", "WebP"],
        3 => &["GIF", "WebP", "MP4"],
        4 => &["10", "15", "20", "30", "60"],
        5 => &["100%", "75%", "50%", "25%"],
        6 => &["Fast", "Balanced", "Best"],
        7 => &["Off", "Low", "Medium", "High"],
        8 => &["Hide", "Show", "Clicks"],
        9 => &["30 s", "1 min", "5 min", "10 min"],
        10 => &["Off", "3 s", "5 s", "10 s"],
        11 => &["Off", "Modern", "Classic"],
        _ => &[],
    }
}

fn seg_width(g: usize) -> f32 {
    match g {
        0 => THEME_SEG_W,
        1 => FORMAT_SEG_W,
        3 => 56.0,
        4 => 40.0,
        5 => 48.0,
        6 => 62.0,
        7 => 50.0,
        8 => 56.0,
        11 => 66.0,
        _ => 50.0,
    }
}

fn measure_segs(text: Option<&Text>) -> [f32; SEG_GROUPS] {
    std::array::from_fn(|g| {
        let Some(t) = text else { return seg_width(g) };
        let widest = seg_labels(g).iter().map(|l| t.advance(l, 11.5)).fold(0.0f32, f32::max);
        (widest + 20.0).ceil().max(40.0)
    })
}

pub(crate) fn rec_inert(hit: Hit, r: &crate::recfmt::Settings) -> bool {
    match hit {
        Hit::Seg(7, _) => r.mp4,
        Hit::RecDither => r.mp4 || r.webp,
        _ => false,
    }
}

fn seg_count(g: usize) -> usize {
    seg_labels(g).len()
}
const SEG_GROUPS: usize = 12;


pub(crate) const EASE_PAGE: Ease = Ease::Spring(1.0);
pub(crate) const EASE_PILL: Ease = Ease::Spring(0.85);
pub(crate) const EASE_SPRING: Ease = Ease::Spring(0.7);
pub(crate) const EASE_OUT: Ease = Ease::Bezier(0.0, 0.0, 0.58, 1.0);
pub(crate) const PAGE_FADE_MS: f32 = 260.0;
pub(crate) const PAGE_RISE_MS: f32 = 420.0;
pub(crate) const PAGE_RISE_PX: f32 = 13.0;
pub(crate) const PILL_MS: f32 = 420.0;
pub(crate) const KNOB_MS: f32 = 480.0;
pub(crate) const HOVER_MS: f32 = 140.0;
pub(crate) const DESC_MS: f32 = 260.0;
const SETTLE_MS: f32 = 760.0;
const EASE_SETTLE: Ease = Ease::Spring(1.0);
const FOLLOW_MS: f32 = 220.0;
const EASE_FOLLOW: Ease = Ease::Spring(1.0);
const MSG_MS: f32 = 360.0;
const MSG_RISE: f32 = 6.0;
const MSG_HOLD_MS: f32 = 3200.0;
const MSG_HOLD_ERR_MS: f32 = 6500.0;
const MSG_OUT_MS: f32 = 320.0;
const MSG_SWAP_MS: f32 = 160.0;
const MQ_SPEED: f32 = 34.0;
const MQ_HOLD: f32 = 1.0;
const MQ_FRAME_MS: u64 = 33;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Close,
    Tab(usize),
    Hotkey(Action),
    Autostart,
    AutoSave,
    RecDither,
    Nested,
    RememberColors,
    ChooseFolder,
    FileName,
    Seg(u8, usize),
    Quality,
    Lossless,
    ConfigDir,
    ConfigReset,
    SaveDirReset,
    RestoreDefaults,
}

#[derive(Clone, PartialEq)]
pub struct View {
    pub hotkeys: [String; crate::config::ACTION_COUNT],
    pub autostart: bool,
    pub auto_save: bool,
    pub nested: bool,
    pub shell_menu: u8,
    pub remember_colors: bool,
    pub save_dir: String,
    pub file_name: String,
    pub file_preview: String,
    pub theme: ThemeMode,
    pub format: usize,
    pub compression: usize,
    pub comp_enabled: bool,
    pub lossless: bool,
    pub lossless_enabled: bool,
    pub output_desc: [String; 3],
    pub config_dir: String,
    pub config_custom: bool,
    pub save_dir_custom: bool,
    pub dark: bool,
    pub rec: crate::recfmt::Settings,
}

#[derive(Clone, Copy)]
pub(crate) enum Ease {
    Spring(f32),
    Bezier(f32, f32, f32, f32),
}

static MOTION_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn refresh_motion() -> bool {
    use windows::{
        Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW},
        core::BOOL,
    };
    let mut on = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut BOOL as *mut std::ffi::c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    }
    .is_ok();
    let off = ok && !on.as_bool();
    MOTION_OFF.store(off, std::sync::atomic::Ordering::Relaxed);
    !off
}

pub(crate) fn animations_on() -> bool {
    !MOTION_OFF.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) struct DescFx<K> {
    seen: Vec<(K, String)>,
    fades: Vec<(K, String, Instant)>,
}

impl<K: Copy + PartialEq> DescFx<K> {
    pub(crate) fn new() -> Self {
        Self { seen: Vec::new(), fades: Vec::new() }
    }

    fn progress(t0: Instant, now: Instant) -> f32 {
        now.saturating_duration_since(t0).as_secs_f32() * 1000.0 / DESC_MS
    }

    pub(crate) fn update(&mut self, rows: &[(K, &str, bool)], now: Instant, reset: bool) {
        self.fades.retain(|f| Self::progress(f.2, now) < 1.0);
        if reset || !animations_on() {
            self.fades.clear();
            self.seen = rows.iter().map(|r| (r.0, r.1.to_string())).collect();
            return;
        }
        for (k, text, may) in rows {
            let Some(e) = self.seen.iter_mut().find(|e| e.0 == *k) else {
                self.seen.push((*k, text.to_string()));
                continue;
            };
            if e.1 == *text {
                continue;
            }
            let left = std::mem::replace(&mut e.1, text.to_string());
            let from = match self.fades.iter().position(|f| f.0 == *k) {
                Some(i) => {
                    let f = self.fades.remove(i);
                    if Self::progress(f.2, now) < 0.5 { f.1 } else { left }
                }
                None => left,
            };
            if *may && !from.is_empty() && !text.is_empty() && from != *text {
                self.fades.push((*k, from, now));
            }
        }
        self.seen.retain(|e| rows.iter().any(|r| r.0 == e.0));
        self.fades.retain(|f| rows.iter().any(|r| r.0 == f.0));
    }

    pub(crate) fn fading(&self, k: K, now: Instant) -> Option<(&str, f32)> {
        let f = self.fades.iter().find(|f| f.0 == k)?;
        let p = Self::progress(f.2, now);
        (p < 1.0).then_some((f.1.as_str(), p))
    }

    pub(crate) fn active(&self, now: Instant) -> bool {
        self.fades.iter().any(|f| Self::progress(f.2, now) < 1.0)
    }

    fn rows(&self, now: Instant) -> Vec<K> {
        self.fades.iter().filter(|f| Self::progress(f.2, now) < 1.0).map(|f| f.0).collect()
    }
}

pub(crate) fn fade_look(p: f32) -> ((f32, f32), (f32, f32)) {
    let smooth = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    let (gone, come) = (smooth(p / 0.7), smooth((p - 0.15) / 0.85));
    ((1.0 - gone, 0.0), (come, 2.0 * (1.0 - come)))
}

pub(crate) fn fade_rect(x: f32, y: f32, px: f32, room: f32, scale: f32) -> [i32; 4] {
    let pad = (3.0 * scale).ceil();
    let (left, top) = (x.floor() as i32, y.floor() as i32 - pad as i32);
    [left, top, left + room.ceil().max(1.0) as i32 + 2, top + (px * 1.9 + pad * 2.0).ceil().max(1.0) as i32]
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fade_text(t: &Text, pix: &mut Pixmap, from: &str, to: &str, p: f32, x: f32, y: f32, px: f32, room: f32, color: [u8; 3], scale: f32) {
    let r = fade_rect(x, y, px, room, scale);
    let pad = (3.0 * scale).ceil();
    let (old, new) = fade_look(p);
    for (text, (alpha, drift)) in [(from, old), (to, new)] {
        if alpha <= 0.0 || text.is_empty() {
            continue;
        }
        let Some(mut layer) = Pixmap::new((r[2] - r[0]) as u32, (r[3] - r[1]) as u32) else { return };
        t.draw(&mut layer, text, x - x.floor(), y - y.floor() + pad + drift * scale, px, color);
        pix.draw_pixmap(r[0], r[1], layer.as_ref(), &PixmapPaint { opacity: alpha.min(1.0), ..Default::default() }, Transform::identity(), None);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Tween {
    from: f32,
    to: f32,
    t0: Instant,
    vel: f32,
}

impl Tween {
    pub(crate) fn at(v: f32) -> Self {
        Self { from: v, to: v, t0: Instant::now(), vel: 0.0 }
    }

    pub(crate) fn value(&self, now: Instant, ms: f32, ease: Ease) -> f32 {
        if !animations_on() {
            return self.to;
        }
        self.at_progress(self.progress(now, ms), ms, ease)
    }

    pub(crate) fn done(&self, now: Instant, ms: f32) -> bool {
        !animations_on() || (self.from == self.to && self.vel == 0.0) || now.saturating_duration_since(self.t0).as_secs_f32() * 1000.0 >= ms
    }

    pub(crate) fn retarget(&mut self, to: f32, now: Instant, ms: f32, ease: Ease) {
        let t = self.progress(now, ms);
        let cur = self.value(now, ms, ease);
        let dt = t.min(1.0 / ms);
        self.vel = if !animations_on() || t >= 1.0 {
            0.0
        } else if dt > 0.0 {
            (cur - self.at_progress(t - dt, ms, ease)) / (dt * ms)
        } else {
            self.vel
        };
        self.from = cur;
        self.to = to;
        self.t0 = now;
    }

    fn progress(&self, now: Instant, ms: f32) -> f32 {
        (now.saturating_duration_since(self.t0).as_secs_f32() * 1000.0 / ms).clamp(0.0, 1.0)
    }

    fn at_progress(&self, t: f32, ms: f32, ease: Ease) -> f32 {
        let carried = match ease {
            Ease::Spring(zeta) => self.vel * ms * spring_kick(zeta, t),
            Ease::Bezier(..) => 0.0,
        };
        self.from + (self.to - self.from) * bezier(ease, t) + carried
    }
}

const SPRING_K: f32 = 8.0;

fn spring(zeta: f32, t: f32) -> f32 {
    const K: f32 = SPRING_K;
    if t <= 0.0 || t >= 1.0 {
        return t.clamp(0.0, 1.0);
    }
    if zeta >= 0.999 {
        let a = K * t;
        return 1.0 - (1.0 + a) * (-a).exp();
    }
    let wd = K / zeta * (1.0 - zeta * zeta).sqrt();
    1.0 - (-K * t).exp() * ((wd * t).cos() + K / wd * (wd * t).sin())
}

fn spring_kick(zeta: f32, t: f32) -> f32 {
    const K: f32 = SPRING_K;
    if t <= 0.0 || t >= 1.0 {
        return 0.0;
    }
    if zeta >= 0.999 {
        return t * (-K * t).exp();
    }
    let wd = K / zeta * (1.0 - zeta * zeta).sqrt();
    (-K * t).exp() * (wd * t).sin() / wd
}

pub(crate) fn bezier(ease: Ease, t: f32) -> f32 {
    let (x1, y1, x2, y2) = match ease {
        Ease::Spring(zeta) => return spring(zeta, t),
        Ease::Bezier(x1, y1, x2, y2) => (x1, y1, x2, y2),
    };
    if t <= 0.0 || t >= 1.0 {
        return t.clamp(0.0, 1.0);
    }
    let axis = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s) * (1.0 - s) + 3.0 * b * s * s * (1.0 - s) + s * s * s;
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if axis(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    axis(y1, y2, (lo + hi) / 2.0)
}

const PRESS_MS: f32 = 90.0;
const RELEASE_MS: f32 = 460.0;
const EASE_RELEASE: Ease = Ease::Spring(0.55);
const PRESS_HOLD_MS: f32 = 100.0;
const PRESS_MAX_MS: f32 = 1500.0;

fn press_phase(target: f32) -> (f32, Ease) {
    if target > 0.5 { (PRESS_MS, EASE_OUT) } else { (RELEASE_MS, EASE_RELEASE) }
}

pub(crate) struct Presses<K> {
    fx: Vec<(K, Tween, f32)>,
    down: Option<(K, Instant, Option<Instant>)>,
}

impl<K: Copy + PartialEq> Presses<K> {
    pub(crate) fn new() -> Self {
        Self { fx: Vec::new(), down: None }
    }

    pub(crate) fn press(&mut self, k: K, now: Instant) {
        self.down = Some((k, now, None));
    }

    pub(crate) fn clear(&mut self) {
        self.fx.clear();
        self.down = None;
    }

    pub(crate) fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.fx.retain(|e| keep(&e.0));
        if self.down.is_some_and(|(k, _, _)| !keep(&k)) {
            self.down = None;
        }
    }

    pub(crate) fn release(&mut self, now: Instant) {
        if let Some((_, _, up)) = self.down.as_mut()
            && up.is_none()
        {
            *up = Some(now);
        }
    }

    fn shown(&self, now: Instant, keep: Option<K>) -> Option<K> {
        let (k, t0, up) = self.down?;
        let held_ms = now.saturating_duration_since(t0).as_secs_f32() * 1000.0;
        let on = if up.is_some() { held_ms < PRESS_HOLD_MS } else { keep == Some(k) || held_ms < PRESS_MAX_MS };
        on.then_some(k)
    }

    pub(crate) fn step(&mut self, now: Instant, keep: Option<K>) -> bool {
        let want = self.shown(now, keep);
        if want.is_none() {
            self.down = None;
        }
        if let Some(k) = want.filter(|k| !self.fx.iter().any(|e| e.0 == *k)) {
            self.fx.push((k, Tween::at(0.0), 0.0));
        }
        let mut moving = false;
        self.fx.retain_mut(|e| {
            let target = if want == Some(e.0) { 1.0 } else { 0.0 };
            if (e.2 - target).abs() > f32::EPSILON {
                let (ms, ease) = press_phase(target);
                e.1.retarget(target, now, ms, ease);
                e.2 = target;
            }
            let done = e.1.done(now, press_phase(e.2).0);
            moving |= !done;
            !(done && target == 0.0)
        });
        moving || self.down.is_some_and(|(_, _, up)| up.is_some())
    }

    pub(crate) fn value(&self, k: &K, now: Instant) -> f32 {
        self.fx.iter().find(|e| e.0 == *k).map_or(0.0, |e| {
            let (ms, ease) = press_phase(e.2);
            e.1.value(now, ms, ease)
        })
    }
}

pub(crate) fn pressed_rect(r: [f32; 4], p: f32) -> [f32; 4] {
    let inset = (r[2].min(r[3]) * 0.07 * p).clamp(-r[3] * 0.03, r[3] * 0.1);
    [r[0] + inset, r[1] + inset, (r[2] - 2.0 * inset).max(1.0), (r[3] - 2.0 * inset).max(1.0)]
}

pub(crate) fn press_wash(pix: &mut Pixmap, r: [f32; 4], radius: f32, p: f32, dark: bool) {
    let a = (36.0 * p.clamp(0.0, 1.0)) as u8;
    if a == 0 {
        return;
    }
    let Some(path) = rrect(r[0], r[1], r[2], r[3], radius) else { return };
    let c = if dark { paint([255, 255, 255], a) } else { paint([10, 24, 64], a) };
    pix.fill_path(&path, &c, FillRule::Winding, Transform::identity(), None);
}

fn quality_name(en: f32, step: usize) -> (Option<usize>, f32) {
    if en >= 0.5 { (Some(step), (2.0 * en - 1.0).clamp(0.0, 1.0)) } else { (None, (1.0 - 2.0 * en).clamp(0.0, 1.0)) }
}

fn quality_of(pos: f32) -> u8 {
    let steps = crate::config::QUALITY_STEPS;
    steps[(pos.round() as usize).min(steps.len() - 1)].1
}

fn magnetic(pos: f32) -> f32 {
    let n = pos.round();
    let d = pos - n;
    let x = (2.0 * d.abs()).min(1.0);
    let pull = 1.0 - x * x * (3.0 - 2.0 * x);
    n + d * (1.0 - pull * pull)
}

struct Knob {
    pos: Tween,
    ms: f32,
    ease: Ease,
    rest: f32,
    goal: f32,
    held: bool,
}

impl Knob {
    fn at(v: f32) -> Self {
        Self { pos: Tween::at(v), ms: SETTLE_MS, ease: EASE_SETTLE, rest: v, goal: v, held: false }
    }

    fn value(&self, now: Instant) -> f32 {
        self.pos.value(now, self.ms, self.ease)
    }

    fn moving(&self, now: Instant) -> bool {
        !self.pos.done(now, self.ms)
    }

    fn go(&mut self, to: f32, now: Instant, ms: f32, ease: Ease) {
        self.pos.retarget(to, now, self.ms, self.ease);
        self.ms = ms;
        self.ease = ease;
    }

    fn frame(&mut self, now: Instant, ready: bool, rest: f32, pointer: Option<f32>) {
        if let Some(p) = pointer {
            let goal = magnetic(p);
            if !self.held || goal != self.goal {
                self.go(goal, now, FOLLOW_MS, EASE_FOLLOW);
            }
            self.goal = goal;
            self.held = true;
        } else if !ready {
            *self = Knob::at(rest);
        } else if self.held || self.rest != rest {
            self.go(rest, now, SETTLE_MS, EASE_SETTLE);
            self.held = false;
        }
        self.rest = rest;
    }
}

impl SettingsWindow {
    fn page_at_rest(&self, now: Instant, s: f32) -> bool {
        let (fade_in, rise) = self.pages.look(self.tab, now);
        fade_in >= 1.0 && (rise * s).round() == 0.0 && (0..TABS.len()).filter(|k| *k != self.tab).all(|k| self.pages.look(k, now).0 <= 0.0)
    }

    pub fn say(&mut self, text: &str, is_error: bool) {
        self.message = Some((text.to_string(), is_error));
        self.msg_seen = None;
        self.window.request_redraw();
    }

    pub fn message_fades_at(&mut self) -> Option<Instant> {
        let (_, is_err) = self.message.as_ref()?;
        if self.listening.is_some() || self.editing_name {
            return None;
        }
        let (_, t0) = self.msg_seen.as_ref()?;
        let at = msg_fade_start(*t0, if *is_err { MSG_HOLD_ERR_MS } else { MSG_HOLD_MS }, self.hint_end);
        let now = Instant::now();
        if overdue(at, now, &mut self.fade_woken) {
            self.window.request_redraw();
        }
        (at > now).then_some(at)
    }

    pub fn frame_wake(&mut self) -> Option<Instant> {
        frame_wake(&mut self.frames, &self.window)
    }

    pub fn frame_revive(&mut self) {
        if self.frames.take_due() {
            self.window.request_redraw();
        }
    }

    pub fn marquee_wake(&mut self) -> Option<Instant> {
        let at = self.mq_next?;
        if at > Instant::now() {
            return Some(at);
        }
        self.mq_next = None;
        self.window.request_redraw();
        None
    }
}

fn msg_fade_start(t0: Instant, hold_ms: f32, hint_end: Option<Instant>) -> Instant {
    let held = t0 + std::time::Duration::from_secs_f32(hold_ms / 1000.0);
    hint_end.map_or(held, |end| held.max(end))
}

fn msg_out(start: Instant, now: Instant, from: &mut Option<Instant>, motion: bool) -> f32 {
    if now < start {
        *from = None;
        return 0.0;
    }
    if !motion {
        return 1.0;
    }
    let from = *from.get_or_insert(now);
    (now.saturating_duration_since(from).as_secs_f32() * 1000.0 / MSG_OUT_MS).min(1.0)
}

fn retire_line(leaving: &mut Vec<(String, bool, f32, f32, Instant)>, shown: Option<(String, bool, f32, f32)>, now: Instant) {
    if let Some((m, is_err, opacity, y)) = shown {
        leaving.push((m, is_err, opacity, y, now));
    }
    leaving.retain(|l| swap_k(l.4, now) < 1.0);
}

fn swap_k(t0: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(t0).as_secs_f32() * 1000.0 / MSG_SWAP_MS
}

fn overdue(at: Instant, now: Instant, answered: &mut Option<Instant>) -> bool {
    if at > now || *answered == Some(at) {
        return false;
    }
    *answered = Some(at);
    true
}

#[derive(Clone, Copy)]
struct Pages {
    fade: [Tween; TABS.len()],
    rise: [Tween; TABS.len()],
}

impl Pages {
    fn new(tab: usize) -> Self {
        Self { fade: std::array::from_fn(|k| Tween::at(if k == tab { 1.0 } else { 0.0 })), rise: [Tween::at(0.0); TABS.len()] }
    }

    fn switch(&mut self, from: usize, to: usize, now: Instant) {
        self.fade[from].retarget(0.0, now, PAGE_FADE_MS, EASE_OUT);
        if self.fade[to].value(now, PAGE_FADE_MS, EASE_OUT) < 0.5 {
            self.rise[to] = Tween::at(PAGE_RISE_PX);
        }
        self.rise[to].retarget(0.0, now, PAGE_RISE_MS, EASE_PAGE);
        self.fade[to].retarget(1.0, now, PAGE_FADE_MS, EASE_OUT);
    }

    fn look(&self, k: usize, now: Instant) -> (f32, f32) {
        (self.fade[k].value(now, PAGE_FADE_MS, EASE_OUT).clamp(0.0, 1.0), self.rise[k].value(now, PAGE_RISE_MS, EASE_PAGE))
    }

    fn moving(&self, now: Instant) -> bool {
        self.fade.iter().any(|t| !t.done(now, PAGE_FADE_MS)) || self.rise.iter().any(|t| !t.done(now, PAGE_RISE_MS))
    }
}

struct HoverFades(Vec<(Hit, Tween)>);

impl HoverFades {
    fn moved(&mut self, from: Option<Hit>, to: Option<Hit>, now: Instant) {
        for (hit, goal) in [(from, 0.0), (to, 1.0)] {
            let Some(hit) = hit else { continue };
            match self.0.iter_mut().find(|(h, _)| *h == hit) {
                Some((_, t)) => t.retarget(goal, now, HOVER_MS, EASE_OUT),
                None if goal > 0.0 => {
                    let mut t = Tween::at(0.0);
                    t.retarget(goal, now, HOVER_MS, EASE_OUT);
                    self.0.push((hit, t));
                }
                None => {}
            }
        }
    }

    fn value(&self, hit: &Hit, now: Instant) -> f32 {
        self.0.iter().find(|(h, _)| h == hit).map_or(0.0, |(_, t)| t.value(now, HOVER_MS, EASE_OUT))
    }

    fn step(&mut self, now: Instant) -> bool {
        self.0.retain(|(_, t)| !t.done(now, HOVER_MS) || t.value(now, HOVER_MS, EASE_OUT) > 0.0);
        self.0.iter().any(|(_, t)| !t.done(now, HOVER_MS))
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Damage {
    Whole,
    Part([i32; 4]),
    Nothing,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiveId {
    Tabs,
    Close,
    Scroll(usize),
    Fade(usize),
    Control(Hit),
    Status,
}

#[derive(Clone)]
struct Live {
    id: LiveId,
    area: [i32; 4],
    sig: u64,
}

struct Line {
    msg: String,
    is_err: bool,
    opacity: f32,
    y: f32,
}

struct Scroll {
    row: usize,
    desc: String,
    x: f32,
    y: f32,
    room: f32,
    off: f32,
    over: f32,
    color: [u8; 3],
}

struct Fade {
    row: usize,
    from: String,
    to: String,
    p: f32,
    x: f32,
    y: f32,
    room: f32,
    color: [u8; 3],
}

struct Still {
    pix: Pixmap,
    tab: usize,
    scale: f32,
    dark: bool,
    view: View,
    listening: Option<Action>,
    scrolling: Vec<usize>,
    text: bool,
    semibold: bool,
}

impl Still {
    #[allow(clippy::too_many_arguments)]
    fn fits(&self, tab: usize, size: (u32, u32), scale: f32, dark: bool, view: &View, listening: Option<Action>, scrolling: &[usize], text: bool, semibold: bool) -> bool {
        (self.pix.width(), self.pix.height()) == size
            && self.tab == tab
            && self.scale == scale
            && self.dark == dark
            && self.listening == listening
            && self.scrolling == scrolling
            && self.text == text
            && self.semibold == semibold
            && self.view.alike(view, tab)
    }
}

impl View {
    fn alike(&self, other: &View, tab: usize) -> bool {
        if self == other {
            return true;
        }
        tab != 1 && self.file_preview != other.file_preview && {
            let mut same = self.clone();
            same.file_preview.clone_from(&other.file_preview);
            same == *other
        }
    }
}

#[derive(Clone, Copy)]
struct Sig(u64);

impl Sig {
    fn new(kind: u64) -> Self {
        Sig(0xcbf29ce484222325).u(kind)
    }

    fn u(self, v: u64) -> Self {
        let mut h = self.0;
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        Sig(h)
    }

    fn f(self, v: f32) -> Self {
        self.u(v.to_bits() as u64)
    }

    fn b(self, v: bool) -> Self {
        self.u(v as u64)
    }
}

fn is_empty(a: [i32; 4]) -> bool {
    a[2] <= a[0] || a[3] <= a[1]
}

fn cover(a: [i32; 4], b: [i32; 4]) -> [i32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn meets(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn restore(dst: &mut Pixmap, src: &Pixmap, r: [i32; 4]) {
    let w = dst.width() as usize;
    let (x0, x1) = (r[0].max(0) as usize, (r[2].max(0) as usize).min(w));
    if x0 >= x1 {
        return;
    }
    let rows = r[1].max(0) as usize..(r[3].max(0) as usize).min(dst.height() as usize);
    let from = src.data();
    let to = dst.data_mut();
    for y in rows {
        to[(y * w + x0) * 4..(y * w + x1) * 4].copy_from_slice(&from[(y * w + x0) * 4..(y * w + x1) * 4]);
    }
}

fn to_argb(dst: &mut [u32], pix: &Pixmap, r: [i32; 4]) {
    let w = pix.width() as usize;
    let (x0, x1) = (r[0].max(0) as usize, (r[2].max(0) as usize).min(w));
    if x0 >= x1 {
        return;
    }
    let rows = r[1].max(0) as usize..(r[3].max(0) as usize).min(pix.height() as usize);
    for y in rows {
        let src = &pix.data()[(y * w + x0) * 4..(y * w + x1) * 4];
        for (d, s) in dst[y * w + x0..y * w + x1].iter_mut().zip(src.as_chunks::<4>().0) {
            *d = (s[3] as u32) << 24 | (s[0] as u32) << 16 | (s[1] as u32) << 8 | s[2] as u32;
        }
    }
}

pub struct SettingsWindow {
    pub window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    pub tab: usize,
    pub hover: Option<Hit>,
    pub cursor: (f32, f32),
    pub listening: Option<Action>,
    pub editing_name: bool,
    pub draft: String,
    pub message: Option<(String, bool)>,
    msg_seen: Option<(String, Instant)>,
    msg_shown: Option<(String, bool, f32, f32)>,
    msg_leaving: Vec<(String, bool, f32, f32, Instant)>,
    msg_out_from: Option<Instant>,
    hint_end: Option<Instant>,
    fade_woken: Option<Instant>,
    pub mods: ModifiersState,
    choose_enabled: bool,
    acrylic: bool,
    pal: Pal,
    semibold: Option<Arc<Text>>,
    bg: Option<Pixmap>,
    canvas: Option<Pixmap>,
    layers: Option<(Pixmap, Pixmap)>,
    pub(crate) frames: FrameClock,
    pub(crate) opening: Option<Opening>,
    icon: Option<(u32, Pixmap)>,
    pill: Tween,
    pages: Pages,
    knobs: [Tween; 6],
    knob_state: [bool; 6],
    knobs_ready: bool,
    hover_seen: Option<Hit>,
    hover_fx: HoverFades,
    segs: [Tween; SEG_GROUPS],
    quality: Knob,
    quality_on: Tween,
    quality_usable: bool,
    quality_step: usize,
    seg_target: [usize; SEG_GROUPS],
    pub drag_quality: bool,
    mq_row: Option<usize>,
    mq_t0: Instant,
    mq_gap: std::cell::Cell<Option<f32>>,
    mq_next: Option<Instant>,
    seg_w: [f32; SEG_GROUPS],
    mq_cur: std::cell::Cell<Option<(usize, f32, f32)>>,
    mq_ret: Option<(usize, usize, f32, f32, Instant)>,
    desc_fx: DescFx<usize>,
    desc_tab: Option<usize>,
    desc_scale: f32,
    desc_view: Option<View>,
    still: Option<Still>,
    shown: Option<Vec<Live>>,
    repaint: std::cell::Cell<bool>,
}

pub(crate) fn enable_acrylic(window: &Window, dark: bool) -> bool {
    use windows::Win32::{
        Graphics::Dwm::{DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute},
        UI::Controls::MARGINS,
    };
    let Some(hwnd) = crate::hwnd_of(window) else { return false };
    set_dark_mode(window, dark);
    unsafe {
        let kind = DWMSBT_TRANSIENTWINDOW;
        let ok = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &kind as *const _ as *const std::ffi::c_void,
            size_of_val(&kind) as u32,
        )
        .is_ok();
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        ok && DwmExtendFrameIntoClientArea(hwnd, &m).is_ok()
    }
}

pub(crate) fn cloak(window: &Window, on: bool) {
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
    if let Some(hwnd) = crate::hwnd_of(window) {
        let v = windows::core::BOOL::from(on);
        unsafe {
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const _ as *const std::ffi::c_void, 4);
        }
    }
}

const NO_WAIT: std::time::Duration = std::time::Duration::from_millis(1);
const STAND_IN_FRAME: std::time::Duration = std::time::Duration::from_millis(8);

pub(crate) fn wait_frame(last: &mut Option<Instant>) {
    pace_frame(|| unsafe { windows::Win32::Graphics::Dwm::DwmFlush() }.is_ok(), last);
}

fn pace_frame(flush: impl FnOnce() -> bool, last: &mut Option<Instant>) {
    let ok = flush();
    if !flush_waited(ok, last.map(|t| t.elapsed())) {
        std::thread::sleep(STAND_IN_FRAME);
    }
    *last = Some(Instant::now());
}

fn flush_waited(ok: bool, since: Option<std::time::Duration>) -> bool {
    ok && since.is_none_or(|d| d >= NO_WAIT)
}

const MIN_FRAME: std::time::Duration = std::time::Duration::from_millis(8);

#[derive(Default)]
pub(crate) struct FrameClock {
    began: Option<Instant>,
    due: Option<Instant>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Wake {
    Idle,
    At(Instant),
    Now,
}

impl FrameClock {
    pub(crate) fn allow(&mut self) -> bool {
        self.admit(Instant::now(), in_modal_loop)
    }

    pub(crate) fn admit(&mut self, now: Instant, modal: impl FnOnce() -> bool) -> bool {
        match self.began {
            Some(began) if now < began + MIN_FRAME && !modal() => {
                self.due = Some(began + MIN_FRAME);
                false
            }
            _ => {
                self.began = Some(now);
                self.due = None;
                true
            }
        }
    }

    pub(crate) fn take_due(&mut self) -> bool {
        self.due.take().is_some()
    }

    pub(crate) fn wake(&mut self, now: Instant) -> Wake {
        match self.due {
            None => Wake::Idle,
            Some(at) if at > now => Wake::At(at),
            Some(_) => {
                self.due = None;
                Wake::Now
            }
        }
    }
}

fn window_exposed(hwnd: windows::Win32::Foundation::HWND) -> bool {
    unsafe { windows::Win32::Graphics::Gdi::GetUpdateRect(hwnd, None, false) }.as_bool()
}

fn in_modal_loop() -> bool {
    use windows::Win32::{
        System::Threading::GetCurrentThreadId,
        UI::WindowsAndMessaging::{GUI_INMENUMODE, GUI_INMOVESIZE, GUI_POPUPMENUMODE, GUI_SYSTEMMENUMODE, GUITHREADINFO, GetGUIThreadInfo},
    };
    let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    if unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut info) }.is_err() {
        return false;
    }
    info.flags.0 & (GUI_INMOVESIZE.0 | GUI_INMENUMODE.0 | GUI_SYSTEMMENUMODE.0 | GUI_POPUPMENUMODE.0) != 0
}

pub(crate) fn frame_wake(clock: &mut FrameClock, window: &Window) -> Option<Instant> {
    match clock.wake(Instant::now()) {
        Wake::Idle => None,
        Wake::At(at) => Some(at),
        Wake::Now => {
            window.request_redraw();
            None
        }
    }
}

const GLASS_IN: f32 = 0.35;

pub(crate) fn opening_frame(base: &Pixmap, t: f32, s: f32) -> Option<Pixmap> {
    if t >= 1.0 {
        return None;
    }
    crate::menu::unfold(base, t, (0.0, 0.0), s)
}

pub(crate) struct Opening {
    window: Arc<Window>,
    began: Instant,
    was_layered: bool,
}

impl Opening {
    pub(crate) fn begin(window: Arc<Window>, mut paint: impl FnMut(f32)) -> Option<Self> {
        use windows::Win32::{
            Foundation::COLORREF,
            UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED},
        };
        refresh_motion();
        let hwnd = crate::hwnd_of(&window);
        if !animations_on() || hwnd.is_none() {
            paint(1.0);
            cloak(&window, false);
            window.request_redraw();
            return None;
        }
        let hwnd = hwnd.unwrap();
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA);
        }
        paint(0.0);
        cloak(&window, false);
        window.request_redraw();
        Some(Self { window, began: Instant::now(), was_layered: ex & WS_EX_LAYERED.0 as isize != 0 })
    }

    pub(crate) fn step(&self, mut paint: impl FnMut(f32)) -> bool {
        use windows::Win32::{Foundation::COLORREF, UI::WindowsAndMessaging::{LWA_ALPHA, SetLayeredWindowAttributes}};
        let k = if animations_on() { (self.began.elapsed().as_secs_f32() * 1000.0 / crate::menu::ANIM_MS).min(1.0) } else { 1.0 };
        paint(k);
        if let Some(hwnd) = crate::hwnd_of(&self.window) {
            let glass = bezier(crate::menu::EASE_FADE, (k / GLASS_IN).min(1.0));
            unsafe { let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), (glass * 255.0).round() as u8, LWA_ALPHA); }
        }
        if k < 1.0 {
            self.window.request_redraw();
        }
        k < 1.0
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        use windows::Win32::{
            Foundation::COLORREF,
            UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED},
        };
        if let Some(hwnd) = crate::hwnd_of(&self.window) {
            unsafe {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                if !self.was_layered {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize));
                }
            }
        }
        self.window.request_redraw();
    }
}

pub(crate) const WINDOW_CLOSE_MS: f32 = 320.0;

const CLOSE_RISE_PX: f32 = 10.0;

const EASE_CLOSE: Ease = Ease::Bezier(0.25, 0.15, 0.75, 0.85);

fn closing_ease(k: f32) -> f32 {
    bezier(EASE_CLOSE, k.clamp(0.0, 1.0))
}

pub(crate) fn closing_frame(base: &Pixmap, backdrop: Option<&Pixmap>, t: f32, s: f32) -> Option<Pixmap> {
    if t <= 0.0 {
        return None;
    }
    let (w, h) = (base.width() as usize, base.height() as usize);
    let rise = ((CLOSE_RISE_PX * s * closing_ease(t)).round() as usize).min(h);
    let mut out = Pixmap::new(base.width(), base.height())?;
    let row = w * 4;
    out.data_mut()[..(h - rise) * row].copy_from_slice(&base.data()[rise * row..]);
    if let Some(back) = backdrop.filter(|b| b.width() == base.width() && b.height() == base.height()) {
        out.data_mut()[(h - rise) * row..].copy_from_slice(&back.data()[(h - rise) * row..]);
    }
    Some(out)
}

pub(crate) fn closing_alpha(k: f32) -> u8 {
    ((1.0 - closing_ease(k)) * 255.0).round() as u8
}

pub(crate) fn dismiss(window: &Window, mut paint: impl FnMut(f32)) {
    use windows::Win32::{
        Foundation::COLORREF,
        UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED},
    };
    refresh_motion();
    let Some(hwnd) = crate::hwnd_of(window) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        let mut frame = None;
        let t0 = Instant::now();
        loop {
            let k = if animations_on() { (t0.elapsed().as_secs_f32() * 1000.0 / WINDOW_CLOSE_MS).min(1.0) } else { 1.0 };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), closing_alpha(k), LWA_ALPHA);
            if k >= 1.0 {
                break;
            }
            paint(k);
            wait_frame(&mut frame);
        }
    }
    cloak(window, true);
}

pub(crate) fn set_dark_mode(window: &Window, dark: bool) {
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    if let Some(hwnd) = crate::hwnd_of(window) {
        let d = windows::core::BOOL::from(dark);
        unsafe {
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const _ as *const std::ffi::c_void, 4);
        }
    }
}

pub(crate) fn strip_caption_buttons(window: &Window) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SetWindowLongPtrW, SetWindowPos, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    };
    if let Some(hwnd) = crate::hwnd_of(window) {
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let new = style & !(WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0);
            if new != style {
                SetWindowLongPtrW(hwnd, GWL_STYLE, new as isize);
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
        }
    }
}

pub(crate) fn center_on_cursor_monitor(el: &ActiveEventLoop, window: &Window) {
    use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let mut cursor = POINT::default();
    let have_cursor = unsafe { GetCursorPos(&mut cursor) }.is_ok();
    let under_cursor = el.available_monitors().find(|m| {
        let (p, s) = (m.position(), m.size());
        have_cursor
            && cursor.x >= p.x
            && cursor.x < p.x + s.width as i32
            && cursor.y >= p.y
            && cursor.y < p.y + s.height as i32
    });
    let Some(mon) = under_cursor.or_else(|| el.primary_monitor()) else { return };
    let (p, s, w) = (mon.position(), mon.size(), window.outer_size());
    window.set_outer_position(winit::dpi::PhysicalPosition::new(
        p.x + (s.width as i32 - w.width as i32) / 2,
        p.y + (s.height as i32 - w.height as i32) / 2,
    ));
}

pub(crate) fn round_corners(window: &Window) {
    corner_preference(window, false);
}

pub(crate) fn square_corners(window: &Window) {
    corner_preference(window, true);
}

fn corner_preference(window: &Window, square: bool) {
    use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute};
    if let Some(hwnd) = crate::hwnd_of(window) {
        let pref = if square { DWMWCP_DONOTROUND } else { DWMWCP_ROUND };
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &pref as *const _ as *const std::ffi::c_void,
                size_of_val(&pref) as u32,
            );
        }
    }
}

pub(crate) fn paint(c: [u8; 3], a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], a);
    p.anti_alias = true;
    p
}

pub(crate) fn paint4(c: [u8; 4]) -> Paint<'static> {
    paint([c[0], c[1], c[2]], c[3])
}

pub(crate) fn gradient_paint(from: Point, to: Point, stops: &[(f32, [u8; 4])]) -> Paint<'static> {
    let mut p = Paint { anti_alias: true, ..Default::default() };
    let stops = stops
        .iter()
        .map(|(t, c)| GradientStop::new(*t, Color::from_rgba8(c[0], c[1], c[2], c[3])))
        .collect();
    if let Some(sh) = LinearGradient::new(from, to, stops, SpreadMode::Pad, Transform::identity()) {
        p.shader = sh;
    }
    p
}

pub(crate) fn accent_paint(r: [f32; 4]) -> Paint<'static> {
    accent_paint_a(r, 255)
}

pub(crate) fn accent_paint_a(r: [f32; 4], alpha: u8) -> Paint<'static> {
    let (a, b) = crate::theme::accent(crate::theme::ui_dark());
    gradient_paint(
        Point::from_xy(r[0], r[1]),
        Point::from_xy(r[0] + r[2], r[1] + r[3]),
        &[(0.0, [a[0], a[1], a[2], alpha]), (1.0, [b[0], b[1], b[2], alpha])],
    )
}

pub(crate) fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

pub(crate) fn fit_text(t: &Text, s: &str, px: f32, max_w: f32) -> String {
    if t.measure(s, px) <= max_w {
        return s.to_string();
    }
    let gs: Vec<&str> = s.graphemes(true).collect();
    let mut width = t.advance("…", px);
    let mut end = 0;
    while end < gs.len() {
        let g = t.advance(gs[end], px);
        if width + g > max_w {
            break;
        }
        width += g;
        end += 1;
    }
    format!("{}…", gs[..end].concat())
}

pub(crate) fn fit_text_left(t: &Text, s: &str, px: f32, max_w: f32) -> String {
    if t.measure(s, px) <= max_w {
        return s.to_string();
    }
    let gs: Vec<&str> = s.graphemes(true).collect();
    let mut width = t.advance("…", px);
    let mut start = gs.len();
    while start > 0 {
        let g = t.advance(gs[start - 1], px);
        if width + g > max_w {
            break;
        }
        width += g;
        start -= 1;
    }
    format!("…{}", gs[start..].concat())
}

#[allow(clippy::too_many_arguments)]
fn draw_line(t: &Text, pix: &mut Pixmap, s: &str, x: f32, y: f32, px: f32, room: f32, keep_tail: bool, color: [u8; 3]) -> f32 {
    let s = normalize_display(s);
    let s = if keep_tail { Cow::Owned(fit_text_left(t, &s, px, room)) } else { s };
    if let Some(end) = t.draw_system_alpha(pix, &s, x, y, px, room, color, 1.0) {
        return end;
    }
    let shown = if keep_tail { s.into_owned() } else { fit_text(t, &s, px, room) };
    t.draw(pix, &shown, x, y, px, color);
    t.measure(&shown, px)
}

pub(crate) fn render_background(w: u32, h: u32, acrylic: bool, pal: &Pal) -> Option<Pixmap> {
    let mut pix = Pixmap::new(w, h)?;
    let (wf, hf) = (w as f32, h as f32);
    let id = Transform::identity();
    let full = Rect::from_xywh(0.0, 0.0, wf, hf)?;
    let blob = |pix: &mut Pixmap, cx: f32, cy: f32, r: f32, c: [u8; 3], a: u8| {
        let center = Point::from_xy(cx * wf, cy * hf);
        let mut p = Paint { anti_alias: true, ..Default::default() };
        if let Some(sh) = RadialGradient::new(
            center,
            0.0,
            center,
            r * wf,
            vec![
                GradientStop::new(0.0, Color::from_rgba8(c[0], c[1], c[2], a)),
                GradientStop::new(1.0, Color::from_rgba8(c[0], c[1], c[2], 0)),
            ],
            SpreadMode::Pad,
            id,
        ) {
            p.shader = sh;
            pix.fill_rect(full, &p, id, None);
        }
    };
    if acrylic {
        pix.fill_rect(full, &paint4(pal.tint), id, None);
        let k = if pal.dark { 1.0 } else { 0.8 };
        blob(&mut pix, 0.10, 0.04, 0.70, [25, 184, 166], (34.0 * k) as u8);
        blob(&mut pix, 1.00, 0.50, 0.80, [47, 91, 224], (38.0 * k) as u8);
        return Some(pix);
    }
    let stops: [(f32, [u8; 4]); 3] = if pal.dark {
        [(0.0, [17, 26, 42, 255]), (0.5, [11, 15, 25, 255]), (1.0, [21, 16, 48, 255])]
    } else {
        [(0.0, [245, 249, 254, 255]), (0.5, [230, 238, 249, 255]), (1.0, [237, 233, 251, 255])]
    };
    pix.fill_rect(full, &gradient_paint(Point::from_xy(0.0, 0.0), Point::from_xy(wf, hf), &stops), id, None);
    let k = if pal.dark { 1.0 } else { 0.7 };
    blob(&mut pix, 0.10, 0.04, 0.70, [25, 184, 166], (80.0 * k) as u8);
    blob(&mut pix, 1.00, 0.50, 0.80, [47, 91, 224], (92.0 * k) as u8);
    blob(&mut pix, 0.15, 1.05, 0.80, [123, 92, 240], (80.0 * k) as u8);
    Some(pix)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn glass_card(pix: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32, pal: &Pal) {
    let id = Transform::identity();
    let Some(body) = crate::toolbar::frame(x, y, w, h, r) else { return };
    pix.fill_path(&body, &paint4(pal.card), FillRule::Winding, id, None);
    let sheen = gradient_paint(
        Point::from_xy(0.0, y),
        Point::from_xy(0.0, y + h * 0.45),
        &[(0.0, [255, 255, 255, if pal.dark { 26 } else { 70 }]), (1.0, [255, 255, 255, 0])],
    );
    pix.fill_path(&body, &sheen, FillRule::Winding, id, None);
    let edge = gradient_paint(
        Point::from_xy(0.0, y),
        Point::from_xy(0.0, y + h),
        &[(0.0, pal.edge[0]), (0.5, pal.edge[1]), (1.0, pal.edge[2])],
    );
    pix.stroke_path(&body, &edge, &Stroke { width: 1.0_f32.max(s * 0.75), ..Default::default() }, id, None);
    if !pal.dark {
        pix.stroke_path(&body, &paint([20, 32, 64], 22), &Stroke { width: 1.0, ..Default::default() }, id, None);
    }
}

pub(crate) fn glass_chip(pix: &mut Pixmap, r: [f32; 4], hover: f32, edge: bool, s: f32, pal: &Pal) {
    let id = Transform::identity();
    let Some(p) = rrect(r[0], r[1], r[2], r[3], r[3] / 2.0) else { return };
    let lift = if pal.dark { (20.0 * hover) as u8 } else { 0 };
    let fill = gradient_paint(
        Point::from_xy(0.0, r[1]),
        Point::from_xy(0.0, r[1] + r[3]),
        &[
            (0.0, [pal.chip[0][0], pal.chip[0][1], pal.chip[0][2], pal.chip[0][3].saturating_add(lift)]),
            (1.0, [pal.chip[1][0], pal.chip[1][1], pal.chip[1][2], pal.chip[1][3].saturating_add(lift)]),
        ],
    );
    pix.fill_path(&p, &fill, FillRule::Winding, id, None);
    if hover > 0.0 && !pal.dark {
        pix.fill_path(&p, &paint([255, 255, 255], (pal.lift[3] as f32 * 0.7 * hover) as u8), FillRule::Winding, id, None);
    }
    let (border, width) = if edge {
        (accent_paint(r), 1.5 * s)
    } else if pal.dark {
        (paint4(pal.chip_border), 1.0_f32.max(s * 0.75))
    } else {
        (paint([20, 32, 64], 40), 1.0_f32.max(s * 0.75))
    };
    pix.stroke_path(&p, &border, &Stroke { width, ..Default::default() }, id, None);
}

pub struct Rects {
    items: [(Hit, [f32; 4]); 40],
    len: usize,
}

impl Rects {
    fn push(&mut self, h: Hit, r: [f32; 4]) {
        if let Some(slot) = self.items.get_mut(self.len) {
            *slot = (h, r);
            self.len += 1;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(Hit, [f32; 4])> {
        self.items[..self.len].iter()
    }
}

impl SettingsWindow {
    pub fn new(el: &ActiveEventLoop, dark: bool, tab: usize) -> Result<Self> {
        let tab = tab.min(TABS.len() - 1);
        let window = Arc::new(el.create_window(
            crate::app_window_attributes()
                .with_title("Snptu settings")
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
        let semibold = Text::load_semibold();
        Ok(Self {
            window,
            surface,
            tab,
            hover: None,
            cursor: (0.0, 0.0),
            listening: None,
            editing_name: false,
            draft: String::new(),
            message: None,
            msg_seen: None,
            msg_shown: None,
            msg_leaving: Vec::new(),
            msg_out_from: None,
            hint_end: None,
            fade_woken: None,
            mods: ModifiersState::empty(),
            choose_enabled: false,
            acrylic,
            pal: Pal::new(dark),
            seg_w: measure_segs(semibold.as_deref()),
            semibold,
            bg: None,
            canvas: None,
            layers: None,
            frames: FrameClock::default(),
            opening: None,
            icon: None,
            pill: Tween::at(tab as f32),
            pages: Pages::new(tab),
            knobs: [Tween::at(0.0); 6],
            knob_state: [false; 6],
            knobs_ready: false,
            hover_seen: None,
            hover_fx: HoverFades(Vec::new()),
            segs: [Tween::at(0.0); SEG_GROUPS],
            quality: Knob::at(0.0),
            quality_on: Tween::at(0.0),
            quality_usable: false,
            quality_step: 1,
            seg_target: [0; SEG_GROUPS],
            drag_quality: false,
            mq_row: None,
            mq_t0: Instant::now(),
            mq_gap: std::cell::Cell::new(None),
            mq_next: None,
            mq_cur: std::cell::Cell::new(None),
            mq_ret: None,
            desc_fx: DescFx::new(),
            desc_tab: None,
            desc_scale: 0.0,
            desc_view: None,
            still: None,
            shown: None,
            repaint: std::cell::Cell::new(true),
        })
    }

    pub fn show(&self) {
        self.repaint.set(true);
        self.window.set_visible(true);
        strip_caption_buttons(&self.window);
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn set_tab(&mut self, i: usize) {
        let i = i.min(TABS.len() - 1);
        if i == self.tab {
            return;
        }
        let now = Instant::now();
        self.begin_return();
        self.pages.switch(self.tab, i, now);
        self.pill.retarget(i as f32, now, PILL_MS, EASE_PILL);
        self.tab = i;
        self.mq_row = None;
        self.track_row();
        self.window.request_redraw();
    }

    fn hv(&self, hit: &Hit, now: Instant) -> f32 {
        self.hover_fx.value(hit, now)
    }

    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    fn row_top(i: usize) -> f32 {
        CARD_Y + i as f32 * ROW_H
    }

    fn row_count(tab: usize) -> usize {
        match tab {
            0 => 6,
            1 => 7,
            2 => 10,
            _ => Action::ALL.len() + 1,
        }
    }

    pub fn rects(&self) -> Rects {
        self.rects_for(self.tab)
    }

    fn rects_for(&self, tab: usize) -> Rects {
        let s = self.scale();
        let right = CARD_X + CARD_W - CARD_PAD;
        let r = |x: f32, y: f32, w: f32, h: f32| [x * s, y * s, w * s, h * s];
        let mut out = Rects { items: [(Hit::Close, [0.0; 4]); 40], len: 0 };
        out.push(Hit::Close, r(W - 40.0, 6.0, 28.0, 28.0));
        for i in 0..TABS.len() {
            out.push(Hit::Tab(i), r(CARD_X + i as f32 * TAB_STEP, TABS_Y, 84.0, TAB_H));
        }
        match tab {
            0 => {
                out.push(Hit::Autostart, r(right - 34.0, Self::row_top(0) + 13.0, 34.0, 20.0));
                for i in 0..3 {
                    out.push(
                        Hit::Seg(0, i),
                        r(right - 3.0 * self.seg_w[0] + i as f32 * self.seg_w[0], Self::row_top(1) + 11.0, self.seg_w[0], 24.0),
                    );
                }
                out.push(Hit::ConfigDir, r(right - 68.0, Self::row_top(2) + 11.0, 68.0, 24.0));
                out.push(Hit::ConfigReset, r(right - 68.0 - 8.0 - 54.0, Self::row_top(2) + 11.0, 54.0, 24.0));
                out.push(Hit::Nested, r(right - 34.0, Self::row_top(3) + 13.0, 34.0, 20.0));
                for i in 0..3 {
                    out.push(Hit::Seg(11, i), r(right - 3.0 * self.seg_w[11] + i as f32 * self.seg_w[11], Self::row_top(4) + 11.0, self.seg_w[11], 24.0));
                }
                out.push(Hit::RestoreDefaults, r(right - 68.0, Self::row_top(5) + 11.0, 68.0, 24.0));
            }
            1 => {
                out.push(Hit::FileName, r(right - 196.0, Self::row_top(0) + 10.0, 196.0, 26.0));
                out.push(Hit::AutoSave, r(right - 34.0, Self::row_top(1) + 13.0, 34.0, 20.0));
                for i in 0..seg_count(1) {
                    out.push(Hit::Seg(1, i), r(right - seg_count(1) as f32 * self.seg_w[1] + i as f32 * self.seg_w[1], Self::row_top(2) + 11.0, self.seg_w[1], 24.0));
                }
                out.push(Hit::Quality, r(right - QUALITY_W, Self::row_top(3) + 11.0, QUALITY_W, 24.0));
                out.push(Hit::Lossless, r(right - 34.0, Self::row_top(4) + 13.0, 34.0, 20.0));
                out.push(Hit::RememberColors, r(right - 34.0, Self::row_top(5) + 13.0, 34.0, 20.0));
                out.push(Hit::RestoreDefaults, r(right - 68.0, Self::row_top(6) + 11.0, 68.0, 24.0));
                if self.choose_enabled {
                    out.push(Hit::ChooseFolder, r(right - 34.0 - 10.0 - 68.0, Self::row_top(1) + 11.0, 68.0, 24.0));
                    out.push(Hit::SaveDirReset, r(right - 34.0 - 10.0 - 68.0 - 8.0 - 62.0, Self::row_top(1) + 11.0, 62.0, 24.0));
                }
            }
            2 => {
                for (row, g) in [(0usize, 3usize), (1, 4), (2, 5), (3, 6), (4, 7), (6, 8), (7, 9), (8, 10)] {
                    let sw = self.seg_w[g];
                    for i in 0..seg_count(g) {
                        out.push(Hit::Seg(g as u8, i), r(right - seg_count(g) as f32 * sw + i as f32 * sw, Self::row_top(row) + 11.0, sw, 24.0));
                    }
                }
                out.push(Hit::RecDither, r(right - 34.0, Self::row_top(5) + 13.0, 34.0, 20.0));
                out.push(Hit::RestoreDefaults, r(right - 68.0, Self::row_top(9) + 11.0, 68.0, 24.0));
            }
            _ => {
                for (i, a) in Action::ALL.iter().enumerate() {
                    out.push(Hit::Hotkey(*a), r(right - 190.0, Self::row_top(i) + 5.0, 190.0, 36.0));
                }
                out.push(Hit::RestoreDefaults, r(right - 68.0, Self::row_top(Action::ALL.len()) + 11.0, 68.0, 24.0));
            }
        }
        out
    }

    pub fn quality_pos_from_x(&self, x: f32) -> f32 {
        let s = self.scale();
        let last = (crate::config::QUALITY_STEPS.len() - 1) as f32;
        let Some(r) = self.rects_for(1).iter().find(|(h, _)| *h == Hit::Quality).map(|(_, r)| *r) else { return 1.0 };
        let (x0, x1) = (r[0] + QUALITY_LABEL_W * s, r[0] + r[2] - 8.0 * s);
        (((x - x0) / (x1 - x0)).clamp(0.0, 1.0) * last).clamp(0.0, last)
    }

    pub fn quality_at(&self, x: f32) -> u8 {
        quality_of(self.quality_pos_from_x(x))
    }

    pub fn on_quality_track(&self, x: f32) -> bool {
        let s = self.scale();
        self.rects_for(1).iter().find(|(h, _)| *h == Hit::Quality).is_some_and(|(_, r)| x >= r[0] + (QUALITY_LABEL_W - 8.0) * s)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.rects()
            .iter()
            .find(|(_, r)| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3])
            .map(|(h, _)| *h)
    }

    pub fn in_title_bar(&self, y: f32) -> bool {
        y < TITLE_H * self.scale() && self.hover != Some(Hit::Close)
    }

    pub fn track_row(&mut self) -> bool {
        let s = self.scale();
        let (x, y) = (self.cursor.0 / s, self.cursor.1 / s);
        let inside = (CARD_X..CARD_X + CARD_W).contains(&x) && y >= CARD_Y;
        let row = if inside { Some(((y - CARD_Y) / ROW_H) as usize).filter(|&r| r < Self::row_count(self.tab)) } else { None };
        if row == self.mq_row {
            return false;
        }
        self.begin_return();
        self.mq_row = row;
        let now = Instant::now();
        self.mq_t0 = now;
        if let Some((_, _, from, over, t0)) = self.mq_ret.filter(|r| Some(r.1) == row && r.0 == self.tab) {
            let left = Self::marquee_home_left(from, now.saturating_duration_since(t0).as_secs_f32(), s);
            if left > 0.0 {
                let t = Self::marquee_resume(over, left, s);
                self.mq_t0 = now.checked_sub(std::time::Duration::from_secs_f32(t)).unwrap_or(now);
            }
            self.mq_ret = None;
        }
        true
    }

    fn marquee_home_left(from: f32, secs: f32, s: f32) -> f32 {
        from - secs * MQ_SPEED * s
    }

    pub fn leave_rows(&mut self) -> bool {
        self.begin_return();
        self.mq_row.take().is_some()
    }

    fn begin_return(&mut self) {
        if let (Some(old), Some((row, off, over))) = (self.mq_row, self.mq_cur.get())
            && row == old
            && off > 0.5
            && animations_on()
        {
            self.mq_ret = Some((self.tab, old, off, over, Instant::now()));
        }
        self.mq_cur.set(None);
    }

    fn marquee_resume(over: f32, at: f32, s: f32) -> f32 {
        MQ_HOLD + at.clamp(0.0, over) / over * Self::marquee_glide(over, s)
    }

    fn marquee_glide(over: f32, s: f32) -> f32 {
        (over / (MQ_SPEED * s)).max(0.6)
    }

    fn marquee_offset(over: f32, t: f32, s: f32, motion: bool) -> f32 {
        const HOLD: f32 = MQ_HOLD;
        let glide = Self::marquee_glide(over, s);
        if t < HOLD || t >= 2.0 * (HOLD + glide) {
            0.0
        } else if !motion {
            if t < 2.0 * HOLD + glide { over } else { 0.0 }
        } else if t < HOLD + glide {
            over * (t - HOLD) / glide
        } else if t < 2.0 * HOLD + glide {
            over
        } else {
            over * (1.0 - (t - 2.0 * HOLD - glide) / glide)
        }
    }

    fn marquee_rest_left(over: f32, t: f32, s: f32, motion: bool) -> Option<f32> {
        const HOLD: f32 = MQ_HOLD;
        let glide = Self::marquee_glide(over, s);
        let end = if motion { 2.0 * (HOLD + glide) } else { 2.0 * HOLD + glide };
        if t >= end {
            None
        } else if t < HOLD {
            Some(HOLD - t)
        } else if !motion || (t >= HOLD + glide && t < 2.0 * HOLD + glide) {
            Some(2.0 * HOLD + glide - t)
        } else {
            Some(0.0)
        }
    }

    fn edge_alpha(c: f32, room: f32, offset: f32, over: f32, fade: f32) -> f32 {
        let ramp = |d: f32, len: f32| if len > 0.0 { (d / len).clamp(0.0, 1.0) } else { 1.0 };
        ramp(c, fade.min(offset.max(0.0))).min(ramp(room - c, fade.min((over - offset).max(0.0))))
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_scrolled(t: &Text, pix: &mut Pixmap, s_: &str, x: f32, y: f32, px: f32, room: f32, offset: f32, over: f32, color: [u8; 3], scale: f32) {
        let pad = 3.0 * scale;
        let (w, h) = (room.ceil() as u32, (px * 1.9 + pad * 2.0).ceil() as u32);
        let Some(mut layer) = Pixmap::new(w.max(1), h.max(1)) else { return };
        t.draw(&mut layer, s_, -offset, pad, px, color);
        let fade = 16.0 * scale;
        let data = layer.data_mut();
        for col in 0..w as usize {
            let k = Self::edge_alpha(col as f32 + 0.5, room, offset, over, fade);
            if k < 1.0 {
                for row in 0..h as usize {
                    let i = (row * w as usize + col) * 4;
                    for b in &mut data[i..i + 4] {
                        *b = (*b as f32 * k) as u8;
                    }
                }
            }
        }
        pix.draw_pixmap(x.round() as i32, (y - pad).round() as i32, layer.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    }

    fn rows_text(&self, tab: usize, view: &View, t: &Text, s: f32) -> Vec<(String, String)> {
        let folder_desc = if view.auto_save { view.save_dir.clone() } else { "Asks you where to save each time".to_string() };
        let restore = || (RESTORE_ROW.0.to_string(), RESTORE_ROW.1.to_string());
        match tab {
            0 => vec![
                ("Start with Windows".into(), if view.autostart { "Launches quietly at sign-in" } else { "Doesn't launch at sign-in" }.into()),
                ("Appearance".into(), "Dark, light, or follow Windows".into()),
                ("Settings folder".into(), fit_text_left(t, &view.config_dir, 11.0 * s, (CARD_W - 2.0 * CARD_PAD - 150.0) * s)),
                ("Nested capture".into(), if view.nested { "A second shortcut can snip the capture screen itself" } else { "The capture screen itself can't be snipped" }.into()),
                (
                    "Explorer menu".into(),
                    match view.shell_menu {
                        crate::config::SHELL_OFF => "Not in the right-click menu",
                        crate::config::SHELL_MODERN => "New menu, and Show more options",
                        _ => "Only under Show more options",
                    }
                    .into(),
                ),
                restore(),
            ],
            1 => vec![
                ("File name".into(), view.file_preview.clone()),
                ("Save folder".into(), folder_desc),
                ("Image format".into(), view.output_desc[0].clone()),
                ("Compression".into(), view.output_desc[1].clone()),
                ("Lossless".into(), view.output_desc[2].clone()),
                ("Annotation colours".into(), if view.remember_colors { "Each tool keeps its last colour between captures" } else { "Each capture starts from the original colours" }.into()),
                restore(),
            ],
            2 => {
                let r = &view.rec;
                vec![
                    ("Format".into(), if r.mp4 { "A video (H.264): smooth and small, plays everywhere".into() } else if r.webp { "Full colour, smaller and sharper".into() } else { "Plays everywhere".into() }),
                    ("Frame rate".into(), "Pictures per second".into()),
                    ("Size".into(), "Scale of the result".into()),
                    ("Quality".into(), ["Lightest on the CPU", "Best for most screens", "Sharpest, needs a fast PC"][r.quality.min(2) as usize].into()),
                    ("Noise filter".into(), if r.mp4 { "GIF and WebP only".into() } else { "Ignores tiny changes".into() }),
                    ("Dithering".into(), if r.webp || r.mp4 { "GIF only".into() } else if r.dither { "Smoother gradients in GIFs".into() } else { "Gradients in GIFs may show bands".into() }),
                    ("Mouse pointer".into(), ["Not recorded", "Recorded", "Recorded, clicks lit"][r.pointer.min(2) as usize].into()),
                    ("Maximum length".into(), "Stops by itself".into()),
                    ("Start delay".into(), "Countdown before recording".into()),
                    restore(),
                ]
            }
            _ => Action::ALL
                .iter()
                .map(|a| (a.title().to_string(), a.desc().to_string()))
                .chain([restore()])
                .collect(),
        }
    }

    fn row_inert(tab: usize, i: usize, view: &View) -> bool {
        tab == 2 && (i == 4 && view.rec.mp4 || i == 5 && (view.rec.mp4 || view.rec.webp))
    }

    fn hotkey_width(&self, st: &Text, a: Action, view: &View, s: f32) -> f32 {
        let chips = self.hotkey_chips(st, a, view, s);
        chips.iter().map(|c| c.1).sum::<f32>() + 4.0 * s * chips.len().saturating_sub(1) as f32
    }

    fn hotkey_chips<'v>(&self, st: &Text, a: Action, view: &'v View, s: f32) -> Vec<(&'v str, f32)> {
        let px = 11.5 * s;
        if self.listening == Some(a) {
            vec![("Press keys\u{2026}", st.measure("Press keys\u{2026}", px) + 20.0 * s)]
        } else if view.hotkeys[a.index()].is_empty() {
            vec![("Not set", st.measure("Not set", px) + 20.0 * s)]
        } else {
            view.hotkeys[a.index()].split(" + ").map(|k| (k, st.measure(k, px) + 16.0 * s)).collect()
        }
    }

    fn control_state(&self, tab: usize, hit: &Hit, view: &View, now: Instant) -> (bool, bool, f32) {
        let inert = (tab == 2 && rec_inert(*hit, &view.rec)) || (*hit == Hit::Lossless && !view.lossless_enabled);
        let hovered = self.hover == Some(*hit) && !inert;
        let hv = if inert { 0.0 } else { self.hv(hit, now) };
        (inert, hovered, hv)
    }

    fn desc_room(&self, tab: usize, i: usize, view: &View, semibold: Option<&Text>, s: f32) -> (f32, bool) {
        let x = (CARD_X + CARD_PAD) * s;
        let is_path = tab == 1 && i == 1 && view.auto_save;
        let (row_top, row_bottom) = (Self::row_top(i) * s, Self::row_top(i + 1) * s);
        let ctl_left = self
            .rects_for(tab)
            .iter()
            .filter(|(h, r)| !matches!(h, Hit::Close | Hit::Tab(_)) && r[1] + r[3] / 2.0 >= row_top && r[1] + r[3] / 2.0 < row_bottom)
            .map(|(h, r)| match (h, semibold) {
                (Hit::Hotkey(a), Some(st)) => r[0] + r[2] - self.hotkey_width(st, *a, view, s),
                _ => r[0],
            })
            .fold(f32::MAX, f32::min);
        let room = if ctl_left == f32::MAX { (CARD_W - 2.0 * CARD_PAD) * s } else { ctl_left - x - 12.0 * s };
        (room, is_path)
    }

    fn draw_still(&self, pix: &mut Pixmap, tab: usize, view: &View, text: Option<&Text>, scrolling: &[usize]) {
        let (s, id, pal) = (self.scale(), Transform::identity(), &self.pal);
        let semibold = self.semibold.as_deref().or(text);
        let rows = Self::row_count(tab);
        glass_card(pix, CARD_X * s, CARD_Y * s, CARD_W * s, rows as f32 * ROW_H * s, 14.0 * s, s, pal);
        for i in 1..rows {
            if let Some(r) = Rect::from_xywh((CARD_X + CARD_PAD) * s, Self::row_top(i) * s, (CARD_W - 2.0 * CARD_PAD) * s, 1.0) {
                pix.fill_rect(r, &paint4(pal.sep), id, None);
            }
        }

        if let Some(t) = text {
            for (i, (title, desc)) in self.rows_text(tab, view, t, s).iter().enumerate() {
                let top = Self::row_top(i);
                let x = (CARD_X + CARD_PAD) * s;
                let inert = Self::row_inert(tab, i, view);
                let (title_c, desc_c) = if inert { (mix(pal.dim, pal.switch_off, 0.2), mix(pal.dim, pal.switch_off, 0.45)) } else { (pal.text, pal.dim) };
                if let Some(st) = semibold {
                    st.draw(pix, title, x, (top + 6.0) * s, 13.0 * s, title_c);
                }
                if scrolling.contains(&i) {
                    continue;
                }
                let (room, is_path) = self.desc_room(tab, i, view, semibold, s);
                draw_line(t, pix, desc, x, (top + 25.0) * s, 11.0 * s, room, is_path, desc_c);
            }
        }
    }

    fn scrolls(&self, tab: usize, view: &View, text: Option<&Text>, now: Instant) -> Vec<Scroll> {
        let Some(t) = text else { return Vec::new() };
        let mut rows: Vec<usize> = Vec::new();
        if tab == self.tab {
            rows.extend(self.mq_row);
        }
        if let Some((page, row, ..)) = self.mq_ret
            && page == tab
            && !rows.contains(&row)
        {
            rows.push(row);
        }
        let fading = self.fade_rows(tab, now);
        rows.retain(|r| !fading.contains(r));
        if rows.is_empty() {
            return Vec::new();
        }
        let (s, pal) = (self.scale(), &self.pal);
        let semibold = self.semibold.as_deref().or(text);
        let texts = self.rows_text(tab, view, t, s);
        let mut out = Vec::new();
        for i in rows {
            let Some((_, desc)) = texts.get(i) else { continue };
            let desc = normalize_display(desc);
            if needs_shaping(&desc) {
                continue;
            }
            let (room, is_path) = self.desc_room(tab, i, view, semibold, s);
            let over = if is_path { 0.0 } else { t.advance(&desc, 11.0 * s) - room };
            if let Some(off) = self.scroll_offset(tab, i, over, now, s) {
                let color = if Self::row_inert(tab, i, view) { mix(pal.dim, pal.switch_off, 0.45) } else { pal.dim };
                out.push(Scroll { row: i, desc: desc.into_owned(), x: (CARD_X + CARD_PAD) * s, y: (Self::row_top(i) + 25.0) * s, room, off, over, color });
            }
        }
        out.sort_by_key(|c| c.row);
        out
    }

    fn fade_rows(&self, tab: usize, now: Instant) -> Vec<usize> {
        if self.desc_tab == Some(tab) { self.desc_fx.rows(now) } else { Vec::new() }
    }

    fn update_descs(&mut self, view: &View, text: Option<&Text>, now: Instant) {
        let Some(t) = text else { return };
        let s = self.scale();
        let tab = self.tab;
        let reset = self.desc_tab != Some(tab) || self.desc_scale != s;
        if !reset && self.desc_view.as_ref().is_some_and(|v| v.alike(view, tab)) {
            return;
        }
        self.desc_tab = Some(tab);
        self.desc_scale = s;
        self.desc_view = Some(view.clone());
        let texts = self.rows_text(tab, view, t, s);
        let rows: Vec<(usize, &str, bool)> = texts.iter().enumerate().map(|(i, (_, d))| (i, d.as_str(), !(tab == 1 && i == 0))).collect();
        self.desc_fx.update(&rows, now, reset);
    }

    fn fades(&self, view: &View, text: Option<&Text>, now: Instant) -> Vec<Fade> {
        let rows = self.fade_rows(self.tab, now);
        let Some(t) = text.filter(|_| !rows.is_empty()) else { return Vec::new() };
        let (s, pal) = (self.scale(), &self.pal);
        let semibold = self.semibold.as_deref().or(text);
        let texts = self.rows_text(self.tab, view, t, s);
        let mut out = Vec::new();
        for row in rows {
            let (Some((_, to)), Some((from, p))) = (texts.get(row), self.desc_fx.fading(row, now)) else { continue };
            let (from, to) = (normalize_display(from), normalize_display(to));
            if needs_shaping(&from) || needs_shaping(&to) {
                continue;
            }
            let (room, is_path) = self.desc_room(self.tab, row, view, semibold, s);
            let fit = |d: &str| if is_path { fit_text_left(t, d, 11.0 * s, room) } else { fit_text(t, d, 11.0 * s, room) };
            let color = if Self::row_inert(self.tab, row, view) { mix(pal.dim, pal.switch_off, 0.45) } else { pal.dim };
            out.push(Fade { row, from: fit(&from), to: fit(&to), p, x: (CARD_X + CARD_PAD) * s, y: (Self::row_top(row) + 25.0) * s, room, color });
        }
        out.sort_by_key(|c| c.row);
        out
    }

    fn scroll_offset(&self, tab: usize, i: usize, over: f32, now: Instant, s: f32) -> Option<f32> {
        if !(over > 0.5 && (tab == self.tab || self.mq_ret.is_some_and(|r| r.0 == tab))) {
            return None;
        }
        let hovered = tab == self.tab && self.mq_row == Some(i);
        let (off, going, rest) = if hovered {
            let t = now.saturating_duration_since(self.mq_t0).as_secs_f32();
            let motion = animations_on();
            let off = Self::marquee_offset(over, t, s, motion);
            self.mq_cur.set(Some((i, off, over)));
            (off, true, Self::marquee_rest_left(over, t, s, motion))
        } else if let Some((_, _, from, _, t0)) = self.mq_ret.filter(|r| r.0 == tab && r.1 == i) {
            let left = Self::marquee_home_left(from, now.saturating_duration_since(t0).as_secs_f32(), s);
            (left.max(0.0), left > 0.0, Some(0.0))
        } else {
            (0.0, false, None)
        };
        if !going {
            return None;
        }
        if let Some(rest) = rest {
            self.mq_gap.set(Some(self.mq_gap.get().map_or(rest, |g| g.min(rest))));
        }
        Some(off)
    }

    fn draw_scroll(&self, pix: &mut Pixmap, t: &Text, c: &Scroll) {
        let s = self.scale();
        Self::draw_scrolled(t, pix, &c.desc, c.x, c.y, 11.0 * s, c.room, c.off, c.over, c.color, s);
    }

    fn draw_page(&self, pix: &mut Pixmap, tab: usize, view: &View, text: Option<&Text>, now: Instant) {
        let scrolls = self.scrolls(tab, view, text, now);
        let scrolling: Vec<usize> = scrolls.iter().map(|c| c.row).collect();
        self.draw_still(pix, tab, view, text, &scrolling);
        if let Some(t) = text {
            for c in &scrolls {
                self.draw_scroll(pix, t, c);
            }
        }
        self.draw_controls(pix, tab, view, text, now, None);
    }

    fn draw_controls(&self, pix: &mut Pixmap, tab: usize, view: &View, text: Option<&Text>, now: Instant, only: Option<LiveId>) {
        let (s, id, pal) = (self.scale(), Transform::identity(), &self.pal);
        let semibold = self.semibold.as_deref().or(text);
        for (hit, r) in self.rects_for(tab).iter() {
            if only.is_some_and(|o| o != Self::live_of(*hit)) {
                continue;
            }
            let (inert, hovered, hv) = self.control_state(tab, hit, view, now);
            let r = *r;
            match *hit {
                Hit::Close | Hit::Tab(_) => {}
                Hit::Autostart | Hit::AutoSave | Hit::RememberColors | Hit::Nested | Hit::Lossless | Hit::RecDither => {
                    let idx = Self::knob_index(*hit);
                    let v = self.knobs[idx].value(now, KNOB_MS, EASE_SPRING);
                    let fill_t = v.clamp(0.0, 1.0);
                    if let Some(p) = rrect(r[0], r[1], r[2], r[3], r[3] / 2.0) {
                        pix.fill_path(&p, &paint(pal.switch_off, if inert { 110 } else if hovered { 255 } else { 230 }), FillRule::Winding, id, None);
                        if fill_t > 0.0 {
                            pix.fill_path(&p, &accent_paint_a(r, (fill_t * 255.0) as u8), FillRule::Winding, id, None);
                        }
                    }
                    let knob = r[3] / 2.0 - 2.5 * s;
                    let (x_off, x_on) = (r[0] + r[3] / 2.0, r[0] + r[2] - r[3] / 2.0);
                    let (ky, kx) = (r[1] + r[3] / 2.0, x_off + (x_on - x_off) * v);
                    if hv > 0.0
                        && let Some(p) = rrect(r[0], r[1], r[2], r[3], r[3] / 2.0)
                    {
                        pix.stroke_path(&p, &accent_paint_a(r, (150.0 * hv) as u8), &Stroke { width: 1.5 * s, ..Default::default() }, id, None);
                    }
                    if let Some(p) = PathBuilder::from_circle(kx, ky + 0.8 * s, knob) {
                        pix.fill_path(&p, &paint([0, 6, 20], if inert { 0 } else { 55 }), FillRule::Winding, id, None);
                    }
                    if let Some(p) = PathBuilder::from_circle(kx, ky, knob) {
                        pix.fill_path(&p, &paint([255, 255, 255], if inert { 150 } else { 255 }), FillRule::Winding, id, None);
                    }
                }
                Hit::Seg(g, i) => {
                    let Some(t) = semibold else { continue };
                    let g = (g as usize).min(SEG_GROUPS - 1);
                    let (count, sw) = (seg_count(g), self.seg_w[g]);
                    let label = seg_labels(g).get(i).copied().unwrap_or_default();
                    let live = !inert;
                    let segv = self.segs[g].value(now, PILL_MS, EASE_PILL);
                    if i == 0 {
                        let track = [r[0], r[1], sw * count as f32 * s, r[3]];
                        if let Some(p) = rrect(track[0], track[1], track[2], track[3], track[3] / 2.0) {
                            pix.fill_path(&p, &paint4(pal.field), FillRule::Winding, id, None);
                            let border = if pal.dark { paint([255, 255, 255], 30) } else { paint([20, 32, 64], 34) };
                            pix.stroke_path(&p, &border, &Stroke { width: 1.0_f32.max(s * 0.75), ..Default::default() }, id, None);
                        }
                        let hl = [r[0] + 2.0 * s + segv * sw * s, r[1] + 2.0 * s, sw * s - 4.0 * s, r[3] - 4.0 * s];
                        if let Some(p) = rrect(hl[0], hl[1], hl[2], hl[3], hl[3] / 2.0) {
                            pix.fill_path(&p, &if inert { accent_paint_a(hl, 70) } else { accent_paint(hl) }, FillRule::Winding, id, None);
                        }
                    }
                    let near = (1.0 - (segv - i as f32).abs()).clamp(0.0, 1.0);
                    let amount = if live { hv * (1.0 - near) } else { 0.0 };
                    if amount > 0.0 {
                        let seg = [r[0] + 2.0 * s, r[1] + 2.0 * s, r[2] - 4.0 * s, r[3] - 4.0 * s];
                        if let Some(p) = rrect(seg[0], seg[1], seg[2], seg[3], seg[3] / 2.0) {
                            pix.fill_path(&p, &paint([pal.lift[0], pal.lift[1], pal.lift[2]], (pal.lift[3] as f32 * amount) as u8), FillRule::Winding, id, None);
                        }
                    }
                    let idle = if live { mix(pal.dim, pal.text, amount) } else { mix(pal.dim, pal.switch_off, 0.35) };
                    let c = mix(idle, [255, 255, 255], if inert { near * 0.6 } else { near });
                    t.draw_centered(pix, label, r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 11.5 * s, c);
                }
                Hit::Quality => {
                    let en = self.quality_on.value(now, PILL_MS, EASE_OUT).clamp(0.0, 1.0);
                    let (x0, x1) = (r[0] + QUALITY_LABEL_W * s, r[0] + r[2] - 8.0 * s);
                    let cy = r[1] + r[3] / 2.0;
                    let steps = crate::config::QUALITY_STEPS;
                    let last = (steps.len() - 1) as f32;
                    let pos = self.quality.value(now).clamp(0.0, last);
                    let t01 = pos / last;
                    if let Some(t) = semibold {
                        let step = if en >= 1.0 { (pos.round() as usize).min(steps.len() - 1) } else { self.quality_step };
                        let (name, a) = quality_name(en, step);
                        let label = name.map_or("Lossless", |i| steps[i].0);
                        t.draw_centered_alpha(pix, label, r[0] + (QUALITY_LABEL_W - 12.0) * s / 2.0, cy, 11.5 * s, mix(pal.dim, pal.text, en), a);
                    }
                    let th = 4.0 * s;
                    if let Some(p) = rrect(x0, cy - th / 2.0, x1 - x0, th, th / 2.0) {
                        pix.fill_path(&p, &paint(pal.switch_off, 230), FillRule::Winding, id, None);
                    }
                    let fill = [x0, cy - th / 2.0, ((x1 - x0) * t01).max(th), th];
                    if let Some(p) = rrect(fill[0], fill[1], fill[2], fill[3], th / 2.0) {
                        if en < 1.0 {
                            pix.fill_path(&p, &paint(mix(pal.switch_off, pal.dim, 0.5), 220), FillRule::Winding, id, None);
                        }
                        if en > 0.0 {
                            pix.fill_path(&p, &accent_paint_a(fill, (255.0 * en) as u8), FillRule::Winding, id, None);
                        }
                    }
                    if en > 0.0 {
                        let alpha = (255.0 * en) as u8;
                        let kx = x0 + (x1 - x0) * t01;
                        if let Some(p) = PathBuilder::from_circle(kx, cy, (5.0 + 2.0 * en) * s) {
                            pix.fill_path(&p, &paint([255, 255, 255], alpha), FillRule::Winding, id, None);
                            let ring = if pal.dark { paint([255, 255, 255], (40.0 * en) as u8) } else { paint([20, 32, 64], (60.0 * en) as u8) };
                            pix.stroke_path(&p, &ring, &Stroke { width: 1.0_f32.max(s * 0.75), ..Default::default() }, id, None);
                        }
                    }
                }
                Hit::Hotkey(action) => {
                    let Some(t) = semibold else { continue };
                    let (ch, gap, px) = (22.0 * s, 4.0 * s, 11.5 * s);
                    let cy = r[1] + r[3] / 2.0;
                    let listening = self.listening == Some(action);
                    let color = if !listening && view.hotkeys[action.index()].is_empty() { pal.dim } else { pal.text };
                    let chips = self.hotkey_chips(t, action, view, s);
                    let mut x = r[0] + r[2] - chips.iter().map(|c| c.1).sum::<f32>() - gap * (chips.len().saturating_sub(1)) as f32;
                    for (k, w) in &chips {
                        let chip = [x, cy - ch / 2.0, *w, ch];
                        glass_chip(pix, chip, if listening { 1.0 } else { hv }, listening, s, pal);
                        t.draw_centered(pix, k, chip[0] + chip[2] / 2.0, cy, px, color);
                        x += w + gap;
                    }
                }
                Hit::FileName => {
                    if let Some(p) = rrect(r[0], r[1], r[2], r[3], 8.0 * s) {
                        pix.fill_path(&p, &paint4(if self.editing_name { pal.field_edit } else { pal.field }), FillRule::Winding, id, None);
                        let (border, width) = if self.editing_name {
                            (accent_paint(r), 1.5 * s)
                        } else {
                            let b = if pal.dark {
                                paint([255, 255, 255], if hovered { 70 } else { 36 })
                            } else {
                                paint([20, 32, 64], if hovered { 110 } else { 60 })
                            };
                            (b, 1.0_f32.max(s * 0.75))
                        };
                        pix.stroke_path(&p, &border, &Stroke { width, ..Default::default() }, id, None);
                    }
                    if let Some(t) = text {
                        let ty = r[1] + r[3] / 2.0 - t.line_height(12.0 * s) / 2.0;
                        let end = draw_line(t, pix, &view.file_name, r[0] + 9.0 * s, ty, 12.0 * s, r[2] - 20.0 * s, true, pal.text);
                        if self.editing_name {
                            let cx = (r[0] + 9.0 * s + end + 1.0 * s).round();
                            if let Some(c) = Rect::from_xywh(cx, ty + 1.0 * s, 1.5 * s, t.line_height(12.0 * s) - 2.0 * s) {
                                pix.fill_rect(c, &paint(pal.text, 255), id, None);
                            }
                        }
                    }
                }
                Hit::ConfigDir | Hit::ConfigReset => {
                    let live = *hit == Hit::ConfigDir || view.config_custom;
                    glass_chip(pix, r, if live { hv } else { 0.0 }, false, s, pal);
                    if let Some(t) = semibold {
                        let label = if *hit == Hit::ConfigDir { "Choose…" } else { "Reset" };
                        t.draw_centered(pix, label, r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 11.5 * s, if live { pal.text } else { mix(pal.dim, pal.switch_off, 0.35) });
                    }
                }
                Hit::RestoreDefaults => {
                    glass_chip(pix, r, hv, false, s, pal);
                    if let Some(t) = semibold {
                        t.draw_centered(pix, "Restore", r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 11.5 * s, pal.text);
                    }
                }
                Hit::SaveDirReset => {
                    let live = view.save_dir_custom;
                    glass_chip(pix, r, if live { hv } else { 0.0 }, false, s, pal);
                    if let Some(t) = semibold {
                        t.draw_centered(pix, "Default", r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 11.5 * s, if live { pal.text } else { mix(pal.dim, pal.switch_off, 0.35) });
                    }
                }
                Hit::ChooseFolder => {
                    glass_chip(pix, r, hv, false, s, pal);
                    if let Some(t) = semibold {
                        t.draw_centered(pix, "Choose…", r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 11.5 * s, pal.text);
                    }
                }
            }
        }
    }

    fn draw_title(&self, pix: &mut Pixmap, text: Option<&Text>) {
        let (s, id, pal) = (self.scale(), Transform::identity(), &self.pal);
        let semibold = self.semibold.as_deref().or(text);
        if let Some((_, icon)) = &self.icon {
            pix.draw_pixmap(
                (14.0 * s).round() as i32,
                ((TITLE_H - 16.0) / 2.0 * s).round() as i32,
                icon.as_ref(),
                &PixmapPaint::default(),
                id,
                None,
            );
        }
        if let Some(t) = semibold {
            t.draw(pix, "Settings", 38.0 * s, (TITLE_H / 2.0 - 8.5) * s, 12.5 * s, pal.dim);
        }
    }

    fn draw_tabs(&self, pix: &mut Pixmap, text: Option<&Text>, now: Instant) {
        let (s, id, pal) = (self.scale(), Transform::identity(), &self.pal);
        let semibold = self.semibold.as_deref().or(text);
        let pill_x = self.pill.value(now, PILL_MS, EASE_PILL);
        let pill_rect = [(CARD_X + pill_x * TAB_STEP) * s, TABS_Y * s, 84.0 * s, TAB_H * s];
        if let Some(p) = rrect(pill_rect[0], pill_rect[1], pill_rect[2], pill_rect[3], pill_rect[3] / 2.0) {
            pix.fill_path(&p, &accent_paint(pill_rect), FillRule::Winding, id, None);
        }
        for (hit, r) in self.rects().iter() {
            let Hit::Tab(i) = *hit else { continue };
            let hv = self.hv(hit, now);
            let r = *r;
            let near = (1.0 - (pill_x - i as f32).abs()).clamp(0.0, 1.0);
            let amount = hv * (1.0 - near);
            if amount > 0.0
                && let Some(p) = rrect(r[0], r[1], r[2], r[3], r[3] / 2.0)
            {
                pix.fill_path(&p, &paint([pal.lift[0], pal.lift[1], pal.lift[2]], (pal.lift[3] as f32 * amount) as u8), FillRule::Winding, id, None);
            }
            if let Some(t) = semibold {
                let c = mix(mix(pal.dim, pal.text, amount), [255, 255, 255], near);
                t.draw_centered(pix, TABS[i], r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 12.0 * s, c);
            }
        }
    }

    fn draw_close(&self, pix: &mut Pixmap, now: Instant) {
        let (s, id, pal) = (self.scale(), Transform::identity(), &self.pal);
        let r = [(W - 40.0) * s, 6.0 * s, 28.0 * s, 28.0 * s];
        let hv = self.hv(&Hit::Close, now);
        if hv > 0.0
            && let Some(p) = PathBuilder::from_circle(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, r[2] / 2.0)
        {
            pix.fill_path(&p, &paint([pal.lift[0], pal.lift[1], pal.lift[2]], (pal.lift[3] as f32 * hv) as u8), FillRule::Winding, id, None);
        }
        let (cx, cy, k) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 4.5 * s);
        let c = mix(pal.dim, pal.text, hv);
        let mut pb = PathBuilder::new();
        pb.move_to(cx - k, cy - k);
        pb.line_to(cx + k, cy + k);
        pb.move_to(cx + k, cy - k);
        pb.line_to(cx - k, cy + k);
        if let Some(p) = pb.finish() {
            let st = Stroke { width: 1.4 * s, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
            pix.stroke_path(&p, &paint(c, 255), &st, id, None);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_live(&self, pix: &mut Pixmap, id: LiveId, view: &View, text: Option<&Text>, now: Instant, scrolls: &[Scroll], fades: &[Fade], lines: &[Line]) {
        match id {
            LiveId::Tabs => self.draw_tabs(pix, text, now),
            LiveId::Close => self.draw_close(pix, now),
            LiveId::Scroll(row) => {
                if let (Some(t), Some(c)) = (text, scrolls.iter().find(|c| c.row == row)) {
                    self.draw_scroll(pix, t, c);
                }
            }
            LiveId::Fade(row) => {
                if let (Some(t), Some(c)) = (text, fades.iter().find(|c| c.row == row)) {
                    fade_text(t, pix, &c.from, &c.to, c.p, c.x, c.y, 11.0 * self.scale(), c.room, c.color, self.scale());
                }
            }
            LiveId::Control(_) => self.draw_controls(pix, self.tab, view, text, now, Some(id)),
            LiveId::Status => {
                if let Some(t) = text {
                    for l in lines {
                        self.draw_status(pix, t, &l.msg, l.is_err, l.opacity, l.y);
                    }
                }
            }
        }
    }

    fn live_of(hit: Hit) -> LiveId {
        match hit {
            Hit::Close => LiveId::Close,
            Hit::Tab(_) => LiveId::Tabs,
            Hit::Seg(g, _) => LiveId::Control(Hit::Seg(g, 0)),
            h => LiveId::Control(h),
        }
    }

    fn knob_index(hit: Hit) -> usize {
        match hit {
            Hit::Autostart => 0,
            Hit::AutoSave => 1,
            Hit::RememberColors => 2,
            Hit::Nested => 3,
            Hit::Lossless => 4,
            _ => 5,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lives(&self, view: &View, text: Option<&Text>, now: Instant, scrolls: &[Scroll], fades: &[Fade], lines: &[Line], size: (u32, u32)) -> Vec<Live> {
        let s = self.scale();
        let semibold = self.semibold.as_deref().or(text);
        let area = |x0: f32, y0: f32, x1: f32, y1: f32| -> [i32; 4] {
            [x0.floor().max(0.0) as i32, y0.floor().max(0.0) as i32, (x1.ceil().max(0.0) as i32).min(size.0 as i32), (y1.ceil().max(0.0) as i32).min(size.1 as i32)]
        };
        let around = |r: [f32; 4], m: f32| area(r[0] - m, r[1] - m, r[0] + r[2] + m, r[1] + r[3] + m);
        let mut out: Vec<Live> = Vec::new();

        let pill_x = self.pill.value(now, PILL_MS, EASE_PILL);
        let mut sig = Sig::new(1).f(pill_x);
        for i in 0..TABS.len() {
            sig = sig.f(self.hv(&Hit::Tab(i), now));
        }
        out.push(Live {
            id: LiveId::Tabs,
            area: area((CARD_X - 24.0) * s, (TABS_Y - 4.0) * s, (CARD_X + (TABS.len() - 1) as f32 * TAB_STEP + 84.0 + 24.0) * s, (TABS_Y + TAB_H + 4.0) * s),
            sig: sig.0,
        });
        out.push(Live {
            id: LiveId::Close,
            area: around([(W - 40.0) * s, 6.0 * s, 28.0 * s, 28.0 * s], 4.0 * s),
            sig: Sig::new(2).f(self.hv(&Hit::Close, now)).0,
        });

        for c in scrolls {
            let (px, pad) = (11.0 * s, 3.0 * s);
            out.push(Live {
                id: LiveId::Scroll(c.row),
                area: area(c.x.round(), (c.y - pad).round(), c.x.round() + c.room.ceil().max(1.0), (c.y - pad).round() + (px * 1.9 + pad * 2.0).ceil().max(1.0)),
                sig: Sig::new(3).u(c.row as u64).f(c.off).f(c.over).0,
            });
        }

        for c in fades {
            let r = fade_rect(c.x, c.y, 11.0 * s, c.room, s);
            let mut sig = Sig::new(11).u(c.row as u64).f(c.p);
            for b in c.from.bytes().chain([0]).chain(c.to.bytes()) {
                sig = sig.u(b as u64);
            }
            out.push(Live { id: LiveId::Fade(c.row), area: area(r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32), sig: sig.0 });
        }

        for (hit, r) in self.rects().iter() {
            if matches!(hit, Hit::Close | Hit::Tab(_)) {
                continue;
            }
            let (inert, hovered, hv) = self.control_state(self.tab, hit, view, now);
            let (a, sig) = match *hit {
                Hit::Autostart | Hit::AutoSave | Hit::RememberColors | Hit::Nested | Hit::Lossless | Hit::RecDither => {
                    let v = self.knobs[Self::knob_index(*hit)].value(now, KNOB_MS, EASE_SPRING);
                    (around(*r, 10.0 * s), Sig::new(4).f(v).f(hv).b(hovered).b(inert))
                }
                Hit::Seg(g, _) => {
                    let g = (g as usize).min(SEG_GROUPS - 1);
                    let segv = self.segs[g].value(now, PILL_MS, EASE_PILL);
                    (around(*r, 8.0 * s), Sig::new(5).f(segv).f(hv).b(inert))
                }
                Hit::Quality => {
                    let en = self.quality_on.value(now, PILL_MS, EASE_OUT);
                    (around(*r, 6.0 * s), Sig::new(6).f(en).f(self.quality.value(now)).u(self.quality_step as u64))
                }
                Hit::Hotkey(a) => {
                    let left = semibold.map_or(r[0], |st| (r[0] + r[2] - self.hotkey_width(st, a, view, s)).min(r[0]));
                    (around([left, r[1], r[0] + r[2] - left, r[3]], 6.0 * s), Sig::new(7).f(hv).b(self.listening == Some(a)))
                }
                Hit::FileName => (around(*r, 6.0 * s), Sig::new(8).b(self.editing_name).b(hovered)),
                _ => (around(*r, 6.0 * s), Sig::new(9).f(hv)),
            };
            let id = Self::live_of(*hit);
            match out.iter_mut().find(|l| l.id == id) {
                Some(l) => {
                    l.area = cover(l.area, a);
                    l.sig = Sig(l.sig).u(sig.0).0;
                }
                None => out.push(Live { id, area: a, sig: sig.0 }),
            }
        }

        let mut a = [0; 4];
        let mut sig = Sig::new(10);
        for l in lines {
            let (pad, px) = (3.0 * s, 11.5 * s);
            let top = (l.y * s - pad).round();
            let one = area((CARD_X * s).round(), top, (CARD_X * s).round() + ((W - 2.0 * CARD_X) * s).ceil(), top + (px * 1.9 + pad * 2.0).ceil());
            a = if is_empty(a) { one } else { cover(a, one) };
            sig = sig.u(l.msg.len() as u64).b(l.is_err).f(l.opacity).f(l.y);
            for b in l.msg.bytes() {
                sig = sig.u(b as u64);
            }
        }
        out.push(Live { id: LiveId::Status, area: a, sig: sig.0 });
        out
    }

    fn draw_rest(&mut self, pix: &mut Pixmap, view: &View, text: Option<&Text>, now: Instant, lines: &[Line]) -> Damage {
        let s = self.scale();
        let size = (pix.width(), pix.height());
        let scrolls = self.scrolls(self.tab, view, text, now);
        let fades = self.fades(view, text, now);
        let mut scrolling: Vec<usize> = scrolls.iter().map(|c| c.row).chain(fades.iter().map(|c| c.row)).collect();
        scrolling.sort_unstable();
        let lives = self.lives(view, text, now, &scrolls, &fades, lines, size);
        let fits = self.still.as_ref().is_some_and(|st| st.fits(self.tab, size, s, self.pal.dark, view, self.listening, &scrolling, text.is_some(), self.semibold.is_some()));
        if !fits {
            self.shown = None;
            let kept = self.still.take().map(|st| st.pix).or_else(|| self.layers.take().map(|(a, _)| a)).filter(|p| (p.width(), p.height()) == size);
            if let (Some(mut still), Some(bg)) = (kept.or_else(|| Pixmap::new(size.0, size.1)), self.bg.as_ref()) {
                still.data_mut().copy_from_slice(bg.data());
                self.draw_title(&mut still, text);
                self.draw_still(&mut still, self.tab, view, text, &scrolling);
                self.still = Some(Still {
                    pix: still,
                    tab: self.tab,
                    scale: s,
                    dark: self.pal.dark,
                    view: view.clone(),
                    listening: self.listening,
                    scrolling: scrolling.clone(),
                    text: text.is_some(),
                    semibold: self.semibold.is_some(),
                });
            }
        }
        let repaint = self.repaint.replace(false);
        let same = self.shown.as_ref().is_some_and(|old| old.len() == lives.len() && old.iter().zip(&lives).all(|(a, b)| a.id == b.id));
        let Some(still) = self.still.as_ref() else {
            if let Some(bg) = self.bg.as_ref() {
                pix.data_mut().copy_from_slice(bg.data());
            }
            self.draw_title(pix, text);
            self.draw_still(pix, self.tab, view, text, &scrolling);
            for l in &lives {
                self.draw_live(pix, l.id, view, text, now, &scrolls, &fades, lines);
            }
            self.shown = None;
            self.layers = None;
            return Damage::Whole;
        };
        let damage = if !fits || !same || repaint {
            pix.data_mut().copy_from_slice(still.pix.data());
            for l in &lives {
                self.draw_live(pix, l.id, view, text, now, &scrolls, &fades, lines);
            }
            Damage::Whole
        } else {
            let old = self.shown.as_deref().unwrap_or(&[]);
            let mut dirty: Option<[i32; 4]> = None;
            for (new, was) in lives.iter().zip(old) {
                if new.sig != was.sig || new.area != was.area {
                    for a in [new.area, was.area] {
                        if !is_empty(a) {
                            dirty = Some(dirty.map_or(a, |d| cover(d, a)));
                        }
                    }
                }
            }
            match dirty {
                None => Damage::Nothing,
                Some(mut d) => {
                    loop {
                        let before = d;
                        for l in &lives {
                            if meets(l.area, d) {
                                d = cover(d, l.area);
                            }
                        }
                        if d == before {
                            break;
                        }
                    }
                    restore(pix, &still.pix, d);
                    for l in lives.iter().filter(|l| meets(l.area, d)) {
                        self.draw_live(pix, l.id, view, text, now, &scrolls, &fades, lines);
                    }
                    Damage::Part(d)
                }
            }
        };
        self.shown = Some(lives);
        self.layers = None;
        damage
    }

    fn draw_switching(&mut self, pix: &mut Pixmap, view: &View, text: Option<&Text>, now: Instant, lines: &[Line]) -> bool {
        let s = self.scale();
        let id = Transform::identity();
        self.still = None;
        self.shown = None;
        if let Some(bg) = self.bg.as_ref() {
            pix.data_mut().copy_from_slice(bg.data());
        }
        self.draw_title(pix, text);
        self.draw_tabs(pix, text, now);
        self.draw_close(pix, now);
        let size = (pix.width(), pix.height());
        let size_ok = |l: &(Pixmap, Pixmap)| (l.0.width(), l.0.height()) == size;
        let layers = self.layers.take().filter(size_ok).or_else(|| Pixmap::new(size.0, size.1).zip(Pixmap::new(size.0, size.1)));
        let Some((mut page_new, mut page_old)) = layers else { return false };
        page_new.fill(Color::TRANSPARENT);
        self.draw_page(&mut page_new, self.tab, view, text, now);
        for from in (0..TABS.len()).filter(|k| *k != self.tab) {
            let (opacity, rise) = self.pages.look(from, now);
            if opacity > 0.0 {
                page_old.fill(Color::TRANSPARENT);
                self.draw_page(&mut page_old, from, view, text, now);
                pix.draw_pixmap(0, (rise * s).round() as i32, page_old.as_ref(), &PixmapPaint { opacity, ..Default::default() }, id, None);
            }
        }
        let (fade_in, rise) = self.pages.look(self.tab, now);
        pix.draw_pixmap(0, (rise * s).round() as i32, page_new.as_ref(), &PixmapPaint { opacity: fade_in, ..Default::default() }, id, None);
        self.layers = Some((page_new, page_old));
        if let Some(t) = text {
            for l in lines {
                self.draw_status(pix, t, &l.msg, l.is_err, l.opacity, l.y);
            }
        }
        true
    }

    fn status_lines(&mut self, text: Option<&Text>, now: Instant, animating: &mut bool) -> Vec<Line> {
        let mut lines = Vec::new();
        let fresh = match (&self.message, &self.msg_seen) {
            (Some((m, _)), Some((seen, _))) => m != seen,
            _ => true,
        };
        let shown = if fresh { self.msg_shown.take().filter(|_| animations_on()) } else { None };
        retire_line(&mut self.msg_leaving, shown, now);
        if text.is_some() {
            for (m, is_err, opacity, y, t0) in &self.msg_leaving {
                let o = bezier(EASE_OUT, swap_k(*t0, now));
                lines.push(Line { msg: m.clone(), is_err: *is_err, opacity: opacity * (1.0 - o), y: y + o * MSG_RISE });
                *animating = true;
            }
        }

        let hint = self.listening.is_some() || self.editing_name;
        if hint {
            self.hint_end = None;
        } else if self.hint_end.is_none() {
            self.hint_end = Some(now);
        }
        if let (Some((msg, is_err)), Some(_)) = (self.message.clone(), text) {
            let since = match &self.msg_seen {
                Some((m, t0)) if *m == msg => *t0,
                _ => {
                    self.msg_seen = Some((msg.clone(), now));
                    self.msg_out_from = None;
                    now
                }
            };
            let age = now.saturating_duration_since(since).as_secs_f32() * 1000.0;
            let k = if animations_on() { (age / MSG_MS).min(1.0) } else { 1.0 };
            let e = bezier(EASE_PAGE, k);
            let start = msg_fade_start(since, if is_err { MSG_HOLD_ERR_MS } else { MSG_HOLD_MS }, self.hint_end);
            let out = if hint {
                self.msg_out_from = None;
                0.0
            } else {
                msg_out(start, now, &mut self.msg_out_from, animations_on())
            };
            if out >= 1.0 {
                self.message = None;
                self.msg_seen = None;
                self.msg_shown = None;
                self.msg_out_from = None;
            } else {
                let o = bezier(EASE_OUT, out);
                let (opacity, y) = (e * (1.0 - o), CARD_Y + Self::row_count(self.tab) as f32 * ROW_H + 14.0 + (1.0 - e) * MSG_RISE + o * MSG_RISE);
                lines.push(Line { msg: msg.clone(), is_err, opacity, y });
                self.msg_shown = Some((msg, is_err, opacity, y));
                *animating |= k < 1.0 || out > 0.0 || (!hint && now >= start);
            }
        } else {
            self.msg_seen = None;
        }
        lines
    }

    pub(crate) fn present_opening(&mut self, t: f32) {
        let Some(pix) = self.canvas.take() else { return };
        let frame = opening_frame(&pix, t, self.scale());
        self.present(frame.as_ref().unwrap_or(&pix), Damage::Whole);
        self.canvas = Some(pix);
    }

    pub(crate) fn present_closing(&mut self, t: f32) {
        let Some(pix) = self.canvas.take() else { return };
        if let Some(frame) = closing_frame(&pix, self.bg.as_ref(), t, self.scale()) {
            self.present(&frame, Damage::Whole);
        }
        self.canvas = Some(pix);
    }

    fn present(&mut self, pix: &Pixmap, damage: Damage) {
        let hwnd = crate::hwnd_of(&self.window);
        let exposed = || hwnd.is_some_and(window_exposed);
        let sent = if exposed() { Damage::Whole } else { damage };
        let (Some(w), Some(h)) = (NonZeroU32::new(pix.width()), NonZeroU32::new(pix.height())) else {
            return;
        };
        let mut done = false;
        if self.surface.resize(w, h).is_ok()
            && let Ok(mut buf) = self.surface.buffer_mut()
        {
            let whole = [0, 0, pix.width() as i32, pix.height() as i32];
            done = match sent {
                Damage::Part(a) if buf.age() == 1 => {
                    to_argb(&mut buf, pix, a);
                    if exposed() {
                        to_argb(&mut buf, pix, whole);
                        buf.present().is_ok()
                    } else {
                        let rect = softbuffer::Rect { x: a[0] as u32, y: a[1] as u32, width: NonZeroU32::new((a[2] - a[0]) as u32).unwrap_or(NonZeroU32::MIN), height: NonZeroU32::new((a[3] - a[1]) as u32).unwrap_or(NonZeroU32::MIN) };
                        buf.present_with_damage(&[rect]).is_ok()
                    }
                }
                Damage::Nothing if buf.age() == 1 => {
                    if exposed() {
                        to_argb(&mut buf, pix, whole);
                        buf.present().is_ok()
                    } else {
                        buf.present_with_damage(&[]).is_ok()
                    }
                }
                _ => {
                    to_argb(&mut buf, pix, whole);
                    buf.present().is_ok()
                }
            };
        }
        if !done {
            self.repaint.set(true);
        }
    }

    pub(crate) fn note_exposed(&self) {
        if crate::hwnd_of(&self.window).is_some_and(window_exposed) {
            self.repaint.set(true);
        }
    }

    pub fn draw(&mut self, view: &View, text: Option<&Text>) {
        if self.draw_opening() {
            return;
        }
        self.draw_at(view, text, Instant::now());
    }

    pub(crate) fn begin_opening(&mut self) {
        self.finish_opening();
        self.opening = Opening::begin(self.window.clone(), |t| self.present_opening(t));
    }

    pub(crate) fn finish_opening(&mut self) {
        self.opening = None;
        self.repaint.set(true);
    }

    fn draw_opening(&mut self) -> bool {
        let Some(opening) = self.opening.take() else { return false };
        let size = self.window.inner_size();
        if self.canvas.as_ref().is_none_or(|p| p.width() != size.width || p.height() != size.height) {
            self.repaint.set(true);
            return false;
        }
        if opening.step(|t| self.present_opening(t)) {
            self.opening = Some(opening);
        } else {
            self.repaint.set(true);
        }
        true
    }

    fn draw_at(&mut self, view: &View, text: Option<&Text>, now: Instant) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let s = self.scale();
        let mut animating = false;
        self.choose_enabled = view.auto_save;

        if self.pal.dark != view.dark {
            self.pal = Pal::new(view.dark);
            self.bg = None;
            set_dark_mode(&self.window, view.dark);
        }
        let pal = self.pal;
        crate::theme::set_ui_dark(pal.dark);

        if self.hover != self.hover_seen {
            self.hover_fx.moved(self.hover_seen, self.hover, now);
            self.hover_seen = self.hover;
        }
        animating |= self.hover_fx.step(now);

        let states = [view.autostart, view.auto_save, view.remember_colors, view.nested, view.lossless, view.rec.dither && !view.rec.webp && !view.rec.mp4];
        for (i, on) in states.into_iter().enumerate() {
            let target = if on { 1.0 } else { 0.0 };
            if !self.knobs_ready {
                self.knobs[i] = Tween::at(target);
            } else if self.knob_state[i] != on {
                self.knobs[i].retarget(target, now, KNOB_MS, EASE_SPRING);
            }
            self.knob_state[i] = on;
        }
        let r = &view.rec;
        let wants = [
            (0, ThemeMode::ALL.iter().position(|m| *m == view.theme).unwrap_or(0)),
            (1, view.format),
            (3, if r.mp4 { 2 } else { r.webp as usize }),
            (4, crate::recfmt::FPS.iter().position(|v| *v == r.fps).unwrap_or(2)),
            (5, crate::recfmt::SCALE.iter().position(|v| *v == r.scale).unwrap_or(0)),
            (6, r.quality.min(2) as usize),
            (7, r.filter.min(3) as usize),
            (8, r.pointer.min(2) as usize),
            (9, crate::recfmt::MAX_SECS.iter().position(|v| *v == r.max_secs).unwrap_or(2)),
            (10, crate::recfmt::COUNTDOWN.iter().position(|v| *v == r.countdown).unwrap_or(1)),
            (11, view.shell_menu.min(2) as usize),
        ];
        for (g, want) in wants {
            if !self.knobs_ready {
                self.segs[g] = Tween::at(want as f32);
            } else if self.seg_target[g] != want {
                self.segs[g].retarget(want as f32, now, PILL_MS, EASE_PILL);
            }
            self.seg_target[g] = want;
            animating |= !self.segs[g].done(now, PILL_MS);
        }
        let last = (crate::config::QUALITY_STEPS.len() - 1) as f32;
        let want = if view.comp_enabled { view.compression.min(last as usize) as f32 } else { last };
        let dragging = self.drag_quality && view.comp_enabled;
        let pointer = dragging.then(|| self.quality_pos_from_x(self.cursor.0));
        self.quality.frame(now, self.knobs_ready, want, pointer);
        animating |= self.quality.moving(now);
        let usable = view.comp_enabled;
        if !self.knobs_ready {
            self.quality_on = Tween::at(if usable { 1.0 } else { 0.0 });
        } else if self.quality_usable != usable {
            self.quality_on.retarget(if usable { 1.0 } else { 0.0 }, now, PILL_MS, EASE_OUT);
        }
        self.quality_usable = usable;
        if usable {
            self.quality_step = view.compression.min(last as usize);
        }
        animating |= !self.quality_on.done(now, PILL_MS);
        self.knobs_ready = true;

        self.update_descs(view, text, now);
        animating |= self.desc_fx.active(now);

        let stale = |p: &Option<Pixmap>| p.as_ref().is_none_or(|p| p.width() != size.width || p.height() != size.height);
        if stale(&self.bg) {
            self.bg = render_background(size.width, size.height, self.acrylic, &pal);
        }
        if stale(&self.canvas) {
            self.canvas = Pixmap::new(size.width, size.height);
            self.shown = None;
        }
        let icon_px = (16.0 * s).round() as u32;
        if self.icon.as_ref().is_none_or(|(px, _)| *px != icon_px) {
            self.icon = Some((icon_px, crate::icon::render(icon_px)));
        }
        if self.bg.is_none() {
            return;
        }
        let Some(mut pix) = self.canvas.take() else { return };

        animating |= !self.pill.done(now, PILL_MS);
        animating |= self.pages.moving(now);
        for i in 0..6 {
            animating |= !self.knobs[i].done(now, KNOB_MS);
        }
        self.mq_gap.set(None);
        self.mq_next = None;
        let lines = self.status_lines(text, now, &mut animating);

        let damage = if self.page_at_rest(now, s) {
            self.draw_rest(&mut pix, view, text, now, &lines)
        } else if self.draw_switching(&mut pix, view, text, now, &lines) {
            Damage::Whole
        } else {
            self.canvas = Some(pix);
            return;
        };
        self.present(&pix, damage);
        self.canvas = Some(pix);
        if animating {
            self.window.request_redraw();
        } else if let Some(rest) = self.mq_gap.get() {
            let wait = ((rest * 1000.0) as u64).max(MQ_FRAME_MS);
            self.mq_next = Some(Instant::now() + std::time::Duration::from_millis(wait));
        }
    }

    fn draw_status(&self, pix: &mut Pixmap, t: &Text, msg: &str, is_err: bool, opacity: f32, y: f32) {
        let (s, pal) = (self.scale(), &self.pal);
        let c = if is_err { pal.danger } else { pal.dim };
        let (px, pad) = (11.5 * s, 3.0 * s);
        if let Some(mut layer) = Pixmap::new(((W - 2.0 * CARD_X) * s).ceil() as u32, (px * 1.9 + pad * 2.0).ceil() as u32) {
            draw_line(t, &mut layer, msg, CARD_PAD * s, pad, px, (CARD_W - 2.0 * CARD_PAD) * s, false, c);
            let paint = PixmapPaint { opacity, ..Default::default() };
            pix.draw_pixmap((CARD_X * s).round() as i32, (y * s - pad).round() as i32, layer.as_ref(), &paint, Transform::identity(), None);
        }
    }
}
