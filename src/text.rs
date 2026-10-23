
use fontdue::{Font, FontSettings, Metrics};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use swash::{
    CacheKey, FontRef, GlyphId, GlyphMetrics,
    scale::{ScaleContext, outline::Outline},
    tag_from_bytes,
    zeno::{Command, PathData},
};

pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub alpha: Vec<u8>,
}

struct Glyph {
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    cov: Coverage,
}

enum Coverage {
    Own(Vec<u8>),
    Kept(Arc<Kept>),
}

impl std::ops::Deref for Coverage {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Own(v) => v,
            Self::Kept(k) => &k.cov,
        }
    }
}

struct Fallback {
    paths: Vec<PathBuf>,
    font: Mutex<Option<Option<Arc<FontFile>>>>,
}

impl Fallback {
    fn get(&self) -> Option<Arc<FontFile>> {
        let mut slot = self.font.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert_with(|| self.paths.iter().find_map(|p| FontFile::read(p)).map(Arc::new)).clone()
    }
}

static FALLBACKS: OnceLock<Vec<Fallback>> = OnceLock::new();

fn fallbacks() -> &'static [Fallback] {
    FALLBACKS.get_or_init(|| {
        let dir = fonts_dir();
        [&["seguisym.ttf"][..], &["Nirmala.ttc", "nirmala.ttf"], &["msyh.ttc"], &["YuGothR.ttc"], &["malgun.ttf"], &["simsun.ttc"]]
            .iter()
            .map(|names| Fallback { paths: names.iter().map(|n| dir.join(n)).collect(), font: Mutex::new(None) })
            .collect()
    })
}

pub fn release_fallbacks() {
    for f in FALLBACKS.get().map_or(&[][..], |v| &v[..]) {
        *f.font.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    for t in [&REGULAR, &SEMIBOLD].into_iter().filter_map(|ui| ui.get()?.as_ref()) {
        *t.whole.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    system::release();
}

struct FontFile {
    bytes: Vec<u8>,
    offset: u32,
    key: CacheKey,
    glyphs: u16,
    scaler: Mutex<(ScaleContext, Outline)>,
    drawn: Mutex<Drawn>,
}

struct Kept {
    m: Metrics,
    cov: Vec<u8>,
}

#[derive(Default)]
struct Drawn {
    glyphs: Vec<((GlyphId, u32), Arc<Kept>)>,
    bytes: usize,
}

const KEPT_GLYPHS: usize = 1024;
const KEPT_BYTES: usize = 1 << 20;

impl FontFile {
    fn read(path: &Path) -> Option<Self> {
        Self::from_bytes(std::fs::read(path).ok()?)
    }

    fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        let face = FontRef::from_index(&bytes, 0)?;
        face.table(tag_from_bytes(b"head"))?;
        let (offset, key, glyphs) = (face.offset, face.key, face.metrics(&[]).glyph_count);
        Some(Self {
            bytes,
            offset,
            key,
            glyphs,
            scaler: Mutex::new((ScaleContext::with_max_entries(1), Outline::new())),
            drawn: Mutex::default(),
        })
    }

    fn face(&self) -> FontRef<'_> {
        FontRef { data: &self.bytes, offset: self.offset, key: self.key }
    }

    fn glyph(&self, c: char) -> Option<GlyphId> {
        Some(self.face().charmap().map(c)).filter(|g| *g != 0 && *g < self.glyphs)
    }

    fn units<'a>(face: &FontRef<'a>, px: f32) -> (f32, GlyphMetrics<'a>) {
        let metrics = face.glyph_metrics(&[]);
        (px / metrics.units_per_em().max(1) as f32, metrics)
    }

    fn advance(&self, g: GlyphId, px: f32) -> f32 {
        let (scale, metrics) = Self::units(&self.face(), px);
        metrics.advance_width(g) * scale
    }

    fn rasterize(&self, g: GlyphId, px: f32) -> (Metrics, Vec<u8>) {
        if px.is_nan() || px <= 0.0 {
            return (Metrics::default(), Vec::new());
        }
        let face = self.face();
        let (scale, metrics) = Self::units(&face, px);
        let mut pen = Lines { scale, ..Default::default() };
        let mut scaler = self.scaler.lock().unwrap_or_else(|e| e.into_inner());
        let (context, outline) = &mut *scaler;
        if context.builder(face).build().scale_outline_into(g, outline) {
            pen.follow(outline.path());
        }
        drop(scaler);
        let (xmin, ymin, width, height, cov) = pen.rasterize();
        let m = Metrics { xmin, ymin, width, height, advance_width: metrics.advance_width(g) * scale, ..Default::default() };
        (m, cov)
    }

    fn drawn(&self, g: GlyphId, px: f32) -> Arc<Kept> {
        let key = (g, px.to_bits());
        let at = |d: &Drawn| d.glyphs.binary_search_by_key(&key, |e| e.0);
        let drawn = self.drawn.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(i) = at(&drawn) {
            return drawn.glyphs[i].1.clone();
        }
        drop(drawn);
        let (m, cov) = self.rasterize(g, px);
        let kept = Arc::new(Kept { m, cov });
        let mut drawn = self.drawn.lock().unwrap_or_else(|e| e.into_inner());
        if drawn.glyphs.len() >= KEPT_GLYPHS || drawn.bytes + kept.cov.len() > KEPT_BYTES {
            drawn.glyphs.clear();
            drawn.bytes = 0;
        }
        if let Err(i) = at(&drawn) {
            drawn.bytes += kept.cov.len();
            drawn.glyphs.insert(i, (key, kept.clone()));
        }
        kept
    }
}

fn pieces(bend: f32) -> u32 {
    ((bend * 10.0).sqrt().ceil() as u32).clamp(1, 64)
}

#[derive(Default)]
struct Lines {
    scale: f32,
    lines: Vec<[f32; 4]>,
    start: [f32; 2],
    at: [f32; 2],
}

impl Lines {
    fn to(&mut self, p: [f32; 2]) {
        self.lines.push([self.at[0], self.at[1], p[0], p[1]]);
        self.at = p;
    }

    fn rasterize(&self) -> (i32, i32, usize, usize, Vec<u8>) {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for l in &self.lines {
            lo = [lo[0].min(l[0]).min(l[2]), lo[1].min(l[1]).min(l[3])];
            hi = [hi[0].max(l[0]).max(l[2]), hi[1].max(l[1]).max(l[3])];
        }
        let (left, bottom) = (lo[0].floor(), lo[1].floor());
        let (w, h) = (hi[0].ceil() - left, hi[1].ceil() - bottom);
        if !(w > 0.0 && h > 0.0 && w <= 4096.0 && h <= 4096.0) {
            return (0, 0, 0, 0, Vec::new());
        }
        let (top, wf) = (bottom + h, w);
        let (w, h) = (w as usize, h as usize);
        let stride = w + 2;
        let mut acc = vec![0f32; stride * h];
        for l in &self.lines {
            let (mut a, mut b, mut dir) = ([l[0] - left, top - l[1]], [l[2] - left, top - l[3]], 1.0);
            if a[1] == b[1] {
                continue;
            }
            if a[1] > b[1] {
                std::mem::swap(&mut a, &mut b);
                dir = -1.0;
            }
            let x_at = |y: f32| a[0] + (b[0] - a[0]) * ((y - a[1]) / (b[1] - a[1]));
            for row in a[1].max(0.0) as usize..(b[1].ceil() as usize).min(h) {
                let (y0, y1) = (a[1].max(row as f32), b[1].min(row as f32 + 1.0));
                if y1 <= y0 {
                    continue;
                }
                let (p, q) = (x_at(y0), x_at(y1));
                let (x0, x1) = (p.min(q).clamp(0.0, wf), p.max(q).clamp(0.0, wf));
                let area = |x: f32| {
                    if x <= x0 {
                        0.0
                    } else if x >= x1 {
                        (x1 - x0) / 2.0 + (x - x1)
                    } else {
                        (x - x0) * (x - x0) / (2.0 * (x1 - x0))
                    }
                };
                let d = dir * (y1 - y0);
                let cells = &mut acc[row * stride..][..stride];
                let (first, last) = (x0 as usize, x1 as usize);
                let mut before = 0.0;
                for (cell, c) in cells[first..=last].iter_mut().zip(first..) {
                    let share = area(c as f32 + 1.0) - area(c as f32);
                    *cell += d * (share - before);
                    before = share;
                }
                cells[last + 1] += d * (1.0 - before);
            }
        }
        let mut cov = Vec::with_capacity(w * h);
        for row in acc.chunks_exact(stride) {
            let mut sum = 0f32;
            cov.extend(row[..w].iter().map(|d| {
                sum += d;
                (sum.abs() * 255.9) as u8
            }));
        }
        (left as i32, bottom as i32, w, h, cov)
    }

    fn follow(&mut self, path: impl PathData) {
        for c in path.commands() {
            match c {
                Command::MoveTo(p) => self.move_to(p.x, p.y),
                Command::LineTo(p) => self.line_to(p.x, p.y),
                Command::QuadTo(c, p) => self.quad_to(c.x, c.y, p.x, p.y),
                Command::CurveTo(a, b, p) => self.curve_to(a.x, a.y, b.x, b.y, p.x, p.y),
                Command::Close => self.close(),
            }
        }
        self.close();
    }

    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = [x * self.scale, y * self.scale];
        self.at = self.start;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.to([x * self.scale, y * self.scale]);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let ([x0, y0], s) = (self.at, self.scale);
        let ([cx, cy], [x, y]) = ([cx0 * s, cy0 * s], [x * s, y * s]);
        let n = pieces((x0 - 2.0 * cx + x).hypot(y0 - 2.0 * cy + y));
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.to([u * u * x0 + 2.0 * u * t * cx + t * t * x, u * u * y0 + 2.0 * u * t * cy + t * t * y]);
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let ([x0, y0], s) = (self.at, self.scale);
        let ([ax, ay], [bx, by], [x, y]) = ([cx0 * s, cy0 * s], [cx1 * s, cy1 * s], [x * s, y * s]);
        let bend = (x0 - 2.0 * ax + bx).hypot(y0 - 2.0 * ay + by).max((ax - 2.0 * bx + x).hypot(ay - 2.0 * by + y));
        let n = pieces(3.0 * bend);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let k = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            self.to([k[0] * x0 + k[1] * ax + k[2] * bx + k[3] * x, k[0] * y0 + k[1] * ay + k[2] * by + k[3] * y]);
        }
    }

    fn close(&mut self) {
        if self.at != self.start {
            self.to(self.start);
        }
    }
}

enum Pick<'a> {
    Primary(&'a Font),
    Whole(Arc<Font>),
    Fallback(Arc<FontFile>, GlyphId),
}

impl Pick<'_> {
    fn rasterize(&self, c: char, px: f32) -> (Metrics, Coverage) {
        let f: &Font = match self {
            Self::Primary(f) => f,
            Self::Whole(f) => f,
            Self::Fallback(f, g) => {
                let kept = f.drawn(*g, px);
                return (kept.m, Coverage::Kept(kept));
            }
        };
        let (m, cov) = f.rasterize(c, px);
        (m, Coverage::Own(cov))
    }

    fn advance(&self, c: char, px: f32) -> f32 {
        let f: &Font = match self {
            Self::Primary(f) => f,
            Self::Whole(f) => f,
            Self::Fallback(f, g) => return f.advance(*g, px),
        };
        f.metrics(c, px).advance_width
    }
}

pub struct Text {
    primary: Font,
    cut: bool,
    path: PathBuf,
    whole: Mutex<Option<Option<Arc<Font>>>>,
    weight: u16,
}

fn fonts_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts")
}

static REGULAR: OnceLock<Option<Arc<Text>>> = OnceLock::new();
static SEMIBOLD: OnceLock<Option<Arc<Text>>> = OnceLock::new();

impl Text {
    pub fn load() -> Option<Arc<Self>> {
        REGULAR.get_or_init(|| Self::load_from(&["segoeui.ttf", "arial.ttf"], 400).map(Arc::new)).clone()
    }

    pub fn load_semibold() -> Option<Arc<Self>> {
        SEMIBOLD
            .get_or_init(|| Self::load_from(&["seguisb.ttf", "segoeuib.ttf", "segoeui.ttf", "arial.ttf"], 600).map(Arc::new))
            .clone()
    }

    pub fn preload() {
        std::thread::spawn(|| drop(Self::load()));
        std::thread::spawn(|| drop(Self::load_semibold()));
    }

    fn load_from(primary_names: &[&str], weight: u16) -> Option<Self> {
        let dir = fonts_dir();
        let (path, primary, cut) = primary_names.iter().find_map(|n| {
            let path = dir.join(n);
            let (font, cut) = read_font(&path)?;
            Some((path, font, cut))
        })?;
        Some(Self { primary, cut, path, whole: Mutex::new(None), weight })
    }

    fn whole(&self) -> Option<Arc<Font>> {
        let mut slot = self.whole.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert_with(|| read_whole_font(&self.path).map(Arc::new)).clone()
    }

    fn shown(&self, c: char) -> Option<char> {
        match c {
            '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}' => None,
            _ if c.is_control() && self.primary.lookup_glyph_index(c) == 0 => matches!(c, '\t' | '\n').then_some(' '),
            _ => Some(c),
        }
    }

    fn font_for(&self, c: char) -> Pick<'_> {
        if self.primary.lookup_glyph_index(c) != 0 {
            if !self.cut || english(c) {
                return Pick::Primary(&self.primary);
            }
            if let Some(whole) = self.whole() {
                return Pick::Whole(whole);
            }
        }
        let tried = if ('\u{1F000}'..='\u{1FFFF}').contains(&c) { 1 } else { usize::MAX };
        fallbacks()
            .iter()
            .take(tried)
            .filter_map(Fallback::get)
            .find_map(|f| f.glyph(c).map(|g| Pick::Fallback(f, g)))
            .unwrap_or(Pick::Primary(&self.primary))
    }

    pub fn vmetrics(&self, px: f32) -> (f32, f32) {
        self.primary
            .horizontal_line_metrics(px)
            .map_or((px * 0.9, -px * 0.25), |m| (m.ascent, m.descent))
    }

    pub fn line_height(&self, px: f32) -> f32 {
        let (a, d) = self.vmetrics(px);
        (a - d).ceil().max(1.0)
    }

    fn run(&self, s: &str, px: f32) -> (Vec<Glyph>, u32, u32) {
        let (ascent, _) = self.vmetrics(px);
        let h = self.line_height(px) as u32;
        let mut pen = 0.0f32;
        let mut glyphs = Vec::new();
        for c in s.chars().filter_map(|c| self.shown(c)) {
            let (m, cov) = self.font_for(c).rasterize(c, px);
            glyphs.push(Glyph {
                x: pen.round() as i32 + m.xmin,
                y: (ascent - (m.ymin + m.height as i32) as f32).round() as i32,
                w: m.width,
                h: m.height,
                cov,
            });
            pen += m.advance_width;
        }
        (glyphs, pen.ceil().max(1.0) as u32, h)
    }

    pub fn measure(&self, s: &str, px: f32) -> f32 {
        self.advance(s, px).ceil().max(1.0)
    }

    pub fn advance(&self, s: &str, px: f32) -> f32 {
        s.chars().filter_map(|c| self.shown(c)).map(|c| self.font_for(c).advance(c, px)).sum()
    }

    pub fn render(&self, s: &str, px: f32) -> Bitmap {
        let (glyphs, w, h) = self.run(s, px);
        let mut alpha = vec![0u8; (w * h) as usize];
        for g in &glyphs {
            for row in 0..g.h as i32 {
                for col in 0..g.w as i32 {
                    let (x, y) = (g.x + col, g.y + row);
                    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                        continue;
                    }
                    let a = g.cov[row as usize * g.w + col as usize];
                    let dst = &mut alpha[(y as u32 * w + x as u32) as usize];
                    *dst = (*dst).max(a);
                }
            }
        }
        Bitmap { w, h, alpha }
    }

    pub fn draw_centered(&self, pix: &mut tiny_skia::Pixmap, s: &str, cx: f32, cy: f32, px: f32, color: [u8; 3]) {
        self.draw_centered_alpha(pix, s, cx, cy, px, color, 1.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_centered_alpha(&self, pix: &mut tiny_skia::Pixmap, s: &str, cx: f32, cy: f32, px: f32, color: [u8; 3], alpha: f32) {
        let (glyphs, _, _) = self.run(s, px);
        let ink = glyphs.iter().filter(|g| g.w > 0 && g.h > 0).fold(None::<[i32; 4]>, |acc, g| {
            let b = [g.x, g.y, g.x + g.w as i32, g.y + g.h as i32];
            Some(acc.map_or(b, |a| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]))
        });
        let Some(b) = ink else { return };
        let my = (b[1] + b[3]) as f32 / 2.0;
        let (mut sum, mut moment) = (0.0f32, 0.0f32);
        for g in &glyphs {
            for row in 0..g.h {
                for col in 0..g.w {
                    let c = g.cov[row * g.w + col] as f32;
                    sum += c;
                    moment += c * (g.x as f32 + col as f32 + 0.5);
                }
            }
        }
        let mx = if sum > 0.0 { moment / sum } else { (b[0] + b[2]) as f32 / 2.0 };
        Self::blit(pix, &glyphs, (cx - mx).round() as i32, (cy - my).round() as i32, color, alpha);
    }

    pub fn draw(&self, pix: &mut tiny_skia::Pixmap, s: &str, x: f32, y: f32, px: f32, color: [u8; 3]) {
        self.draw_alpha(pix, s, x, y, px, color, 1.0);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_alpha(&self, pix: &mut tiny_skia::Pixmap, s: &str, x: f32, y: f32, px: f32, color: [u8; 3], alpha: f32) {
        let (glyphs, _, _) = self.run(s, px);
        Self::blit(pix, &glyphs, x.round() as i32, y.round() as i32, color, alpha);
    }

    fn blit(pix: &mut tiny_skia::Pixmap, glyphs: &[Glyph], ox: i32, oy: i32, color: [u8; 3], alpha: f32) {
        let fade = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
        let (pw, ph) = (pix.width() as i32, pix.height() as i32);
        let data = pix.data_mut();
        for g in glyphs {
            for row in 0..g.h as i32 {
                for col in 0..g.w as i32 {
                    let (dx, dy) = (ox + g.x + col, oy + g.y + row);
                    if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                        continue;
                    }
                    let i = ((dy * pw + dx) * 4) as usize;
                    blend(&mut data[i..i + 4], color, (g.cov[row as usize * g.w + col as usize] as u32 * fade) >> 8);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_system(&self, pix: &mut tiny_skia::Pixmap, s: &str, x: f32, y: f32, px: f32, max_w: f32, color: [u8; 3]) -> bool {
        self.draw_system_alpha(pix, s, x, y, px, max_w, color, 1.0).is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_system_alpha(&self, pix: &mut tiny_skia::Pixmap, s: &str, x: f32, y: f32, px: f32, max_w: f32, color: [u8; 3], alpha: f32) -> Option<f32> {
        if !needs_shaping(s) {
            return None;
        }
        let m = system::line(s, px, self.weight, max_w)?;
        let reach = (m.x + m.w as i32) as f32;
        let (ascent, _) = self.vmetrics(px);
        let (ox, oy) = (x.round() as i32 + m.x, y.round() as i32 + ascent.round() as i32 + m.y);
        let fade = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
        let (pw, ph) = (pix.width() as i32, pix.height() as i32);
        let (c0, c1) = ((-ox).max(0), (pw - ox).min(m.w as i32));
        let (r0, r1) = ((-oy).max(0), (ph - oy).min(m.h as i32));
        if c0 >= c1 || r0 >= r1 {
            return Some(reach);
        }
        let data = pix.data_mut();
        for row in r0..r1 {
            let src = &m.cov[row as usize * m.w..][c0 as usize..c1 as usize];
            let at = (((oy + row) * pw + ox + c0) * 4) as usize;
            for (&c, d) in src.iter().zip(data[at..at + src.len() * 4].as_chunks_mut::<4>().0) {
                blend(d, color, (c as u32 * fade) >> 8);
            }
        }
        Some(reach)
    }

}

fn blend(d: &mut [u8], color: [u8; 3], cov: u32) {
    if cov == 0 {
        return;
    }
    let inv = 255 - cov;
    for k in 0..3 {
        d[k] = ((color[k] as u32 * cov + d[k] as u32 * inv) / 255) as u8;
    }
    d[3] = (cov + d[3] as u32 * inv / 255) as u8;
}

pub fn needs_shaping(s: &str) -> bool {
    const SHAPED: [(u32, u32); 25] = [
        (0x0300, 0x036F),
        (0x0483, 0x0489),
        (0x0590, 0x109F),
        (0x1100, 0x11FF),
        (0x1780, 0x18AF),
        (0x1900, 0x1C4F),
        (0x1CD0, 0x1CFF),
        (0x1DC0, 0x1DFF),
        (0x200C, 0x200F),
        (0x202A, 0x202E),
        (0x2066, 0x2069),
        (0x20D0, 0x20FF),
        (0x302A, 0x302F),
        (0x3099, 0x309A),
        (0xA800, 0xAAFF),
        (0xABC0, 0xABFF),
        (0xD7B0, 0xD7FF),
        (0xFB1D, 0xFDFF),
        (0xFE20, 0xFE2F),
        (0xFE70, 0xFEFF),
        (0x10800, 0x11FFF),
        (0x1E800, 0x1EFFF),
        (0x1F1E6, 0x1F1FF),
        (0x1F3FB, 0x1F3FF),
        (0xE0020, 0xE007F),
    ];
    let mut prev = None::<char>;
    for c in s.chars() {
        let u = c as u32;
        let complex = u >= 0x300 && SHAPED.binary_search_by(|&(lo, hi)| if hi < u { std::cmp::Ordering::Less } else if lo > u { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }).is_ok();
        if complex || c == '\u{FE0F}' && !prev.is_some_and(is_pictographic) {
            return true;
        }
        prev = Some(c);
    }
    false
}

fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2190..=0x21FF | 0x2300..=0x23FF | 0x24C2
        | 0x25A0..=0x27BF | 0x2900..=0x297F | 0x2B00..=0x2BFF | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
}

pub fn normalize_display(s: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    use windows::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError},
        Globalization::{IsNormalizedString, NormalizationC, NormalizeString},
    };
    if s.chars().all(|c| (c as u32) < 0x300) {
        return Cow::Borrowed(s);
    }
    let src: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        if IsNormalizedString(NormalizationC, &src).is_ok() {
            return Cow::Borrowed(s);
        }
        let mut want = NormalizeString(NormalizationC, &src, None);
        for _ in 0..4 {
            if want <= 0 {
                break;
            }
            let mut out = vec![0u16; want as usize];
            let got = NormalizeString(NormalizationC, &src, Some(&mut out));
            if got > 0 {
                out.truncate(got as usize);
                return match String::from_utf16(&out) {
                    Ok(n) if n != s => Cow::Owned(n),
                    _ => Cow::Borrowed(s),
                };
            }
            if GetLastError() != ERROR_INSUFFICIENT_BUFFER {
                break;
            }
            want = -got;
        }
    }
    Cow::Borrowed(s)
}

mod system {
    use super::{Arc, Mutex};
    use windows::{
        Win32::{
            Foundation::{COLORREF, RECT},
            Graphics::Gdi::{
                ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET,
                DIB_RGB_COLORS, DT_CALCRECT, DT_END_ELLIPSIS, DT_NOCLIP, DT_NOPREFIX, DT_SINGLELINE, DeleteDC, DeleteObject, DrawTextW, GdiFlush, GetTextMetricsW, HBITMAP,
                HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
            },
        },
        core::w,
    };

    pub(super) struct Mask {
        pub x: i32,
        pub y: i32,
        pub w: usize,
        pub h: usize,
        pub cov: Vec<u8>,
    }

    struct Key {
        text: Box<str>,
        px: u32,
        weight: u16,
        room: i32,
    }

    struct Kept {
        masks: Vec<(Key, Arc<Mask>)>,
        bytes: usize,
    }

    static KEPT: Mutex<Kept> = Mutex::new(Kept { masks: Vec::new(), bytes: 0 });

    pub(super) const KEPT_MASKS: usize = 64;
    pub(super) const KEPT_BYTES: usize = 1 << 20;

    const MAX_UNITS: usize = 1024;

    pub(super) fn release() {
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        kept.masks = Vec::new();
        kept.bytes = 0;
    }

    fn room(max_w: f32) -> i32 {
        if max_w.is_nan() { 0 } else { max_w.clamp(0.0, 8192.0).floor() as i32 }
    }

    pub(super) fn line(s: &str, px: f32, weight: u16, max_w: f32) -> Option<Arc<Mask>> {
        let (bits, room) = (px.to_bits(), room(max_w));
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        let e = match kept.masks.iter().position(|(k, _)| k.px == bits && k.weight == weight && k.room == room && *k.text == *s) {
            Some(i) => kept.masks.remove(i),
            None => {
                let m = Arc::new(render(s, px, weight, room)?);
                let size = m.cov.len();
                if size > KEPT_BYTES {
                    return Some(m);
                }
                while !kept.masks.is_empty() && (kept.masks.len() >= KEPT_MASKS || kept.bytes + size > KEPT_BYTES) {
                    let (_, old) = kept.masks.remove(0);
                    kept.bytes -= old.cov.len();
                }
                kept.bytes += size;
                (Key { text: s.into(), px: bits, weight, room }, m)
            }
        };
        let m = e.1.clone();
        kept.masks.push(e);
        Some(m)
    }

    fn utf16(s: &str) -> Vec<u16> {
        let mut out = Vec::with_capacity(s.len().min(MAX_UNITS));
        for (i, c) in s.char_indices() {
            let c = match c {
                '\u{202D}' => '\u{202A}',
                '\u{202E}' => '\u{202B}',
                '\u{2066}' => '\u{202A}',
                '\u{2067}' => '\u{202B}',
                '\u{2068}' => if first_strong_is_rtl(&s[i + c.len_utf8()..]) { '\u{202B}' } else { '\u{202A}' },
                '\u{2069}' => '\u{202C}',
                '\u{2060}' => '\u{FEFF}',
                c => c,
            };
            if out.len() + 3 > MAX_UNITS {
                out.push(0x2026);
                break;
            }
            out.extend_from_slice(c.encode_utf16(&mut [0; 2]));
        }
        out
    }

    fn first_strong_is_rtl(s: &str) -> bool {
        for c in s.chars() {
            if c == '\u{2069}' {
                break;
            }
            if matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF) {
                return true;
            }
            if c.is_alphabetic() {
                return false;
            }
        }
        false
    }

    struct Gdi {
        dc: HDC,
        font: HFONT,
        old_font: HGDIOBJ,
        bmp: HBITMAP,
        old_bmp: HGDIOBJ,
        tm: TEXTMETRICW,
    }

    impl Gdi {
        fn new(px: f32, weight: u16) -> Option<Self> {
            let size = px.round();
            if !(1.0..=1024.0).contains(&size) {
                return None;
            }
            unsafe {
                let dc = CreateCompatibleDC(None);
                if dc.is_invalid() {
                    return None;
                }
                let mut g = Gdi { dc, font: HFONT::default(), old_font: HGDIOBJ::default(), bmp: HBITMAP::default(), old_bmp: HGDIOBJ::default(), tm: TEXTMETRICW::default() };
                let face = if weight >= 600 { w!("Segoe UI Semibold") } else { w!("Segoe UI") };
                g.font = CreateFontW(-(size as i32), 0, 0, 0, weight as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, face);
                if g.font.is_invalid() {
                    return None;
                }
                g.old_font = SelectObject(dc, g.font.into());
                if g.old_font.is_invalid() || !GetTextMetricsW(dc, &mut g.tm).as_bool() {
                    return None;
                }
                Some(g)
            }
        }

        fn width(&self, text: &mut [u16]) -> Option<i32> {
            if text.is_empty() {
                return Some(0);
            }
            let mut r = RECT::default();
            let ok = unsafe { DrawTextW(self.dc, text, &mut r, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX) } != 0;
            ok.then_some(r.right - r.left)
        }
    }

    impl Drop for Gdi {
        fn drop(&mut self) {
            unsafe {
                if !self.old_bmp.is_invalid() {
                    SelectObject(self.dc, self.old_bmp);
                }
                if !self.old_font.is_invalid() {
                    SelectObject(self.dc, self.old_font);
                }
                if !self.bmp.is_invalid() {
                    let _ = DeleteObject(self.bmp.into());
                }
                if !self.font.is_invalid() {
                    let _ = DeleteObject(self.font.into());
                }
                let _ = DeleteDC(self.dc);
            }
        }
    }

    pub(super) fn render(s: &str, px: f32, weight: u16, room: i32) -> Option<Mask> {
        let mut text = utf16(s);
        if text.is_empty() || room <= 0 {
            return None;
        }
        let room = room.min(8192);
        let mut gdi = Gdi::new(px, weight)?;
        let whole = gdi.width(&mut text)?;
        let (cut, line_w) = (whole > room, whole.clamp(0, room));
        let (ascent, height) = (gdi.tm.tmAscent, gdi.tm.tmHeight);
        let pad = px.round() as i32;
        let (w, h) = (line_w + 2 * pad, height + 2 * pad);
        if w <= 0 || h <= 0 || w > 16384 || h > 4096 {
            return None;
        }
        let info = BITMAPINFO {
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
        let bits = unsafe {
            let mut bits = std::ptr::null_mut();
            gdi.bmp = CreateDIBSection(Some(gdi.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                return None;
            }
            gdi.old_bmp = SelectObject(gdi.dc, gdi.bmp.into());
            if gdi.old_bmp.is_invalid() {
                return None;
            }
            SetBkMode(gdi.dc, TRANSPARENT);
            SetTextColor(gdi.dc, COLORREF(0x00FF_FFFF));
            bits as *mut u32
        };
        let (wu, hu) = (w as usize, h as usize);
        let lit = |p: &u32| *p & 0xFF_FFFF != 0;
        let (mut right, mut bounds) = (pad + line_w, (wu, hu, 0, 0));
        for _ in 0..3 {
            unsafe { std::ptr::write_bytes(bits, 0, wu * hu) };
            let mut r = RECT { left: pad, top: pad, right, bottom: pad + height };
            if unsafe { DrawTextW(gdi.dc, &mut text, &mut r, DT_NOCLIP | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS) } == 0 {
                return None;
            }
            let _ = unsafe { GdiFlush() };
            let pixels = unsafe { std::slice::from_raw_parts(bits, wu * hu) };
            let (mut x0, mut y0, mut x1, mut y1) = (wu, hu, 0, 0);
            for (y, row) in pixels.chunks_exact(wu).enumerate() {
                if let (Some(a), Some(b)) = (row.iter().position(lit), row.iter().rposition(lit)) {
                    (x0, y0, x1, y1) = (x0.min(a), y0.min(y), x1.max(b + 1), y + 1);
                }
            }
            bounds = (x0, y0, x1, y1);
            let over = x1 as i32 - (pad + room + 1);
            if !cut || over <= 0 || right - over <= pad {
                break;
            }
            right -= over;
        }
        let (x0, y0, mut x1, y1) = bounds;
        if cut {
            x1 = x1.min((pad + room + 1) as usize);
        }
        if x0 >= x1 {
            return Some(Mask { x: 0, y: 0, w: 0, h: 0, cov: Vec::new() });
        }
        let mut cov = Vec::with_capacity((x1 - x0) * (y1 - y0));
        let pixels = unsafe { std::slice::from_raw_parts(bits, wu * hu) };
        for row in pixels.chunks_exact(wu).take(y1).skip(y0) {
            cov.extend(row[x0..x1].iter().map(|p| {
                let [b, g, r, _] = p.to_le_bytes();
                b.max(g).max(r)
            }));
        }
        Some(Mask { x: x0 as i32 - pad, y: y0 as i32 - (pad + ascent), w: x1 - x0, h: y1 - y0, cov })
    }
}

fn read_font(path: &Path) -> Option<(Font, bool)> {
    let mut bytes = std::fs::read(path).ok()?;
    if keep_english(&mut bytes) {
        if let Ok(font) = Font::from_bytes(&bytes[..], ui_font_settings()) {
            return Some((font, true));
        }
        bytes = std::fs::read(path).ok()?;
    }
    Some((Font::from_bytes(bytes, ui_font_settings()).ok()?, false))
}

fn read_whole_font(path: &Path) -> Option<Font> {
    Font::from_bytes(std::fs::read(path).ok()?, ui_font_settings()).ok()
}

fn ui_font_settings() -> FontSettings {
    FontSettings { load_substitutions: false, ..FontSettings::default() }
}

const ENGLISH: [(u32, u32); 7] =
    [(0x20, 0x7E), (0xA0, 0xFF), (0x2010, 0x2027), (0x2030, 0x203A), (0x20AC, 0x20AC), (0x2122, 0x2122), (0x2190, 0x2193)];

fn english(c: char) -> bool {
    ENGLISH.iter().any(|(first, last)| (*first..=*last).contains(&(c as u32)))
}

fn keep_english(bytes: &mut [u8]) -> bool {
    cut_to_english(bytes).is_some()
}

fn cut_to_english(bytes: &mut [u8]) -> Option<()> {
    fn be16(b: &[u8], at: usize) -> Option<usize> {
        Some(u16::from_be_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?) as usize)
    }
    fn be32(b: &[u8], at: usize) -> Option<usize> {
        Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize)
    }
    if !matches!(bytes.get(..4)?, [0, 1, 0, 0] | b"true") {
        return None;
    }
    let tables = be16(bytes, 4)?;
    let table = |tag: &[u8; 4]| -> Option<(usize, usize)> {
        (0..tables).find_map(|i| {
            let record = bytes.get(12 + 16 * i..12 + 16 * i + 16)?;
            (&record[..4] == tag).then(|| Some((be32(record, 8)?, be32(record, 12)?))).flatten()
        })
    };
    let (head, _) = table(b"head")?;
    let (maxp, _) = table(b"maxp")?;
    let (loca, loca_len) = table(b"loca")?;
    let (glyf, glyf_len) = table(b"glyf")?;
    let long = be16(bytes, head + 50)? != 0;
    let n = be16(bytes, maxp + 4)?;
    let step = if long { 4 } else { 2 };
    if n == 0 || loca_len < (n + 1) * step || loca + (n + 1) * step > bytes.len() || glyf + glyf_len > bytes.len() {
        return None;
    }
    let at: Vec<usize> = (0..=n).map(|i| if long { be32(bytes, loca + 4 * i) } else { Some(be16(bytes, loca + 2 * i)? * 2) }).collect::<Option<_>>()?;
    if at.windows(2).any(|w| w[0] > w[1]) || at[n] > glyf_len {
        return None;
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    {
        let map = FontRef::from_index(bytes, 0)?.charmap();
        for c in ENGLISH.iter().flat_map(|(first, last)| *first..=*last).filter_map(char::from_u32) {
            if let Some(g) = keep.get_mut(map.map(c) as usize) {
                *g = true;
            }
        }
    }
    let mut todo: Vec<usize> = (0..n).filter(|g| keep[*g]).collect();
    while let Some(g) = todo.pop() {
        let outline = bytes.get(glyf + at[g]..glyf + at[g + 1])?;
        if outline.len() < 10 || (be16(outline, 0)? as i16) >= 0 {
            continue;
        }
        let mut part = 10;
        loop {
            let (flags, glyph) = (be16(outline, part)?, be16(outline, part + 2)?);
            if glyph >= n {
                return None;
            }
            if !keep[glyph] {
                keep[glyph] = true;
                todo.push(glyph);
            }
            part += 4 + if flags & 0x1 != 0 { 4 } else { 2 };
            part += if flags & 0x8 != 0 { 2 } else if flags & 0x40 != 0 { 4 } else if flags & 0x80 != 0 { 8 } else { 0 };
            if flags & 0x20 == 0 {
                break;
            }
        }
    }
    let (mut outlines, mut starts) = (Vec::new(), Vec::with_capacity(n + 1));
    for g in 0..n {
        starts.push(outlines.len());
        if keep[g] {
            outlines.extend_from_slice(bytes.get(glyf + at[g]..glyf + at[g + 1])?);
            while outlines.len() % 4 != 0 {
                outlines.push(0);
            }
        }
    }
    starts.push(outlines.len());
    if outlines.len() > glyf_len || (!long && outlines.len() > 0x1FFFE) {
        return None;
    }
    bytes[glyf..glyf + glyf_len].fill(0);
    bytes[glyf..glyf + outlines.len()].copy_from_slice(&outlines);
    for (i, start) in starts.iter().enumerate() {
        if long {
            bytes[loca + 4 * i..loca + 4 * i + 4].copy_from_slice(&(*start as u32).to_be_bytes());
        } else {
            bytes[loca + 2 * i..loca + 2 * i + 2].copy_from_slice(&((*start / 2) as u16).to_be_bytes());
        }
    }
    Some(())
}
