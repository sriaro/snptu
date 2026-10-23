
use crate::{
    menu::{self, grab_backdrop, paint, pal},
    text::Text,
};
use std::{cell::RefCell, ffi::c_void, rc::Rc, sync::Arc};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateCompatibleDC, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_BACK, VK_ESCAPE, VK_RETURN},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, IDC_ARROW, LoadCursorW, RegisterClassExW, SW_SHOW, SetForegroundWindow,
                ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_ACTIVATE, WM_CAPTURECHANGED, WM_CHAR, WM_ERASEBKGND, WM_KEYDOWN,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::w,
};


const W: f32 = 244.0;
const H: f32 = 302.0;
const MARGIN: f32 = 2.0;
const SV: [f32; 4] = [16.0, 16.0, 212.0, 150.0];
const HUE: [f32; 4] = [16.0, 178.0, 212.0, 14.0];
const PREVIEW: [f32; 3] = [30.0, 218.0, 13.0];
const HEX: [f32; 4] = [56.0, 204.0, 108.0, 28.0];
const RECENT_Y: f32 = 262.0;
const RECENT: usize = 8;

type Rgb = [u8; 3];
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    Live,
    Commit,
    Cancel,
}

type Callback = Rc<dyn Fn(Rgb, Pick)>;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Sv,
    Hue,
}

struct Popup {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut c_void,
    size: (i32, i32),
    body: [i32; 4],
    scale: f32,
    dark: bool,
    text: Arc<Text>,
    backdrop: Option<Pixmap>,
    h: f32,
    sat: f32,
    val: f32,
    hex: String,
    initial: Rgb,
    drag: Drag,
    sv: Option<(f32, Pixmap)>,
    cb: Callback,
}

thread_local! {
    static STATE: RefCell<Option<Popup>> = const { RefCell::new(None) };
    static RECENTS: RefCell<Vec<Rgb>> = const { RefCell::new(Vec::new()) };
}

fn with<R>(f: impl FnOnce(&mut Popup) -> R) -> Option<R> {
    STATE
        .try_with(|c| {
            let mut g = c.try_borrow_mut().ok()?;
            g.as_mut().map(f)
        })
        .ok()
        .flatten()
}


fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(360.0) / 60.0;
    let (i, f) = (h.floor() as i32 % 6, h - h.floor());
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8]
}

fn rgb_to_hsv(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn hex_of(c: Rgb) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

pub fn parse_hex(s: &str) -> Option<Rgb> {
    let t = s.trim().trim_start_matches('#');
    if !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digits = |x: &str| u8::from_str_radix(x, 16).ok();
    match t.len() {
        6 => Some([digits(&t[0..2])?, digits(&t[2..4])?, digits(&t[4..6])?]),
        3 => {
            let d = |i: usize| digits(&t[i..i + 1]).map(|v| v * 17);
            Some([d(0)?, d(1)?, d(2)?])
        }
        _ => None,
    }
}

fn complete_hex(s: &str) -> Option<Rgb> {
    parse_hex(s).filter(|_| s.len() == 6)
}


pub fn show(at: (i32, i32), initial: Rgb, scale: f32, dark: bool, text: Arc<Text>, cb: impl Fn(Rgb, Pick) + 'static) {
    dismiss_silently();
    let (bw, bh) = ((W * scale).round() as i32, (H * scale).round() as i32);
    let wa = menu::work_area(POINT { x: at.0, y: at.1 });
    let x = at.0.clamp(wa[0], (wa[2] - bw).max(wa[0]));
    let y = if at.1 + bh > wa[3] { (wa[3] - bh).max(wa[1]) } else { at.1.max(wa[1]) };
    let m = (MARGIN * scale).round() as i32;
    let (ww, wh) = (bw + 2 * m, bh + 2 * m);
    let Some(hinst) = register_class() else { return };
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("SnptuPicker"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)
    }) else {
        return;
    };
    let dc = unsafe { CreateCompatibleDC(None) };
    let Some((bmp, bits)) = menu::make_dib(dc, ww, wh) else {
        unsafe {
            let _ = DeleteDC(dc);
            let _ = DestroyWindow(hwnd);
        }
        return;
    };
    unsafe {
        SelectObject(dc, HGDIOBJ(bmp.0));
    }
    let (h, sat, val) = rgb_to_hsv(initial);
    let popup = Popup {
        hwnd,
        dc,
        bmp,
        bits,
        size: (ww, wh),
        body: [x, y, bw, bh],
        scale,
        dark,
        text,
        backdrop: grab_backdrop(x, y, bw, bh, scale),
        h,
        sat,
        val,
        hex: hex_of(initial),
        initial,
        drag: Drag::None,
        sv: None,
        cb: Rc::new(cb),
    };
    STATE.with(|c| *c.borrow_mut() = Some(popup));
    render();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetCapture(hwnd);
    }
}

fn dismiss_silently() {
    let Some(p) = STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take())) else { return };
    unsafe {
        let _ = ReleaseCapture();
        let _ = DestroyWindow(p.hwnd);
        let _ = DeleteDC(p.dc);
        let _ = DeleteObject(HGDIOBJ(p.bmp.0));
    }
}

pub fn close(commit: bool) {
    let Some(p) = STATE.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take())) else { return };
    let rgb = if commit { hsv_to_rgb(p.h, p.sat, p.val) } else { p.initial };
    unsafe {
        let _ = ReleaseCapture();
        let _ = DestroyWindow(p.hwnd);
        let _ = DeleteDC(p.dc);
        let _ = DeleteObject(HGDIOBJ(p.bmp.0));
    }
    if commit {
        RECENTS.with(|r| {
            let mut r = r.borrow_mut();
            r.retain(|c| *c != rgb);
            r.insert(0, rgb);
            r.truncate(RECENT);
        });
    }
    (p.cb)(rgb, if commit { Pick::Commit } else { Pick::Cancel });
}

pub fn is_open() -> bool {
    with(|_| ()).is_some()
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
            lpszClassName: w!("SnptuPicker"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
    Some(hinst)
}


fn sv_square(h: f32, w: u32, hh: u32) -> Option<Pixmap> {
    let mut pix = Pixmap::new(w, hh)?;
    let data = pix.data_mut();
    for y in 0..hh {
        let v = 1.0 - y as f32 / (hh - 1).max(1) as f32;
        for x in 0..w {
            let s = x as f32 / (w - 1).max(1) as f32;
            let c = hsv_to_rgb(h, s, v);
            let i = ((y * w + x) * 4) as usize;
            data[i] = c[0];
            data[i + 1] = c[1];
            data[i + 2] = c[2];
            data[i + 3] = 255;
        }
    }
    Some(pix)
}

fn rounded(pix: &mut Pixmap, r: [f32; 4], radius: f32, src: &Pixmap) {
    use tiny_skia::{FilterQuality, Paint, Pattern};
    let Some(path) = crate::toolbar::rrect(r[0], r[1], r[2], r[3], radius) else { return };
    let p = Paint { shader: Pattern::new(src.as_ref(), SpreadMode::Pad, FilterQuality::Nearest, 1.0, Transform::from_translate(r[0], r[1])), anti_alias: true, ..Default::default() };
    pix.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
}

fn draw(p: &mut Popup) -> Option<Pixmap> {
    let s = p.scale;
    let id = Transform::identity();
    let m = MARGIN * s;
    let c = pal(p.dark);
    let mut pix = Pixmap::new(p.size.0 as u32, p.size.1 as u32)?;
    menu::paint_glass(&mut pix, m, p.body[2] as f32, p.body[3] as f32, 16.0 * s, s, p.backdrop.as_ref(), p.dark)?;
    let to_px = |r: [f32; 4]| [m + r[0] * s, m + r[1] * s, r[2] * s, r[3] * s];

    let sv = to_px(SV);
    let (sw, sh) = (sv[2].round() as u32, sv[3].round() as u32);
    if p.sv.as_ref().is_none_or(|(h, pm)| *h != p.h || pm.width() != sw || pm.height() != sh) {
        p.sv = sv_square(p.h, sw, sh).map(|pm| (p.h, pm));
    }
    if let Some((_, pm)) = &p.sv {
        rounded(&mut pix, sv, 8.0 * s, pm);
        if let Some(path) = crate::toolbar::rrect(sv[0], sv[1], sv[2], sv[3], 8.0 * s) {
            pix.stroke_path(&path, &paint([255, 255, 255], 40), &Stroke { width: s.max(1.0), ..Default::default() }, id, None);
        }
    }
    let (kx, ky) = (sv[0] + sv[2] * p.sat, sv[1] + sv[3] * (1.0 - p.val));
    if let Some(ring) = PathBuilder::from_circle(kx, ky, 6.5 * s) {
        pix.stroke_path(&ring, &paint([0, 0, 0], 120), &Stroke { width: 3.0 * s, ..Default::default() }, id, None);
        pix.stroke_path(&ring, &paint([255, 255, 255], 255), &Stroke { width: 1.8 * s, ..Default::default() }, id, None);
    }

    let hu = to_px(HUE);
    let stops: Vec<GradientStop> = (0..=6)
        .map(|i| {
            let col = hsv_to_rgb(i as f32 * 60.0, 1.0, 1.0);
            GradientStop::new(i as f32 / 6.0, Color::from_rgba8(col[0], col[1], col[2], 255))
        })
        .collect();
    if let (Some(path), Some(sh)) = (
        crate::toolbar::rrect(hu[0], hu[1], hu[2], hu[3], hu[3] / 2.0),
        LinearGradient::new(Point::from_xy(hu[0], 0.0), Point::from_xy(hu[0] + hu[2], 0.0), stops, SpreadMode::Pad, id),
    ) {
        let fill = tiny_skia::Paint { shader: sh, anti_alias: true, ..Default::default() };
        pix.fill_path(&path, &fill, FillRule::Winding, id, None);
    }
    let hx = hu[0] + hu[2] * (p.h / 360.0);
    if let Some(ring) = PathBuilder::from_circle(hx, hu[1] + hu[3] / 2.0, 8.0 * s) {
        pix.fill_path(&ring, &paint(hsv_to_rgb(p.h, 1.0, 1.0), 255), FillRule::Winding, id, None);
        pix.stroke_path(&ring, &paint([255, 255, 255], 255), &Stroke { width: 2.2 * s, ..Default::default() }, id, None);
    }

    let cur = hsv_to_rgb(p.h, p.sat, p.val);
    let pv = [m + PREVIEW[0] * s, m + PREVIEW[1] * s, PREVIEW[2] * s];
    if let Some(circle) = PathBuilder::from_circle(pv[0], pv[1], pv[2]) {
        pix.fill_path(&circle, &paint(cur, 255), FillRule::Winding, id, None);
        pix.stroke_path(&circle, &paint([255, 255, 255], 70), &Stroke { width: s.max(1.0), ..Default::default() }, id, None);
    }
    let hexr = to_px(HEX);
    if let Some(path) = crate::toolbar::rrect(hexr[0], hexr[1], hexr[2], hexr[3], 8.0 * s) {
        pix.fill_path(&path, &paint([255, 255, 255], if p.dark { 22 } else { 120 }), FillRule::Winding, id, None);
        let ok = complete_hex(&p.hex).is_some();
        let edge = if ok { [c.dim[0], c.dim[1], c.dim[2]] } else { [230, 90, 90] };
        pix.stroke_path(&path, &paint(edge, 120), &Stroke { width: s.max(1.0), ..Default::default() }, id, None);
    }
    let px = 13.0 * s;
    let ty = hexr[1] + hexr[3] / 2.0 - p.text.line_height(px) / 2.0;
    p.text.draw(&mut pix, "#", hexr[0] + 10.0 * s, ty, px, c.dim);
    let hex_x = hexr[0] + 10.0 * s + p.text.measure("#", px) + 3.0 * s;
    p.text.draw(&mut pix, &p.hex, hex_x, ty, px, c.ink);
    let cx = (hex_x + p.text.measure(&p.hex, px) + 1.0 * s).round();
    if let Some(r) = tiny_skia::Rect::from_xywh(cx, ty + 1.0 * s, 1.5 * s, p.text.line_height(px) - 2.0 * s) {
        pix.fill_rect(r, &paint(c.ink, 255), id, None);
    }

    let recents = RECENTS.with(|r| r.borrow().clone());
    p.text.draw(&mut pix, "Recent", m + 16.0 * s, m + (RECENT_Y - 17.0) * s, 10.5 * s, c.dim);
    for i in 0..RECENT {
        let sw = 20.0 * s;
        let x = m + 16.0 * s + i as f32 * (sw + 7.4 * s);
        let y = m + RECENT_Y * s;
        if let Some(path) = crate::toolbar::rrect(x, y, sw, sw, 5.0 * s) {
            match recents.get(i) {
                Some(col) => pix.fill_path(&path, &paint(*col, 255), FillRule::Winding, id, None),
                None => pix.fill_path(&path, &paint([255, 255, 255], if p.dark { 14 } else { 90 }), FillRule::Winding, id, None),
            }
            pix.stroke_path(&path, &paint([255, 255, 255], 60), &Stroke { width: s.max(1.0), ..Default::default() }, id, None);
        }
    }
    Some(pix)
}

fn render() {
    let work = with(|p| {
        let pix = draw(p)?;
        let n = (p.size.0 * p.size.1) as usize * 4;
        let dst = unsafe { std::slice::from_raw_parts_mut(p.bits as *mut u8, n) };
        menu::rgba_to_bgra(dst, pix.data());
        let m = (MARGIN * p.scale).round() as i32;
        Some((p.hwnd, p.dc, POINT { x: p.body[0] - m, y: p.body[1] - m }, SIZE { cx: p.size.0, cy: p.size.1 }))
    })
    .flatten();
    if let Some((hwnd, dc, pos, size)) = work {
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let src = POINT { x: 0, y: 0 };
        unsafe {
            let _ = UpdateLayeredWindow(hwnd, None, Some(&pos), Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
        }
    }
}


fn client_xy(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

fn changed() {
    let report = with(|p| {
        p.hex = hex_of(hsv_to_rgb(p.h, p.sat, p.val));
        (p.cb.clone(), hsv_to_rgb(p.h, p.sat, p.val))
    });
    render();
    if let Some((cb, rgb)) = report {
        cb(rgb, Pick::Live);
    }
}

fn pointer(x: f32, y: f32, down: bool) {
    let hit = with(|p| {
        let s = p.scale;
        let (lx, ly) = (x / s - MARGIN, y / s - MARGIN);
        let inside = |r: [f32; 4]| lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3];
        if down {
            if lx < 0.0 || ly < 0.0 || lx >= W || ly >= H {
                return Some(false);
            }
            if inside(SV) {
                p.drag = Drag::Sv;
            } else if inside([HUE[0] - 6.0, HUE[1] - 6.0, HUE[2] + 12.0, HUE[3] + 12.0]) {
                p.drag = Drag::Hue;
            } else {
                let recents = RECENTS.with(|r| r.borrow().clone());
                for (i, col) in recents.iter().enumerate() {
                    let sx = 16.0 + i as f32 * (20.0 + 7.4);
                    if lx >= sx && lx < sx + 20.0 && (RECENT_Y..RECENT_Y + 20.0).contains(&ly) {
                        let (h, sat, val) = rgb_to_hsv(*col);
                        p.h = h;
                        p.sat = sat;
                        p.val = val;
                        return Some(true);
                    }
                }
                return None;
            }
        }
        match p.drag {
            Drag::Sv => {
                p.sat = ((lx - SV[0]) / SV[2]).clamp(0.0, 1.0);
                p.val = (1.0 - (ly - SV[1]) / SV[3]).clamp(0.0, 1.0);
                Some(true)
            }
            Drag::Hue => {
                p.h = (((lx - HUE[0]) / HUE[2]).clamp(0.0, 1.0) * 360.0).min(359.99);
                Some(true)
            }
            Drag::None => None,
        }
    })
    .flatten();
    match hit {
        Some(true) => changed(),
        Some(false) => close(true),
        None => {}
    }
}

fn typed(c: char) {
    if !c.is_ascii_hexdigit() {
        return;
    }
    let apply = with(|p| {
        if p.hex.len() >= 6 {
            p.hex.clear();
        }
        p.hex.push(c.to_ascii_uppercase());
        if let Some(rgb) = complete_hex(&p.hex) {
            let (h, s, v) = rgb_to_hsv(rgb);
            p.h = h;
            p.sat = s;
            p.val = v;
            return true;
        }
        false
    })
    .unwrap_or(false);
    if apply {
        let report = with(|p| (p.cb.clone(), hsv_to_rgb(p.h, p.sat, p.val)));
        if let Some((cb, rgb)) = report {
            cb(rgb, Pick::Live);
        }
    }
    render();
}

fn key(vk: u16) {
    if vk == VK_ESCAPE.0 {
        close(false);
    } else if vk == VK_RETURN.0 {
        close(true);
    } else if vk == VK_BACK.0 {
        with(|p| {
            p.hex.pop();
        });
        render();
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let (x, y) = client_xy(lparam);
            pointer(x, y, true);
        }
        WM_MOUSEMOVE if wparam.0 & 1 != 0 => {
            let (x, y) = client_xy(lparam);
            pointer(x, y, false);
        }
        WM_LBUTTONUP => {
            with(|p| p.drag = Drag::None);
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wparam.0 as u32) {
                typed(c);
            }
        }
        WM_KEYDOWN => key(wparam.0 as u16),
        WM_ACTIVATE if wparam.0 & 0xFFFF == 0 => close(true),
        WM_CAPTURECHANGED if HWND(lparam.0 as *mut c_void) != hwnd => {
            if with(|p| p.hwnd == hwnd) == Some(true) {
                close(true);
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}
