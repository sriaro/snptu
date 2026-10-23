
use crate::annot::{PALETTE, Style, Tool};
use crate::menu::paint;
use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path, PathBuilder, Pixmap,
    Point, SpreadMode, Stroke, StrokeDash, Transform,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Tool(Tool),
    Undo,
    Redo,
    Pin,
    Save,
    Close,
    Confirm,
    Slider,
    Strength,
    Swatch(usize),
    Fill(u8),
    ArrowStyle(u8),
    Dash,
    MosaicKind(u8),
    MosaicShape(u8),
    Bold,
    CustomColor,
}

pub struct UiState {
    pub tool: Option<Tool>,
    pub style: Style,
    pub can_undo: bool,
    pub can_redo: bool,
    pub knob: Option<f32>,
    pub knob2: Option<f32>,
    pub dark: bool,
}

pub struct Toolbar {
    pub items: Vec<(Item, [f32; 4])>,
    seps: Vec<(f32, [f32; 4])>,
    pub w: u32,
    pub h: u32,
    pub margin: f32,
    pub pill: [f32; 4],
    pub opt_pill: Option<[f32; 4]>,
    scale: f32,
}

const TEXT: [u8; 3] = [239, 243, 250];
const TEXT_DIM: [u8; 3] = [151, 165, 186];

const CORNER_SCALE: f32 = 1.3;

pub fn frame(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let half = (w.min(h)) / 2.0;
    let re = (r * CORNER_SCALE).min(half);
    if re < 0.5 {
        return tiny_skia::Rect::from_xywh(x, y, w, h).map(PathBuilder::from_rect);
    }
    let n = if re >= half - 0.01 { 2.6_f32 } else { 4.0_f32 };
    let e = 2.0 / n;
    const SEG: usize = 14;
    let mut pb = PathBuilder::new();
    let half_pi = std::f32::consts::FRAC_PI_2;
    let corners = [
        (x + re, y + re, -1.0, -1.0, 0.0, half_pi),
        (x + w - re, y + re, 1.0, -1.0, half_pi, 0.0),
        (x + w - re, y + h - re, 1.0, 1.0, 0.0, half_pi),
        (x + re, y + h - re, -1.0, 1.0, half_pi, 0.0),
    ];
    let mut first = true;
    for (cx, cy, sx, sy, a0, a1) in corners {
        for i in 0..=SEG {
            let a = a0 + (a1 - a0) * i as f32 / SEG as f32;
            let (c, s) = (a.cos().max(0.0).powf(e), a.sin().max(0.0).powf(e));
            let (px, py) = (cx + sx * re * c, cy + sy * re * s);
            if first {
                pb.move_to(px, py);
                first = false;
            } else {
                pb.line_to(px, py);
            }
        }
    }
    pb.close();
    pb.finish()
}

pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = r * 0.5523;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn poly(pts: &[(f32, f32)]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.finish()
}

impl Toolbar {
    pub fn new(scale: f32, tool: Option<Tool>, panel_above: bool) -> Self {
        let sc = scale;
        let (m, pad, btn, gap, sep) = (14.0 * sc, 6.0 * sc, 30.0 * sc, 1.0 * sc, 12.0 * sc);
        let end_pad = 11.0 * sc;
        let mut items = Vec::new();
        let mut seps: Vec<(f32, [f32; 4])> = Vec::new();

        let main_h = btn + 2.0 * pad;
        let panel_h = 46.0 * sc;
        let panel_gap = 6.0 * sc;
        let main_y = if tool.is_some() && panel_above { m + panel_h + panel_gap } else { m };
        let y = main_y + pad;
        let mut x = m + end_pad;
        let group = |items: &mut Vec<(Item, [f32; 4])>, x: &mut f32, list: &[Item]| {
            for it in list {
                items.push((*it, [*x, y, btn, btn]));
                *x += btn + gap;
            }
        };
        let tools: Vec<Item> = Tool::ALL.iter().map(|t| Item::Tool(*t)).collect();
        group(&mut items, &mut x, &tools);
        let main_seps_from = seps.len();
        x += sep / 2.0 - gap;
        seps.push((x, [0.0; 4]));
        x += sep / 2.0;
        group(&mut items, &mut x, &[Item::Undo, Item::Redo]);
        x += sep / 2.0 - gap;
        seps.push((x, [0.0; 4]));
        x += sep / 2.0;
        group(&mut items, &mut x, &[Item::Pin, Item::Save]);
        x += sep / 2.0 - gap;
        seps.push((x, [0.0; 4]));
        x += sep / 2.0;
        group(&mut items, &mut x, &[Item::Close, Item::Confirm]);
        let main_w = x - gap + end_pad - m;
        let pill = [m, main_y, main_w, main_h];
        for sp in &mut seps[main_seps_from..] {
            sp.1 = pill;
        }

        let mut opt_pill = None;
        let mut total_w = main_w;
        if let Some(t) = tool {
            let panel_y = if panel_above { m } else { main_y + main_h + panel_gap };
            let cy = panel_y + panel_h / 2.0;
            let mut px = m + end_pad;
            let obtn = 26.0 * sc;
            items.push((Item::Slider, [px, cy - obtn / 2.0, 150.0 * sc, obtn]));
            px += 150.0 * sc;
            if t == Tool::Mosaic {
                items.push((Item::Strength, [px + 6.0 * sc, cy - obtn / 2.0, 130.0 * sc, obtn]));
                px += 136.0 * sc;
            }
            let mut opts: Vec<Item> = Vec::new();
            match t {
                Tool::Rect | Tool::Ellipse => opts.extend([Item::Fill(0), Item::Fill(1), Item::Fill(2), Item::Dash]),
                Tool::Arrow => opts.extend([Item::ArrowStyle(0), Item::ArrowStyle(1), Item::ArrowStyle(2), Item::ArrowStyle(3), Item::ArrowStyle(4), Item::Dash]),
                Tool::Pen => opts.push(Item::Dash),
                Tool::Mosaic => opts.extend([Item::MosaicShape(0), Item::MosaicShape(1), Item::MosaicKind(0), Item::MosaicKind(1)]),
                Tool::Text => opts.push(Item::Bold),
                Tool::Marker | Tool::Number | Tool::Eraser => {}
            }
            let opt_seps_from = seps.len();
            if !opts.is_empty() {
                px += sep / 2.0;
                seps.push((px, [0.0; 4]));
                px += sep / 2.0;
                for it in opts {
                    items.push((it, [px, cy - obtn / 2.0, obtn, obtn]));
                    px += obtn + gap;
                }
                px -= gap;
            }
            if !matches!(t, Tool::Mosaic | Tool::Eraser) {
                px += sep / 2.0;
                seps.push((px, [0.0; 4]));
                px += sep / 2.0;
                let (sw, sg) = (15.0 * sc, 3.0 * sc);
                for (i, _) in PALETTE.iter().enumerate() {
                    let (col, row) = ((i % 8) as f32, (i / 8) as f32);
                    let sx = px + col * (sw + sg);
                    let sy = cy - (2.0 * sw + sg) / 2.0 + row * (sw + sg);
                    items.push((Item::Swatch(i), [sx, sy, sw, sw]));
                }
                items.push((Item::CustomColor, [px + 8.0 * (sw + sg), cy - (2.0 * sw + sg) / 2.0, sw, 2.0 * sw + sg]));
                px += 9.0 * (sw + sg) - sg;
            }
            let panel_w = px + end_pad - m;
            let op = [m, panel_y, panel_w, panel_h];
            for sp in &mut seps[opt_seps_from..] {
                sp.1 = op;
            }
            opt_pill = Some(op);
            total_w = total_w.max(panel_w);
        }
        let total_h = match opt_pill {
            Some(_) => main_h + panel_gap + panel_h,
            None => main_h,
        };
        Self {
            items,
            seps,
            w: (total_w + 2.0 * m).ceil() as u32,
            h: (total_h + 2.0 * m).ceil() as u32,
            margin: m,
            pill,
            opt_pill,
            scale,
        }
    }

    pub fn slider_level(&self, index: usize, x: f32) -> u32 {
        1 + (self.slider_pos(index, x) * 11.0).round() as u32
    }

    pub fn slider_pos(&self, index: usize, x: f32) -> f32 {
        let r = self.items[index].1;
        let (t0, t1) = (r[0] + 34.0 * self.scale, r[0] + r[2] - 10.0 * self.scale);
        ((x - t0) / (t1 - t0)).clamp(0.0, 1.0)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.items
            .iter()
            .position(|(_, r)| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3])
    }

    pub fn in_pill(&self, x: f32, y: f32) -> bool {
        let inside = |p: [f32; 4]| x >= p[0] && x < p[0] + p[2] && y >= p[1] && y < p[1] + p[3];
        inside(self.pill) || self.opt_pill.is_some_and(inside)
    }

    fn draw_pill(&self, pix: &mut Pixmap, pill: [f32; 4], dark: bool) -> Option<()> {
        let sc = self.scale;
        let id = Transform::identity();
        let [px, py, pw, ph] = pill;
        let radius = ph / 2.0;
        for i in 1..=12 {
            let g = i as f32 * 0.9 * sc;
            if let Some(p) = frame(px - g, py - g + 3.0 * sc, pw + 2.0 * g, ph + 2.0 * g, radius + g) {
                pix.fill_path(&p, &paint([0, 5, 20], 3), FillRule::Winding, id, None);
            }
        }
        let body = frame(px, py, pw, ph, radius)?;
        let clear = Paint { blend_mode: BlendMode::Clear, ..Default::default() };
        pix.fill_path(&body, &clear, FillRule::Winding, id, None);
        if let Some(sheen) = LinearGradient::new(
            Point::from_xy(0.0, py),
            Point::from_xy(0.0, py + ph * 0.55),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, if dark { 46 } else { 110 })),
                GradientStop::new(1.0, Color::from_rgba8(255, 255, 255, 0)),
            ],
            SpreadMode::Pad,
            id,
        ) {
            let p = Paint { shader: sheen, anti_alias: true, ..Default::default() };
            pix.fill_path(&body, &p, FillRule::Winding, id, None);
        }
        if let Some(edge) = LinearGradient::new(
            Point::from_xy(0.0, py),
            Point::from_xy(0.0, py + ph),
            vec![
                GradientStop::new(0.0, Color::from_rgba8(255, 255, 255, if dark { 96 } else { 220 })),
                GradientStop::new(0.5, if dark { Color::from_rgba8(255, 255, 255, 34) } else { Color::from_rgba8(20, 32, 64, 26) }),
                GradientStop::new(1.0, if dark { Color::from_rgba8(255, 255, 255, 52) } else { Color::from_rgba8(20, 32, 64, 46) }),
            ],
            SpreadMode::Pad,
            id,
        ) {
            let p = Paint { shader: edge, anti_alias: true, ..Default::default() };
            pix.stroke_path(&body, &p, &Stroke { width: 1.0 * sc.clamp(1.0, 1.5), ..Default::default() }, id, None);
        }
        Some(())
    }

    pub fn draw(&self, pix: &mut Pixmap, hover: Option<usize>, st: &UiState) {
        let sc = self.scale;
        let id = Transform::identity();
        pix.fill(tiny_skia::Color::TRANSPARENT);
        let (ink, ink_dim, line): ([u8; 3], [u8; 3], [u8; 3]) = if st.dark { (TEXT, TEXT_DIM, [255, 255, 255]) } else { ([20, 30, 52], [112, 124, 148], [20, 32, 64]) };
        let thin = if st.dark { 1.0 } else { 0.75 };
        if self.draw_pill(pix, self.pill, st.dark).is_none() {
            return;
        }
        if let Some(op) = self.opt_pill {
            let _ = self.draw_pill(pix, op, st.dark);
        }
        for (sx, pill) in &self.seps {
            let (py, ph) = (pill[1], pill[3]);
            if let Some(p) = poly(&[(*sx, py + 14.0 * sc), (*sx, py + ph - 14.0 * sc)]) {
                pix.stroke_path(&p, &paint(line, (36.0 * if st.dark { 1.0 } else { 1.3 }) as u8), &Stroke { width: 1.0, ..Default::default() }, id, None);
            }
        }

        let acc = crate::theme::accent(st.dark);
        let accent = |r: [f32; 4]| -> Paint<'static> {
            let mut fill = Paint { anti_alias: true, ..Default::default() };
            if let Some(g) = LinearGradient::new(
                Point::from_xy(r[0], r[1]),
                Point::from_xy(r[0] + r[2], r[1] + r[3]),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(acc.0[0], acc.0[1], acc.0[2], 255)),
                    GradientStop::new(1.0, Color::from_rgba8(acc.1[0], acc.1[1], acc.1[2], 255)),
                ],
                SpreadMode::Pad,
                id,
            ) {
                fill.shader = g;
            }
            fill
        };

        for (i, (item, r)) in self.items.iter().enumerate() {
            let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
            let hovered = hover == Some(i);
            let ignored = match (item, st.tool) {
                (Item::Slider, Some(t)) => !st.style.uses_size(t),
                (Item::Dash, Some(t)) => !st.style.uses_dash(t),
                _ => false,
            };
            let active = match item {
                Item::Tool(t) => st.tool == Some(*t),
                Item::Fill(v) => st.style.fill == *v,
                Item::ArrowStyle(v) => st.style.arrow == *v,
                Item::MosaicKind(v) => st.style.mosaic == *v,
                Item::MosaicShape(v) => st.style.fill == *v,
                Item::Dash => st.style.dash != 0 && !ignored,
                Item::Bold => st.style.bold != 0,
                _ => false,
            };
            let disabled = (*item == Item::Undo && !st.can_undo) || (*item == Item::Redo && !st.can_redo) || ignored;

            match item {
                Item::Swatch(ci) => {
                    if let Some(p) = rrect(r[0], r[1], r[2], r[3], 4.0 * sc) {
                        pix.fill_path(&p, &paint(PALETTE[*ci], 255), FillRule::Winding, id, None);
                        pix.stroke_path(&p, &paint(line, (70.0 * thin) as u8), &Stroke { width: 1.0, ..Default::default() }, id, None);
                        swatch_ring(pix, r, sc, st.style.color == *ci && st.style.custom.is_none(), hovered, st.dark, line);
                    }
                    continue;
                }
                Item::CustomColor => {
                    if let Some(p) = rrect(r[0], r[1], r[2], r[3], 4.0 * sc) {
                        match st.style.custom {
                            Some(c) => pix.fill_path(&p, &paint(c, 255), FillRule::Winding, id, None),
                            None => {
                                let stops: Vec<GradientStop> = [
                                    (0.0, [235, 64, 52]),
                                    (0.2, [250, 176, 5]),
                                    (0.4, [60, 190, 90]),
                                    (0.6, [20, 190, 230]),
                                    (0.8, [70, 90, 230]),
                                    (1.0, [180, 70, 200]),
                                ]
                                .iter()
                                .map(|(t, c)| GradientStop::new(*t, Color::from_rgba8(c[0], c[1], c[2], 255)))
                                .collect();
                                if let Some(sh) = LinearGradient::new(Point::from_xy(r[0], r[1]), Point::from_xy(r[0], r[1] + r[3]), stops, SpreadMode::Pad, id) {
                                    let fill = Paint { shader: sh, anti_alias: true, ..Default::default() };
                                    pix.fill_path(&p, &fill, FillRule::Winding, id, None);
                                }
                                let (cx, cy, k) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0, 3.2 * sc);
                                if let Some(plus) = poly(&[(cx - k, cy), (cx + k, cy)]) {
                                    pix.stroke_path(&plus, &paint([255, 255, 255], 240), &Stroke { width: 1.6 * sc, line_cap: LineCap::Round, ..Default::default() }, id, None);
                                }
                                if let Some(plus) = poly(&[(cx, cy - k), (cx, cy + k)]) {
                                    pix.stroke_path(&plus, &paint([255, 255, 255], 240), &Stroke { width: 1.6 * sc, line_cap: LineCap::Round, ..Default::default() }, id, None);
                                }
                            }
                        }
                        pix.stroke_path(&p, &paint(line, (70.0 * thin) as u8), &Stroke { width: 1.0, ..Default::default() }, id, None);
                        swatch_ring(pix, r, sc, st.style.custom.is_some(), hovered, st.dark, line);
                    }
                    continue;
                }
                Item::Slider | Item::Strength => {
                    let fade = |a: u8| if ignored { a / 2 } else { a };
                    let strength = *item == Item::Strength;
                    let level = if strength { st.style.strength as u32 } else { st.style.width }.clamp(1, 12) as f32;
                    if strength {
                        let cell = (1.6 + level * 0.3) * sc;
                        let (gx, gy) = (r[0] + 14.0 * sc - cell, cy - cell);
                        for (dx, dy, a) in [(0.0, 0.0, 255u8), (1.0, 1.0, 255), (1.0, 0.0, 90), (0.0, 1.0, 90)] {
                            if let Some(rc) = tiny_skia::Rect::from_xywh(gx + dx * cell, gy + dy * cell, cell, cell) {
                                pix.fill_rect(rc, &paint(ink, a), id, None);
                            }
                        }
                    } else {
                        let dot_r = (1.5 + level * 0.55) * sc;
                        if let Some(p) = PathBuilder::from_circle(r[0] + 14.0 * sc, cy, dot_r) {
                            let c = st.style.rgb();
                            pix.fill_path(&p, &paint(c, fade(255)), FillRule::Winding, id, None);
                            pix.stroke_path(&p, &paint(line, fade((70.0 * thin) as u8)), &Stroke { width: 1.0, ..Default::default() }, id, None);
                        }
                    }
                    let (t0, t1) = (r[0] + 34.0 * sc, r[0] + r[2] - 10.0 * sc);
                    if let Some(track) = poly(&[(t0, cy), (t1, cy)]) {
                        let round = Stroke { width: 3.0 * sc, line_cap: LineCap::Round, ..Default::default() };
                        pix.stroke_path(&track, &paint(line, fade(if st.dark { 60 } else { 50 })), &round, id, None);
                        let exact = if strength { st.knob2 } else { st.knob };
                        let kx = t0 + (t1 - t0) * exact.unwrap_or((level - 1.0) / 11.0);
                        if let Some(fillp) = poly(&[(t0, cy), (kx, cy)]).filter(|_| !ignored) {
                            pix.stroke_path(&fillp, &accent([t0, cy, kx - t0 + 1.0, 1.0]), &round, id, None);
                        }
                        if let Some(knob) = PathBuilder::from_circle(kx, cy, 6.0 * sc) {
                            pix.fill_path(&knob, &paint([0, 6, 20], fade(70)), FillRule::Winding, id, None);
                            pix.fill_path(&knob, &paint([255, 255, 255], fade(255)), FillRule::Winding, id, None);
                        }
                    }
                    continue;
                }
                _ => {}
            }

            if active {
                if let Some(p) = rrect(r[0], r[1], r[2], r[3], 11.0 * sc) {
                    pix.fill_path(&p, &accent(*r), FillRule::Winding, id, None);
                }
            } else if hovered
                && !disabled
                && let Some(p) = rrect(r[0], r[1], r[2], r[3], 11.0 * sc)
            {
                pix.fill_path(&p, &paint(line, if st.dark { 30 } else { 24 }), FillRule::Winding, id, None);
            }
            let (col, alpha) = match item {
                _ if active => ([255, 255, 255], 255),
                _ if disabled => (ink_dim, 110),
                Item::Confirm => (crate::theme::success(st.dark), 255),
                _ => (ink, 240),
            };
            icon(pix, *item, cx, cy, sc, col, alpha);
        }
    }
}

fn swatch_ring(pix: &mut Pixmap, r: &[f32; 4], sc: f32, chosen: bool, hovered: bool, dark: bool, line: [u8; 3]) {
    let id = Transform::identity();
    if chosen {
        if let Some(ring) = rrect(r[0] - 2.0 * sc, r[1] - 2.0 * sc, r[2] + 4.0 * sc, r[3] + 4.0 * sc, 6.0 * sc) {
            pix.stroke_path(&ring, &paint(if dark { [255, 255, 255] } else { [20, 32, 64] }, 240), &Stroke { width: 1.6 * sc, ..Default::default() }, id, None);
        }
    } else if hovered && let Some(ring) = rrect(r[0] - 1.5 * sc, r[1] - 1.5 * sc, r[2] + 3.0 * sc, r[3] + 3.0 * sc, 5.5 * sc) {
        pix.stroke_path(&ring, &paint(line, 110), &Stroke { width: 1.2 * sc, ..Default::default() }, id, None);
    }
}

fn icon(pix: &mut Pixmap, item: Item, cx: f32, cy: f32, k: f32, col: [u8; 3], alpha: u8) {
    let id = Transform::identity();
    let p = paint(col, alpha);
    let st = Stroke { width: 1.7 * k, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    let line = |pix: &mut Pixmap, pts: &[(f32, f32)]| {
        if let Some(path) = poly(pts) {
            pix.stroke_path(&path, &p, &st, id, None);
        }
    };
    match item {
        Item::Tool(Tool::Rect) => {
            if let Some(path) = rrect(cx - 8.5 * k, cy - 7.0 * k, 17.0 * k, 14.0 * k, 3.0 * k) {
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Tool(Tool::Arrow) => {
            line(pix, &[(cx - 6.6 * k, cy + 6.6 * k), (cx + 6.6 * k, cy - 6.6 * k)]);
            line(pix, &[(cx + 0.6 * k, cy - 6.6 * k), (cx + 6.6 * k, cy - 6.6 * k), (cx + 6.6 * k, cy - 0.6 * k)]);
        }
        Item::Tool(Tool::Pen) => {
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 8.0 * k, cy + 4.0 * k);
            pb.cubic_to(cx - 5.0 * k, cy - 11.0 * k, cx + 1.0 * k, cy + 11.0 * k, cx + 8.0 * k, cy - 4.0 * k);
            if let Some(path) = pb.finish() {
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Tool(Tool::Ellipse) => {
            if let Some(path) = PathBuilder::from_oval(tiny_skia::Rect::from_xywh(cx - 8.5 * k, cy - 6.8 * k, 17.0 * k, 13.6 * k).unwrap_or_else(|| tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap())) {
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Tool(Tool::Marker) => {
            let g = |x: f32, y: f32| (cx + (x - 12.0) * k * 0.82, cy + (y - 12.0) * k * 0.82);
            line(pix, &[g(9.0, 11.0), g(3.0, 17.0), g(3.0, 20.0), g(12.0, 20.0), g(15.0, 17.0)]);
            let mut pb = PathBuilder::new();
            let m = g(22.0, 12.0);
            pb.move_to(m.0, m.1);
            let q = g(17.4, 16.6);
            pb.line_to(q.0, q.1);
            let (c, e) = (g(16.0, 18.0), g(14.6, 16.6));
            pb.quad_to(c.0, c.1, e.0, e.1);
            let q = g(9.4, 11.4);
            pb.line_to(q.0, q.1);
            let (c, e) = (g(8.0, 10.0), g(9.4, 8.6));
            pb.quad_to(c.0, c.1, e.0, e.1);
            let q = g(14.0, 4.0);
            pb.line_to(q.0, q.1);
            if let Some(path) = pb.finish() {
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Tool(Tool::Eraser) => {
            let rot = Transform::from_rotate_at(-45.0, cx, cy);
            let (w, h) = (15.4 * k, 8.2 * k);
            if let Some(body) = rrect(cx - w / 2.0, cy - h / 2.0, w, h, 2.2 * k) {
                pix.stroke_path(&body, &p, &st, rot, None);
            }
            let dx = cx - w / 2.0 + w * 0.36;
            if let Some(div) = poly(&[(dx, cy - h / 2.0), (dx, cy + h / 2.0)]) {
                pix.stroke_path(&div, &p, &st, rot, None);
            }
            if let Some(band) = rrect(cx - w / 2.0 + 1.0 * k, cy - h / 2.0 + 1.0 * k, w * 0.36 - 1.0 * k, h - 2.0 * k, 1.2 * k) {
                pix.fill_path(&band, &paint(col, alpha / 3), FillRule::Winding, rot, None);
            }
        }
        Item::Redo => {
            let cx = cx + 0.75 * k;
            line(pix, &[(cx + 3.5 * k, cy - 6.0 * k), (cx + 7.5 * k, cy - 2.0 * k), (cx + 3.5 * k, cy + 2.0 * k)]);
            let mut pb = PathBuilder::new();
            pb.move_to(cx + 7.0 * k, cy - 2.0 * k);
            pb.line_to(cx - 2.0 * k, cy - 2.0 * k);
            pb.cubic_to(cx - 9.0 * k, cy - 2.0 * k, cx - 9.0 * k, cy + 7.0 * k, cx - 2.0 * k, cy + 7.0 * k);
            pb.line_to(cx + 3.0 * k, cy + 7.0 * k);
            if let Some(path) = pb.finish() {
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Pin => {
            let rot = Transform::from_rotate_at(35.0, cx, cy);
            let k = k * 1.22;
            let head = [
                (cx - 2.6 * k, cy - 6.6 * k),
                (cx + 2.6 * k, cy - 6.6 * k),
                (cx + 2.6 * k, cy - 1.0 * k),
                (cx + 5.2 * k, cy + 2.2 * k),
                (cx - 5.2 * k, cy + 2.2 * k),
                (cx - 2.6 * k, cy - 1.0 * k),
                (cx - 2.6 * k, cy - 6.6 * k),
            ];
            if let Some(path) = poly(&head) {
                pix.fill_path(&path, &paint(col, alpha / 4), FillRule::Winding, rot, None);
                pix.stroke_path(&path, &p, &st, rot, None);
            }
            if let Some(path) = poly(&[(cx, cy + 2.2 * k), (cx, cy + 6.6 * k)]) {
                pix.stroke_path(&path, &p, &st, rot, None);
            }
        }
        Item::Tool(Tool::Text) => {
            line(pix, &[(cx - 6.0 * k, cy - 6.6 * k), (cx + 6.0 * k, cy - 6.6 * k)]);
            line(pix, &[(cx, cy - 6.6 * k), (cx, cy + 7.0 * k)]);
        }
        Item::Tool(Tool::Mosaic) => {
            let s = 6.6 * k;
            for (dx, dy, a) in [(0.0, 0.0, alpha), (1.0, 1.0, alpha), (1.0, 0.0, alpha / 3), (0.0, 1.0, alpha / 3)] {
                if let Some(r) = tiny_skia::Rect::from_xywh(cx - s - k / 2.0 + dx * (s + k), cy - s - k / 2.0 + dy * (s + k), s, s) {
                    pix.fill_rect(r, &paint(col, a), id, None);
                }
            }
        }
        Item::Tool(Tool::Number) => {
            if let Some(path) = PathBuilder::from_circle(cx, cy, 7.6 * k) {
                pix.stroke_path(&path, &p, &st, id, None);
            }
            let thin = Stroke { width: 1.5 * k, ..st.clone() };
            let stem = cx + 0.7 * k;
            if let Some(path) = poly(&[(cx - 1.6 * k, cy - 1.9 * k), (stem, cy - 3.3 * k), (stem, cy + 3.3 * k)]) {
                pix.stroke_path(&path, &p, &thin, id, None);
            }
        }
        Item::Undo => {
            let cx = cx - 0.75 * k;
            line(pix, &[(cx - 3.5 * k, cy - 6.0 * k), (cx - 7.5 * k, cy - 2.0 * k), (cx - 3.5 * k, cy + 2.0 * k)]);
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 7.0 * k, cy - 2.0 * k);
            pb.line_to(cx + 2.0 * k, cy - 2.0 * k);
            pb.cubic_to(cx + 9.0 * k, cy - 2.0 * k, cx + 9.0 * k, cy + 7.0 * k, cx + 2.0 * k, cy + 7.0 * k);
            pb.line_to(cx - 3.0 * k, cy + 7.0 * k);
            if let Some(path) = pb.finish() {
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Save => {
            line(pix, &[(cx - 7.0 * k, cy + 1.0 * k), (cx - 7.0 * k, cy + 7.0 * k), (cx + 7.0 * k, cy + 7.0 * k), (cx + 7.0 * k, cy + 1.0 * k)]);
            line(pix, &[(cx, cy - 7.0 * k), (cx, cy + 3.0 * k)]);
            line(pix, &[(cx - 4.0 * k, cy - 1.0 * k), (cx, cy + 3.0 * k), (cx + 4.0 * k, cy - 1.0 * k)]);
        }
        Item::Close => {
            line(pix, &[(cx - 7.0 * k, cy - 7.0 * k), (cx + 7.0 * k, cy + 7.0 * k)]);
            line(pix, &[(cx + 7.0 * k, cy - 7.0 * k), (cx - 7.0 * k, cy + 7.0 * k)]);
        }
        Item::Slider | Item::Strength | Item::Swatch(_) | Item::CustomColor => {}
        Item::Fill(v) => {
            if let Some(path) = rrect(cx - 6.0 * k, cy - 5.0 * k, 12.0 * k, 10.0 * k, 2.0 * k) {
                match v {
                    1 => pix.fill_path(&path, &paint(col, alpha / 3), FillRule::Winding, id, None),
                    2 => pix.fill_path(&path, &p, FillRule::Winding, id, None),
                    _ => {}
                }
                pix.stroke_path(&path, &p, &st, id, None);
            }
        }
        Item::Dash => {
            let dashed = Stroke { width: 1.9 * k, line_cap: LineCap::Butt, dash: StrokeDash::new(vec![3.2 * k, 2.6 * k], 0.0), ..Default::default() };
            if let Some(path) = poly(&[(cx - 8.0 * k, cy), (cx + 8.0 * k, cy)]) {
                pix.stroke_path(&path, &p, &dashed, id, None);
            }
        }
        Item::ArrowStyle(v) => {
            let (a, b) = ((cx - 7.0 * k, cy), (cx + 7.0 * k, cy));
            let head = [(cx + 1.0 * k, cy - 4.5 * k), b, (cx + 1.0 * k, cy + 4.5 * k)];
            match v {
                0 => line(pix, &[a, b]),
                1 => {
                    line(pix, &[a, b]);
                    line(pix, &head);
                }
                2 => {
                    line(pix, &[a, (cx + 1.0 * k, cy)]);
                    if let Some(path) = poly(&[head[0], head[1], head[2], head[0]]) {
                        pix.fill_path(&path, &p, FillRule::Winding, id, None);
                    }
                }
                3 => {
                    line(pix, &[a, (cx + 1.0 * k, cy)]);
                    if let Some(path) = poly(&[head[0], head[1], head[2], head[0]]) {
                        pix.stroke_path(&path, &p, &st, id, None);
                    }
                }
                _ => {
                    let wedge = [
                        (cx - 7.0 * k, cy - 0.5 * k),
                        (cx + 0.5 * k, cy - 2.6 * k),
                        (cx + 0.5 * k, cy - 4.6 * k),
                        (cx + 7.0 * k, cy),
                        (cx + 0.5 * k, cy + 4.6 * k),
                        (cx + 0.5 * k, cy + 2.6 * k),
                        (cx - 7.0 * k, cy + 0.5 * k),
                        (cx - 7.0 * k, cy - 0.5 * k),
                    ];
                    if let Some(path) = poly(&wedge) {
                        pix.fill_path(&path, &p, FillRule::Winding, id, None);
                    }
                }
            }
        }
        Item::MosaicShape(v) => {
            if v == 0 {
                if let Some(path) = poly(&[(cx - 7.0 * k, cy + 5.0 * k), (cx + 3.0 * k, cy - 3.0 * k)]) {
                    let fat = Stroke { width: 5.0 * k, line_cap: LineCap::Round, ..Default::default() };
                    pix.stroke_path(&path, &paint(col, alpha), &fat, id, None);
                }
            } else if let Some(path) = rrect(cx - 6.5 * k, cy - 5.0 * k, 13.0 * k, 10.0 * k, 1.5 * k) {
                let dashed = Stroke { width: 1.7 * k, line_cap: LineCap::Butt, dash: StrokeDash::new(vec![3.0 * k, 2.2 * k], 0.0), ..Default::default() };
                pix.stroke_path(&path, &p, &dashed, id, None);
            }
        }
        Item::MosaicKind(v) => {
            if v == 0 {
                let s = 4.0 * k;
                for (dx, dy, a) in [(0.0, 0.0, alpha), (1.0, 1.0, alpha), (1.0, 0.0, alpha / 3), (0.0, 1.0, alpha / 3)] {
                    if let Some(r) = tiny_skia::Rect::from_xywh(cx - s - 0.5 * k + dx * (s + k), cy - s - 0.5 * k + dy * (s + k), s, s) {
                        pix.fill_rect(r, &paint(col, a), id, None);
                    }
                }
            } else {
                for (r, a) in [(7.5, alpha / 7), (6.0, alpha / 6), (4.5, alpha / 4), (3.0, alpha / 3)] {
                    if let Some(path) = PathBuilder::from_circle(cx, cy, r * k) {
                        pix.fill_path(&path, &paint(col, a), FillRule::Winding, id, None);
                    }
                }
            }
        }
        Item::Bold => {
            let thick = Stroke { width: 2.8 * k, line_cap: LineCap::Butt, ..Default::default() };
            if let Some(path) = poly(&[(cx - 6.0 * k, cy - 5.5 * k), (cx + 6.0 * k, cy - 5.5 * k)]) {
                pix.stroke_path(&path, &p, &thick, id, None);
            }
            if let Some(path) = poly(&[(cx, cy - 5.5 * k), (cx, cy + 7.0 * k)]) {
                pix.stroke_path(&path, &p, &thick, id, None);
            }
        }
        Item::Confirm => {
            line(pix, &[(cx - 8.0 * k, cy + 0.5 * k), (cx - 3.0 * k, cy + 5.5 * k), (cx + 8.0 * k, cy - 5.5 * k)]);
        }
    }
}
