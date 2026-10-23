
use anyhow::Result;
use std::{num::NonZeroU32, sync::Arc};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event_loop::ActiveEventLoop,
    platform::windows::{WindowAttributesExtWindows, WindowExtWindows},
    window::{CursorIcon, Window, WindowId, WindowLevel},
};
use windows::Win32::{
    Foundation::POINT,
    UI::{
        Input::KeyboardAndMouse::GetCapture,
        WindowsAndMessaging::{GetCursorPos, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos},
    },
};

const EDGE_PX: f32 = 7.0;

#[derive(Clone, Copy)]
struct Grab {
    dir: (i8, i8),
    anchor: (i32, i32),
}

const MAX_IMAGE_SIDE: u32 = 16384;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

const MAX_WINDOW_SIDE: f32 = 16384.0;
const MAX_WINDOW_PIXELS: f32 = 48.0 * 1024.0 * 1024.0;

pub fn size_ok(w: u32, h: u32) -> bool {
    w > 0 && h > 0 && w <= MAX_IMAGE_SIDE && h <= MAX_IMAGE_SIDE && w as u64 * h as u64 <= MAX_IMAGE_PIXELS
}

pub fn valid_image(rgba: &[u8], w: u32, h: u32) -> bool {
    size_ok(w, h) && rgba.len() as u64 == w as u64 * h as u64 * 4
}

fn max_zoom(w: u32, h: u32) -> f32 {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let by_side = MAX_WINDOW_SIDE / w.max(h);
    let by_area = (MAX_WINDOW_PIXELS / (w * h)).sqrt();
    by_side.min(by_area).min(8.0)
}

const MIN_SIDE: u32 = 16;

fn window_size(w: u32, h: u32, zoom: f32) -> (u32, u32) {
    (((w as f32 * zoom).round() as u32).max(MIN_SIDE), ((h as f32 * zoom).round() as u32).max(MIN_SIDE))
}

fn drawn_size(iw: usize, ih: usize, zoom: f32, dw: usize, dh: usize) -> (usize, usize) {
    let side = |i: usize, d: usize| {
        let t = ((i as f32 * zoom).round() as usize).max(1);
        if d == MIN_SIDE as usize && t < d { t } else { d }
    };
    (side(iw, dw), side(ih, dh))
}

fn start_geometry(w: u32, h: u32, place: Place, work_of: impl FnOnce(i32, i32) -> [i32; 4]) -> (f32, (i32, i32)) {
    match place {
        Place::At(x, y) => (max_zoom(w, h).clamp(0.1, 1.0), (x, y)),
        Place::Around(cx, cy) => {
            let work = work_of(cx, cy);
            let (aw, ah) = ((work[2] - work[0]).max(1) as f32, (work[3] - work[1]).max(1) as f32);
            let fit = (0.95 * aw / w as f32).min(0.95 * ah / h as f32);
            let zoom = fit.min(1.0).min(max_zoom(w, h)).max(0.1);
            let (ww, wh) = window_size(w, h, zoom);
            let x = (cx - ww as i32 / 2).clamp(work[0], (work[2] - ww as i32).max(work[0]));
            let y = (cy - wh as i32 / 2).clamp(work[1], (work[3] - wh as i32).max(work[1]));
            (zoom, (x, y))
        }
    }
}

fn edge_dir(p: (f32, f32), size: (f32, f32), e: f32) -> Option<(i8, i8)> {
    let axis = |v: f32, len: f32| {
        let e = e.min(len / 4.0);
        if v < e { -1 } else if v >= len - e { 1 } else { 0 }
    };
    let dir = (axis(p.0, size.0), axis(p.1, size.1));
    (dir != (0, 0)).then_some(dir)
}

fn grab_zoom(g: Grab, p: (i32, i32), iw: u32, ih: u32) -> f32 {
    let along = |v: i32, anchor: i32, dir: i8, len: u32| ((v - anchor) * dir as i32).max(0) as f32 / len.max(1) as f32;
    along(p.0, g.anchor.0, g.dir.0, iw).max(along(p.1, g.anchor.1, g.dir.1, ih))
}

const CHECKER: [u32; 2] = [255, 204];
const FILL: u32 = 0x303030;

fn shade(p: &[u8], x: usize, y: usize) -> u32 {
    let (r, g, b, a) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
    if a == 255 {
        return r << 16 | g << 8 | b;
    }
    let back = CHECKER[(x / 8 + y / 8) & 1];
    let mix = |c: u32| (c * a + back * (255 - a) + 127) / 255;
    mix(r) << 16 | mix(g) << 8 | mix(b)
}

#[derive(Clone, Copy)]
pub enum Place {
    At(i32, i32),
    Around(i32, i32),
}

pub struct PendingPin {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub place: Place,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinCmd {
    Copy,
    SaveAs,
    Shadow,
    OnTop,
    Zoom(u32),
    Opacity(u32),
    Gray,
    Invert,
    FlipH,
    FlipV,
    RotateCw,
    RotateCcw,
    Paste,
    Convert,
    ReplaceFile,
    Close,
    Destroy,
}

pub struct Pin {
    pub window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    img: Vec<u8>,
    iw: u32,
    ih: u32,
    zoom: f32,
    pub cursor: (f32, f32),
    pub shadow: bool,
    pub on_top: bool,
    pub opacity: u32,
    pub hidden: bool,
    pub replacing: bool,
    click_through: bool,
    pub hovered: bool,
    grab: Option<Grab>,
    cursor_icon: CursorIcon,
    lut: Vec<u32>,
    painted: Option<(u32, u32)>,
}

fn repaint_needed(painted: Option<(u32, u32)>, size: (u32, u32)) -> bool {
    painted != Some(size)
}

impl Pin {
    pub fn new(el: &ActiveEventLoop, p: PendingPin) -> Result<Self> {
        if !valid_image(&p.rgba, p.w, p.h) {
            anyhow::bail!("unusable image ({} x {}, {} bytes)", p.w, p.h, p.rgba.len());
        }
        let (zoom, at) = start_geometry(p.w, p.h, p.place, |x, y| crate::menu::work_area(POINT { x, y }));
        let size = window_size(p.w, p.h, zoom);
        let size = PhysicalSize::new(size.0, size.1);
        let window = Arc::new(el.create_window(
            Window::default_attributes()
                .with_title("Snptu pin")
                .with_decorations(false)
                .with_undecorated_shadow(true)
                .with_resizable(false)
                .with_skip_taskbar(true)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_inner_size(size)
                .with_position(PhysicalPosition::new(at.0, at.1))
                .with_visible(false),
        )?);
        crate::disable_window_transitions(&window);
        crate::settings::square_corners(&window);
        let context = softbuffer::Context::new(window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut pin = Self {
            window,
            surface,
            img: p.rgba,
            iw: p.w,
            ih: p.h,
            zoom,
            cursor: (0.0, 0.0),
            shadow: true,
            on_top: true,
            opacity: 100,
            hidden: false,
            replacing: false,
            click_through: false,
            hovered: false,
            grab: None,
            cursor_icon: CursorIcon::Default,
            lut: Vec::new(),
            painted: None,
        };
        crate::settings::cloak(&pin.window, true);
        pin.window.set_visible(true);
        pin.draw();
        unsafe {
            let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        }
        crate::settings::cloak(&pin.window, false);
        Ok(pin)
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn image(&self) -> (u32, u32, &[u8]) {
        (self.iw, self.ih, &self.img)
    }

    pub fn size_label(&self) -> String {
        format!("{} × {}", self.iw, self.ih)
    }

    pub fn draw(&mut self) {
        self.painted = None;
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if self.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = self.surface.buffer_mut() else { return };
        let (dw, dh) = (size.width as usize, size.height as usize);
        let (iw, ih) = (self.iw as usize, self.ih as usize);
        if iw == 0 || ih == 0 || self.img.len() < iw * ih * 4 {
            return;
        }
        let (tw, th) = drawn_size(iw, ih, self.zoom, dw, dh);
        self.lut.clear();
        self.lut.extend((0..tw).map(|x| (((x as f32 + 0.5) * iw as f32 / tw as f32) as usize).min(iw - 1) as u32));
        for y in 0..dh {
            let dst = &mut buf[y * dw..(y + 1) * dw];
            if y >= th {
                dst.fill(FILL);
                continue;
            }
            let sy = (((y as f32 + 0.5) * ih as f32 / th as f32) as usize).min(ih - 1);
            let src = &self.img[sy * iw * 4..(sy + 1) * iw * 4];
            for (x, (d, &sx)) in dst.iter_mut().zip(&self.lut).enumerate() {
                *d = shade(&src[sx as usize * 4..sx as usize * 4 + 4], x, y);
            }
            dst[tw..].fill(FILL);
        }
        if buf.present().is_ok() {
            self.painted = Some((size.width, size.height));
        }
    }

    pub fn resized(&mut self) {
        let size = self.window.inner_size();
        if repaint_needed(self.painted, (size.width, size.height)) {
            self.draw();
        }
    }

    fn place(&mut self, x: i32, y: i32, w: u32, h: u32) {
        if let Some(hwnd) = crate::hwnd_of(&self.window) {
            unsafe {
                let _ = SetWindowPos(hwnd, None, x, y, w as i32, h as i32, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            self.draw();
        } else {
            self.window.set_outer_position(PhysicalPosition::new(x, y));
            let _ = self.window.request_inner_size(PhysicalSize::new(w, h));
            self.window.request_redraw();
        }
    }

    pub fn zoomed_size(&self) -> (u32, u32) {
        window_size(self.iw, self.ih, self.zoom)
    }

    fn resize_to_zoom(&mut self, keep_cursor: bool, old_zoom: f32) {
        let (w, h) = self.zoomed_size();
        let mut pos = self.window.outer_position().unwrap_or_default();
        if keep_cursor {
            let k = self.zoom / old_zoom;
            pos.x += (self.cursor.0 * (1.0 - k)).round() as i32;
            pos.y += (self.cursor.1 * (1.0 - k)).round() as i32;
        }
        self.place(pos.x, pos.y, w, h);
    }


    pub fn edge_at(&self, p: (f32, f32)) -> Option<(i8, i8)> {
        let size = self.window.inner_size();
        edge_dir(p, (size.width as f32, size.height as f32), EDGE_PX * self.window.scale_factor() as f32)
    }

    pub fn update_cursor(&mut self) {
        let icon = match self.grab.map(|g| g.dir).or_else(|| self.edge_at(self.cursor)) {
            Some((_, 0)) => CursorIcon::EwResize,
            Some((0, _)) => CursorIcon::NsResize,
            Some((a, b)) if a == b => CursorIcon::NwseResize,
            Some(_) => CursorIcon::NeswResize,
            None => CursorIcon::Default,
        };
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            self.window.set_cursor(icon);
        }
    }

    pub fn begin_grab(&mut self, dir: (i8, i8)) {
        let pos = self.window.outer_position().unwrap_or_default();
        let size = self.window.inner_size();
        let anchor = (
            if dir.0 < 0 { pos.x + size.width as i32 } else { pos.x },
            if dir.1 < 0 { pos.y + size.height as i32 } else { pos.y },
        );
        self.grab = Some(Grab { dir, anchor });
    }

    pub fn end_grab(&mut self) {
        self.grab = None;
        self.update_cursor();
    }

    pub fn grabbing(&self) -> bool {
        self.grab.is_some()
    }

    pub fn drag_resize(&mut self) {
        let Some(g) = self.grab else { return };
        if crate::hwnd_of(&self.window).is_some_and(|h| unsafe { GetCapture() } != h) {
            self.end_grab();
            return;
        }
        let mut p = POINT::default();
        if unsafe { GetCursorPos(&mut p) }.is_err() {
            return;
        }
        let zoom = self.clamp_zoom(grab_zoom(g, (p.x, p.y), self.iw, self.ih));
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        self.zoom = zoom;
        let (w, h) = self.zoomed_size();
        let x = if g.dir.0 < 0 { g.anchor.0 - w as i32 } else { g.anchor.0 };
        let y = if g.dir.1 < 0 { g.anchor.1 - h as i32 } else { g.anchor.1 };
        self.place(x, y, w, h);
    }

    fn clamp_zoom(&self, z: f32) -> f32 {
        z.clamp(0.1, max_zoom(self.iw, self.ih).max(0.1))
    }

    pub fn zoom_by(&mut self, steps: f32) {
        let old = self.zoom;
        self.zoom = self.clamp_zoom(self.zoom * 1.1f32.powf(steps));
        if (self.zoom - old).abs() > f32::EPSILON {
            self.resize_to_zoom(true, old);
        }
    }

    pub fn set_zoom_percent(&mut self, percent: u32) {
        let old = self.zoom;
        self.zoom = self.clamp_zoom(percent as f32 / 100.0);
        self.resize_to_zoom(false, old);
    }

    pub fn set_image(&mut self, rgba: Vec<u8>, w: u32, h: u32) -> bool {
        if !valid_image(&rgba, w, h) {
            return false;
        }
        self.img = rgba;
        self.iw = w;
        self.ih = h;
        self.zoom = self.clamp_zoom(self.zoom);
        self.resize_to_zoom(false, self.zoom);
        true
    }

    pub fn gray(&mut self) {
        for p in self.img.as_chunks_mut::<4>().0 {
            let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8;
            p[..3].fill(l);
        }
        self.window.request_redraw();
    }

    pub fn invert(&mut self) {
        for p in self.img.as_chunks_mut::<4>().0 {
            for c in &mut p[..3] {
                *c = 255 - *c;
            }
        }
        self.window.request_redraw();
    }

    pub fn flip_h(&mut self) {
        let w = self.iw as usize;
        for row in self.img.chunks_exact_mut(w * 4) {
            for x in 0..w / 2 {
                for k in 0..4 {
                    row.swap(x * 4 + k, (w - 1 - x) * 4 + k);
                }
            }
        }
        self.window.request_redraw();
    }

    pub fn flip_v(&mut self) {
        let (w, h) = (self.iw as usize, self.ih as usize);
        for y in 0..h / 2 {
            let (top, bottom) = self.img.split_at_mut((h - 1 - y) * w * 4);
            top[y * w * 4..(y + 1) * w * 4].swap_with_slice(&mut bottom[..w * 4]);
        }
        self.window.request_redraw();
    }

    pub fn rotate(&mut self, clockwise: bool) {
        let (w, h) = (self.iw as usize, self.ih as usize);
        let mut out = vec![0u8; self.img.len()];
        for y in 0..h {
            for x in 0..w {
                let (nx, ny) = if clockwise { (h - 1 - y, x) } else { (y, w - 1 - x) };
                let (s, d) = ((y * w + x) * 4, (ny * h + nx) * 4);
                out[d..d + 4].copy_from_slice(&self.img[s..s + 4]);
            }
        }
        let _ = self.set_image(out, h as u32, w as u32);
    }

    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        if hidden {
            self.grab = None;
        }
        self.window.set_visible(!hidden);
        self.refresh_ex_style();
    }

    pub fn set_shadow(&mut self, on: bool) {
        self.shadow = on;
        self.window.set_undecorated_shadow(on);
        self.refresh_ex_style();
    }

    pub fn set_on_top(&mut self, on: bool) {
        self.on_top = on;
        self.window.set_window_level(if on { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
        self.refresh_ex_style();
    }

    pub fn set_opacity(&mut self, percent: u32) {
        self.opacity = percent.clamp(10, 100);
        self.refresh_ex_style();
    }

    pub fn set_click_through(&mut self, on: bool) {
        self.click_through = on;
        self.refresh_ex_style();
    }

    fn refresh_ex_style(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED,
            WS_EX_TRANSPARENT,
        };
        let Some(hwnd) = crate::hwnd_of(&self.window) else { return };
        let layered = self.click_through || self.opacity < 100;
        unsafe {
            let mut style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            if layered {
                style |= WS_EX_LAYERED.0 as isize;
            } else {
                style &= !(WS_EX_LAYERED.0 as isize);
            }
            if self.click_through {
                style |= WS_EX_TRANSPARENT.0 as isize;
            } else {
                style &= !(WS_EX_TRANSPARENT.0 as isize);
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
            if layered {
                let pct = if self.click_through { self.opacity.min(80) } else { self.opacity };
                let _ = SetLayeredWindowAttributes(hwnd, Default::default(), (pct * 255 / 100) as u8, LWA_ALPHA);
            }
        }
    }
}
