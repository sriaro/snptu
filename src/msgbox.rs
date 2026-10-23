
use crate::{menu, text::Text};
use std::{cell::RefCell, ffi::c_void, path::Path, sync::Arc, time::Instant};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Point,
    SpreadMode, Stroke, Transform,
};
use unicode_segmentation::UnicodeSegmentation;
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MonitorFromPoint, MonitorFromRect, SelectObject},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::WM_MOUSELEAVE,
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_ESCAPE, VK_LEFT, VK_RETURN,
                VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FindWindowExW,
                FlashWindowEx, GetCursorPos, GetMessageW, GetWindowRect, HICON, HTCAPTION, HTCLIENT,
                HWND_NOTOPMOST, IDC_ARROW, KillTimer, LoadCursorW, LoadIconW, MB_ICONWARNING, MB_OK, MSG, MessageBoxW, RegisterClassExW,
                SPI_GETHIGHCONTRAST, SPI_GETSCREENREADER, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetTimer,
                SetWindowPos, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_ACTIVATE, WM_CLOSE, WM_DESTROY, WM_DPICHANGED,
                WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_SETTINGCHANGE,
                WM_TIMER, WNDCLASSEXW, WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{BOOL, PCWSTR, w},
};


const W: f32 = 456.0;
const SHADOW: f32 = 26.0;
const BLUR: f32 = 18.0;
const DROP: f32 = 5.0;
const RADIUS: f32 = 12.0;
const TITLE_H: f32 = 40.0;
const PAD: f32 = 24.0;
const ICON: f32 = 28.0;
const GAP: f32 = 14.0;
const HEAD_PX: f32 = 16.0;
const BODY_PX: f32 = 13.5;
const SMALL_PX: f32 = 12.5;
const BTN_H: f32 = 32.0;
const FOOT_H: f32 = 64.0;
const ANIM_MS: f32 = 180.0;
const ANIM_TIMER: usize = 1;
const HEAD_LINES: usize = 3;
const TEXT_LINES: usize = 10;
const PATH_LINES: usize = 4;
const WHY_LINES: usize = 3;
const MAX_ROWS: usize = 6;
const MAX_LINE: usize = 2_000;
const KEY_GRACE_MS: u128 = 350;
const SCREEN_MARGIN: f32 = 24.0;


#[derive(Debug, Clone, PartialEq)]
enum Block {
    Text(String),
    Path(String),
    Row { name: String, why: String },
}

#[derive(Debug, Clone, PartialEq)]
struct Notice {
    heading: String,
    blocks: Vec<Block>,
    copy: String,
}

fn unix_lines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn looks_like_path(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')) || s.starts_with("\\\\")
}

impl Notice {
    fn parse(text: &str) -> Notice {
        let clean = unix_lines(text);
        let mut paras = clean.split("\n\n").map(|p| p.trim_matches('\n')).filter(|p| !p.trim().is_empty());
        let first = paras.next().map_or("Snptu", str::trim);
        let list = first.ends_with(':');
        let heading = first.trim_end_matches(':').trim_end().to_string();
        let mut blocks = Vec::new();
        for p in paras {
            let one = p.trim();
            if !one.contains('\n') && looks_like_path(one) {
                blocks.push(Block::Path(one.to_string()));
            } else if list {
                for line in p.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let (name, why) = line.split_once(": ").unwrap_or((line, ""));
                    blocks.push(Block::Row { name: name.trim().to_string(), why: why.trim().to_string() });
                }
            } else {
                blocks.push(Block::Text(p.trim_end().to_string()));
            }
        }
        Notice { heading, blocks, copy: clean.trim().to_string() }
    }

    fn path(&self) -> Option<&str> {
        self.blocks.iter().find_map(|b| if let Block::Path(p) = b { Some(p.as_str()) } else { None })
    }
}


fn pieces(s: &str, path: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev_space = false;
    for (i, c) in s.char_indices() {
        let space = c == ' ';
        if i > start && prev_space && !space {
            out.push(&s[start..i]);
            start = i;
        }
        if path && (c == '\\' || c == '/') {
            let end = i + c.len_utf8();
            out.push(&s[start..end]);
            start = end;
            prev_space = false;
            continue;
        }
        prev_space = space;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn wrap(t: &Text, s: &str, px: f32, max_w: f32, path: bool, most: usize) -> Vec<String> {
    let mut out = Vec::new();
    for hard in s.split('\n') {
        if out.len() >= most {
            break;
        }
        let hard = clip(hard.trim_end());
        if hard.is_empty() {
            out.push(String::new());
            continue;
        }
        let (mut line, mut width) = (String::new(), 0.0f32);
        for piece in pieces(hard, path) {
            if out.len() >= most {
                break;
            }
            let visible = piece.trim_end_matches(' ');
            let vw = t.advance(visible, px);
            if !line.is_empty() && width + vw > max_w {
                out.push(line.trim_end().to_string());
                line.clear();
                width = 0.0;
            }
            if vw > max_w {
                for g in visible.graphemes(true) {
                    let gw = t.advance(g, px);
                    if !line.is_empty() && width + gw > max_w {
                        out.push(std::mem::take(&mut line));
                        width = 0.0;
                    }
                    line.push_str(g);
                    width += gw;
                }
                if piece.len() > visible.len() && !line.is_empty() {
                    line.push(' ');
                    width += t.advance(" ", px);
                }
            } else {
                line.push_str(piece);
                width += t.advance(piece, px);
            }
        }
        if !line.is_empty() {
            out.push(line.trim_end().to_string());
        }
    }
    out.truncate(most);
    out
}

fn clip(s: &str) -> &str {
    s.char_indices().nth(MAX_LINE).map_or(s, |(i, _)| &s[..i])
}

fn cap(mut lines: Vec<String>, n: usize, t: &Text, px: f32, max_w: f32) -> Vec<String> {
    if lines.len() <= n || n == 0 {
        return lines;
    }
    lines.truncate(n);
    if let Some(last) = lines.last_mut() {
        let ellipsis = t.advance("…", px);
        let parts: Vec<(usize, f32)> = last.graphemes(true).map(|g| (g.len(), t.advance(g, px))).collect();
        let mut width: f32 = parts.iter().map(|p| p.1).sum();
        let (mut keep, mut cut) = (parts.len(), last.len());
        while keep > 0 && width + ellipsis > max_w {
            keep -= 1;
            cut -= parts[keep].0;
            width -= parts[keep].1;
        }
        last.truncate(cut);
        last.push('…');
    }
    lines
}

fn cap_tail(mut lines: Vec<String>, n: usize, t: &Text, px: f32, max_w: f32) -> Vec<String> {
    if lines.len() <= n || n == 0 {
        return lines;
    }
    let mut tail = lines.split_off(lines.len() - n);
    let first = &mut tail[0];
    let ellipsis = t.advance("…", px);
    let parts: Vec<(usize, f32)> = first.graphemes(true).map(|g| (g.len(), t.advance(g, px))).collect();
    let mut width: f32 = parts.iter().map(|p| p.1).sum();
    let (mut skip, mut cut) = (0, 0);
    while skip < parts.len() && width + ellipsis > max_w {
        width -= parts[skip].1;
        cut += parts[skip].0;
        skip += 1;
    }
    *first = format!("…{}", &first[cut..]);
    tail
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tone {
    Ink,
    Soft,
    Hint,
}

enum Cmd {
    Text { x: f32, y: f32, s: String, px: f32, bold: bool, tone: Tone },
    Chip([f32; 4]),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Reveal,
    Copy,
    Ok,
}

struct Btn {
    rect: [f32; 4],
    act: Act,
    label: String,
    primary: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Close,
    Button(usize),
}

struct Plan {
    w: f32,
    h: f32,
    title_h: f32,
    foot_y: f32,
    icon: [f32; 4],
    cmds: Vec<Cmd>,
    buttons: Vec<Btn>,
    close: [f32; 4],
}

struct Fonts<'a> {
    reg: &'a Text,
    bold: &'a Text,
}

fn plan(n: &Notice, f: &Fonts, s: f32, reveal: bool, copied: bool, max_h: f32) -> Plan {
    let w = (W * s).round();
    let x0 = (PAD + ICON + GAP) * s;
    let col = w - x0 - PAD * s;
    let title_h = TITLE_H * s;
    let mut cmds = Vec::new();
    let mut y = title_h + 8.0 * s;

    let hp = HEAD_PX * s;
    let head_lh = (f.bold.line_height(hp) * 1.1).round();
    let head_top = y;
    for line in cap(wrap(f.bold, &n.heading, hp, col, false, HEAD_LINES + 1), HEAD_LINES, f.bold, hp, col) {
        cmds.push(Cmd::Text { x: x0, y, s: line, px: hp, bold: true, tone: Tone::Ink });
        y += head_lh;
    }
    let isz = ICON * s;
    let icon = [PAD * s, head_top + (head_lh - isz) / 2.0, isz, isz];

    let (bp, sp) = (BODY_PX * s, SMALL_PX * s);
    let (body_lh, small_lh) = ((f.reg.line_height(bp) + 3.0 * s).round(), (f.reg.line_height(sp) + 3.0 * s).round());
    let limit = max_h - (22.0 * s + FOOT_H * s + 8.0 * s + small_lh);
    let lines_in = |from: f32, lh: f32| ((limit - from) / lh).floor().max(1.0) as usize;
    let mut rows = 0;
    let mut hidden = 0;
    let mut first = true;
    let mut full = false;
    for b in &n.blocks {
        if full {
            hidden += 1;
            continue;
        }
        if let Block::Row { .. } = b {
            rows += 1;
            if rows > MAX_ROWS {
                hidden += 1;
                continue;
            }
        }
        let top = y + if first || matches!(b, Block::Row { .. }) { 8.0 } else { 10.0 } * s;
        match b {
            Block::Text(t) => {
                let lines = wrap(f.reg, t, bp, col, false, TEXT_LINES + 1);
                let fits = lines.len().min(TEXT_LINES);
                if !first && top + fits as f32 * body_lh > limit {
                    full = true;
                    hidden += 1;
                    continue;
                }
                let room = lines_in(top, body_lh);
                full = fits > room;
                y = top;
                for line in cap(lines, TEXT_LINES.min(room), f.reg, bp, col) {
                    cmds.push(Cmd::Text { x: x0, y, s: line, px: bp, bold: false, tone: Tone::Soft });
                    y += body_lh;
                }
            }
            Block::Path(p) => {
                let (pad_x, pad_y) = (10.0 * s, 7.0 * s);
                let inner = col - 2.0 * pad_x;
                let lines = wrap(f.reg, p, sp, inner, true, usize::MAX);
                let fits = lines.len().min(PATH_LINES);
                if !first && top + fits as f32 * small_lh + 2.0 * pad_y > limit {
                    full = true;
                    hidden += 1;
                    continue;
                }
                let room = lines_in(top + 2.0 * pad_y, small_lh);
                full = fits > room;
                let lines = cap_tail(lines, PATH_LINES.min(room), f.reg, sp, inner);
                let h = lines.len() as f32 * small_lh + 2.0 * pad_y;
                cmds.push(Cmd::Chip([x0, top, col, h]));
                for (i, line) in lines.into_iter().enumerate() {
                    cmds.push(Cmd::Text { x: x0 + pad_x, y: top + pad_y + i as f32 * small_lh, s: line, px: sp, bold: false, tone: Tone::Ink });
                }
                y = top + h;
            }
            Block::Row { name, why } => {
                let names = cap(wrap(f.bold, name, bp, col, true, 3), 2, f.bold, bp, col);
                let whys = cap(wrap(f.reg, why, sp, col, false, WHY_LINES + 1), WHY_LINES, f.reg, sp, col);
                let h = names.len() as f32 * body_lh + whys.len() as f32 * small_lh;
                if !first && top + h > limit {
                    full = true;
                    hidden += 1;
                    continue;
                }
                y = top;
                for line in names {
                    cmds.push(Cmd::Text { x: x0, y, s: line, px: bp, bold: true, tone: Tone::Ink });
                    y += body_lh;
                }
                for line in whys {
                    cmds.push(Cmd::Text { x: x0, y, s: line, px: sp, bold: false, tone: Tone::Hint });
                    y += small_lh;
                }
            }
        }
        first = false;
    }
    if hidden > 0 {
        y += 8.0 * s;
        let more = format!("… and {hidden} more");
        cmds.push(Cmd::Text { x: x0, y, s: more, px: sp, bold: false, tone: Tone::Hint });
        y += small_lh;
    }

    let content_end = y.max(icon[1] + icon[3]);
    let foot_y = (content_end + 22.0 * s).ceil();
    let foot_h = FOOT_H * s;
    let h = (foot_y + foot_h).ceil();

    let bh = BTN_H * s;
    let by = (foot_y + (foot_h - bh) / 2.0).round();
    let mut buttons = Vec::new();
    let mut right = w - 20.0 * s;
    let wanted: [(Act, &str, bool, bool); 3] = [
        (Act::Ok, "OK", true, true),
        (Act::Copy, if copied { "Copied" } else { "Copy" }, false, true),
        (Act::Reveal, "Show in folder", false, reveal),
    ];
    for (act, label, primary, on) in wanted {
        if !on {
            continue;
        }
        let font = if primary { f.bold } else { f.reg };
        let bw = (font.advance(label, bp) + 36.0 * s).max(84.0 * s).round();
        buttons.push(Btn { rect: [right - bw, by, bw, bh], act, label: label.to_string(), primary });
        right -= bw + 8.0 * s;
    }
    buttons.reverse();

    let close = [w - (8.0 + 36.0) * s, ((title_h - 28.0 * s) / 2.0).round(), 36.0 * s, 28.0 * s];
    Plan { w, h, title_h, foot_y, icon, cmds, buttons, close }
}

fn hit_at(p: &Plan, x: f32, y: f32) -> Option<Hit> {
    let inside = |r: &[f32; 4]| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3];
    if inside(&p.close) {
        return Some(Hit::Close);
    }
    p.buttons.iter().position(|b| inside(&b.rect)).map(Hit::Button)
}

fn on_title(p: &Plan, x: f32, y: f32) -> bool {
    x >= 0.0 && x < p.w && y >= 0.0 && y < p.title_h && hit_at(p, x, y) != Some(Hit::Close)
}


#[derive(Default, Clone, Copy)]
struct View {
    hover: Option<Hit>,
    press: Option<Hit>,
    focus: usize,
    kbd: bool,
}

fn rrect(r: [f32; 4], radius: f32) -> Option<tiny_skia::Path> {
    crate::toolbar::rrect(r[0], r[1], r[2], r[3], radius)
}

fn tone_colour(t: Tone, c: &menu::MenuPal) -> [u8; 3] {
    match t {
        Tone::Ink => c.ink,
        Tone::Soft => crate::settings::mix(c.ink, c.hint, 0.3),
        Tone::Hint => crate::settings::mix(c.ink, c.hint, 0.55),
    }
}

fn backdrop_luma(b: &Pixmap) -> f32 {
    let data = b.data();
    let step = (data.len() / 4 / 2_000).max(1);
    let (mut sum, mut n) = (0.0f32, 0.0f32);
    for px in data.as_chunks::<4>().0.iter().step_by(step) {
        sum += 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
        n += 1.0;
    }
    if n > 0.0 { sum / n } else { 128.0 }
}

fn veil_alpha(luma: f32, dark: bool) -> u8 {
    let t = if dark { (luma - 70.0) / 115.0 } else { (190.0 - luma) / 120.0 };
    (t.clamp(0.0, 1.0) * 215.0) as u8
}

fn primary_fill(dark: bool) -> ([u8; 3], [u8; 3]) {
    let (a, b) = crate::theme::accent(dark);
    let deep = |c: [u8; 3]| crate::settings::mix(c, [0, 0, 0], 0.30);
    (deep(a), deep(b))
}

fn shadow(pix: &mut Pixmap, m: f32, bw: f32, bh: f32, radius: f32, s: f32, dark: bool) {
    let layers = (BLUR * s).round().max(1.0) as i32;
    let peak = if dark { 0.50 } else { 0.20 };
    for i in 1..=layers {
        let t = (layers - i) as f32 / layers as f32;
        let a = 3.0 * peak / layers as f32 * t * t;
        if a < 0.002 {
            continue;
        }
        let e = i as f32;
        let Some(path) = rrect([m - e, m - e + DROP * s, bw + 2.0 * e, bh + 2.0 * e], radius + e) else { continue };
        let Some(colour) = Color::from_rgba(0.0, 0.0, 0.0, a.min(1.0)) else { continue };
        let mut p = Paint::default();
        p.set_color(colour);
        p.anti_alias = true;
        pix.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
    }
}

fn warning_icon(pix: &mut Pixmap, r: [f32; 4], s: f32) {
    let id = Transform::identity();
    let (x, y, size) = (r[0], r[1], r[2]);
    let cx = x + size / 2.0;
    let mut tri = PathBuilder::new();
    tri.move_to(cx, y + 0.10 * size);
    tri.line_to(x + 0.95 * size, y + 0.86 * size);
    tri.line_to(x + 0.05 * size, y + 0.86 * size);
    tri.close();
    if let Some(path) = tri.finish() {
        let amber = menu::gradient(y, y + size, &[(0.0, [255, 208, 72, 255]), (1.0, [248, 158, 16, 255])]);
        pix.fill_path(&path, &amber, FillRule::Winding, id, None);
        let st = Stroke { width: 2.6 * s, line_join: LineJoin::Round, ..Default::default() };
        pix.stroke_path(&path, &amber, &st, id, None);
    }
    let ink = menu::paint([58, 38, 4], 255);
    let mut bar = PathBuilder::new();
    bar.move_to(cx, y + 0.36 * size);
    bar.line_to(cx, y + 0.60 * size);
    if let Some(path) = bar.finish() {
        let st = Stroke { width: 2.5 * s, line_cap: LineCap::Round, ..Default::default() };
        pix.stroke_path(&path, &ink, &st, id, None);
    }
    if let Some(dot) = PathBuilder::from_circle(cx, y + 0.72 * size, 1.5 * s) {
        pix.fill_path(&dot, &ink, FillRule::Winding, id, None);
    }
}

fn draw(p: &Plan, f: &Fonts, logo: &Pixmap, dark: bool, s: f32, v: &View, backdrop: Option<&Pixmap>) -> Option<Pixmap> {
    let id = Transform::identity();
    let c = menu::pal(dark);
    let m = (SHADOW * s).round();
    let mut pix = Pixmap::new((p.w + 2.0 * m) as u32, (p.h + 2.0 * m) as u32)?;
    let radius = RADIUS * s;
    shadow(&mut pix, m, p.w, p.h, radius, s, dark);
    let veil = backdrop.map_or(0, |b| veil_alpha(backdrop_luma(b), dark));
    menu::paint_glass_veiled(&mut pix, m, p.w, p.h, radius, s, backdrop, dark, veil)?;
    let at = |r: [f32; 4]| [r[0] + m, r[1] + m, r[2], r[3]];

    if let (Some(body), Some(mut mask)) = (crate::toolbar::frame(m, m, p.w, p.h, radius), Mask::new(pix.width(), pix.height())) {
        mask.fill_path(&body, FillRule::Winding, true, id);
        let band = if dark { ([0, 0, 0], 58) } else { ([20, 32, 64], 14) };
        if let Some(r) = tiny_skia::Rect::from_xywh(m, m + p.foot_y, p.w, p.h - p.foot_y) {
            pix.fill_rect(r, &menu::paint(band.0, band.1), id, Some(&mask));
        }
    }
    if let Some(r) = tiny_skia::Rect::from_xywh(m, m + p.foot_y, p.w, 1.0_f32.max(s.floor())) {
        pix.fill_rect(r, &menu::paint([c.sep[0], c.sep[1], c.sep[2]], c.sep[3]), id, None);
    }

    let logo_px = logo.width() as f32;
    pix.draw_pixmap((m + 16.0 * s) as i32, (m + (p.title_h - logo_px) / 2.0).round() as i32, logo.as_ref(), &PixmapPaint::default(), id, None);
    let small = 12.0 * s;
    let small_lh = f.reg.line_height(small);
    f.reg.draw(&mut pix, "Snptu", m + 16.0 * s + logo_px + 8.0 * s, m + (p.title_h - small_lh) / 2.0, small, tone_colour(Tone::Hint, &c));
    let close = at(p.close);
    let close_hot = v.hover == Some(Hit::Close);
    if close_hot && let Some(path) = rrect(close, 7.0 * s) {
        let press = v.press == Some(Hit::Close);
        pix.fill_path(&path, &menu::paint([196, 43, 28], if press { 200 } else { 255 }), FillRule::Winding, id, None);
    }
    {
        let (cx, cy, d) = (close[0] + close[2] / 2.0, close[1] + close[3] / 2.0, 4.2 * s);
        let mut x = PathBuilder::new();
        x.move_to(cx - d, cy - d);
        x.line_to(cx + d, cy + d);
        x.move_to(cx + d, cy - d);
        x.line_to(cx - d, cy + d);
        if let Some(path) = x.finish() {
            let st = Stroke { width: 1.4 * s, line_cap: LineCap::Round, ..Default::default() };
            let colour = if close_hot { [255, 255, 255] } else { c.ink };
            pix.stroke_path(&path, &menu::paint(colour, 255), &st, id, None);
        }
    }

    warning_icon(&mut pix, at(p.icon), s);

    for cmd in &p.cmds {
        match cmd {
            Cmd::Chip(r) => {
                if let Some(path) = rrect(at(*r), 8.0 * s) {
                    pix.fill_path(&path, &menu::paint(c.ink, if dark { 20 } else { 16 }), FillRule::Winding, id, None);
                    pix.stroke_path(&path, &menu::paint(c.ink, 30), &Stroke { width: s.max(1.0), ..Default::default() }, id, None);
                }
            }
            Cmd::Text { x, y, s: text, px, bold, tone } => {
                let font = if *bold { f.bold } else { f.reg };
                font.draw(&mut pix, text, m + x, m + y, *px, tone_colour(*tone, &c));
            }
        }
    }

    let bp = BODY_PX * s;
    for (i, b) in p.buttons.iter().enumerate() {
        let hot = v.hover == Some(Hit::Button(i));
        let down = hot && v.press == Some(Hit::Button(i));
        let r = at(b.rect);
        let Some(path) = rrect(r, 7.0 * s) else { continue };
        let font = if b.primary { f.bold } else { f.reg };
        if b.primary {
            let (top, bottom) = primary_fill(dark);
            let stops = vec![
                GradientStop::new(0.0, Color::from_rgba8(top[0], top[1], top[2], 255)),
                GradientStop::new(1.0, Color::from_rgba8(bottom[0], bottom[1], bottom[2], 255)),
            ];
            let mut paint = Paint { anti_alias: true, ..Default::default() };
            if let Some(g) = LinearGradient::new(Point::from_xy(r[0], r[1]), Point::from_xy(r[0] + r[2], r[1] + r[3]), stops, SpreadMode::Pad, id) {
                paint.shader = g;
            }
            pix.fill_path(&path, &paint, FillRule::Winding, id, None);
            if down {
                pix.fill_path(&path, &menu::paint([0, 0, 0], 46), FillRule::Winding, id, None);
            } else if hot {
                pix.fill_path(&path, &menu::paint([0, 0, 0], 24), FillRule::Winding, id, None);
            }
        } else {
            let (base, edge) = if dark { ([255, 255, 255], 26) } else { ([20, 32, 64], 34) };
            let alpha = match (down, hot, dark) {
                (true, _, true) => 12,
                (_, true, true) => 38,
                (_, _, true) => 20,
                (true, _, false) => 8,
                (_, true, false) => 26,
                _ => 14,
            };
            pix.fill_path(&path, &menu::paint(base, alpha), FillRule::Winding, id, None);
            pix.stroke_path(&path, &menu::paint(base, edge), &Stroke { width: s.max(1.0), ..Default::default() }, id, None);
        }
        let label_lh = font.line_height(bp);
        let tw = font.advance(&b.label, bp);
        let colour = if b.primary { [255, 255, 255] } else { c.ink };
        font.draw(&mut pix, &b.label, r[0] + (r[2] - tw) / 2.0, r[1] + (r[3] - label_lh) / 2.0, bp, colour);
        if v.kbd
            && v.focus == i
            && let Some(ring) = rrect([r[0] - 2.5 * s, r[1] - 2.5 * s, r[2] + 5.0 * s, r[3] + 5.0 * s], 9.0 * s)
        {
            let st = Stroke { width: 1.6 * s, ..Default::default() };
            pix.stroke_path(&ring, &menu::paint(c.ink, 215), &st, id, None);
        }
    }
    Some(pix)
}


struct Dlg {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut c_void,
    size: (i32, i32),
    origin: (i32, i32),
    notice: Notice,
    plan: Plan,
    fonts: (Arc<Text>, Arc<Text>),
    logo: Pixmap,
    dark: bool,
    s: f32,
    backdrop: Option<Pixmap>,
    view: View,
    reveal: Option<String>,
    copied: bool,
    tracking: bool,
    done: bool,
    pic: Option<Pixmap>,
    born: Instant,
    animate: bool,
    placed: bool,
    replace: bool,
    max_h: f32,
    space_down: bool,
    last_reveal: Option<Instant>,
}

impl Drop for Dlg {
    fn drop(&mut self) {
        unsafe {
            if !self.hwnd.is_invalid() {
                let _ = DestroyWindow(self.hwnd);
            }
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<Dlg>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Dlg) -> R) -> Option<R> {
    STATE
        .try_with(|c| {
            let mut g = c.try_borrow_mut().ok()?;
            g.as_mut().map(f)
        })
        .ok()
        .flatten()
}

pub fn show(given: &str) {
    let text = &*crate::text::normalize_display(given);
    if needs_system_text(text) || assistive() {
        classic(text);
        return;
    }
    let notice = Notice { copy: unix_lines(given).trim().to_string(), ..Notice::parse(text) };
    let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&notice))).unwrap_or(false);
    if !shown {
        classic(text);
    }
    crate::mem::release();
}

fn needs_system_text(text: &str) -> bool {
    crate::text::needs_shaping(text)
}

fn assistive() -> bool {
    #[repr(C)]
    struct HighContrast {
        size: u32,
        flags: u32,
        scheme: *mut u16,
    }
    const HCF_HIGHCONTRASTON: u32 = 1;
    let mut reader = BOOL(0);
    let mut contrast = HighContrast { size: size_of::<HighContrast>() as u32, flags: 0, scheme: std::ptr::null_mut() };
    unsafe {
        let r = SystemParametersInfoW(SPI_GETSCREENREADER, 0, Some(&mut reader as *mut BOOL as *mut c_void), Default::default()).is_ok() && reader.as_bool();
        let h = SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.size, Some(&mut contrast as *mut HighContrast as *mut c_void), Default::default())
            .is_ok()
            && contrast.flags & HCF_HIGHCONTRASTON != 0;
        r || h
    }
}

fn classic(text: &str) {
    let t = crate::wide(text);
    let c = crate::wide("Snptu");
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONWARNING);
    }
}

struct Leave(DPI_AWARENESS_CONTEXT);

impl Drop for Leave {
    fn drop(&mut self) {
        STATE.with(|c| {
            if let Ok(mut g) = c.try_borrow_mut() {
                drop(g.take());
            }
        });
        if !self.0.is_invalid() {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

fn run(n: &Notice) -> bool {
    if STATE.with(|c| c.try_borrow().map_or(true, |g| g.is_some())) {
        return false;
    }
    let _leave = Leave(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) });
    let ok = open(n);
    if ok {
        pump();
    }
    ok
}

fn pump() {
    let mut msg = MSG::default();
    while !with(|d| d.done).unwrap_or(true) {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn register_class() -> Option<HINSTANCE> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let hinst = unsafe { GetModuleHandleW(None) }.ok().map(|h| HINSTANCE(h.0))?;
    ONCE.call_once(|| unsafe {
        let icon = LoadIconW(Some(hinst), PCWSTR(std::ptr::without_provenance(1))).unwrap_or(HICON::default());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("SnptuMessage"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
    Some(hinst)
}

fn rect_work(r: &RECT) -> [i32; 4] {
    menu::monitor_work(unsafe { MonitorFromRect(r, MONITOR_DEFAULTTONEAREST) })
}

fn cursor_monitor() -> ([i32; 4], f32) {
    let mut cur = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cur);
    }
    let mon = unsafe { MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST) };
    let work = menu::monitor_work(mon);
    let (mut dx, mut dy) = (96u32, 96u32);
    let ok = unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_ok();
    (work, if ok && dx >= 96 { (dx as f32 / 96.0).clamp(1.0, 4.0) } else { 1.0 })
}

fn open(n: &Notice) -> bool {
    let (Some(reg), Some(bold)) = (Text::load(), Text::load_semibold()) else { return false };
    let Some(hinst) = register_class() else { return false };
    crate::settings::refresh_motion();
    let dark = crate::config::saved_theme().is_dark();
    let (work, s) = cursor_monitor();
    let reveal = n.path().filter(|p| revealable(p)).map(str::to_string);
    let fonts = Fonts { reg: &reg, bold: &bold };
    let max_h = body_limit(work, s);
    let plan = plan(n, &fonts, s, reveal.is_some(), false, max_h);

    let m = (SHADOW * s).round() as i32;
    let (bw, bh) = (plan.w as i32, plan.h as i32);
    let (ww, wh) = (bw + 2 * m, bh + 2 * m);
    let off = (open_dialogs() % 6) as f32 * 30.0 * s;
    let (dx, dy) = (off as i32, off as i32);
    let bx = (work[0] + (work[2] - work[0] - bw) / 2 + dx).min(work[2] - bw).max(work[0]);
    let by = (work[1] + (work[3] - work[1] - bh) * 9 / 20 + dy).min(work[3] - bh).max(work[1]);
    let title = window_title(&n.heading);
    let logo = crate::icon::render((16.0 * s).round().max(16.0) as u32);
    let backdrop = menu::grab_backdrop(bx, by, bw, bh, s);
    let animate = crate::settings::animations_on();

    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_APPWINDOW,
            w!("SnptuMessage"),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            bx - m,
            by - m,
            ww,
            wh,
            None,
            None,
            Some(hinst),
            None,
        )
    }) else {
        return false;
    };
    let dc = unsafe { CreateCompatibleDC(None) };
    let Some((bmp, bits)) = menu::make_dib(dc, ww, wh) else {
        unsafe {
            let _ = DeleteDC(dc);
            let _ = DestroyWindow(hwnd);
        }
        return false;
    };
    unsafe {
        SelectObject(dc, HGDIOBJ(bmp.0));
    }
    let focus = plan.buttons.iter().position(|b| b.act == Act::Ok).unwrap_or(0);
    let dlg = Dlg {
        hwnd,
        dc,
        bmp,
        bits,
        size: (ww, wh),
        origin: (bx - m, by - m),
        notice: n.clone(),
        plan,
        fonts: (reg, bold),
        logo,
        dark,
        s,
        backdrop,
        view: View { focus, ..View::default() },
        reveal,
        copied: false,
        tracking: false,
        done: false,
        pic: None,
        born: Instant::now(),
        animate,
        placed: false,
        replace: false,
        max_h,
        space_down: false,
        last_reveal: None,
    };
    STATE.with(|c| *c.borrow_mut() = Some(dlg));
    if !redraw() {
        return false;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        if !SetForegroundWindow(hwnd).as_bool() {
            let flash = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                uCount: 3,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&flash);
        }
        if animate {
            SetTimer(Some(hwnd), ANIM_TIMER, 15, None);
        }
    }
    true
}

fn body_limit(work: [i32; 4], s: f32) -> f32 {
    (work[3] - work[1]) as f32 - SCREEN_MARGIN * s
}

fn open_dialogs() -> usize {
    let (mut n, mut prev) = (0, None);
    while let Ok(h) = unsafe { FindWindowExW(None, prev, w!("SnptuMessage"), PCWSTR::null()) } {
        n += 1;
        prev = Some(h);
        if n >= 64 {
            break;
        }
    }
    n
}

fn window_title(heading: &str) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let mut cut = false;
    for c in heading.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        if out.len() + units.len() > 119 {
            cut = true;
            break;
        }
        out.extend_from_slice(units);
    }
    if cut {
        out.push(0x2026);
    }
    out.push(0);
    out
}

fn rescale(dpi: u32, to: Option<RECT>) {
    let s = (dpi as f32 / 96.0).clamp(1.0, 4.0);
    let work = to.map_or_else(|| cursor_monitor().0, |r| rect_work(&r));
    let top_left = to.map(|r| (r.left, r.top));
    let changed = with(|d| {
        if (s - d.s).abs() < 0.01 {
            return false;
        }
        let fonts = Fonts { reg: &d.fonts.0, bold: &d.fonts.1 };
        let max_h = body_limit(work, s);
        let plan = plan(&d.notice, &fonts, s, d.reveal.is_some(), d.copied, max_h);
        let m = (SHADOW * s).round() as i32;
        let (ww, wh) = (plan.w as i32 + 2 * m, plan.h as i32 + 2 * m);
        let Some((bmp, bits)) = menu::make_dib(d.dc, ww, wh) else { return false };
        unsafe {
            SelectObject(d.dc, HGDIOBJ(bmp.0));
            let _ = DeleteObject(HGDIOBJ(d.bmp.0));
        }
        (d.bmp, d.bits, d.size, d.plan, d.s, d.max_h) = (bmp, bits, (ww, wh), plan, s, max_h);
        d.logo = crate::icon::render((16.0 * s).round().max(16.0) as u32);
        d.backdrop = None;
        if let Some(p) = top_left {
            d.origin = p;
        }
        d.replace = true;
        true
    });
    if changed == Some(true) {
        redraw();
    }
}

fn redraw() -> bool {
    with(|d| {
        let fonts = Fonts { reg: &d.fonts.0, bold: &d.fonts.1 };
        d.pic = draw(&d.plan, &fonts, &d.logo, d.dark, d.s, &d.view, d.backdrop.as_ref());
    });
    present()
}

fn progress(animate: bool, placed: bool, elapsed_ms: f32) -> f32 {
    if !animate {
        1.0
    } else if !placed {
        0.0
    } else {
        (elapsed_ms / ANIM_MS).min(1.0)
    }
}

fn present() -> bool {
    let work = with(|d| {
        let pic = d.pic.as_ref()?;
        let t = progress(d.animate, d.placed, d.born.elapsed().as_secs_f32() * 1000.0);
        let frame;
        let shown = if t < 1.0 {
            let anchor = (d.size.0 as f32 / 2.0, d.size.1 as f32 / 2.0);
            frame = menu::compose(pic, t, anchor, d.s)?;
            &frame
        } else {
            pic
        };
        let n = (d.size.0 as usize * d.size.1 as usize * 4).min(shown.data().len());
        let dst = unsafe { std::slice::from_raw_parts_mut(d.bits as *mut u8, n) };
        menu::rgba_to_bgra(dst, shown.data());
        let place = (!d.placed || d.replace).then_some((POINT { x: d.origin.0, y: d.origin.1 }, SIZE { cx: d.size.0, cy: d.size.1 }));
        if !d.placed {
            d.born = Instant::now();
        }
        d.placed = true;
        d.replace = false;
        Some((d.hwnd, d.dc, place, t >= 1.0))
    })
    .flatten();
    let Some((hwnd, dc, place, settled)) = work else { return false };
    let pushed = menu::push(hwnd, dc, 255, place);
    if settled {
        unsafe {
            let _ = KillTimer(Some(hwnd), ANIM_TIMER);
        }
    }
    pushed
}

fn close() {
    let hwnd = with(|d| {
        d.done = true;
        std::mem::take(&mut d.hwnd)
    });
    if let Some(h) = hwnd.filter(|h| !h.is_invalid()) {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

fn copy_text() {
    let Some(text) = with(|d| d.notice.copy.clone()) else { return };
    if arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok() {
        with(|d| {
            d.copied = true;
            let fonts = Fonts { reg: &d.fonts.0, bold: &d.fonts.1 };
            let (reveal, s, max_h) = (d.reveal.is_some(), d.s, d.max_h);
            d.plan = plan(&d.notice, &fonts, s, reveal, true, max_h);
        });
        redraw();
    }
}

fn reveal() {
    let fresh = with(|d| {
        let ok = d.last_reveal.is_none_or(|t| t.elapsed().as_millis() >= 1_000);
        if ok {
            d.last_reveal = Some(Instant::now());
        }
        ok
    });
    if fresh != Some(true) {
        return;
    }
    let Some(path) = with(|d| d.reveal.clone()).flatten() else { return };
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new(crate::app_convert::windows_dir().join("explorer.exe"))
        .raw_arg(select_arg(&path))
        .spawn();
}

fn select_arg(path: &str) -> String {
    format!("/select,\"{}\"", path.trim_end_matches(['\\', '/']))
}

fn revealable(path: &str) -> bool {
    path.starts_with("\\\\") || Path::new(path).exists()
}

fn activate(hit: Hit) {
    match hit {
        Hit::Close => close(),
        Hit::Button(i) => match with(|d| d.plan.buttons.get(i).map(|b| b.act)).flatten() {
            Some(Act::Ok) => close(),
            Some(Act::Copy) => copy_text(),
            Some(Act::Reveal) => reveal(),
            None => {}
        },
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(hwnd, msg, wp, lp)))
        .unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCHITTEST => {
            let mut r = RECT::default();
            let rect_ok = unsafe { GetWindowRect(hwnd, &mut r) }.is_ok();
            let (sx, sy) = menu::signed(lp);
            let on = rect_ok
                && with(|d| {
                    let m = (SHADOW * d.s).round();
                    on_title(&d.plan, (sx - r.left) as f32 - m, (sy - r.top) as f32 - m)
                })
                .unwrap_or(false);
            return LRESULT(if on { HTCAPTION } else { HTCLIENT } as isize);
        }
        WM_MOUSEMOVE => {
            let (x, y) = menu::signed(lp);
            let mut track = false;
            let changed = with(|d| {
                let m = (SHADOW * d.s).round();
                let hit = hit_at(&d.plan, x as f32 - m, y as f32 - m);
                track = !d.tracking;
                d.tracking = true;
                std::mem::replace(&mut d.view.hover, hit) != hit
            })
            .unwrap_or(false);
            if track {
                let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut t);
                }
            }
            if changed {
                redraw();
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            let changed = with(|d| {
                d.tracking = false;
                d.view.hover.take().is_some()
            })
            .unwrap_or(false);
            if changed {
                redraw();
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let (x, y) = menu::signed(lp);
            let hit = with(|d| {
                let m = (SHADOW * d.s).round();
                let hit = hit_at(&d.plan, x as f32 - m, y as f32 - m);
                d.view.press = hit;
                hit
            })
            .flatten();
            if hit.is_some() {
                unsafe {
                    SetCapture(hwnd);
                }
                redraw();
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let (x, y) = menu::signed(lp);
            unsafe {
                let _ = ReleaseCapture();
            }
            let pressed = with(|d| {
                let m = (SHADOW * d.s).round();
                let up = hit_at(&d.plan, x as f32 - m, y as f32 - m);
                let down = d.view.press.take();
                d.view.hover = up;
                if up.is_some() && up == down { up } else { None }
            })
            .flatten();
            match pressed {
                Some(hit) => activate(hit),
                None => {
                    redraw();
                }
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let key = wp.0 as u16;
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            let fresh = (lp.0 >> 30) & 1 == 0 && with(|d| d.born.elapsed().as_millis() >= KEY_GRACE_MS) == Some(true);
            if key == VK_ESCAPE.0 {
                close();
            } else if key == VK_RETURN.0 {
                if let Some(i) = with(|d| d.view.focus).filter(|_| fresh) {
                    activate(Hit::Button(i));
                }
            } else if key == VK_SPACE.0 {
                if fresh {
                    with(|d| d.space_down = true);
                }
            } else if key == VK_TAB.0 || key == VK_LEFT.0 || key == VK_RIGHT.0 {
                let back = key == VK_LEFT.0 || (key == VK_TAB.0 && shift);
                with(|d| {
                    let n = d.plan.buttons.len().max(1);
                    d.view.focus = if back { (d.view.focus + n - 1) % n } else { (d.view.focus + 1) % n };
                    d.view.kbd = true;
                });
                redraw();
            } else if ctrl && key == b'C' as u16 {
                copy_text();
            }
            return LRESULT(0);
        }
        WM_KEYUP => {
            if wp.0 as u16 == VK_SPACE.0
                && let Some(i) = with(|d| std::mem::take(&mut d.space_down).then_some(d.view.focus)).flatten()
            {
                activate(Hit::Button(i));
            }
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == ANIM_TIMER => {
            present();
            return LRESULT(0);
        }
        WM_ENTERSIZEMOVE => {
            if with(|d| d.backdrop.take().is_some()) == Some(true) {
                redraw();
            }
        }
        WM_ACTIVATE => {
            if wp.0 & 0xFFFF == 0 {
                unsafe {
                    let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        }
        WM_DPICHANGED => {
            let to = unsafe { (lp.0 as *const RECT).as_ref() }.copied();
            rescale((wp.0 & 0xFFFF) as u32, to);
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            let named = lp.0 != 0 && unsafe { PCWSTR(lp.0 as *const u16).to_string() }.is_ok_and(|n| n == "ImmersiveColorSet");
            if named {
                let dark = crate::config::saved_theme().is_dark();
                if with(|d| std::mem::replace(&mut d.dark, dark) != dark) == Some(true) {
                    redraw();
                }
            }
        }
        WM_CLOSE => {
            close();
            return LRESULT(0);
        }
        WM_DESTROY => {
            with(|d| {
                d.done = true;
                d.hwnd = HWND::default();
            });
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
