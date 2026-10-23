
use anyhow::{Result, anyhow, bail};
use gif::{DisposalMethod, Encoder, Frame, Repeat};
use std::{
    borrow::Cow,
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
    io::{Seek, Write},
    time::Instant,
};


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub webp: bool,
    pub mp4: bool,
    pub fps: u8,
    pub scale: u8,
    pub quality: u8,
    pub filter: u8,
    pub dither: bool,
    pub pointer: u8,
    pub max_secs: u32,
    pub countdown: u8,
}

pub const COUNTDOWN: [u8; 4] = [0, 3, 5, 10];
pub const FPS: [u8; 5] = [10, 15, 20, 30, 60];
pub const SCALE: [u8; 4] = [100, 75, 50, 25];
pub const MAX_SECS: [u32; 4] = [30, 60, 300, 600];

impl Default for Settings {
    fn default() -> Self {
        Self { webp: false, mp4: false, fps: 20, scale: 100, quality: 1, filter: 1, dither: true, pointer: 1, max_secs: 300, countdown: 3 }
    }
}

impl Settings {
    pub fn parse_line(&mut self, line: &str) -> bool {
        let Some((k, v)) = line.strip_prefix("rec_").and_then(|r| r.split_once('=')) else { return false };
        let v = v.trim();
        let num = v.parse::<u32>().ok();
        match k {
            "format" => {
                self.webp = v.eq_ignore_ascii_case("webp");
                self.mp4 = v.eq_ignore_ascii_case("mp4");
            }
            "fps" => {
                if let Some(n) = num.and_then(|n| u8::try_from(n).ok()).filter(|n| FPS.contains(n)) {
                    self.fps = n;
                }
            }
            "scale" => {
                if let Some(n) = num.and_then(|n| u8::try_from(n).ok()).filter(|n| SCALE.contains(n)) {
                    self.scale = n;
                }
            }
            "quality" => {
                if let Some(n) = num {
                    self.quality = n.min(2) as u8;
                }
            }
            "filter" => {
                if let Some(n) = num {
                    self.filter = n.min(3) as u8;
                }
            }
            "dither" => self.dither = v != "0",
            "pointer" => {
                if let Some(n) = num {
                    self.pointer = n.min(2) as u8;
                }
            }
            "max_seconds" => {
                if let Some(n) = num.filter(|n| MAX_SECS.contains(n)) {
                    self.max_secs = n;
                }
            }
            "countdown" => {
                if let Some(n) = num.and_then(|n| u8::try_from(n).ok()).filter(|n| COUNTDOWN.contains(n)) {
                    self.countdown = n;
                }
            }
            _ => return false,
        }
        true
    }

    pub fn lines(&self) -> String {
        format!(
            "rec_format={}\nrec_fps={}\nrec_scale={}\nrec_quality={}\nrec_filter={}\nrec_dither={}\nrec_pointer={}\nrec_max_seconds={}\nrec_countdown={}\n",
            self.ext(),
            self.fps,
            self.scale,
            self.quality,
            self.filter,
            if self.dither { 1 } else { 0 },
            self.pointer,
            self.max_secs,
            self.countdown
        )
    }

    pub fn ext(&self) -> &'static str {
        if self.mp4 {
            "mp4"
        } else if self.webp {
            "webp"
        } else {
            "gif"
        }
    }

    pub fn grab_fps(&self) -> u64 {
        if self.webp || self.mp4 { self.fps as u64 } else { (self.fps as u64).min(50) }
    }

    pub fn uses_filter(&self) -> bool {
        !self.mp4
    }

    pub fn uses_dither(&self) -> bool {
        !self.mp4 && !self.webp
    }

    pub fn tolerance(&self) -> u8 {
        [0, 2, 5, 10][self.filter.min(3) as usize]
    }

    pub fn out_size(&self, w: u32, h: u32) -> (u32, u32) {
        if self.scale >= 100 {
            return (w, h);
        }
        let f = |v: u32| ((v as u64 * self.scale as u64 / 100) as u32).max(16);
        (f(w), f(h))
    }
}


#[derive(PartialEq, Eq, Debug)]
pub enum Flow {
    Continue,
    Full,
}

pub(crate) const BUDGET_BYTES: u64 = 1 << 30;

struct Counting<W: Write> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

struct WebpOut {
    out: std::io::BufWriter<std::fs::File>,
    cfg: zenwebp::EncoderConfig,
    size: (u32, u32),
    pending: Option<WebpFrame>,
    len: u64,
    budget: u64,
}

struct WebpFrame {
    still: Vec<u8>,
    chunks: usize,
    rect: [u32; 4],
    ms: u32,
}

impl WebpFrame {
    fn len(&self) -> u64 {
        (8 + 16 + self.still.len() - self.chunks) as u64
    }
}

impl WebpOut {
    fn new(file: std::fs::File, cfg: zenwebp::EncoderConfig, size: (u32, u32)) -> Self {
        Self { out: std::io::BufWriter::with_capacity(1 << 20, file), cfg, size, pending: None, len: 0, budget: BUDGET_BYTES }
    }

    fn frame(&mut self, rgba: &[u8], rect: [u32; 4], ms: u32) -> Result<Flow> {
        let [x, y, w, h] = rect;
        let sub = if (w, h) == self.size {
            Cow::Borrowed(rgba)
        } else {
            let (cw, w) = (self.size.0 as usize, w as usize);
            let mut s = Vec::with_capacity(w * h as usize * 4);
            for row in y as usize..(y + h) as usize {
                let i = (row * cw + x as usize) * 4;
                s.extend_from_slice(&rgba[i..i + w * 4]);
            }
            Cow::Owned(s)
        };
        let still = zenwebp::EncodeRequest::new(&self.cfg, &sub, zenwebp::PixelLayout::Rgba8, w, h).encode().map_err(|e| anyhow!("WebP: {e}"))?;
        let chunks = still.len() - crate::anim::webp_still_chunks(&still).len();
        if let Some(prev) = self.pending.replace(WebpFrame { still, chunks, rect, ms }) {
            let shown_for = ms.saturating_sub(prev.ms);
            self.write(&prev, shown_for)?;
        }
        Ok(if self.size() > self.budget { Flow::Full } else { Flow::Continue })
    }

    fn size(&self) -> u64 {
        match &self.pending {
            None => self.len,
            Some(f) if self.len == 0 => f.still.len() as u64,
            Some(f) => self.len + f.len(),
        }
    }

    fn write(&mut self, f: &WebpFrame, ms: u32) -> Result<()> {
        if self.len == 0 {
            let head = crate::anim::webp_anim_head(self.size.0, self.size.1, 0);
            self.out.write_all(&head)?;
            self.len = head.len() as u64;
        }
        let chunks = &f.still[f.chunks..];
        let head = crate::anim::webp_frame_head(f.rect, ms, chunks.len())?;
        self.out.write_all(&head)?;
        self.out.write_all(chunks)?;
        self.len += f.len();
        Ok(())
    }

    fn finish(mut self, end_ms: u32) -> Result<()> {
        let Some(last) = self.pending.take() else { bail!("nothing was recorded") };
        if self.len == 0 {
            self.out.write_all(&last.still)?;
        } else {
            self.write(&last, end_ms.saturating_sub(last.ms).max(100))?;
            let size = u32::try_from(self.len - 8).map_err(|_| anyhow!("WebP: the recording would be larger than 4 GB"))?;
            self.out.seek(std::io::SeekFrom::Start(4))?;
            self.out.write_all(&size.to_le_bytes())?;
        }
        self.out.flush()?;
        Ok(())
    }
}

fn webp_config(quality: u8) -> zenwebp::EncoderConfig {
    use zenwebp::{EncoderConfig, LosslessConfig, LossyConfig};
    match quality {
        0 => EncoderConfig::Lossy(LossyConfig::new().with_quality(70.0).with_method(0)),
        1 => EncoderConfig::Lossy(LossyConfig::new().with_quality(82.0).with_method(2)),
        _ => EncoderConfig::Lossless(LosslessConfig::new().with_quality(100.0).with_method(2)),
    }
}

enum Sink {
    Mp4(crate::mp4::Mp4Writer),
    Gif {
        enc: Encoder<Counting<std::io::BufWriter<std::fs::File>>>,
        pending: Option<Frame<'static>>,
        emitted_cs: u64,
    },
    Webp(WebpOut),
}

struct Scaler {
    resizer: fast_image_resize::Resizer,
    out: fast_image_resize::images::Image<'static>,
    options: fast_image_resize::ResizeOptions,
}

impl Scaler {
    fn new((w, h): (u32, u32)) -> Self {
        use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)).use_alpha(false);
        Self { resizer: Resizer::new(), out: Image::new(w, h, PixelType::U8x4), options }
    }

    fn scale(&mut self, px: &[u8], w: u32, h: u32) -> Result<&[u8]> {
        use fast_image_resize::{PixelType, images::ImageRef};
        let src = ImageRef::new(w, h, px, PixelType::U8x4).map_err(|e| anyhow!("resize: {e}"))?;
        self.resizer.resize(&src, &mut self.out, &self.options).map_err(|e| anyhow!("resize: {e}"))?;
        Ok(self.out.buffer())
    }
}

pub struct Pipeline {
    s: Settings,
    in_size: (u32, u32),
    out_size: (u32, u32),
    scaler: Option<Scaler>,
    shown: Vec<u8>,
    mask: Vec<u8>,
    first: bool,
    t0: Option<Instant>,
    last_at: Option<Instant>,
    sink: Sink,
}

impl Pipeline {
    pub fn new_mp4(s: Settings, size: (u32, u32), path: std::path::PathBuf) -> Result<Self> {
        let out_size = s.out_size(size.0, size.1);
        let sink = Sink::Mp4(crate::mp4::Mp4Writer::new(path, out_size, s.fps as u32, s.quality)?);
        let scaler = (out_size != size).then(|| Scaler::new(out_size));
        Ok(Self { s, in_size: size, out_size, scaler, shown: Vec::new(), mask: Vec::new(), first: true, t0: None, last_at: None, sink })
    }

    pub fn new(s: Settings, size: (u32, u32), file: std::fs::File) -> Result<Self> {
        let out_size = s.out_size(size.0, size.1);
        let (ow, oh) = (out_size.0 as usize, out_size.1 as usize);
        let sink = if s.webp {
            Sink::Webp(WebpOut::new(file, webp_config(s.quality), out_size))
        } else {
            let w = Counting { inner: std::io::BufWriter::with_capacity(1 << 20, file), bytes: 0 };
            let mut enc = Encoder::new(w, out_size.0 as u16, out_size.1 as u16, &[])?;
            enc.set_repeat(Repeat::Infinite)?;
            Sink::Gif { enc, pending: None, emitted_cs: 0 }
        };
        Ok(Self {
            s,
            in_size: size,
            out_size,
            scaler: (out_size != size).then(|| Scaler::new(out_size)),
            shown: vec![0; ow * oh * 4],
            mask: vec![0; ow * oh],
            first: true,
            t0: None,
            last_at: None,
            sink,
        })
    }

    pub fn frame(&mut self, at: Instant, bgra: &mut [u8]) -> Result<Flow> {
        let (iw, ih) = self.in_size;
        let (ow, oh) = (self.out_size.0 as usize, self.out_size.1 as usize);
        if bgra.len() != iw as usize * ih as usize * 4 {
            bail!("a picture of the wrong size");
        }
        self.t0.get_or_insert(at);
        self.last_at = Some(at);
        if let Sink::Mp4(w) = &mut self.sink {
            let small = self.scaler.as_mut().map(|sc| sc.scale(bgra, iw, ih)).transpose()?;
            let t0 = self.t0.unwrap_or(at);
            w.write(at.saturating_duration_since(t0), small.unwrap_or(bgra))?;
            return Ok(Flow::Continue);
        }
        for p in bgra.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            p[3] = 255;
        }
        let small = self.scaler.as_mut().map(|sc| sc.scale(bgra, iw, ih)).transpose()?;
        let rgba = small.unwrap_or(bgra);

        let tol = self.s.tolerance() as i16;
        let first = self.first;
        let (mut x0, mut y0, mut x1, mut y1) = (ow, oh, 0usize, 0usize);
        let mut n = 0u64;
        for y in 0..oh {
            let row = y * ow;
            let mut any = false;
            for x in 0..ow {
                let i = (row + x) * 4;
                let (cur, old) = (&rgba[i..i + 3], &self.shown[i..i + 3]);
                let differs = first
                    || cur != old
                        && ((cur[0] as i16 - old[0] as i16).abs() > tol
                            || (cur[1] as i16 - old[1] as i16).abs() > tol
                            || (cur[2] as i16 - old[2] as i16).abs() > tol);
                if differs {
                    self.shown[i..i + 4].copy_from_slice(&rgba[i..i + 4]);
                    self.mask[row + x] = 1;
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    any = true;
                    n += 1;
                } else {
                    self.mask[row + x] = 0;
                }
            }
            if any {
                y0 = y0.min(y);
                y1 = y + 1;
            }
        }
        if n == 0 {
            return Ok(Flow::Continue);
        }
        self.first = false;
        let t0 = self.t0.unwrap_or(at);
        let (rw, rh) = (x1 - x0, y1 - y0);
        match &mut self.sink {
            Sink::Mp4(_) => unreachable!(),
            Sink::Gif { enc, pending, emitted_cs } => {
                let mut sub = Vec::with_capacity(rw * rh * 4);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let i = y * ow + x;
                        if self.mask[i] != 0 {
                            sub.extend_from_slice(&self.shown[i * 4..i * 4 + 4]);
                        } else {
                            sub.extend_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                }
                let q = quantize(&sub, rw, rh, self.s.quality, self.s.dither);
                let frame = Frame {
                    width: rw as u16,
                    height: rh as u16,
                    left: x0 as u16,
                    top: y0 as u16,
                    dispose: DisposalMethod::Keep,
                    transparent: q.transparent,
                    palette: Some(q.palette),
                    buffer: Cow::Owned(q.indices),
                    ..Frame::default()
                };
                if let Some(mut p) = pending.take() {
                    let target = at.saturating_duration_since(t0).as_millis() as u64 / 10;
                    let cs = target.saturating_sub(*emitted_cs).clamp(2, u16::MAX as u64);
                    *emitted_cs += cs;
                    p.delay = cs as u16;
                    enc.write_frame(&p)?;
                }
                *pending = Some(frame);
                if enc.get_ref().bytes > BUDGET_BYTES {
                    return Ok(Flow::Full);
                }
            }
            Sink::Webp(out) => {
                let ts = at.saturating_duration_since(t0).as_millis().min(u32::MAX as u128) as u32;
                let (x0, y0) = (x0 & !1, y0 & !1);
                let (x1, y1) = (((x1 + 1) & !1).min(ow), ((y1 + 1) & !1).min(oh));
                return out.frame(&self.shown, [x0, y0, x1 - x0, y1 - y0].map(|v| v as u32), ts);
            }
        }
        Ok(Flow::Continue)
    }

    pub fn finish(self) -> Result<()> {
        let end = self.last_at;
        let t0 = self.t0;
        drop(self.scaler);
        match self.sink {
            Sink::Mp4(w) => w.finish(),
            Sink::Gif { mut enc, pending, emitted_cs } => {
                let Some(mut p) = pending else { bail!("nothing was recorded") };
                let held = match (end, t0) {
                    (Some(e), Some(t)) => (e.saturating_duration_since(t).as_millis() as u64 / 10).saturating_sub(emitted_cs),
                    _ => 10,
                };
                p.delay = held.clamp(10, u16::MAX as u64) as u16;
                enc.write_frame(&p)?;
                let mut w = enc.into_inner()?;
                w.flush()?;
                Ok(())
            }
            Sink::Webp(out) => {
                let end_ms = match (end, t0) {
                    (Some(e), Some(t)) => e.saturating_duration_since(t).as_millis().min(u32::MAX as u128) as u32,
                    _ => 0,
                };
                out.finish(end_ms)
            }
        }
    }
}


pub struct Quantized {
    pub palette: Vec<u8>,
    pub indices: Vec<u8>,
    pub transparent: Option<u8>,
}

#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0.rotate_left(5) ^ *b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.0 = (self.0.rotate_left(5) ^ v as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

pub fn quantize(rgba: &[u8], w: usize, h: usize, quality: u8, dither: bool) -> Quantized {
    quantize_with(rgba, w, h, quality, dither, false)
}

pub fn quantize_with(rgba: &[u8], w: usize, h: usize, quality: u8, dither: bool, spare: bool) -> Quantized {
    let opaque = |p: &[u8]| p[3] >= 128;
    let mut map: HashMap<u32, u8, BuildHasherDefault<Fx>> = HashMap::default();
    let mut palette: Vec<u8> = Vec::new();
    let mut has_clear = false;
    let mut exact = true;
    let mut indices = vec![0u8; w * h];
    for (px, out) in rgba.as_chunks::<4>().0.iter().zip(indices.iter_mut()) {
        if !opaque(px) {
            has_clear = true;
            continue;
        }
        let key = u32::from_le_bytes([px[0], px[1], px[2], 0]);
        let next = map.len();
        let idx = *map.entry(key).or_insert(next as u8);
        if map.len() > 256 {
            exact = false;
            break;
        }
        if idx as usize == next {
            palette.extend_from_slice(&px[..3]);
        }
        *out = idx;
    }
    if map.len() > 255 && (has_clear || spare) {
        exact = false;
    }
    if exact {
        let transparent = has_clear.then(|| {
            let t = (palette.len() / 3) as u8;
            palette.extend_from_slice(&[0, 0, 0]);
            t
        });
        if let Some(t) = transparent {
            for (px, out) in rgba.as_chunks::<4>().0.iter().zip(indices.iter_mut()) {
                if !opaque(px) {
                    *out = t;
                }
            }
        }
        if palette.is_empty() {
            palette.extend_from_slice(&[0, 0, 0]);
        }
        return Quantized { palette, indices, transparent };
    }

    let sample_cap = [48_000usize, 128_000, 256_000][quality.min(2) as usize];
    let total = rgba.as_chunks::<4>().0.iter().filter(|&p| opaque(p)).count();
    let stride = total.div_ceil(sample_cap).max(1);
    let mut hist: HashMap<u32, u32, BuildHasherDefault<Fx>> = HashMap::default();
    for (k, px) in rgba.as_chunks::<4>().0.iter().filter(|&p| opaque(p)).enumerate() {
        if k % stride == 0 {
            *hist.entry(u32::from_le_bytes([px[0], px[1], px[2], 0])).or_insert(0) += 1;
        }
    }
    let mut entries: Vec<Entry> = hist.into_iter().map(|(k, n)| Entry { c: [k as u8, (k >> 8) as u8, (k >> 16) as u8], w: (n as f32).sqrt() }).collect();
    entries.sort_unstable_by_key(|e| e.c);
    let centers = median_cut(&mut entries, 255, [0usize, 1, 2][quality.min(2) as usize]);
    let n = centers.len().max(1);
    let mut palette: Vec<u8> = centers.iter().flatten().copied().collect();
    if palette.is_empty() {
        palette.extend_from_slice(&[0, 0, 0]);
    }
    let has_clear = has_clear || rgba.as_chunks::<4>().0.iter().any(|p| !opaque(p));
    let transparent = has_clear.then(|| {
        let t = n as u8;
        palette.extend_from_slice(&[0, 0, 0]);
        t
    });
    let amp = if dither { [6i32, 10, 14][quality.min(2) as usize] } else { 0 };
    let pal: Vec<[i32; 3]> = palette[..n * 3].as_chunks::<3>().0.iter().map(|c| [c[0] as i32, c[1] as i32, c[2] as i32]).collect();
    crate::par::rows(&mut indices, w.max(1), |y, row| {
        let mut cache = [(u32::MAX, 0u8); 256];
        let mut find = |c: [u8; 3]| -> usize {
            let key = u32::from_le_bytes([c[0], c[1], c[2], 0]);
            let slot = (key.wrapping_mul(0x9E37_79B1) >> 24) as usize;
            if cache[slot].0 == key {
                return cache[slot].1 as usize;
            }
            let i = nearest(&pal, [c[0] as i32, c[1] as i32, c[2] as i32]);
            cache[slot] = (key, i as u8);
            i
        };
        for (x, out) in row.iter_mut().enumerate() {
            let i = (y * w + x) * 4;
            let px = &rgba[i..i + 4];
            if !opaque(px) {
                *out = transparent.unwrap_or(0);
                continue;
            }
            let first = find([px[0], px[1], px[2]]);
            *out = first as u8;
            if amp > 0 {
                let near = &palette[first * 3..first * 3 + 3];
                let err = (0..3).map(|k| (px[k] as i32 - near[k] as i32).abs()).max().unwrap_or(0);
                if err > 3 {
                    let t = BAYER8[y & 7][x & 7] as i32 * 2 - 63;
                    let d = t * err.min(amp) / 63;
                    let mut c = [px[0], px[1], px[2]];
                    for v in &mut c {
                        *v = (*v as i32 + d).clamp(0, 255) as u8;
                    }
                    *out = find(c) as u8;
                }
            }
        }
    });
    Quantized { palette, indices, transparent }
}

struct Entry {
    c: [u8; 3],
    w: f32,
}

const CW: [f64; 3] = [3.0, 4.0, 2.0];

fn median_cut(entries: &mut [Entry], max: usize, refine: usize) -> Vec<[u8; 3]> {
    struct Cell {
        lo: usize,
        hi: usize,
        err: f64,
        axis: usize,
    }
    let measure = |e: &[Entry]| -> (f64, usize) {
        let (mut sw, mut s, mut s2) = (0f64, [0f64; 3], [0f64; 3]);
        for x in e {
            let w = x.w as f64;
            sw += w;
            for k in 0..3 {
                let v = x.c[k] as f64;
                s[k] += w * v;
                s2[k] += w * v * v;
            }
        }
        let (mut total, mut best, mut axis) = (0f64, -1f64, 0usize);
        for k in 0..3 {
            let var = CW[k] * (s2[k] - s[k] * s[k] / sw.max(1e-9)).max(0.0);
            total += var;
            if var > best {
                best = var;
                axis = k;
            }
        }
        (total, axis)
    };
    let mut cells: Vec<Cell> = Vec::with_capacity(max);
    if !entries.is_empty() {
        let (err, axis) = measure(entries);
        cells.push(Cell { lo: 0, hi: entries.len(), err, axis });
    }
    while cells.len() < max {
        let Some((at, _)) = cells.iter().enumerate().filter(|(_, c)| c.hi - c.lo > 1 && c.err > 0.0).max_by(|a, b| a.1.err.total_cmp(&b.1.err)) else {
            break;
        };
        let c = cells.swap_remove(at);
        let part = &mut entries[c.lo..c.hi];
        part.sort_unstable_by_key(|e| e.c[c.axis]);
        let half = part.iter().map(|e| e.w as f64).sum::<f64>() / 2.0;
        let (mut acc, mut cut) = (0f64, 1usize);
        for (i, e) in part.iter().enumerate().take(part.len() - 1) {
            acc += e.w as f64;
            cut = i + 1;
            if acc >= half {
                break;
            }
        }
        let mid = c.lo + cut;
        let (e1, a1) = measure(&entries[c.lo..mid]);
        let (e2, a2) = measure(&entries[mid..c.hi]);
        cells.push(Cell { lo: c.lo, hi: mid, err: e1, axis: a1 });
        cells.push(Cell { lo: mid, hi: c.hi, err: e2, axis: a2 });
    }
    let mean = |e: &[Entry]| -> [f64; 3] {
        let (mut sw, mut s) = (0f64, [0f64; 3]);
        for x in e {
            sw += x.w as f64;
            for (sum, c) in s.iter_mut().zip(x.c) {
                *sum += x.w as f64 * c as f64;
            }
        }
        [s[0] / sw.max(1e-9), s[1] / sw.max(1e-9), s[2] / sw.max(1e-9)]
    };
    let mut centers: Vec<[f64; 3]> = cells.iter().map(|c| mean(&entries[c.lo..c.hi])).collect();
    if refine > 0 && centers.len() > 1 {
        let step = entries.len().div_ceil(12_000).max(1);
        for _ in 0..refine {
            let mut acc = vec![(0f64, [0f64; 3]); centers.len()];
            for e in entries.iter().step_by(step) {
                let mut best = (f64::MAX, 0usize);
                for (i, m) in centers.iter().enumerate() {
                    let d = (0..3).map(|k| CW[k] * (m[k] - e.c[k] as f64).powi(2)).sum::<f64>();
                    if d < best.0 {
                        best = (d, i);
                    }
                }
                let a = &mut acc[best.1];
                a.0 += e.w as f64;
                for k in 0..3 {
                    a.1[k] += e.w as f64 * e.c[k] as f64;
                }
            }
            for (m, a) in centers.iter_mut().zip(&acc) {
                if a.0 > 0.0 {
                    *m = [a.1[0] / a.0, a.1[1] / a.0, a.1[2] / a.0];
                }
            }
        }
    }
    let mut out: Vec<[u8; 3]> = centers.iter().map(|m| [m[0].round() as u8, m[1].round() as u8, m[2].round() as u8]).collect();
    out.sort_unstable_by_key(|c| (c[1], c[0], c[2]));
    out.dedup();
    out
}

fn nearest(pal: &[[i32; 3]], c: [i32; 3]) -> usize {
    let start = pal.partition_point(|p| p[1] < c[1]);
    let dist = |p: &[i32; 3]| 3 * (p[0] - c[0]).pow(2) + 4 * (p[1] - c[1]).pow(2) + 2 * (p[2] - c[2]).pow(2);
    let (mut best, mut best_d) = (start.min(pal.len() - 1), i32::MAX);
    let (mut up, mut down) = (start, start);
    loop {
        let mut moved = false;
        if up < pal.len() {
            if 4 * (pal[up][1] - c[1]).pow(2) >= best_d {
                up = pal.len();
            } else {
                let d = dist(&pal[up]);
                if d < best_d {
                    (best, best_d) = (up, d);
                }
                up += 1;
                moved = true;
            }
        }
        if down > 0 {
            if 4 * (pal[down - 1][1] - c[1]).pow(2) >= best_d {
                down = 0;
            } else {
                let d = dist(&pal[down - 1]);
                if d < best_d {
                    (best, best_d) = (down - 1, d);
                }
                down -= 1;
                moved = true;
            }
        }
        if !moved {
            return best;
        }
    }
}
