
use crate::{capture::Frame, menu::paint, shaped::Shaper, text::Text};
use std::cell::RefCell;
use tiny_skia::{FillRule, LineCap, LineJoin, Path, PathBuilder, Pixmap, Rect, Stroke, Transform};

pub type Pt = (f32, f32);
pub type Color = [u8; 3];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rect,
    Ellipse,
    Arrow,
    Pen,
    Marker,
    Mosaic,
    Text,
    Number,
    Eraser,
}

pub const PALETTE: [Color; 16] = [
    [0, 0, 0],
    [127, 127, 127],
    [237, 28, 36],
    [255, 127, 39],
    [255, 242, 0],
    [34, 177, 76],
    [0, 162, 232],
    [63, 72, 204],
    [255, 255, 255],
    [195, 195, 195],
    [136, 0, 21],
    [255, 174, 201],
    [163, 73, 164],
    [185, 122, 87],
    [153, 217, 234],
    [181, 230, 29],
];

#[derive(Clone, Copy)]
pub struct Style {
    pub width: u32,
    pub color: usize,
    pub fill: u8,
    pub arrow: u8,
    pub dash: u8,
    pub mosaic: u8,
    pub custom: Option<Color>,
    pub strength: u8,
    pub bold: u8,
}

impl Style {
    pub fn rgb(&self) -> Color {
        self.custom.unwrap_or(PALETTE[self.color.min(PALETTE.len() - 1)])
    }

    pub fn uses_size(&self, tool: Tool) -> bool {
        match tool {
            Tool::Mosaic => self.fill == 0,
            Tool::Rect | Tool::Ellipse => self.fill != 2,
            _ => true,
        }
    }

    pub fn uses_dash(&self, tool: Tool) -> bool {
        match tool {
            Tool::Rect | Tool::Ellipse => self.fill != 2,
            Tool::Arrow => self.arrow != 4,
            _ => true,
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self { width: 3, color: 2, fill: 0, arrow: 2, dash: 0, custom: None, mosaic: 0, strength: 5, bold: 0 }
    }
}

impl Tool {
    pub const ALL: [Tool; 9] =
        [Tool::Rect, Tool::Ellipse, Tool::Arrow, Tool::Pen, Tool::Marker, Tool::Mosaic, Tool::Text, Tool::Number, Tool::Eraser];

    pub fn index(self) -> usize {
        self as usize
    }
}

const MARKER_WIDTH: f32 = 3.4;
const MARKER_ALPHA: u8 = 105;

#[derive(Clone, PartialEq)]
pub enum Annot {
    Rect { a: Pt, b: Pt, color: Color, width: f32, fill: u8, dash: bool },
    Ellipse { a: Pt, b: Pt, color: Color, width: f32, fill: u8, dash: bool },
    Arrow { a: Pt, b: Pt, color: Color, width: f32, style: u8, dash: bool },
    Pen { pts: Vec<Pt>, color: Color, width: f32, dash: bool },
    Marker { pts: Vec<Pt>, color: Color, width: f32 },
    Text { pos: Pt, text: String, preedit: String, color: Color, size: f32, bold: bool, caret: bool },
    Mosaic { a: Pt, b: Pt, pts: Vec<Pt>, brush: f32, blur: bool, block: f32 },
    Number { pos: Pt, n: u32, color: Color, radius: f32 },
}

fn dist_to_segment(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

fn snap_angle(a: Pt, p: Pt) -> Pt {
    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
    let len = dx.hypot(dy);
    if len < 1.0 {
        return p;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let ang = (dy.atan2(dx) / step).round() * step;
    (a.0 + len * ang.cos(), a.1 + len * ang.sin())
}

fn square_end(a: Pt, p: Pt) -> Pt {
    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
    let s = dx.abs().max(dy.abs());
    (a.0 + s * if dx < 0.0 { -1.0 } else { 1.0 }, a.1 + s * if dy < 0.0 { -1.0 } else { 1.0 })
}

fn stroke_end(pts: &mut Vec<Pt>, p: Pt, straight: bool) {
    if straight {
        if let Some(&first) = pts.first() {
            let q = snap_angle(first, p);
            pts.truncate(1);
            pts.push(q);
        }
    } else if let Some(last) = pts.last()
        && (last.0 - p.0).hypot(last.1 - p.1) >= 1.0
    {
        pts.push(p);
    }
}

fn text_w(cx: &Ctx, text: &str, preedit: &str, size: f32, bold: bool) -> Option<f32> {
    cx.shaper.map(|sh| {
        let mut sh = sh.borrow_mut();
        sh.width(text, size, bold) + sh.width(preedit, size, bold)
    })
}

fn polyline_hit(pts: &[Pt], p: Pt, reach: f32) -> bool {
    match pts {
        [] => false,
        [one] => (one.0 - p.0).hypot(one.1 - p.1) <= reach,
        _ => pts.windows(2).any(|w| dist_to_segment(p, w[0], w[1]) <= reach),
    }
}

impl Annot {
    pub fn set_end(&mut self, p: Pt, straight: bool) {
        match self {
            Annot::Rect { a, b, .. } | Annot::Ellipse { a, b, .. } => *b = if straight { square_end(*a, p) } else { p },
            Annot::Arrow { a, b, .. } => *b = if straight { snap_angle(*a, p) } else { p },
            Annot::Mosaic { pts, brush, .. } if *brush > 0.0 => stroke_end(pts, p, straight),
            Annot::Mosaic { a, b, .. } => *b = if straight { square_end(*a, p) } else { p },
            Annot::Pen { pts, .. } | Annot::Marker { pts, .. } => stroke_end(pts, p, straight),
            _ => {}
        }
    }

    pub fn is_tiny(&self) -> bool {
        match self {
            Annot::Mosaic { brush, .. } if *brush > 0.0 => false,
            Annot::Rect { a, b, .. } | Annot::Ellipse { a, b, .. } | Annot::Mosaic { a, b, .. } => {
                (a.0 - b.0).abs() < 3.0 || (a.1 - b.1).abs() < 3.0
            }
            Annot::Arrow { a, b, .. } => (a.0 - b.0).hypot(a.1 - b.1) < 4.0,
            _ => false,
        }
    }

    pub fn hit(&self, p: Pt, tol: f32, cx: &Ctx) -> bool {
        match self {
            Annot::Rect { a, b, width, fill, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                let reach = width / 2.0 + tol;
                let inside_outer = p.0 >= x0 - reach && p.0 <= x1 + reach && p.1 >= y0 - reach && p.1 <= y1 + reach;
                let inside_inner = p.0 > x0 + reach && p.0 < x1 - reach && p.1 > y0 + reach && p.1 < y1 - reach;
                inside_outer && (*fill != 0 || !inside_inner)
            }
            Annot::Ellipse { a, b, width, fill, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                let (cx_, cy_) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                let (rx, ry) = (((x1 - x0) / 2.0).max(1.0), ((y1 - y0) / 2.0).max(1.0));
                let d = ((p.0 - cx_) / rx).hypot((p.1 - cy_) / ry);
                (*fill != 0 && d <= 1.0) || (d - 1.0).abs() * rx.min(ry) <= width / 2.0 + tol
            }
            Annot::Arrow { a, b, width, .. } => {
                dist_to_segment(p, *a, *b) <= width / 2.0 + tol + 2.0 || (p.0 - b.0).hypot(p.1 - b.1) <= width * 2.5 + tol
            }
            Annot::Pen { pts, width, .. } => polyline_hit(pts, p, width / 2.0 + tol),
            Annot::Marker { pts, width, .. } => polyline_hit(pts, p, width * MARKER_WIDTH / 2.0 + tol),
            Annot::Text { pos, text, preedit, size, bold, .. } => {
                let w = text_w(cx, text, preedit, *size, *bold).unwrap_or(size * 0.6 * text.chars().count() as f32);
                p.0 >= pos.0 - tol && p.0 <= pos.0 + w + tol && p.1 >= pos.1 - tol && p.1 <= pos.1 + size * 1.3 + tol
            }
            Annot::Mosaic { pts, brush, .. } if *brush > 0.0 => polyline_hit(pts, p, brush / 2.0 + tol),
            Annot::Mosaic { a, b, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1
            }
            Annot::Number { pos, radius, .. } => (p.0 - pos.0).hypot(p.1 - pos.1) <= radius + tol,
        }
    }
}

pub fn brush_diameter(level: f32, sc: f32) -> f32 {
    (6.0 + 4.5 * level) * sc
}

pub fn mosaic_block(strength: f32, sc: f32) -> f32 {
    (3.0 + 2.8 * strength) * sc
}

pub fn number_radius(level: f32, sc: f32) -> f32 {
    (8.0 + 1.5 * level) * sc
}

fn nearest_level(f: impl Fn(u32) -> f32, v: f32) -> u32 {
    (1..=12u32).min_by(|a, b| (f(*a) - v).abs().total_cmp(&(f(*b) - v).abs())).unwrap_or(1)
}

fn set_swatch(st: &mut Style, c: Color) {
    match PALETTE.iter().position(|p| *p == c) {
        Some(i) => {
            st.color = i;
            st.custom = None;
        }
        None => st.custom = Some(c),
    }
}

fn points_box(pts: &[Pt]) -> Option<[f32; 4]> {
    let first = *pts.first()?;
    Some(pts.iter().fold([first.0, first.1, first.0, first.1], |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]))
}

fn box_dragged(g: [f32; 4], h: usize, p: Pt, square: bool) -> [f32; 4] {
    let corner = matches!(h, 0 | 2 | 4 | 6);
    if square && corner {
        let anchor = match h {
            0 => (g[2], g[3]),
            2 => (g[0], g[3]),
            4 => (g[0], g[1]),
            _ => (g[2], g[1]),
        };
        let end = square_end(anchor, p);
        let (x0, y0, x1, y1) = norm(anchor, end);
        return [x0, y0, x1.max(x0 + 2.0), y1.max(y0 + 2.0)];
    }
    let mut n = g;
    if matches!(h, 0 | 6 | 7) {
        n[0] = p.0;
    }
    if matches!(h, 2..=4) {
        n[2] = p.0;
    }
    if matches!(h, 0..=2) {
        n[1] = p.1;
    }
    if matches!(h, 4..=6) {
        n[3] = p.1;
    }
    let (x0, y0, x1, y1) = norm((n[0], n[1]), (n[2], n[3]));
    [x0, y0, x1.max(x0 + 2.0), y1.max(y0 + 2.0)]
}

impl Annot {
    pub fn tool(&self) -> Tool {
        match self {
            Annot::Rect { .. } => Tool::Rect,
            Annot::Ellipse { .. } => Tool::Ellipse,
            Annot::Arrow { .. } => Tool::Arrow,
            Annot::Pen { .. } => Tool::Pen,
            Annot::Marker { .. } => Tool::Marker,
            Annot::Text { .. } => Tool::Text,
            Annot::Mosaic { .. } => Tool::Mosaic,
            Annot::Number { .. } => Tool::Number,
        }
    }

    fn is_area(&self) -> bool {
        match self {
            Annot::Rect { fill, .. } | Annot::Ellipse { fill, .. } => *fill != 0,
            Annot::Mosaic { brush, .. } => *brush <= 0.0,
            _ => false,
        }
    }

    pub fn editable(&self) -> bool {
        match self {
            Annot::Pen { .. } | Annot::Marker { .. } => false,
            Annot::Mosaic { brush, .. } => *brush <= 0.0,
            _ => true,
        }
    }

    fn contains(&self, p: Pt, tol: f32) -> bool {
        match self {
            Annot::Rect { a, b, width, .. } | Annot::Mosaic { a, b, brush: width, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                let reach = if matches!(self, Annot::Rect { .. }) { width / 2.0 + tol } else { tol };
                p.0 >= x0 - reach && p.0 <= x1 + reach && p.1 >= y0 - reach && p.1 <= y1 + reach
            }
            Annot::Ellipse { a, b, width, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                let (rx, ry) = (((x1 - x0) / 2.0).max(1.0), ((y1 - y0) / 2.0).max(1.0));
                let d = (((p.0 - (x0 + x1) / 2.0) / rx).hypot((p.1 - (y0 + y1) / 2.0) / ry) - 1.0) * rx.min(ry);
                d <= width / 2.0 + tol
            }
            _ => false,
        }
    }

    fn geom(&self, cx: &Ctx) -> Option<[f32; 4]> {
        match self {
            Annot::Rect { a, b, .. } | Annot::Ellipse { a, b, .. } | Annot::Arrow { a, b, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                Some([x0, y0, x1, y1])
            }
            Annot::Mosaic { pts, brush, .. } if *brush > 0.0 => points_box(pts),
            Annot::Mosaic { a, b, .. } => {
                let (x0, y0, x1, y1) = norm(*a, *b);
                Some([x0, y0, x1, y1])
            }
            Annot::Pen { pts, .. } | Annot::Marker { pts, .. } => points_box(pts),
            Annot::Text { pos, text, preedit, size, bold, .. } => {
                let w = text_w(cx, text, preedit, *size, *bold).unwrap_or(size * 0.6 * text.chars().count() as f32);
                Some([pos.0, pos.1, pos.0 + w.max(size * 0.5), pos.1 + size * 1.3])
            }
            Annot::Number { pos, radius, .. } => Some([pos.0 - radius, pos.1 - radius, pos.0 + radius, pos.1 + radius]),
        }
    }

    pub fn bounds(&self, cx: &Ctx) -> Option<[f32; 4]> {
        let g = self.geom(cx)?;
        let pad = match self {
            Annot::Rect { width, fill, .. } | Annot::Ellipse { width, fill, .. } if *fill != 2 => width / 2.0,
            Annot::Pen { width, .. } => width / 2.0,
            Annot::Marker { width, .. } => width * MARKER_WIDTH / 2.0,
            Annot::Mosaic { brush, .. } => brush / 2.0,
            Annot::Arrow { width, .. } => width / 2.0,
            _ => 0.0,
        };
        Some([g[0] - pad, g[1] - pad, g[2] + pad, g[3] + pad])
    }

    pub fn handles(&self, cx: &Ctx) -> Vec<Pt> {
        match self {
            Annot::Arrow { a, b, .. } => vec![*a, *b],
            Annot::Text { .. } | Annot::Number { .. } => Vec::new(),
            _ => self.geom(cx).map_or_else(Vec::new, |g| {
                let (mx, my) = ((g[0] + g[2]) / 2.0, (g[1] + g[3]) / 2.0);
                vec![(g[0], g[1]), (mx, g[1]), (g[2], g[1]), (g[2], my), (g[2], g[3]), (mx, g[3]), (g[0], g[3]), (g[0], my)]
            }),
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        let mv = |p: &mut Pt| *p = (p.0 + dx, p.1 + dy);
        match self {
            Annot::Rect { a, b, .. } | Annot::Ellipse { a, b, .. } | Annot::Arrow { a, b, .. } => {
                mv(a);
                mv(b);
            }
            Annot::Mosaic { a, b, pts, .. } => {
                mv(a);
                mv(b);
                pts.iter_mut().for_each(mv);
            }
            Annot::Pen { pts, .. } | Annot::Marker { pts, .. } => pts.iter_mut().for_each(mv),
            Annot::Text { pos, .. } | Annot::Number { pos, .. } => mv(pos),
        }
    }

    pub fn dragged(&self, h: usize, p: Pt, shift: bool, cx: &Ctx) -> Annot {
        let mut out = self.clone();
        match &mut out {
            Annot::Arrow { a, b, .. } => match h {
                0 => *a = if shift { snap_angle(*b, p) } else { p },
                _ => *b = if shift { snap_angle(*a, p) } else { p },
            },
            Annot::Text { .. } | Annot::Number { .. } => {}
            _ => {
                let Some(g) = self.geom(cx) else { return out };
                let square = shift && (matches!(self, Annot::Rect { .. } | Annot::Ellipse { .. }) || matches!(self, Annot::Mosaic { brush, .. } if *brush <= 0.0));
                let n = box_dragged(g, h, p, square);
                match &mut out {
                    Annot::Rect { a, b, .. } | Annot::Ellipse { a, b, .. } => {
                        *a = (n[0], n[1]);
                        *b = (n[2], n[3]);
                    }
                    Annot::Mosaic { a, b, pts, brush, .. } => {
                        if *brush > 0.0 {
                            scale_points(pts, g, n);
                        } else {
                            *a = (n[0], n[1]);
                            *b = (n[2], n[3]);
                        }
                    }
                    Annot::Pen { pts, .. } | Annot::Marker { pts, .. } => scale_points(pts, g, n),
                    _ => {}
                }
            }
        }
        out
    }

    pub fn set_look(&mut self, st: &Style, sc: f32) {
        let (rgb, level) = (st.rgb(), st.width as f32);
        match self {
            Annot::Rect { color, width, fill, dash, .. } | Annot::Ellipse { color, width, fill, dash, .. } => {
                *color = rgb;
                *width = level * sc;
                *fill = st.fill;
                *dash = st.dash != 0;
            }
            Annot::Arrow { color, width, style, dash, .. } => {
                *color = rgb;
                *width = level * sc;
                *style = st.arrow;
                *dash = st.dash != 0;
            }
            Annot::Pen { color, width, dash, .. } => {
                *color = rgb;
                *width = level * sc;
                *dash = st.dash != 0;
            }
            Annot::Marker { color, width, .. } => {
                *color = rgb;
                *width = level * sc;
            }
            Annot::Text { pos, color, size, bold, .. } => {
                let new = crate::text_size(st.width, sc);
                pos.1 += (*size - new) * 0.7;
                *size = new;
                *color = rgb;
                *bold = st.bold != 0;
            }
            Annot::Number { color, radius, .. } => {
                *color = rgb;
                *radius = number_radius(level, sc);
            }
            Annot::Mosaic { brush, blur, block, .. } => {
                *blur = st.mosaic == 1;
                *block = mosaic_block(st.strength as f32, sc);
                if *brush > 0.0 {
                    *brush = brush_diameter(level, sc);
                }
            }
        }
    }

    pub fn look(&self, base: &Style, sc: f32) -> Style {
        let mut st = *base;
        match self {
            Annot::Rect { color, width, fill, dash, .. } | Annot::Ellipse { color, width, fill, dash, .. } => {
                set_swatch(&mut st, *color);
                st.width = nearest_level(|l| l as f32 * sc, *width);
                st.fill = *fill;
                st.dash = *dash as u8;
            }
            Annot::Arrow { color, width, style, dash, .. } => {
                set_swatch(&mut st, *color);
                st.width = nearest_level(|l| l as f32 * sc, *width);
                st.arrow = *style;
                st.dash = *dash as u8;
            }
            Annot::Pen { color, width, dash, .. } => {
                set_swatch(&mut st, *color);
                st.width = nearest_level(|l| l as f32 * sc, *width);
                st.dash = *dash as u8;
            }
            Annot::Marker { color, width, .. } => {
                set_swatch(&mut st, *color);
                st.width = nearest_level(|l| l as f32 * sc, *width);
            }
            Annot::Text { color, size, bold, .. } => {
                set_swatch(&mut st, *color);
                st.width = nearest_level(|l| crate::text_size(l, sc), *size);
                st.bold = *bold as u8;
            }
            Annot::Number { color, radius, .. } => {
                set_swatch(&mut st, *color);
                st.width = nearest_level(|l| number_radius(l as f32, sc), *radius);
            }
            Annot::Mosaic { brush, blur, block, .. } => {
                st.mosaic = *blur as u8;
                st.strength = nearest_level(|l| mosaic_block(l as f32, sc), *block) as u8;
                if *brush > 0.0 {
                    st.fill = 0;
                    st.width = nearest_level(|l| brush_diameter(l as f32, sc), *brush);
                } else {
                    st.fill = 1;
                }
            }
        }
        st
    }
}

fn scale_points(pts: &mut [Pt], from: [f32; 4], to: [f32; 4]) {
    let (fw, fh) = (from[2] - from[0], from[3] - from[1]);
    let (sx, sy) = (if fw > 0.5 { (to[2] - to[0]) / fw } else { 1.0 }, if fh > 0.5 { (to[3] - to[1]) / fh } else { 1.0 });
    for p in pts {
        *p = (to[0] + (p.0 - from[0]) * sx, to[1] + (p.1 - from[1]) * sy);
    }
}

pub struct Ctx<'a> {
    pub frame: &'a Frame,
    pub origin: (i32, i32),
    pub text: Option<&'a Text>,
    pub shaper: Option<&'a RefCell<Shaper>>,
    pub scale: f32,
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

fn stroke_dashed(width: f32, dashed: bool) -> Stroke {
    let mut st = stroke(width);
    if dashed {
        st.dash = tiny_skia::StrokeDash::new(vec![width * 2.6, width * 2.0], 0.0);
    }
    st
}

fn norm(a: Pt, b: Pt) -> (f32, f32, f32, f32) {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn arrow_head_len(width: f32, scale: f32, len: f32) -> f32 {
    (width * 4.0 + 6.0 * scale).min(len * 0.7)
}

fn smooth_path(pts: &[Pt]) -> Option<Path> {
    let first = *pts.first()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first.0, first.1);
    if pts.len() >= 3 {
        for i in 1..pts.len() - 1 {
            let mid = ((pts[i].0 + pts[i + 1].0) / 2.0, (pts[i].1 + pts[i + 1].1) / 2.0);
            pb.quad_to(pts[i].0, pts[i].1, mid.0, mid.1);
        }
    }
    let last = pts[pts.len() - 1];
    pb.line_to(last.0, last.1);
    pb.finish()
}

pub fn draw(pix: &mut Pixmap, a: &Annot, cx: &Ctx) {
    let (ox, oy) = (cx.origin.0 as f32, cx.origin.1 as f32);
    let tf = Transform::from_translate(-ox, -oy);
    match a {
        Annot::Rect { a, b, color, width, fill, dash } => {
            let (x0, y0, x1, y1) = norm(*a, *b);
            if let Some(r) = Rect::from_ltrb(x0, y0, x1, y1) {
                let path = PathBuilder::from_rect(r);
                match fill {
                    1 => pix.fill_path(&path, &paint(*color, 90), FillRule::Winding, tf, None),
                    2 => pix.fill_path(&path, &paint(*color, 255), FillRule::Winding, tf, None),
                    _ => {}
                }
                if *fill != 2 {
                    pix.stroke_path(&path, &paint(*color, 255), &stroke_dashed(*width, *dash), tf, None);
                }
            }
        }
        Annot::Ellipse { a, b, color, width, fill, dash } => {
            let (x0, y0, x1, y1) = norm(*a, *b);
            if let Some(path) = Rect::from_ltrb(x0, y0, x1, y1).and_then(PathBuilder::from_oval) {
                match fill {
                    1 => pix.fill_path(&path, &paint(*color, 90), FillRule::Winding, tf, None),
                    2 => pix.fill_path(&path, &paint(*color, 255), FillRule::Winding, tf, None),
                    _ => {}
                }
                if *fill != 2 {
                    pix.stroke_path(&path, &paint(*color, 255), &stroke_dashed(*width, *dash), tf, None);
                }
            }
        }
        Annot::Arrow { a, b, color, width, style, dash } => {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = dx.hypot(dy);
            if len < 2.0 {
                return;
            }
            let (ux, uy) = (dx / len, dy / len);
            let hl = arrow_head_len(*width, cx.scale, len);
            let hw = hl * 0.45;
            let base = (b.0 - ux * hl, b.1 - uy * hl);
            let line_paint = paint(*color, 255);
            let line_end = match style {
                0 => *b,
                1 => (b.0 - ux * hl * 0.2, b.1 - uy * hl * 0.2),
                3 => base,
                _ => (b.0 - ux * hl * 0.8, b.1 - uy * hl * 0.8),
            };
            if *style == 4 {
                let (nx, ny) = (-uy, ux);
                let tail = width * 0.12;
                let body = width * 0.75;
                let pts = [
                    (a.0 + nx * tail, a.1 + ny * tail),
                    (base.0 + nx * body, base.1 + ny * body),
                    (base.0 + nx * hw, base.1 + ny * hw),
                    (b.0, b.1),
                    (base.0 - nx * hw, base.1 - ny * hw),
                    (base.0 - nx * body, base.1 - ny * body),
                    (a.0 - nx * tail, a.1 - ny * tail),
                ];
                let mut pb = PathBuilder::new();
                pb.move_to(pts[0].0, pts[0].1);
                for q in &pts[1..] {
                    pb.line_to(q.0, q.1);
                }
                pb.close();
                if let Some(p) = pb.finish() {
                    pix.fill_path(&p, &line_paint, FillRule::Winding, tf, None);
                    pix.stroke_path(&p, &line_paint, &stroke((width * 0.25).max(0.8)), tf, None);
                }
                return;
            }
            let mut shaft = PathBuilder::new();
            shaft.move_to(a.0, a.1);
            shaft.line_to(line_end.0, line_end.1);
            if let Some(p) = shaft.finish() {
                pix.stroke_path(&p, &line_paint, &stroke_dashed(*width, *dash), tf, None);
            }
            match style {
                0 => {}
                1 => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(base.0 - uy * hw, base.1 + ux * hw);
                    pb.line_to(b.0, b.1);
                    pb.line_to(base.0 + uy * hw, base.1 - ux * hw);
                    if let Some(p) = pb.finish() {
                        pix.stroke_path(&p, &line_paint, &stroke(*width), tf, None);
                    }
                }
                _ => {
                    let mut head = PathBuilder::new();
                    head.move_to(b.0, b.1);
                    head.line_to(base.0 - uy * hw, base.1 + ux * hw);
                    head.line_to(base.0 + uy * hw, base.1 - ux * hw);
                    head.close();
                    if let Some(p) = head.finish() {
                        if *style == 3 {
                            pix.stroke_path(&p, &line_paint, &stroke(*width), tf, None);
                        } else {
                            pix.fill_path(&p, &line_paint, FillRule::Winding, tf, None);
                        }
                    }
                }
            }
        }
        Annot::Pen { pts, color, width, dash } => {
            let Some(&first) = pts.first() else { return };
            if pts.len() < 2 {
                if let Some(p) = PathBuilder::from_circle(first.0, first.1, width / 2.0) {
                    pix.fill_path(&p, &paint(*color, 255), FillRule::Winding, tf, None);
                }
                return;
            }
            if let Some(p) = smooth_path(pts) {
                pix.stroke_path(&p, &paint(*color, 255), &stroke_dashed(*width, *dash), tf, None);
            }
        }
        Annot::Marker { pts, color, width } => {
            let w = width * MARKER_WIDTH;
            let Some(&first) = pts.first() else { return };
            if pts.len() < 2 {
                if let Some(p) = PathBuilder::from_circle(first.0, first.1, w / 2.0) {
                    pix.fill_path(&p, &paint(*color, MARKER_ALPHA), FillRule::Winding, tf, None);
                }
                return;
            }
            if let Some(p) = smooth_path(pts) {
                let st = Stroke { width: w, line_cap: LineCap::Butt, line_join: LineJoin::Round, ..Default::default() };
                pix.stroke_path(&p, &paint(*color, MARKER_ALPHA), &st, tf, None);
            }
        }
        Annot::Text { pos, text, preedit, color, size, bold, caret } => {
            let Some(sh) = cx.shaper else { return };
            let mut sh = sh.borrow_mut();
            let (x, y) = (pos.0 - ox, pos.1 - oy);
            sh.draw(pix, text, (x, y), *size, (*color, 255), *bold);
            let mut end = x + sh.width(text, *size, *bold);
            let line_h = (size * 1.3).ceil();
            if !preedit.is_empty() {
                sh.draw(pix, preedit, (end, y), *size, (*color, 170), *bold);
                let w = sh.width(preedit, *size, *bold);
                if let Some(r) = Rect::from_xywh(end, y + line_h * 0.9, w.max(1.0), (size / 14.0).max(1.0)) {
                    pix.fill_rect(r, &paint(*color, 170), Transform::identity(), None);
                }
                end += w;
            }
            if *caret
                && let Some(r) = Rect::from_xywh(end + 1.0, y + line_h * 0.08, (size / 12.0).max(1.5), line_h * 0.84)
            {
                pix.fill_rect(r, &paint(*color, 255), Transform::identity(), None);
            }
        }
        Annot::Mosaic { a, b, pts, brush, blur, block } => {
            if *brush > 0.0 {
                mosaic_brush(pix, pts, *brush, *blur, *block, cx, None);
            } else {
                mosaic(pix, *a, *b, *blur, *block, cx);
            }
        }
        Annot::Number { pos, n, color, radius } => {
            if let Some(p) = PathBuilder::from_circle(pos.0, pos.1, *radius) {
                pix.fill_path(&p, &paint(*color, 255), FillRule::Winding, tf, None);
            }
            if let Some(t) = cx.text {
                let lum = 0.299 * color[0] as f32 + 0.587 * color[1] as f32 + 0.114 * color[2] as f32;
                let ink = if lum > 170.0 { [24, 24, 28] } else { [255, 255, 255] };
                t.draw_centered(pix, &n.to_string(), pos.0 - ox, pos.1 - oy, radius * 1.25, ink);
            }
        }
    }
}

fn mosaic_brush(pix: &mut Pixmap, pts: &[Pt], d: f32, blur: bool, block: f32, cx: &Ctx, only: Option<[i32; 4]>) {
    let Some([lx, ly, hx, hy]) = points_box(pts) else { return };
    let (lo, hi) = ((lx, ly), (hx, hy));
    let (ox, oy) = cx.origin;
    let (pw, ph) = (pix.width() as i32, pix.height() as i32);
    let cell = (block.round() as i32).max(3);
    let r = d / 2.0 + 1.0;
    let (mut fx0, mut fy0) = (((lo.0 - r).floor() as i32).max(ox), ((lo.1 - r).floor() as i32).max(oy));
    let (mut fx1, mut fy1) = (((hi.0 + r).ceil() as i32).min(ox + pw), ((hi.1 + r).ceil() as i32).min(oy + ph));
    if let Some(o) = only {
        (fx0, fy0, fx1, fy1) = (fx0.max(o[0]), fy0.max(o[1]), fx1.min(o[2]), fy1.min(o[3]));
    }
    if fx1 <= fx0 || fy1 <= fy0 {
        return;
    }
    let near = |v: i32| v - v.rem_euclid(cell);
    let far = |v: i32| v + (cell - v.rem_euclid(cell)) % cell;
    let more = if blur { cell } else { 0 };
    let (gx0, gy0) = (near(fx0 - more).max(near(ox)), near(fy0 - more).max(near(oy)));
    let (gx1, gy1) = (far(fx1 + more).min(ox + pw), far(fy1 + more).min(oy + ph));
    let (tw, th) = ((gx1 - gx0) as u32, (gy1 - gy0) as u32);
    let ww = (fx1 - fx0) as usize;
    let Some(mut tmp) = Pixmap::new(tw, th) else { return };
    let sub = Ctx { frame: cx.frame, origin: (gx0, gy0), text: cx.text, shaper: cx.shaper, scale: cx.scale };
    mosaic(&mut tmp, (gx0 as f32, gy0 as f32), (gx1 as f32, gy1 as f32), blur, block, &sub);
    let cover = brush_coverage(pts, d, [fx0, fy0, fx1, fy1]);
    let t = tmp.data();
    let dst = pix.data_mut();
    for (y, crow) in cover.chunks_exact(ww).enumerate() {
        let ti = ((fy0 - gy0) as usize + y) * tw as usize * 4 + (fx0 - gx0) as usize * 4;
        let di = ((fy0 - oy) as usize + y) * pw as usize * 4 + (fx0 - ox) as usize * 4;
        let (trow, drow) = (&t[ti..][..ww * 4], &mut dst[di..][..ww * 4]);
        for ((&a, tp), dp) in crow.iter().zip(trow.as_chunks::<4>().0).zip(drow.as_chunks_mut::<4>().0) {
            if a == 0 {
                continue;
            }
            let a = a as u32;
            for k in 0..4 {
                dp[k] = ((tp[k] as u32 * a + dp[k] as u32 * (255 - a) + 127) / 255) as u8;
            }
        }
    }
}

fn brush_coverage(pts: &[Pt], d: f32, win: [i32; 4]) -> Vec<u8> {
    let (w, h) = ((win[2] - win[0]).max(0) as usize, (win[3] - win[1]).max(0) as usize);
    let mut cover = vec![0u8; w * h];
    let Some(&first) = pts.first() else { return cover };
    let r = d / 2.0;
    let reach = r + 1.0;
    let (inner, outer) = (((r - 0.5).max(0.0)).powi(2), (r + 0.5).powi(2));
    let mut piece = |a: Pt, b: Pt| {
        let x0 = ((a.0.min(b.0) - reach).floor() as i32).max(win[0]);
        let x1 = ((a.0.max(b.0) + reach).ceil() as i32).min(win[2]);
        let y0 = ((a.1.min(b.1) - reach).floor() as i32).max(win[1]);
        let y1 = ((a.1.max(b.1) + reach).ceil() as i32).min(win[3]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            let row = &mut cover[(y - win[1]) as usize * w..][..w];
            for x in x0..x1 {
                let at = &mut row[(x - win[0]) as usize];
                if *at == 255 {
                    continue;
                }
                let px = x as f32 + 0.5;
                let t = if len2 > 0.0 { (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (qx, qy) = (a.0 + t * dx - px, a.1 + t * dy - py);
                let d2 = qx * qx + qy * qy;
                if d2 >= outer {
                    continue;
                }
                let c = if d2 <= inner { 255 } else { ((r + 0.5 - d2.sqrt()) * 255.0 + 0.5) as u8 };
                *at = (*at).max(c);
            }
        }
    };
    if pts.len() < 2 {
        piece(first, first);
        return cover;
    }
    let near_window = |pp: [Pt; 3]| {
        let lo = (pp[0].0.min(pp[1].0).min(pp[2].0), pp[0].1.min(pp[1].1).min(pp[2].1));
        let hi = (pp[0].0.max(pp[1].0).max(pp[2].0), pp[0].1.max(pp[1].1).max(pp[2].1));
        hi.0 + reach > win[0] as f32 && lo.0 - reach < win[2] as f32 && hi.1 + reach > win[1] as f32 && lo.1 - reach < win[3] as f32
    };
    let mut at = first;
    for i in 1..pts.len().saturating_sub(1) {
        let end = ((pts[i].0 + pts[i + 1].0) / 2.0, (pts[i].1 + pts[i + 1].1) / 2.0);
        if near_window([at, pts[i], end]) {
            flatten_quad(at, pts[i], end, &mut piece);
        }
        at = end;
    }
    piece(at, pts[pts.len() - 1]);
    cover
}

fn flatten_quad(p0: Pt, c: Pt, p1: Pt, piece: &mut impl FnMut(Pt, Pt)) {
    let bend = (p0.0 - 2.0 * c.0 + p1.0).hypot(p0.1 - 2.0 * c.1 + p1.1) / 4.0;
    let n = ((bend / 0.05).sqrt().ceil() as usize).clamp(1, 32);
    let mut from = p0;
    for j in 1..=n {
        let (t, u) = (j as f32 / n as f32, 1.0 - j as f32 / n as f32);
        let to = if j == n { p1 } else { (u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0, u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1) };
        piece(from, to);
        from = to;
    }
}

fn draw_part(pix: &mut Pixmap, a: &Annot, cx: &Ctx, only: Option<[i32; 4]>) {
    match (a, only) {
        (Annot::Mosaic { pts, brush, blur, block, .. }, Some(o)) if *brush > 0.0 => mosaic_brush(pix, pts, *brush, *blur, *block, cx, Some(o)),
        _ => draw(pix, a, cx),
    }
}

fn mosaic(pix: &mut Pixmap, a: Pt, b: Pt, blur: bool, block: f32, cx: &Ctx) {
    let (x0, y0, x1, y1) = norm(a, b);
    let (ox, oy) = cx.origin;
    let (pw, ph) = (pix.width() as i32, pix.height() as i32);
    let (ix0, iy0) = (((x0 - ox as f32).floor() as i32).max(0), ((y0 - oy as f32).floor() as i32).max(0));
    let (ix1, iy1) = (((x1 - ox as f32).ceil() as i32).min(pw), ((y1 - oy as f32).ceil() as i32).min(ph));
    if ix1 <= ix0 || iy1 <= iy0 {
        return;
    }
    let block = (block.round() as i32).max(3);
    let (fw, fh) = (cx.frame.w as i32, cx.frame.h as i32);
    let src = cx.frame.bgra();
    let (rw, rh) = (ix1 - ix0, iy1 - iy0);
    let (cols, rows) = ((rw + block - 1) / block, (rh + block - 1) / block);
    let band = |range: std::ops::Range<usize>| -> Vec<[f32; 3]> {
        let mut out = Vec::with_capacity(range.len() * cols as usize);
        for cy in range {
            let cy = cy as i32;
            for cx_ in 0..cols {
                let (bx0, by0) = (ix0 + cx_ * block, iy0 + cy * block);
                let (bx1, by1) = ((bx0 + block).min(ix1), (by0 + block).min(iy1));
                let (mut r, mut g, mut bl, mut n) = (0u32, 0u32, 0u32, 0u32);
                for y in by0..by1 {
                    let fy = (y + oy).clamp(0, fh - 1);
                    for x in bx0..bx1 {
                        let fx = (x + ox).clamp(0, fw - 1);
                        let i = ((fy * fw + fx) * 4) as usize;
                        bl += src[i] as u32;
                        g += src[i + 1] as u32;
                        r += src[i + 2] as u32;
                        n += 1;
                    }
                }
                let n = n.max(1) as f32;
                out.push([r as f32 / n, g as f32 / n, bl as f32 / n]);
            }
        }
        out
    };
    let cells: Vec<[f32; 3]> = if (rw as i64 * rh as i64) < 64 * 1024 {
        band(0..rows as usize)
    } else {
        crate::par::ranges(rows as usize, 2, band).into_iter().flatten().collect()
    };

    let between = |v: i32, n: i32| -> (i32, i32, f32) {
        let (c, u) = (v / block, v % block);
        let t = (u as f32 + 0.5) / block as f32 - 0.5;
        let c0 = if t < 0.0 { (c - 1).max(0) } else { c };
        (c0, (c0 + 1).min(n - 1), (t + (c - c0) as f32).clamp(0.0, 1.0))
    };
    let paint_row = |y: i32, row: &mut [u8]| {
        let ly = y - iy0;
        for x in ix0..ix1 {
            let lx = x - ix0;
            let rgb = if blur {
                let (c0, c1, fx) = between(lx, cols);
                let (r0, r1, fy) = between(ly, rows);
                let at = |c: i32, r: i32| cells[(r * cols + c) as usize];
                let (p00, p10, p01, p11) = (at(c0, r0), at(c1, r0), at(c0, r1), at(c1, r1));
                let mut out = [0.0f32; 3];
                for k in 0..3 {
                    let top = p00[k] + (p10[k] - p00[k]) * fx;
                    let bot = p01[k] + (p11[k] - p01[k]) * fx;
                    out[k] = top + (bot - top) * fy;
                }
                out
            } else {
                cells[((ly / block) * cols + lx / block) as usize]
            };
            let i = (x * 4) as usize;
            row[i] = rgb[0].round() as u8;
            row[i + 1] = rgb[1].round() as u8;
            row[i + 2] = rgb[2].round() as u8;
            row[i + 3] = 255;
        }
    };
    let data = pix.data_mut();
    let region = &mut data[(iy0 * pw * 4) as usize..(iy1 * pw * 4) as usize];
    crate::par::rows(region, (pw * 4) as usize, |ry, row| paint_row(iy0 + ry as i32, row));
}

fn points_span(pts: &[Pt]) -> (Pt, Pt) {
    let b = points_box(pts).unwrap_or([f32::MAX, f32::MAX, f32::MIN, f32::MIN]);
    ((b[0], b[1]), (b[2], b[3]))
}

fn bbox(a: &Annot, cx: &Ctx) -> [i32; 4] {
    let sc = cx.scale;
    let (lo, hi, pad): (Pt, Pt, f32) = match a {
        Annot::Rect { a, b, width, .. } | Annot::Ellipse { a, b, width, .. } | Annot::Arrow { a, b, width, .. } => {
            let (x0, y0, x1, y1) = norm(*a, *b);
            ((x0, y0), (x1, y1), width * 3.0 + 14.0 * sc)
        }
        Annot::Mosaic { pts, brush, .. } if *brush > 0.0 => {
            let (lo, hi) = points_span(pts);
            (lo, hi, brush / 2.0 + 3.0)
        }
        Annot::Mosaic { a, b, .. } => {
            let (x0, y0, x1, y1) = norm(*a, *b);
            ((x0, y0), (x1, y1), 2.0)
        }
        Annot::Pen { pts, width, .. } | Annot::Marker { pts, width, .. } => {
            let (lo, hi) = points_span(pts);
            let w = if matches!(a, Annot::Marker { .. }) { width * MARKER_WIDTH } else { *width };
            (lo, hi, w * 2.0 + 2.0)
        }
        Annot::Text { pos, text, preedit, size, bold, .. } => {
            let h = (size * 1.3).ceil();
            let w = text_w(cx, text, preedit, *size, *bold).unwrap_or(size * 8.0);
            (*pos, (pos.0 + w + 6.0, pos.1 + h), size * 0.55 + 6.0)
        }
        Annot::Number { pos, radius, .. } => ((pos.0 - radius, pos.1 - radius), (pos.0 + radius, pos.1 + radius), 3.0),
    };
    [(lo.0 - pad).floor() as i32, (lo.1 - pad).floor() as i32, (hi.0 + pad).ceil() as i32, (hi.1 + pad).ceil() as i32]
}

fn union(a: Option<[i32; 4]>, b: Option<[i32; 4]>) -> Option<[i32; 4]> {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (a, b) => a.or(b),
    }
}

fn renumber(items: &mut [Annot]) -> bool {
    let mut next = 1;
    let mut changed = false;
    for a in items {
        if let Annot::Number { n, .. } = a {
            changed |= *n != next;
            *n = next;
            next += 1;
        }
    }
    changed
}

fn chrome_area(a: &Annot, cx: &Ctx) -> Option<[i32; 4]> {
    let b = a.bounds(cx)?;
    let pad = 12.0 * cx.scale;
    Some([(b[0] - pad).floor() as i32, (b[1] - pad).floor() as i32, (b[2] + pad).ceil() as i32, (b[3] + pad).ceil() as i32])
}

const ACCENT: Color = [92, 140, 255];

fn draw_chrome(pix: &mut Pixmap, a: &Annot, cx: &Ctx) {
    let sc = cx.scale;
    let tf = Transform::from_translate(-(cx.origin.0 as f32), -(cx.origin.1 as f32));
    match a {
        Annot::Text { .. } => {
            if let Some(b) = a.bounds(cx) {
                let pad = 2.0 * sc;
                if let Some(r) = Rect::from_ltrb(b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad) {
                    pix.stroke_path(&PathBuilder::from_rect(r), &paint(ACCENT, 255), &stroke(1.2 * sc), tf, None);
                }
            }
        }
        Annot::Number { pos, radius, .. } => {
            if let Some(c) = PathBuilder::from_circle(pos.0, pos.1, radius + 2.5 * sc) {
                pix.stroke_path(&c, &paint(ACCENT, 255), &stroke(1.2 * sc), tf, None);
            }
        }
        _ => {}
    }
    for h in a.handles(cx) {
        if let Some(c) = PathBuilder::from_circle(h.0, h.1, 4.5 * sc) {
            pix.fill_path(&c, &paint([255, 255, 255], 255), FillRule::Winding, tf, None);
            pix.stroke_path(&c, &paint(ACCENT, 255), &stroke(1.5 * sc), tf, None);
        }
    }
}

pub type Region = [u32; 4];

const SCRATCH_KEEP: usize = 1 << 20;

fn clip_text(a: &mut Annot) {
    if let Annot::Text { text, preedit, .. } = a {
        for s in [text, preedit] {
            if let Some((i, _)) = s.char_indices().nth(crate::shaped::MAX_CHARS) {
                s.truncate(i);
            }
        }
    }
}

enum Undo {
    Added(usize),
    Removed(usize, Annot),
    Changed(usize, Annot, bool),
}

enum Redo {
    Add(usize, Annot),
    Remove(usize),
    Change(usize, Annot),
}

struct Shown {
    pts: Vec<Pt>,
    brush: f32,
    blur: bool,
    block: f32,
    area: Option<[i32; 4]>,
}

pub struct Layer {
    pub w: u32,
    pub h: u32,
    pub origin: (i32, i32),
    committed: Pixmap,
    work: Pixmap,
    pub items: Vec<Annot>,
    pub live: Option<Annot>,
    selected: Option<usize>,
    lifted: bool,
    undo: Vec<Undo>,
    redo: Vec<Redo>,
    prev: Option<[i32; 4]>,
    full: bool,
    scratch: Vec<u8>,
    shown: Option<Shown>,
}

impl Layer {
    pub fn new(w: u32, h: u32, origin: (i32, i32)) -> Option<Self> {
        Some(Self {
            w,
            h,
            origin,
            committed: Pixmap::new(w, h)?,
            work: Pixmap::new(w, h)?,
            items: Vec::new(),
            live: None,
            selected: None,
            lifted: false,
            undo: Vec::new(),
            redo: Vec::new(),
            prev: None,
            full: false,
            scratch: Vec::new(),
            shown: None,
        })
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn local(&self, b: [i32; 4]) -> [i32; 4] {
        [b[0] - self.origin.0, b[1] - self.origin.1, b[2] - self.origin.0, b[3] - self.origin.1]
    }

    fn rerender(&mut self, dirty: Option<[i32; 4]>, cx: &Ctx) -> Region {
        self.redraw(dirty, cx);
        self.refresh(cx)
    }

    fn redraw(&mut self, dirty: Option<[i32; 4]>, cx: &Ctx) {
        self.shown = None;
        let renumbered = renumber(&mut self.items);
        self.committed.fill(tiny_skia::Color::TRANSPARENT);
        let lifted = if self.lifted { self.selected } else { None };
        for (i, a) in self.items.iter().enumerate() {
            if Some(i) != lifted {
                draw(&mut self.committed, a, cx);
            }
        }
        match dirty {
            Some(d) if !renumbered => self.prev = union(self.prev, Some(self.local(d))),
            _ => self.full = true,
        }
    }

    pub fn set_live(&mut self, live: Option<Annot>, cx: &Ctx) -> Region {
        self.live = live;
        self.refresh(cx)
    }

    pub fn commit(&mut self, mut a: Annot, cx: &Ctx) -> Region {
        if let Some(back) = self.drop_edit(cx) {
            self.redraw(Some(back), cx);
        }
        clip_text(&mut a);
        self.shown = None;
        draw(&mut self.committed, &a, cx);
        self.prev = union(self.prev, Some(self.local(bbox(&a, cx))));
        self.items.push(a);
        self.undo.push(Undo::Added(self.items.len() - 1));
        self.redo.clear();
        self.live = None;
        self.selected = Some(self.items.len() - 1).filter(|&i| self.items[i].editable());
        self.refresh(cx)
    }

    pub fn undo(&mut self, cx: &Ctx) -> Region {
        let back = self.drop_edit(cx);
        let dirty = match self.undo.pop() {
            Some(Undo::Added(i)) if i < self.items.len() => {
                let a = self.items.remove(i);
                let d = bbox(&a, cx);
                self.redo.push(Redo::Add(i, a));
                self.selected = None;
                Some(d)
            }
            Some(Undo::Removed(i, a)) => {
                let d = bbox(&a, cx);
                self.items.insert(i.min(self.items.len()), a);
                self.redo.push(Redo::Remove(i));
                self.selected = None;
                Some(d)
            }
            Some(Undo::Changed(i, old, _)) if i < self.items.len() => {
                let d = union(Some(bbox(&old, cx)), Some(bbox(&self.items[i], cx)));
                let now = std::mem::replace(&mut self.items[i], old);
                self.redo.push(Redo::Change(i, now));
                self.selected = Some(i);
                d
            }
            _ => None,
        };
        if dirty.is_none() && back.is_none() {
            return [0; 4];
        }
        self.rerender(union(back, dirty), cx)
    }

    pub fn redo(&mut self, cx: &Ctx) -> Region {
        let back = self.drop_edit(cx);
        let dirty = match self.redo.pop() {
            Some(Redo::Add(i, a)) => {
                let at = i.min(self.items.len());
                let d = bbox(&a, cx);
                self.items.insert(at, a);
                self.undo.push(Undo::Added(at));
                self.selected = None;
                Some(d)
            }
            Some(Redo::Remove(i)) if i < self.items.len() => {
                let a = self.items.remove(i);
                let d = bbox(&a, cx);
                self.undo.push(Undo::Removed(i, a));
                self.selected = None;
                Some(d)
            }
            Some(Redo::Change(i, new)) if i < self.items.len() => {
                let d = union(Some(bbox(&new, cx)), Some(bbox(&self.items[i], cx)));
                let was = std::mem::replace(&mut self.items[i], new);
                self.undo.push(Undo::Changed(i, was, false));
                self.selected = Some(i);
                d
            }
            _ => None,
        };
        if dirty.is_none() && back.is_none() {
            return [0; 4];
        }
        self.rerender(union(back, dirty), cx)
    }

    pub fn erase_at(&mut self, p: Pt, tol: f32, cx: &Ctx) -> Option<Region> {
        let i = (0..self.items.len()).rev().find(|&i| self.items[i].hit(p, tol, cx))?;
        let back = self.drop_edit(cx);
        let a = self.items.remove(i);
        let dirty = union(back, Some(bbox(&a, cx)));
        self.undo.push(Undo::Removed(i, a));
        self.redo.clear();
        self.selected = None;
        Some(self.rerender(dirty, cx))
    }


    pub fn selected(&self) -> Option<&Annot> {
        self.items.get(self.selected?)
    }

    fn drop_edit(&mut self, cx: &Ctx) -> Option<[i32; 4]> {
        if !self.lifted {
            return None;
        }
        self.lifted = false;
        self.live = None;
        Some(self.selected.and_then(|i| self.items.get(i)).map_or([0; 4], |a| bbox(a, cx)))
    }

    pub fn hit_any(&self, p: Pt, tol: f32, cx: &Ctx) -> bool {
        self.items.iter().enumerate().any(|(i, a)| a.editable() && (a.hit(p, tol, cx) || self.selected == Some(i) && a.contains(p, tol)))
    }

    pub fn handle_at(&self, p: Pt, reach: f32, cx: &Ctx) -> Option<usize> {
        let d = |h: &Pt| (h.0 - p.0).hypot(h.1 - p.1);
        let hs = self.selected()?.handles(cx);
        (0..hs.len()).filter(|&i| d(&hs[i]) <= reach).min_by(|&a, &b| d(&hs[a]).total_cmp(&d(&hs[b])))
    }

    pub fn select_at(&mut self, p: Pt, tol: f32, cx: &Ctx) -> Option<Region> {
        let sel = self.selected.filter(|&i| i < self.items.len());
        let keep = sel.filter(|&i| self.items[i].hit(p, tol, cx));
        let held = sel.filter(|&i| keep.is_none() && self.items[i].contains(p, tol));
        let i = keep
            .or_else(|| (0..self.items.len()).rev().find(|&i| self.items[i].editable() && self.items[i].hit(p, tol, cx) && (held.is_none() || !self.items[i].is_area())))
            .or(held)?;
        let back = self.drop_edit(cx);
        self.selected = Some(i);
        if back.is_some() {
            return Some(self.rerender(back, cx));
        }
        Some(self.refresh(cx))
    }

    pub fn deselect(&mut self, cx: &Ctx) -> Region {
        if self.selected.is_none() {
            return [0; 4];
        }
        let back = self.drop_edit(cx);
        self.selected = None;
        if back.is_some() {
            return self.rerender(back, cx);
        }
        self.refresh(cx)
    }

    pub fn begin_edit(&mut self, cx: &Ctx) -> Option<(Annot, Region)> {
        let a = self.selected()?.clone();
        self.lifted = true;
        self.live = Some(a.clone());
        let region = self.rerender(Some(bbox(&a, cx)), cx);
        Some((a, region))
    }

    pub fn end_edit(&mut self, changed: bool, cx: &Ctx) -> Region {
        let live = self.live.take();
        let Some(i) = self.selected.filter(|i| self.lifted && *i < self.items.len()) else {
            self.lifted = false;
            return [0; 4];
        };
        self.lifted = false;
        if let (true, Some(new)) = (changed, live) {
            let old = std::mem::replace(&mut self.items[i], new);
            self.undo.push(Undo::Changed(i, old, false));
            self.redo.clear();
        }
        self.rerender(Some(bbox(&self.items[i], cx)), cx)
    }

    pub fn restyle_selected(&mut self, st: &Style, sc: f32, cx: &Ctx) -> Option<Region> {
        let i = self.selected.filter(|_| !self.lifted)?;
        let mut new = self.items.get(i)?.clone();
        new.set_look(st, sc);
        if new == self.items[i] {
            return None;
        }
        let old = std::mem::replace(&mut self.items[i], new);
        let dirty = union(Some(bbox(&old, cx)), Some(bbox(&self.items[i], cx)));
        match self.undo.last() {
            Some(Undo::Changed(j, first, true)) if *j == i => {
                if *first == self.items[i] {
                    self.undo.pop();
                }
            }
            _ => self.undo.push(Undo::Changed(i, old, true)),
        }
        self.redo.clear();
        Some(self.rerender(dirty, cx))
    }

    pub fn delete_selected(&mut self, cx: &Ctx) -> Option<Region> {
        let back = self.drop_edit(cx);
        let Some(i) = self.selected.take().filter(|i| *i < self.items.len()) else {
            return back.map(|b| self.rerender(Some(b), cx));
        };
        let a = self.items.remove(i);
        let dirty = union(back, Some(bbox(&a, cx)));
        self.undo.push(Undo::Removed(i, a));
        self.redo.clear();
        Some(self.rerender(dirty, cx))
    }

    pub fn live_text_mut(&mut self) -> Option<&mut String> {
        match &mut self.live {
            Some(Annot::Text { text, .. }) => Some(text),
            _ => None,
        }
    }

    pub fn set_live_preedit(&mut self, s: String) {
        if let Some(Annot::Text { preedit, .. }) = &mut self.live {
            *preedit = s;
        }
    }

    pub fn set_live_bold(&mut self, b: bool) {
        if let Some(Annot::Text { bold, .. }) = &mut self.live {
            *bold = b;
        }
    }

    pub fn set_live_color(&mut self, c: Color) {
        if let Some(Annot::Text { color, .. }) = &mut self.live {
            *color = c;
        }
    }

    pub fn set_live_size(&mut self, new: f32) -> bool {
        match &mut self.live {
            Some(Annot::Text { pos, size, .. }) if *size != new => {
                pos.1 += (*size - new) * 0.7;
                *size = new;
                true
            }
            _ => false,
        }
    }

    fn stroke_change(&self) -> Option<[i32; 4]> {
        let (Some(Annot::Mosaic { pts, brush, blur, block, .. }), Some(shown)) = (&self.live, &self.shown) else { return None };
        if *brush <= 0.0 || *brush != shown.brush || *blur != shown.blur || *block != shown.block || self.prev != shown.area {
            return None;
        }
        let same = pts.iter().zip(&shown.pts).take_while(|(a, b)| a == b).count();
        let from = same.saturating_sub(3);
        let (now, was) = (points_box(&pts[from.min(pts.len())..]), points_box(&shown.pts[from.min(shown.pts.len())..]));
        let [lx, ly, hx, hy] = now.into_iter().chain(was).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])?;
        let (lo, hi) = ((lx, ly), (hx, hy));
        let pad = brush / 2.0 + 3.0;
        Some(self.local([(lo.0 - pad).floor() as i32, (lo.1 - pad).floor() as i32, (hi.0 + pad).ceil() as i32, (hi.1 + pad).ceil() as i32]))
    }

    pub fn refresh(&mut self, cx: &Ctx) -> Region {
        if let Some(a) = &mut self.live {
            clip_text(a);
        }
        let chrome: Option<&Annot> = if self.lifted { self.live.as_ref() } else { self.selected.and_then(|i| self.items.get(i)) };
        let chrome_box = chrome.and_then(|a| chrome_area(a, cx)).map(|b| self.local(b));
        let nb = union(self.live.as_ref().map(|a| self.local(bbox(a, cx))), chrome_box);
        let part = if chrome.is_none() && !self.full { self.stroke_change() } else { None };
        let mut region = part.or_else(|| union(self.prev, nb));
        if self.full {
            region = Some([0, 0, self.w as i32, self.h as i32]);
            self.full = false;
        }
        self.prev = nb;
        self.shown = match &self.live {
            Some(Annot::Mosaic { pts, brush, blur, block, .. }) if *brush > 0.0 && chrome.is_none() => {
                Some(Shown { pts: pts.clone(), brush: *brush, blur: *blur, block: *block, area: nb })
            }
            _ => None,
        };
        let Some(r) = region else { return [0; 4] };
        let (x0, y0) = (r[0].max(0), r[1].max(0));
        let (x1, y1) = (r[2].min(self.w as i32), r[3].min(self.h as i32));
        if x1 <= x0 || y1 <= y0 {
            return [0; 4];
        }
        let stride = self.w as usize * 4;
        let (src, dst) = (self.committed.data(), self.work.data_mut());
        for y in y0..y1 {
            let o = y as usize * stride;
            dst[o + x0 as usize * 4..o + x1 as usize * 4].copy_from_slice(&src[o + x0 as usize * 4..o + x1 as usize * 4]);
        }
        if let Some(a) = &self.live {
            let (ox, oy) = self.origin;
            draw_part(&mut self.work, a, cx, part.map(|p| [p[0] + ox, p[1] + oy, p[2] + ox, p[3] + oy]));
        }
        if let Some(a) = chrome {
            draw_chrome(&mut self.work, a, cx);
        }
        [x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32]
    }

    pub fn region_bytes(&mut self, r: Region) -> &[u8] {
        let stride = self.w as usize * 4;
        if r[0] == 0 && r[2] == self.w {
            return &self.work.data()[r[1] as usize * stride..(r[1] + r[3]) as usize * stride];
        }
        let need = r[2] as usize * r[3] as usize * 4;
        if self.scratch.capacity() > need.max(SCRATCH_KEEP) * 4 {
            self.scratch = Vec::new();
        }
        self.scratch.clear();
        for y in r[1]..r[1] + r[3] {
            let o = y as usize * stride + r[0] as usize * 4;
            self.scratch.extend_from_slice(&self.work.data()[o..o + r[2] as usize * 4]);
        }
        &self.scratch
    }

    pub fn composite_over_at(&self, base: &mut [u8], bw: usize, bh: usize, off: (i32, i32)) {
        let (lw, lh) = (self.w as i32, self.h as i32);
        let src = self.committed.data();
        for y in 0..bh {
            let ly = y as i32 + off.1;
            if ly < 0 || ly >= lh {
                continue;
            }
            for x in 0..bw {
                let lx = x as i32 + off.0;
                if lx < 0 || lx >= lw {
                    continue;
                }
                let l = &src[(ly as usize * lw as usize + lx as usize) * 4..][..4];
                let a = l[3] as u32;
                if a == 0 {
                    continue;
                }
                let b = &mut base[(y * bw + x) * 4..][..4];
                for k in 0..3 {
                    b[k] = (l[k] as u32 + b[k] as u32 * (255 - a) / 255).min(255) as u8;
                }
            }
        }
    }
}
