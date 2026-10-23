
use crate::{
    gfx::{LoupeGpu, loupe_tex_gen},
    text::Text,
};
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use windows::Win32::{
    Foundation::POINT,
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN},
};

const CELLS_X: f32 = 25.0;
const CELLS_Y: f32 = 17.0;

#[derive(PartialEq, Clone, Copy)]
struct Key {
    x: i32,
    y: i32,
    rgb: [u8; 3],
    hex: bool,
    copied: bool,
    dark: bool,
}

pub struct Info {
    pix: Option<Pixmap>,
    key: Option<Key>,
    tex_gen: u64,
}

impl Info {
    pub fn new() -> Self {
        Self { pix: None, key: None, tex_gen: 0 }
    }

    pub fn reset(&mut self) {
        self.key = None;
    }
}

pub fn monitor_bounds(p: (f32, f32), frame: (f32, f32)) -> [f32; 4] {
    let whole = [0.0, 0.0, frame.0, frame.1];
    unsafe {
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        if (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) != (frame.0 as i32, frame.1 as i32) {
            return whole;
        }
        let mon = MonitorFromPoint(POINT { x: vx + p.0.floor() as i32, y: vy + p.1.floor() as i32 }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return whole;
        }
        let r = mi.rcMonitor;
        let b = [((r.left - vx) as f32).max(0.0), ((r.top - vy) as f32).max(0.0), ((r.right - vx) as f32).min(frame.0), ((r.bottom - vy) as f32).min(frame.1)];
        if b[2] > b[0] && b[3] > b[1] { b } else { whole }
    }
}

pub fn layout(cursor: (f32, f32), frame: (f32, f32), scale: f32) -> LoupeGpu {
    place(cursor, monitor_bounds(cursor, frame), scale)
}

fn place(cursor: (f32, f32), bounds: [f32; 4], scale: f32) -> LoupeGpu {
    let cell = (10.0 * scale).round().max(4.0);
    let (aw, ah) = (CELLS_X * cell, CELLS_Y * cell);
    let info_h = (92.0 * scale).round();
    let total_h = ah + info_h;
    let gap = 22.0 * scale;
    let mut x = cursor.0 + gap;
    let mut y = cursor.1 + gap;
    if x + aw > bounds[2] {
        x = cursor.0 - gap - aw;
    }
    if y + total_h > bounds[3] {
        y = cursor.1 - gap - total_h;
    }
    let x = x.clamp(bounds[0], (bounds[2] - aw).max(bounds[0])).round();
    let y = y.clamp(bounds[1], (bounds[3] - total_h).max(bounds[1])).round();
    LoupeGpu {
        area: [x, y, aw, ah],
        info: [x, y + ah, aw, info_h],
        src: (cursor.0.floor(), cursor.1.floor()),
        cell,
        radius: 14.0 * scale,
    }
}

pub fn color_text(rgb: [u8; 3], hex: bool) -> String {
    if hex {
        format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
    } else {
        format!("{}, {}, {}", rgb[0], rgb[1], rgb[2])
    }
}

impl Info {
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        text: &Text,
        bold: Option<&Text>,
        gpu: &LoupeGpu,
        screen: (i32, i32),
        rgb: [u8; 3],
        hex: bool,
        copied: bool,
        dark: bool,
        scale: f32,
    ) -> Option<(&[u8], u32, u32)> {
        let key = Key { x: screen.0, y: screen.1, rgb, hex, copied, dark };
        let (w, h) = (gpu.info[2] as u32, gpu.info[3] as u32);
        let size_ok = self.pix.as_ref().is_some_and(|p| p.width() == w && p.height() == h);
        let tex_gen = loupe_tex_gen();
        if size_ok && self.key == Some(key) && self.tex_gen == tex_gen {
            return None;
        }
        self.tex_gen = tex_gen;
        if !size_ok {
            self.pix = Pixmap::new(w, h);
        }
        let pix = self.pix.as_mut()?;
        pix.fill(Color::TRANSPARENT);
        let bold = bold.unwrap_or(text);
        let (wf, s) = (w as f32, scale);
        let (ink, dim) = if dark { ([239, 243, 250], [151, 165, 186]) } else { ([22, 32, 56], [84, 98, 124]) };
        bold.draw_centered(pix, &format!("({} , {})", screen.0, screen.1), wf / 2.0, 15.0 * s, 14.0 * s, ink);
        let label = color_text(rgb, hex);
        let sw = 14.0 * s;
        let tw = text.measure(&label, 14.0 * s);
        let left = (wf - (sw + 8.0 * s + tw)) / 2.0;
        let mut p = Paint::default();
        p.set_color_rgba8(rgb[0], rgb[1], rgb[2], 255);
        if let Some(r) = Rect::from_xywh(left, 31.0 * s, sw, sw) {
            pix.fill_rect(r, &p, Transform::identity(), None);
            let mut edge = Paint::default();
            edge.set_color_rgba8(255, 255, 255, 90);
            let st = tiny_skia::Stroke { width: 1.0, ..Default::default() };
            pix.stroke_path(&tiny_skia::PathBuilder::from_rect(r), &edge, &st, Transform::identity(), None);
        }
        text.draw(pix, &label, left + sw + 8.0 * s, 31.0 * s + (sw - text.line_height(14.0 * s)) / 2.0, 14.0 * s, ink);
        if copied {
            bold.draw_centered(pix, "Color copied", wf / 2.0, 61.0 * s, 12.0 * s, if dark { [92, 140, 255] } else { [40, 92, 230] });
        } else {
            text.draw_centered(pix, "Press C to copy color", wf / 2.0, 61.0 * s, 11.5 * s, dim);
        }
        text.draw_centered(pix, "Press Shift to switch between RGB/HEX", wf / 2.0, 77.0 * s, 11.5 * s, dim);
        self.key = Some(key);
        Some((pix.data(), w, h))
    }
}
